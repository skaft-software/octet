use super::*;
use crate::tool_composition::{ToolCompositionMode, ToolCompositionService};
use octet_ai::{ConstrainedSampling, GrammarVariants};
use serde_json::{json, Value};

fn definition() -> ToolDefinition {
    ToolDefinition {
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
        operation: None,
        name: "compose".into(),
        description: "Run a program".into(),
        parameters: json!({"type":"object","properties":{"code":{"type":"string"}},"required":["code"]}),
        output_schema: None,
        composition: Some(ToolCompositionConfig {
            mode: ToolCompositionMode::Only,
            inline_budget: 16_000,
        }),
        constrained_sampling: Some(ConstrainedSampling::Grammar {
            variants: GrammarVariants {
                openai_lark: Some("start: /.+/".into()),
                openai_regex: None,
            },
        }),
    }
}

fn manifest(version: &str) -> ExtensionManifest {
    ExtensionManifest::parse(&format!(
        r#"name = "generic-composition-probe"
version = "0.1.0"
api_version = "{version}"
[entrypoint]
command = "probe"
[contributes]
tools = ["compose"]
"#
    ))
    .unwrap()
}

fn initialize(version: &str, feature: bool) -> InitializeResponse {
    let mut features = API_0_2_REQUIRED_FEATURES
        .iter()
        .map(|feature| (*feature).to_owned())
        .collect::<Vec<_>>();
    if feature {
        features.push(EXTENSION_FEATURE_TOOL_COMPOSITION.into());
    }
    InitializeResponse {
        api_version: version.into(),
        tools: vec![definition()],
        commands: Vec::new(),
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: Some(ExtensionProtocolResponse {
            version: version.into(),
            features,
            limits: ExtensionProtocolLimits {
                resource_refs_v1: None,
                max_concurrent_requests: 4,
            },
            lifecycle_events: Vec::new(),
        }),
    }
}

#[test]
fn composition_negotiation_is_generic_opt_in_and_api_04_only() {
    assert!(!ExtensionRuntimeConfig::new(".").tool_composition);
    assert!(!API_0_2_OPTIONAL_FEATURES.contains(&EXTENSION_FEATURE_TOOL_COMPOSITION));
    let offer = OfferedHostServices {
        tool_composition: true,
        ..OfferedHostServices::default()
    };
    let (_, protocol) = negotiate_contributions_with_host_services(
        &manifest("0.4"),
        initialize("0.4", true),
        4,
        offer,
    )
    .unwrap();
    assert!(protocol.supports(EXTENSION_FEATURE_TOOL_COMPOSITION));
    assert!(negotiate_contributions_with_host_services(
        &manifest("0.4"),
        initialize("0.4", true),
        4,
        OfferedHostServices::default()
    )
    .is_err());
    assert!(negotiate_contributions_with_host_services(
        &manifest("0.2"),
        initialize("0.2", true),
        4,
        offer
    )
    .is_err());
    assert!(negotiate_contributions_with_host_services(
        &manifest("0.4"),
        initialize("0.4", false),
        4,
        offer
    )
    .is_err());
}

