use super::*;
use crate::tool_composition::ToolCompositionService;
use serde_json::{json, Value};

#[derive(Default)]
struct PreflightService {
    calls: StdMutex<Vec<Value>>,
}

#[async_trait::async_trait]
impl ToolCompositionService for PreflightService {
    async fn context(&self) -> Result<Value, ToolError> {
        Ok(json!({}))
    }
    async fn call(&self, _: String, _: Value, _: CancellationToken) -> Result<Value, ToolError> {
        panic!("preflight must not fall through to ordinary call")
    }
    async fn prepare(
        &self,
        name: String,
        arguments: Value,
        builtin: bool,
        _: CancellationToken,
    ) -> Result<Value, ToolError> {
        lock_std_mutex(&self.calls)
            .push(json!({"kind":"prepare","name":name,"arguments":arguments,"builtin":builtin}));
        Ok(
            json!({"prepared_id":"receipt","tool_call":{"id":"outer/1","name":name,"arguments":arguments}}),
        )
    }
    async fn call_prepared(
        &self,
        prepared_id: String,
        name: String,
        arguments: Value,
        builtin: bool,
        _: CancellationToken,
        _: Option<mpsc::Sender<Value>>,
    ) -> Result<Value, ToolError> {
        lock_std_mutex(&self.calls).push(json!({"kind":"prepared","prepared_id":prepared_id,"name":name,"arguments":arguments,"builtin":builtin}));
        Ok(
            json!({"tool_call":{"id":"outer/1","name":name,"arguments":arguments},"content":[{"Text":"prepared"}],"is_error":false}),
        )
    }
    async fn call_builtin(
        &self,
        name: String,
        arguments: Value,
        _: CancellationToken,
        full_outcome: bool,
        _: Option<mpsc::Sender<Value>>,
    ) -> Result<Value, ToolError> {
        lock_std_mutex(&self.calls).push(
            json!({"kind":"builtin","name":name,"arguments":arguments,"full_outcome":full_outcome}),
        );
        Ok(json!({"content":[{"Text":"builtin"}],"is_error":false}))
    }
    async fn store(
        &self,
        _: serde_json::Map<String, Value>,
        _: Vec<String>,
    ) -> Result<(), ToolError> {
        Ok(())
    }
}

fn state() -> (
    ProtocolReadState,
    mpsc::Receiver<WriterFrame>,
    Arc<PreflightService>,
) {
    let (events, _) = broadcast::channel(8);
    let (state, frames) =
        super::super::tests::protocol_read_state_for_test(ManifestContributions::default(), events);
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = "0.4".into();
        protocol
            .features
            .insert(EXTENSION_FEATURE_TOOL_COMPOSITION.into());
        protocol
            .features
            .insert("composition_builtin_tools_v1".into());
        protocol.features.insert("composition_preflight_v1".into());
    }
    let owner = ExtensionResourceOwner {
        session_id: "preflight-owner".into(),
        extension_instance_id: state.instance_id.clone(),
        process_generation: state.generation,
    };
    lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    let service = Arc::new(PreflightService::default());
    let progress = ToolProgressSink::null().with_composition(service.clone());
    let (sender, _) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        7,
        PendingRequest {
            method: "tool/call".into(),
            sender,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: Some(progress.clone()),
            child_interaction_progress: Some(progress),
            resource_owner: Some(owner),
            last_progress_sequence: None,
            tool_call_policy_digest: Some([0; 32]),
            bulk_hook_parent: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
    (state, frames, service)
}

fn request(state: &ProtocolReadState, id: u64, method: &str, params: Value) {
    handle_protocol_line(
        &serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .unwrap(),
        state,
    )
    .unwrap();
}

async fn response(frames: &mut mpsc::Receiver<WriterFrame>) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), frames.recv())
        .await
        .unwrap()
        .unwrap();
    serde_json::from_slice(&frame.line).unwrap()
}

#[tokio::test]
async fn preflight_and_builtin_transport_preserve_target_receipt_and_value_envelope() {
    let (state, mut frames, service) = state();
    request(
        &state,
        100,
        "composition/prepare",
        json!({"parent_request_id":7,"name":"read","arguments":4,"builtin":true}),
    );
    let prepared = response(&mut frames).await;
    assert_eq!(
        prepared["result"]["value"],
        json!({"prepared_id":"receipt","tool_call":{"id":"outer/1","name":"read","arguments":4}})
    );
    request(
        &state,
        101,
        "composition/call",
        json!({"parent_request_id":7,"name":"read","arguments":4,"builtin":true,"prepared_id":"receipt","full_outcome":true}),
    );
    assert_eq!(
        response(&mut frames).await["result"]["value"]["content"][0]["Text"],
        "prepared"
    );
    request(
        &state,
        102,
        "composition/call",
        json!({"parent_request_id":7,"name":"read","arguments":{},"builtin":true,"full_outcome":true}),
    );
    assert_eq!(
        response(&mut frames).await["result"]["value"]["content"][0]["Text"],
        "builtin"
    );
    assert_eq!(
        *lock_std_mutex(&service.calls),
        vec![
            json!({"kind":"prepare","name":"read","arguments":4,"builtin":true}),
            json!({"kind":"prepared","name":"read","arguments":4,"builtin":true,"prepared_id":"receipt"}),
            json!({"kind":"builtin","name":"read","arguments":{},"full_outcome":true}),
        ]
    );
}

