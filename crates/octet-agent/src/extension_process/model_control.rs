//! Bounded owner-scoped model controls on the existing foreground driver.
use super::*;

/// Secret-free request for an authoritative host selection.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionModelControl {
    /// Select an already configured provider/model pair.
    Model {
        /// Pi provider identity, not a credential or arbitrary endpoint.
        provider: String,
        /// Provider-facing model identifier.
        id: String,
    },
    /// Change the effective portable reasoning selection.
    Thinking {
        /// One of the seven public Pi thinking levels.
        level: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    parent_request_id: u64,
    resource_owner: ExtensionResourceOwner,
    selection: ExtensionModelControl,
}
impl OwnerScopedHostRequest for Request {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        Some(&self.resource_owner)
    }
}

pub(super) fn dispatch_model_control(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    params: serde_json::Value,
) -> Result<(), String> {
    let Some((request, admitted)) = admit_host_request::<Request>(
        state,
        object,
        "model/select",
        EXTENSION_FEATURE_SESSION_CONTROL_V1,
        params,
    )?
    else {
        return Ok(());
    };
    if let Err(failure) = validate_explicit_request_owner(state, &admitted.owner) {
        return refuse_admitted_request(state, &admitted, failure);
    }
    let valid = match &request.selection {
        ExtensionModelControl::Model { provider, id } => [provider, id].into_iter().all(|value| {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }),
        ExtensionModelControl::Thinking { level } => matches!(
            level.as_str(),
            "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ),
    };
    if !valid {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::InvalidRequest,
                "invalid model selection".into(),
            ),
        );
    }
    if lock_std_mutex(&state.tombstones).contains(request.parent_request_id) {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "model selection parent was cancelled".into(),
            ),
        );
    }
    let Some(service) = state
        .session_lifecycle
        .as_ref()
        .filter(|service| service.supports_model_control())
    else {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::UnsupportedFeature,
                "model selection consumer unavailable".into(),
            ),
        );
    };
    // Until the request-boundary control is integrated, reject an active tool or
    // hook promptly rather than park a synchronous request behind its own run.
    let live_command = lock_std_mutex(&state.pending)
        .get(&request.parent_request_id)
        .is_some_and(|parent| parent.method == "command/execute");
    if !live_command {
        return refuse_admitted_request(
            state,
            &admitted,
            (
                ExtensionRequestFailure::UnsupportedFeature,
                "active-turn model selection requires the request-boundary consumer".into(),
            ),
        );
    }
    let response_state = {
        let children = lock_std_mutex(&state.child_requests);
        let Some(child) = children.get(&admitted.request_id) else {
            return Ok(());
        };
        Arc::clone(&child.response_state)
    };
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            return refuse_admitted_request(
                state,
                &admitted,
                (
                    ExtensionRequestFailure::BoundsExceeded,
                    "model selection worker limit exceeded".into(),
                ),
            )
        }
    };
    let authority = SessionCompactionAuthority {
        owner: request.resource_owner,
        issued: Arc::clone(&state.issued_resource_owners),
        closed: Arc::clone(&state.closed),
        draining: Arc::clone(&state.draining),
        response: Arc::clone(&response_state),
    };
    let receiver = match service.try_submit_model_control(request.selection, authority) {
        Ok(receiver) => receiver,
        Err(error) => {
            return refuse_admitted_request(
                state,
                &admitted,
                (
                    match error {
                        SessionLifecycleSubmitError::Full => {
                            ExtensionRequestFailure::BoundsExceeded
                        }
                        SessionLifecycleSubmitError::Unavailable => {
                            ExtensionRequestFailure::NotForegroundOwner
                        }
                    },
                    "model selection driver unavailable or full".into(),
                ),
            )
        }
    };
    let writer = state.writer.clone();
    let frame_limit = Arc::clone(&state.frame_limit);
    let child_requests = Arc::clone(&state.child_requests);
    let health = Arc::clone(&state.health);
    let events = state.events.clone();
    let request_id = admitted.request_id;
    tokio::spawn(async move {
        let result = tokio::select! {
            biased;
            _ = child_response_settled(response_state) => return,
            result = receiver => result.unwrap_or_else(|_| Err("model selection consumer retired".into())),
        };
        let response = match result {
            Ok(result) => serde_json::json!({"jsonrpc":"2.0","id":request_id,"result":result}),
            Err(message) => {
                serde_json::json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32002,"message":message}})
            }
        };
        deliver_api_v03_session_lifecycle_response(
            child_requests,
            request_id,
            writer,
            frame_limit,
            health,
            events,
            response,
        )
        .await;
        drop(worker);
    });
    Ok(())
}