#[test]
fn composition_definition_validates_budget_grammar_and_old_wires() {
    let good = definition();
    validate_tool_definitions(std::slice::from_ref(&good), "0.4").unwrap();
    for api in ["0.1", "0.2", "0.3"] {
        assert!(validate_tool_definitions(std::slice::from_ref(&good), api).is_err());
    }
    let mut bad = good.clone();
    bad.composition.as_mut().unwrap().inline_budget = 16_001;
    assert!(validate_tool_definitions(&[bad], "0.4")
        .unwrap_err()
        .to_string()
        .contains("inline_budget"));
    for variants in [
        GrammarVariants::default(),
        GrammarVariants {
            openai_lark: Some(" ".into()),
            openai_regex: None,
        },
        GrammarVariants {
            openai_lark: Some("a".repeat(65_537)),
            openai_regex: None,
        },
        GrammarVariants {
            openai_lark: Some("a".repeat(32_769)),
            openai_regex: Some("b".repeat(32_768)),
        },
    ] {
        let mut bad = good.clone();
        bad.constrained_sampling = Some(ConstrainedSampling::Grammar { variants });
        assert!(validate_tool_definitions(&[bad], "0.4").is_err());
    }
    for schema in [
        json!({}),
        json!({"type":"string"}),
        json!({"type":"object","required":[]}),
        json!({"type":"object","required":["code"],"properties":{"code":{"type":"number"}}}),
    ] {
        let mut bad = good.clone();
        bad.parameters = schema;
        assert!(validate_tool_definitions(&[bad], "0.4").is_err());
    }
    let old: ToolDefinition =
        serde_json::from_value(json!({"name":"plain","description":"Plain","parameters":{}}))
            .unwrap();
    let serialized = serde_json::to_value(old).unwrap();
    assert_eq!(
        serialized,
        json!({"name":"plain","description":"Plain","parameters":{},"output_schema":null})
    );
    for composition in [
        json!({"mode":"off","inline_budget":0}),
        json!({"mode":"on","inline_budget":0,"resource_owner":"override"}),
    ] {
        let mut value = serde_json::to_value(&good).unwrap();
        value["composition"] = composition;
        assert!(serde_json::from_value::<ToolDefinition>(value).is_err());
    }
}

type StoreWrites = (serde_json::Map<String, Value>, Vec<String>);

#[derive(Default)]
struct FakeCompositionService {
    calls: StdMutex<Vec<(String, Value)>>,
    writes: StdMutex<Vec<StoreWrites>>,
    contexts: AtomicUsize,
    entered: Notify,
    release: Notify,
    wait: AtomicBool,
    dropped: AtomicBool,
    token: StdMutex<Option<CancellationToken>>,
    result: StdMutex<Option<Value>>,
    error: StdMutex<Option<String>>,
}

struct CallDrop<'a>(&'a FakeCompositionService);
impl Drop for CallDrop<'_> {
    fn drop(&mut self) {
        self.0.dropped.store(true, Ordering::Release);
    }
}

#[async_trait::async_trait]
impl ToolCompositionService for FakeCompositionService {
    async fn context(&self) -> Result<Value, ToolError> {
        self.contexts.fetch_add(1, Ordering::AcqRel);
        Ok(
            json!({"tools":[{"name":"echo","description":"Echo","parameters":{"type":"object"},"output_schema":{"type":"object"}}],"store":{},"limits":{"timeout_ms":30_000,"max_calls":256}}),
        )
    }
    async fn call(
        &self,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError> {
        let _drop = CallDrop(self);
        lock_std_mutex(&self.calls).push((name, arguments));
        *lock_std_mutex(&self.token) = Some(cancellation);
        self.entered.notify_one();
        if self.wait.load(Ordering::Acquire) {
            self.release.notified().await;
        }
        if let Some(message) = lock_std_mutex(&self.error).clone() {
            return Err(ToolError::new(message));
        }
        Ok(lock_std_mutex(&self.result)
            .clone()
            .unwrap_or(json!({"ok":true})))
    }
    async fn store(
        &self,
        set: serde_json::Map<String, Value>,
        delete: Vec<String>,
    ) -> Result<(), ToolError> {
        lock_std_mutex(&self.writes).push((set, delete));
        Ok(())
    }
}

fn state() -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
    let (events, _) = broadcast::channel(8);
    let (state, frames) =
        super::super::tests::protocol_read_state_for_test(ManifestContributions::default(), events);
    let mut protocol = write_std_lock(&state.protocol);
    protocol.version = "0.4".into();
    protocol
        .features
        .insert(EXTENSION_FEATURE_TOOL_COMPOSITION.into());
    drop(protocol);
    (state, frames)
}