#[tokio::test]
async fn preflight_and_builtin_transport_require_each_negotiated_feature() {
    let (state, mut frames, service) = state();
    write_std_lock(&state.protocol)
        .features
        .remove("composition_preflight_v1");
    request(
        &state,
        100,
        "composition/prepare",
        json!({"parent_request_id":7,"name":"read","arguments":{}}),
    );
    assert_eq!(response(&mut frames).await["error"]["code"], -32601);
    request(
        &state,
        101,
        "composition/call",
        json!({"parent_request_id":7,"name":"read","arguments":{},"prepared_id":"receipt","full_outcome":true}),
    );
    assert_eq!(response(&mut frames).await["error"]["code"], -32601);
    write_std_lock(&state.protocol)
        .features
        .insert("composition_preflight_v1".into());
    write_std_lock(&state.protocol)
        .features
        .remove("composition_builtin_tools_v1");
    for (id, method, extra) in [
        (102, "composition/prepare", json!({})),
        (103, "composition/call", json!({"full_outcome":true})),
    ] {
        let mut params = json!({"parent_request_id":7,"name":"read","arguments":{},"builtin":true});
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        request(&state, id, method, params);
        assert_eq!(response(&mut frames).await["error"]["code"], -32601);
    }
    assert!(lock_std_mutex(&service.calls).is_empty());
}

#[test]
fn preflight_negotiation_requires_api_04_the_base_feature_and_an_offered_service() {
    use super::tests::{initialize, manifest};
    for feature in ["composition_builtin_tools_v1", "composition_preflight_v1"] {
        let mut response = initialize("0.4", true);
        response
            .protocol
            .as_mut()
            .unwrap()
            .features
            .push(feature.into());
        let offer = OfferedHostServices {
            tool_composition: true,
            ..OfferedHostServices::default()
        };
        let (_, protocol) = negotiate_contributions_with_host_services(
            &manifest("0.4"),
            response.clone(),
            4,
            offer,
        )
        .unwrap();
        assert!(protocol.supports(feature));
        assert!(negotiate_contributions_with_host_services(
            &manifest("0.4"),
            response.clone(),
            4,
            OfferedHostServices::default()
        )
        .is_err());
        response
            .protocol
            .as_mut()
            .unwrap()
            .features
            .retain(|name| name != EXTENSION_FEATURE_TOOL_COMPOSITION);
        assert!(
            negotiate_contributions_with_host_services(&manifest("0.4"), response, 4, offer)
                .is_err()
        );
        let mut response = initialize("0.2", false);
        response.tools[0].composition = None;
        response.tools[0].constrained_sampling = None;
        assert!(negotiate_contributions_with_host_services(
            &manifest("0.2"),
            response.clone(),
            4,
            offer,
        )
        .is_ok());
        response
            .protocol
            .as_mut()
            .unwrap()
            .features
            .push(feature.into());
        assert!(
            negotiate_contributions_with_host_services(&manifest("0.2"), response, 4, offer)
                .is_err()
        );
    }
}

#[test]
fn prepared_composition_params_remain_bounded_and_require_complete_outcomes() {
    for params in [
        json!({"parent_request_id":7,"name":"read","arguments":{},"prepared_id":"receipt"}),
        json!({"parent_request_id":7,"name":"read","arguments":{},"prepared_id":"","full_outcome":true}),
        json!({"parent_request_id":7,"name":"read","arguments":{},"prepared_id":"x".repeat(513),"full_outcome":true}),
        json!({"parent_request_id":7,"name":"read","arguments":{},"builtin":"true","full_outcome":true}),
    ] {
        assert!(parse_composition_operation(CompositionMethod::Call, params).is_err());
    }
    for params in [
        json!({"parent_request_id":7,"name":"","arguments":{}}),
        json!({"parent_request_id":7,"name":"read","arguments":{"huge":"x".repeat(MAX_COMPOSITION_ARGUMENT_BYTES)}}),
        json!({"parent_request_id":7,"name":"read","arguments":{},"prepared_id":"receipt"}),
    ] {
        assert!(parse_composition_operation(CompositionMethod::Prepare, params).is_err());
    }
}
