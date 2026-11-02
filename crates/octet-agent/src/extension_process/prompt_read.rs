use super::*;

pub(super) const FEATURE: &str = "run_system_prompt_read_v1";
static NEXT_SCOPE: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
pub(super) struct Mailbox {
    scopes: StdMutex<HashMap<ExtensionResourceOwner, Arc<Scope>>>,
}

struct Scope {
    id: String,
    text: String,
    cancellation: CancellationToken,
}

pub(super) struct Guard {
    mailbox: Arc<Mailbox>,
    owner: ExtensionResourceOwner,
    scope: Arc<Scope>,
}

impl Mailbox {
    pub(super) fn bind(
        self: &Arc<Self>,
        owner: ExtensionResourceOwner,
        text: &str,
        cancellation: CancellationToken,
    ) -> Guard {
        let scope = Arc::new(Scope {
            id: format!("run-prompt-{}", NEXT_SCOPE.fetch_add(1, Ordering::Relaxed)),
            text: text.to_owned(),
            cancellation,
        });
        lock_std_mutex(&self.scopes).insert(owner.clone(), scope.clone());
        Guard {
            mailbox: self.clone(),
            owner,
            scope,
        }
    }

    pub(super) fn scope(&self, owner: &ExtensionResourceOwner) -> Option<String> {
        lock_std_mutex(&self.scopes)
            .get(owner)
            .filter(|scope| !scope.cancellation.is_cancelled())
            .map(|scope| scope.id.clone())
    }
}

impl Guard {
    pub(super) fn scope(&self) -> &str {
        &self.scope.id
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut scopes = lock_std_mutex(&self.mailbox.scopes);
        if scopes
            .get(&self.owner)
            .is_some_and(|scope| Arc::ptr_eq(scope, &self.scope))
        {
            scopes.remove(&self.owner);
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    parent_request_id: u64,
    #[serde(default)]
    resource_owner: Option<ExtensionResourceOwner>,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

impl OwnerScopedHostRequest for Request {
    fn parent_request_id(&self) -> u64 {
        self.parent_request_id
    }
    fn request_resource_owner(&self) -> Option<&ExtensionResourceOwner> {
        self.resource_owner.as_ref()
    }
}

pub(super) fn respond(
    state: &ProtocolReadState,
    request: &Request,
    admitted: &AdmittedExtensionRequest,
    scope_id: &str,
) -> Result<(), String> {
    let refuse =
        |failure, detail: &str| refuse_admitted_request(state, admitted, (failure, detail.into()));
    if !read_std_lock(&state.protocol).supports(FEATURE) {
        return refuse(
            ExtensionRequestFailure::UnsupportedFeature,
            "run system prompt reads were not negotiated",
        );
    }
    let pending = lock_std_mutex(&state.pending);
    if request.parent_request_id != 0
        && (lock_std_mutex(&state.tombstones).contains(request.parent_request_id)
            || !pending
                .get(&request.parent_request_id)
                .is_some_and(|parent| {
                    parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                        && parent.resource_owner.as_ref() == Some(&admitted.owner)
                }))
    {
        return refuse(
            ExtensionRequestFailure::NotForegroundOwner,
            "run prompt parent is no longer live",
        );
    }
    let issued = lock_std_mutex(&state.issued_resource_owners);
    if !issued.contains(&admitted.owner) {
        return refuse(
            ExtensionRequestFailure::NotForegroundOwner,
            "run prompt owner was retired",
        );
    }
    let scopes = lock_std_mutex(&state.prompt_reads.scopes);
    if state.closed.load(Ordering::Acquire) || state.draining.load(Ordering::Acquire) {
        return refuse(
            ExtensionRequestFailure::NotForegroundOwner,
            "run prompt owner is closed or draining",
        );
    }
    let Some(scope) = scopes
        .get(&admitted.owner)
        .filter(|scope| scope.id == scope_id && !scope.cancellation.is_cancelled())
    else {
        return refuse(
            ExtensionRequestFailure::NotForegroundOwner,
            "run prompt scope is no longer live",
        );
    };
    if scope.text.len() > MAX_EXTENSION_SYSTEM_PROMPT_BYTES {
        return refuse(
            ExtensionRequestFailure::BoundsExceeded,
            "run system prompt exceeds the disclosure bound",
        );
    }
    if scope.text.contains('\0') {
        return refuse(
            ExtensionRequestFailure::InvalidRequest,
            "run system prompt contains NUL",
        );
    }
    try_queue_child_response(
        &state.child_requests,
        &admitted.request_id,
        &state.writer,
        state.max_message_bytes(),
        serde_json::json!({"jsonrpc":"2.0", "id":admitted.request_id, "result":{"text":scope.text}}),
    ).map(|_| ())
}

#[cfg(test)]
#[path = "prompt_read_tests.rs"]
mod tests;