fn parent(state: &ProtocolReadState, id: u64, service: Arc<FakeCompositionService>) {
    let owner = ExtensionResourceOwner {
        session_id: format!("owner-{id}"),
        extension_instance_id: state.instance_id.clone(),
        process_generation: state.generation,
    };
    lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    let progress = ToolProgressSink::null().with_composition(service);
    let (sender, _) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        id,
        PendingRequest {
            sender,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: Some(progress.clone()),
            child_interaction_progress: Some(progress),
            resource_owner: Some(owner),
            last_progress_sequence: None,
            tool_call_policy_digest: Some([0; 32]),
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
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

async fn workers_finished(state: &ProtocolReadState) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.child_work_slots.available_permits() != MAX_CHILD_WORKERS {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn composition_context_call_and_store_reach_only_the_bound_dispatcher() {
    let (state, mut frames) = state();
    let first = Arc::new(FakeCompositionService::default());
    let second = Arc::new(FakeCompositionService::default());
    parent(&state, 7, first.clone());
    parent(&state, 8, second.clone());
    request(
        &state,
        100,
        methods::COMPOSITION_CONTEXT,
        json!({"parent_request_id":7}),
    );
    let context = response(&mut frames).await;
    assert_eq!(
        context["result"]["limits"],
        json!({"timeout_ms":30000,"max_calls":256})
    );
    assert_eq!(context["result"]["tools"][0]["name"], "echo");
    request(
        &state,
        101,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"echo","arguments":{"text":"hello"}}),
    );
    assert_eq!(
        response(&mut frames).await["result"],
        json!({"value":{"ok":true}})
    );
    request(
        &state,
        102,
        methods::COMPOSITION_STORE,
        json!({"parent_request_id":7,"set":{"answer":42},"delete":["old"]}),
    );
    assert_eq!(response(&mut frames).await["result"], json!({}));
    workers_finished(&state).await;
    assert_eq!(
        *lock_std_mutex(&first.calls),
        vec![("echo".into(), json!({"text":"hello"}))]
    );
    assert_eq!(
        lock_std_mutex(&first.writes)[0],
        (
            serde_json::from_value(json!({"answer":42})).unwrap(),
            vec!["old".into()]
        )
    );
    assert_eq!(first.contexts.load(Ordering::Acquire), 1);
    assert!(lock_std_mutex(&second.calls).is_empty());
    assert_eq!(second.contexts.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn composition_params_and_unknown_outer_fields_are_nonfatal_refusals() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    parent(&state, 7, service.clone());
    let mut deep = json!(0);
    for _ in 0..32 {
        deep = json!({"nested":deep});
    }
    let cases = [
        (
            methods::COMPOSITION_CONTEXT,
            json!({"parent_request_id":7,"extra":true}),
        ),
        (
            methods::COMPOSITION_CONTEXT,
            json!({"parent_request_id":"7"}),
        ),
        (
            methods::COMPOSITION_CALL,
            json!({"parent_request_id":7,"name":"echo","arguments":[]}),
        ),
        (
            methods::COMPOSITION_CALL,
            json!({"parent_request_id":7,"name":"echo","arguments":{},"resource_owner":{"session_id":"override"}}),
        ),
        (
            methods::COMPOSITION_CALL,
            json!({"parent_request_id":7,"name":"n".repeat(129),"arguments":{}}),
        ),
        (
            methods::COMPOSITION_CALL,
            json!({"parent_request_id":7,"name":"echo","arguments":{"text":"x".repeat(MAX_COMPOSITION_ARGUMENT_BYTES)}}),
        ),
        (
            methods::COMPOSITION_CALL,
            json!({"parent_request_id":7,"name":"echo","arguments":{"deep":deep}}),
        ),
        (
            methods::COMPOSITION_STORE,
            json!({"parent_request_id":7,"set":[],"delete":[]}),
        ),
        (
            methods::COMPOSITION_STORE,
            json!({"parent_request_id":7,"set":{},"delete":[3]}),
        ),
        (
            methods::COMPOSITION_STORE,
            json!({"parent_request_id":7,"set":{},"delete":[],"owner":"override"}),
        ),
    ];
    for (offset, (method, params)) in cases.into_iter().enumerate() {
        request(&state, 100 + offset as u64, method, params);
        assert_eq!(response(&mut frames).await["error"]["code"], -32602);
    }
    let invalid = json!({"jsonrpc":"2.0","id":200,"method":methods::COMPOSITION_CONTEXT,"params":{"parent_request_id":7},"extra":true});
    handle_protocol_line(&serde_json::to_vec(&invalid).unwrap(), &state).unwrap();
    assert_eq!(response(&mut frames).await["error"]["code"], -32602);
    assert!(lock_std_mutex(&service.calls).is_empty());
    assert_eq!(service.contexts.load(Ordering::Acquire), 0);
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(!state.closed.load(Ordering::Acquire));
}

#[tokio::test]
async fn composition_missing_stale_command_hook_and_foreign_parents_are_denied() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    for (id, reason) in [
        (7, "command"),
        (8, "hook"),
        (9, "ownerless"),
        (10, "foreign"),
        (11, "stale"),
        (12, "unbound"),
    ] {
        parent(&state, id, service.clone());
        let mut pending = lock_std_mutex(&state.pending);
        let binding = pending.get_mut(&id).unwrap();
        match reason {
            "command" => binding.tool_call_policy_digest = None,
            "hook" => binding.child_interaction_progress = None,
            "ownerless" => binding.resource_owner = None,
            "foreign" => {
                binding
                    .resource_owner
                    .as_mut()
                    .unwrap()
                    .extension_instance_id = "other-process".into()
            }
            "stale" => binding.terminal.store(REQUEST_COMPLETED, Ordering::Release),
            "unbound" => binding.child_interaction_progress = Some(ToolProgressSink::null()),
            _ => unreachable!(),
        }
    }
    for id in [7, 8, 9, 10, 11, 12, 99] {
        request(
            &state,
            100 + id,
            methods::COMPOSITION_CONTEXT,
            json!({"parent_request_id":id}),
        );
        assert_eq!(response(&mut frames).await["error"]["code"], -32002);
    }
    assert_eq!(service.contexts.load(Ordering::Acquire), 0);
    for api in ["0.1", "0.2"] {
        write_std_lock(&state.protocol).version = api.into();
        request(
            &state,
            if api == "0.1" { 300 } else { 301 },
            methods::COMPOSITION_CONTEXT,
            json!({"parent_request_id":7}),
        );
        assert_eq!(response(&mut frames).await["error"]["code"], -32601);
    }
    write_std_lock(&state.protocol).version = "0.4".into();
    write_std_lock(&state.protocol)
        .features
        .remove(EXTENSION_FEATURE_TOOL_COMPOSITION);
    request(
        &state,
        302,
        methods::COMPOSITION_CONTEXT,
        json!({"parent_request_id":7}),
    );
    assert_eq!(response(&mut frames).await["error"]["code"], -32601);
}

#[tokio::test]
async fn composition_child_cancel_cancels_nested_token_drops_future_and_answers_once() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    service.wait.store(true, Ordering::Release);
    parent(&state, 7, service.clone());
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"wait","arguments":{}}),
    );
    service.entered.notified().await;
    handle_protocol_line(
        br#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":100}}"#,
        &state,
    )
    .unwrap();
    assert!(lock_std_mutex(&service.token)
        .as_ref()
        .unwrap()
        .is_cancelled());
    assert_eq!(response(&mut frames).await["error"]["code"], -32800);
    workers_finished(&state).await;
    assert!(service.dropped.load(Ordering::Acquire));
    service.release.notify_one();
    tokio::task::yield_now().await;
    assert!(frames.try_recv().is_err());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[tokio::test]
async fn composition_parent_settlement_cancels_call_and_spawn_race_has_no_effect() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    service.wait.store(true, Ordering::Release);
    parent(&state, 7, service.clone());
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"wait","arguments":{}}),
    );
    service.entered.notified().await;
    handle_protocol_line(br#"{"jsonrpc":"2.0","id":7,"result":{}}"#, &state).unwrap();
    assert!(lock_std_mutex(&service.token)
        .as_ref()
        .unwrap()
        .is_cancelled());
    workers_finished(&state).await;
    assert!(service.dropped.load(Ordering::Acquire));
    assert_eq!(
        response(&mut frames).await["method"],
        methods::CANCEL_REQUEST
    );
    assert!(frames.try_recv().is_err());
    parent(&state, 8, service.clone());
    let calls = lock_std_mutex(&service.calls).len();
    request(
        &state,
        101,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":8,"name":"must-not-run","arguments":{}}),
    );
    // Do not yield: settlement wins before the newly spawned worker is polled.
    handle_protocol_line(br#"{"jsonrpc":"2.0","id":8,"result":{}}"#, &state).unwrap();
    workers_finished(&state).await;
    assert_eq!(lock_std_mutex(&service.calls).len(), calls);
    assert_eq!(
        response(&mut frames).await["method"],
        methods::CANCEL_REQUEST
    );
}

#[tokio::test]
async fn composition_service_errors_and_worker_limit_do_not_break_transport() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    *lock_std_mutex(&service.error) = Some("tool is not in the effective loadout".into());
    parent(&state, 7, service.clone());
    let worker = state
        .child_work_slots
        .clone()
        .acquire_many_owned(MAX_CHILD_WORKERS as u32)
        .await
        .unwrap();
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"echo","arguments":{}}),
    );
    let error = response(&mut frames).await;
    assert_eq!(error["error"]["code"], -32002);
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("worker limit"));
    assert!(lock_std_mutex(&service.calls).is_empty());
    drop(worker);
    request(
        &state,
        101,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"echo","arguments":{}}),
    );
    let error = response(&mut frames).await;
    assert_eq!(
        error["error"],
        json!({"code":-32002,"message":"tool is not in the effective loadout"})
    );
    workers_finished(&state).await;
    assert!(!state.closed.load(Ordering::Acquire));
}

#[tokio::test]
async fn composition_oversized_values_spill_privately_and_parent_cleanup_removes_files() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    let value = json!({"text":"x".repeat(DEFAULT_EXTENSION_MESSAGE_BYTES)});
    *lock_std_mutex(&service.result) = Some(value.clone());
    parent(&state, 7, service.clone());
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"large","arguments":{}}),
    );
    let reply = response(&mut frames).await;
    let metadata = &reply["result"]["value_file"];
    let name = metadata["path"].as_str().unwrap();
    assert_eq!(Path::new(name).components().count(), 1);
    let path = state
        .artifact_store
        .scratch_directory(1)
        .unwrap()
        .join(name);
    let raw =
        crate::secure_fs::read_private_file_bounded(&path, MAX_COMPOSITION_FILE_BYTES).unwrap();
    assert_eq!(metadata["bytes"], raw.len());
    assert_eq!(metadata["sha256"], crate::tool::content_hash(&raw));
    assert_eq!(serde_json::from_slice::<Value>(&raw).unwrap(), value);
    workers_finished(&state).await;
    handle_protocol_line(br#"{"jsonrpc":"2.0","id":7,"result":{}}"#, &state).unwrap();
    assert!(!path.exists());
}

#[test]
fn composition_file_fallback_is_bounded_and_undelivered_files_are_removed() {
    let store = ArtifactStore::new().unwrap();
    store.begin_generation(1).unwrap();
    let files = CompositionFiles::default();
    let token = CancellationToken::default();
    let id = ExtensionRequestId::Number(1);
    let (line, file) = encode_composition_response(
        &id,
        CompositionResult::Context(json!({"tools":"a".repeat(1024)})),
        512,
        &store,
        1,
        &files,
        &token,
    )
    .unwrap();
    let reply: Value = serde_json::from_slice(&line).unwrap();
    assert!(reply["result"]["context_file"].is_object());
    let path = file.as_ref().unwrap().path.clone();
    assert!(path.exists());
    // This guard also runs if a cancelled spawn_blocking result is abandoned.
    drop(file);
    assert!(!path.exists());
    assert!(encode_composition_response(
        &id,
        CompositionResult::Call(json!("x".repeat(MAX_COMPOSITION_FILE_BYTES))),
        512,
        &store,
        1,
        &files,
        &token
    )
    .unwrap_err()
    .contains("8 MiB"));
    files.count.store(256, Ordering::Release);
    assert!(encode_composition_response(
        &id,
        CompositionResult::Call(json!("x".repeat(1024))),
        512,
        &store,
        1,
        &files,
        &token
    )
    .unwrap_err()
    .contains("256"));
    token.cancel();
    assert!(encode_composition_response(
        &id,
        CompositionResult::Call(json!(0)),
        512,
        &store,
        1,
        &files,
        &token
    )
    .unwrap_err()
    .contains("cancelled"));
}

#[test]
fn composition_response_cancel_race_preserves_exactly_one_terminal() {
    let (state, mut frames) = state();
    let id = ExtensionRequestId::Number(1);
    let registered = insert_child_request(&state, id.clone(), Some(7), None).unwrap();
    let token = CancellationToken::default();
    *lock_std_mutex(&registered.response_state.composition_cancellation) = Some(token.clone());
    assert!(cancel_composition_request(&state, &id).unwrap());
    assert!(token.is_cancelled());
    assert_eq!(
        try_queue_child_response(
            &state.child_requests,
            &id,
            &state.writer,
            state.max_message_bytes(),
            json!({"jsonrpc":"2.0","id":id,"result":{"value":"late"}})
        ),
        Ok(ChildResponseAdmission::AlreadySettled)
    );
    let response: Value = serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    assert_eq!(response["error"]["code"], -32800);
    assert!(frames.try_recv().is_err());
    let id = ExtensionRequestId::Number(2);
    let registered = insert_child_request(&state, id.clone(), Some(7), None).unwrap();
    *lock_std_mutex(&registered.response_state.composition_cancellation) =
        Some(CancellationToken::default());
    assert_eq!(
        try_queue_child_response(
            &state.child_requests,
            &id,
            &state.writer,
            state.max_message_bytes(),
            json!({"jsonrpc":"2.0","id":id,"result":{"value":"won"}})
        ),
        Ok(ChildResponseAdmission::Queued)
    );
    assert!(!cancel_composition_request(&state, &id).unwrap());
    let response: Value = serde_json::from_slice(&frames.try_recv().unwrap().line).unwrap();
    assert_eq!(response["result"]["value"], "won");
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn composition_lost_parent_before_child_notification_cannot_poll_dispatcher() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    parent(&state, 7, service.clone());
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"must-not-run","arguments":{}}),
    );
    // Simulate the interval between removing a terminal parent and publishing
    // child cancellation: the child is still ACTIVE when the task starts.
    lock_std_mutex(&state.pending).remove(&7);
    state.pending_changed.notify_waiters();
    workers_finished(&state).await;
    assert!(lock_std_mutex(&service.calls).is_empty());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(frames.try_recv().is_err());
}

#[tokio::test]
async fn composition_undeliverable_spill_response_unlinks_private_file() {
    let (mut state, frames) = state();
    let (writer, receiver) = mpsc::channel(1);
    drop(receiver);
    drop(frames);
    state.writer = writer;
    let service = Arc::new(FakeCompositionService::default());
    *lock_std_mutex(&service.result) = Some(json!("x".repeat(DEFAULT_EXTENSION_MESSAGE_BYTES)));
    parent(&state, 7, service);
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"large","arguments":{}}),
    );
    workers_finished(&state).await;
    let scratch = state.artifact_store.scratch_directory(1).unwrap();
    assert_eq!(std::fs::read_dir(scratch).unwrap().count(), 0);
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[tokio::test]
async fn composition_value_over_file_limit_is_an_actionable_error_not_transport_failure() {
    let (state, mut frames) = state();
    let service = Arc::new(FakeCompositionService::default());
    *lock_std_mutex(&service.result) = Some(json!("x".repeat(MAX_COMPOSITION_FILE_BYTES)));
    parent(&state, 7, service);
    request(
        &state,
        100,
        methods::COMPOSITION_CALL,
        json!({"parent_request_id":7,"name":"too-large","arguments":{}}),
    );
    let reply = response(&mut frames).await;
    assert_eq!(reply["error"]["code"], -32002);
    assert!(reply["error"]["message"]
        .as_str()
        .unwrap()
        .contains("reduce or filter"));
    workers_finished(&state).await;
    assert!(!state.closed.load(Ordering::Acquire));
    assert_eq!(
        read_std_lock(&state.health).state,
        ExtensionHealthState::Ready
    );
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "optional codemode bundle requires Node >=22.19 on PATH"]
async fn codemode_bundle_runs_vendored_vm_through_host_transport() {
    let temp = tempfile::TempDir::new().unwrap();
    let bundle = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-codemode")
        .canonicalize()
        .unwrap();
    let manifest_path = bundle.join(EXTENSION_MANIFEST_FILENAME);
    let descriptor = DiscoveredExtension {
        manifest: ExtensionManifest::load(&manifest_path).unwrap(),
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(temp.path().canonicalize().unwrap());
    config.tool_composition = true;
    config.supervise = false;
    let process = ExtensionProcess::start(descriptor, config).await.unwrap();
    let connection = read_std_lock(&process.inner.connection).clone();
    let definition = process.tool_definitions().remove(0);
    assert_eq!(
        definition.composition.as_ref().unwrap().mode,
        ToolCompositionMode::On
    );
    assert!(definition.constrained_sampling.is_some());
    let service = Arc::new(FakeCompositionService::default());
    *lock_std_mutex(&service.result) = Some(json!({"ok":true,"text":"🌱".repeat(300_000)}));
    let (started, _operation) = oneshot::channel();
    let result = process
        .call_tool_controlled(
            connection,
            definition,
            0,
            json!({"code":"const row=await tools.echo({message:'from VM'});store('answer',row.text.length);return row.ok ? 'vm-ok' : 'vm-failed';"}),
            process.current_context_for_resource_owner("real-vm-owner"),
            CancellationToken::default(),
            ToolProgressSink::null().with_composition(service.clone()),
            started,
        )
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert!(result.content.contains("vm-ok"));
    assert_eq!(
        *lock_std_mutex(&service.calls),
        vec![("echo".into(), json!({"message":"from VM"}))]
    );
    assert_eq!(lock_std_mutex(&service.writes)[0].0["answer"], 600_000);
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn composition_real_wire_negotiates_dispatches_and_reads_file_fallback() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("probe.py");
    std::fs::write(&script, r#"import hashlib, json, os, pathlib, sys

def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)

def receive():
    return json.loads(sys.stdin.readline())

def request(parent, id, method, **params):
    send({'jsonrpc': '2.0', 'id': id, 'method': method,
          'params': dict(parent_request_id=parent, **params)})
    reply = receive()
    assert reply['id'] == id, reply
    assert 'error' not in reply, reply
    result = reply['result']
    for field in ('value_file', 'context_file'):
        if field in result:
            metadata = result[field]
            name = metadata['path']
            assert pathlib.Path(name).name == name
            path = pathlib.Path(os.environ['OCTET_EXTENSION_SCRATCH']) / name
            raw = path.read_bytes()
            assert len(raw) == metadata['bytes']
            assert hashlib.sha256(raw).hexdigest() == metadata['sha256']
            path.unlink()
            value = json.loads(raw)
            return {'value': value} if field == 'value_file' else value
    return result

init = receive()
api = init['params']['api_version']
assert api == '0.4'
assert 'tool_composition_v1' in init['params']['protocol']['optional_features']
send({'jsonrpc': '2.0', 'id': init['id'], 'result': {
    'api_version': api, 'commands': [], 'tools': [{
        'name': 'compose', 'description': 'Generic composing fixture',
        'parameters': {'type': 'object', 'properties': {'code': {'type': 'string'}}, 'required': ['code']},
        'output_schema': {'type': 'object', 'properties': {'roundtrips': {'type': 'integer'}}, 'required': ['roundtrips']},
        'composition': {'mode': 'only', 'inline_budget': 16000},
        'constrained_sampling': {'type': 'grammar', 'variants': {'openai_regex': '.+'}}
    }], 'protocol': {'version': api,
        'features': ['request_cancellation', 'content_parts', 'tool_composition_v1'],
        'limits': {'max_concurrent_requests': 1}}
}})
while True:
    call = receive()
    if call['method'] == 'shutdown':
        send({'jsonrpc': '2.0', 'id': call['id'], 'result': {}})
        break
    assert call['method'] == 'tool/call', call
    parent = call['id']
    context = request(parent, 'context', 'composition/context')
    assert context['limits'] == {'timeout_ms': 30000, 'max_calls': 256}
    result = request(parent, 'call', 'composition/call', name='echo', arguments={'text': 'from wire'})
    assert len(result['value']['text']) == 1048576
    assert request(parent, 'store', 'composition/store', set={'answer': 42}, delete=['old']) == {}
    send({'jsonrpc': '2.0', 'id': parent, 'result': {
        'content': [{'type': 'text', 'text': 'three reverse roundtrips'}],
        'structured_content': {'roundtrips': 3}, 'is_error': False
    }})
"#).unwrap();
    let mut manifest = manifest("0.4");
    manifest.entrypoint.command = "python3".into();
    manifest.entrypoint.args = vec![script.to_string_lossy().into_owned()];
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.tool_composition = true;
    config.supervise = false;
    config.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(descriptor, config).await.unwrap();
    let connection = read_std_lock(&process.inner.connection).clone();
    let definition = process.tool_definitions().remove(0);
    let tools = process.process_tools(connection.clone(), std::slice::from_ref(&definition));
    assert_eq!(
        tools.tools[0].composition_config().unwrap().mode,
        ToolCompositionMode::Only
    );
    assert_eq!(tools.tools[0].output_schema(), definition.output_schema);
    assert_eq!(
        tools.tools[0].definition().constrained_sampling,
        definition.constrained_sampling
    );
    let service = Arc::new(FakeCompositionService::default());
    *lock_std_mutex(&service.result) =
        Some(json!({"text":"x".repeat(DEFAULT_EXTENSION_MESSAGE_BYTES)}));
    let (started, _operation) = oneshot::channel();
    let result = process
        .call_tool_controlled(
            connection.clone(),
            definition,
            0,
            json!({"code":"fixture program"}),
            process.current_context_for_resource_owner("wire-owner"),
            CancellationToken::default(),
            ToolProgressSink::null().with_composition(service.clone()),
            started,
        )
        .await
        .unwrap();
    assert_eq!(result.content, "three reverse roundtrips");
    assert_eq!(result.structured_content, Some(json!({"roundtrips":3})));
    assert_eq!(
        *lock_std_mutex(&service.calls),
        vec![("echo".into(), json!({"text":"from wire"}))]
    );
    assert_eq!(lock_std_mutex(&service.writes).len(), 1);
    assert!(lock_std_mutex(&connection.pending).is_empty());
    assert!(process.shutdown().await);
}
