use super::*;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[path = "agent_sessions/tests.rs"]
mod agent_sessions;
#[path = "builtin_override_tests.rs"]
mod builtin_overrides;
#[path = "cache_warming_tests.rs"]
mod cache_warming;
#[path = "composer_history_tests.rs"]
mod composer_history;
#[path = "prompt_metadata_tests.rs"]
mod prompt_metadata;

const VALID_MANIFEST: &str = r#"
name = "git-tools"
version = "0.1.0"
api_version = "0.1"
description = "Local git helpers"

[entrypoint]
command = "git-tools"
args = ["--stdio"]

[capabilities]
filesystem = "workspace"
process = true
network = false

[contributes]
tools = ["git_status"]
commands = ["checkpoint"]
hooks = ["after_tool_call"]
ui = ["status"]
context = true
tool_renderers = ["git_status"]
notifications = true
confirmations = true
"#;

pub(super) fn protocol_read_state_for_test(
    declared: ManifestContributions,
    events: broadcast::Sender<ExtensionEvent>,
) -> (ProtocolReadState, mpsc::Receiver<WriterFrame>) {
    let artifact_store = ArtifactStore::new().expect("artifact store");
    artifact_store.begin_generation(1).expect("generation");
    let (writer, frames) = mpsc::channel(8);
    let (catalog_updates, _catalog_update_requests) = mpsc::channel(8);
    (
        ProtocolReadState {
            resources: Arc::new(StdMutex::new(ResourceRegistry::default())),
            resource_cleanup_changed: Arc::new(Notify::new()),
            pending: Arc::new(StdMutex::new(HashMap::new())),
            issued_resource_owners: Arc::new(StdMutex::new(HashSet::new())),
            session_leaf: Arc::new(session_leaf::SessionLeafMailbox::default()),
            remote_ui: Arc::new(RemoteUiMailbox::new(None)),
            pending_changed: Arc::new(Notify::new()),
            closed: Arc::new(AtomicBool::new(false)),
            draining: Arc::new(AtomicBool::new(false)),
            events,
            presentation_rate: StdMutex::new(PresentationUpdateRate::default()),
            presentation_updates: None,
            presentation_sequence: AtomicU64::new(0),
            generation: 1,
            instance_id: "instance-test".into(),
            frame_limit: Arc::new(ProtocolFrameLimit::new(
                DEFAULT_EXTENSION_MESSAGE_BYTES,
                false,
            )),
            declared,
            writer,
            child_requests: Arc::new(StdMutex::new(HashMap::new())),
            seen_child_request_ids: StdMutex::new(HashSet::new()),
            child_work_slots: Arc::new(Semaphore::new(MAX_CHILD_WORKERS)),
            tombstones: Arc::new(StdMutex::new(RequestTombstones::default())),
            protocol: Arc::new(StdRwLock::new(ExtensionNegotiatedProtocol::api_0_1(
                DEFAULT_PENDING_REQUESTS,
            ))),
            api_v03_contract: Arc::new(StdRwLock::new(None)),
            provider_registry: None,
            provider_owner: ExtensionProviderOwner {
                extension_instance_id: "instance-test".into(),
                generation: 1,
            },
            provider_streams: Arc::new(StdMutex::new(HashMap::new())),
            tool_catalog: Arc::new(StdRwLock::new(Vec::new())),
            catalog_updates,
            delegation_service: Arc::new(StdRwLock::new(None)),
            session_lifecycle: None,
            event_bus: None,
            approval_store: Arc::new(ExtensionApprovalStore::new()),
            secret_broker: None,
            extension_identity: ExtensionIdentity {
                name: "test-extension".into(),
                version: "0.1.0".into(),
                manifest_path: PathBuf::from("/test/extension.toml"),
                source: ExtensionSource::Explicit,
            },
            allowed_secrets: Arc::new(BTreeSet::new()),
            health: Arc::new(StdRwLock::new(ConnectionHealth {
                state: ExtensionHealthState::Ready,
                last_error: None,
            })),
            artifact_store,
            child: None,
            termination: None,
        },
        frames,
    )
}

pub(super) fn insert_test_parent(
    state: &ProtocolReadState,
    id: u64,
    resource_owner: Option<ExtensionResourceOwner>,
) {
    let (reply, _reply_rx) = oneshot::channel();
    if let Some(owner) = &resource_owner {
        lock_std_mutex(&state.issued_resource_owners).insert(owner.clone());
    }
    lock_std_mutex(&state.pending).insert(
        id,
        PendingRequest {
            method: "tool/call".into(),
            sender: reply,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: None,
            child_interaction_progress: None,
            resource_owner,
            last_progress_sequence: None,
            tool_call_policy_digest: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
}

pub(super) fn test_resource_owner(session_id: &str) -> ExtensionResourceOwner {
    ExtensionResourceOwner {
        session_id: session_id.into(),
        extension_instance_id: "instance-test".into(),
        process_generation: 1,
    }
}

pub(super) fn wave1_line(id: u64, method: &str, params: serde_json::Value) -> Vec<u8> {
    let mut line = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    }))
    .expect("request JSON");
    line.push(b'\n');
    line
}

/// Contract name of one typed JSON-RPC error response, i.e. the first token
/// of `error.message`, with the numeric code checked alongside it.
pub(super) fn wave1_error(frame: &WriterFrame) -> (i64, String) {
    let value: serde_json::Value =
        serde_json::from_slice(&frame.line).expect("error JSON response");
    let code = value["error"]["code"].as_i64().expect("numeric error code");
    let message = value["error"]["message"].as_str().expect("error message");
    let name = message
        .split(':')
        .next()
        .expect("contract name prefix")
        .to_owned();
    (code, name)
}

fn wave1_negotiate(state: &ProtocolReadState, features: &[&str]) {
    let mut protocol = write_std_lock(&state.protocol);
    protocol.version = EXTENSION_API_VERSION_0_2.into();
    for feature in features {
        protocol.features.insert((*feature).to_owned());
    }
}

#[test]
fn wave1_composer_dispatch_requires_feature_owner_and_bounds() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Feature gate precedes every other check and stays non-fatal.
    handle_protocol_line(
        &wave1_line(
            100,
            methods::COMPOSER_SET,
            serde_json::json!({"parent_request_id": 1, "text": "hi"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    wave1_negotiate(&state, &[EXTENSION_FEATURE_COMPOSER]);

    // deny_unknown_fields on the request body.
    handle_protocol_line(
        &wave1_line(
            101,
            methods::COMPOSER_SET,
            serde_json::json!({"parent_request_id": 1, "text": "hi", "extra": true}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // Over-bound text is a bounds refusal, not a silent truncation.
    handle_protocol_line(
        &wave1_line(
            102,
            methods::COMPOSER_SET,
            serde_json::json!({
                "parent_request_id": 1,
                "text": "x".repeat(MAX_EXTENSION_COMPOSER_TEXT_BYTES + 1),
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("bounds refusal")),
        (-32602, "bounds_exceeded".to_owned())
    );

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            103,
            methods::COMPOSER_INSERT,
            serde_json::json!({"parent_request_id": 2, "text": "x"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    // Happy path fan-out carries the authoritative owner and generation.
    handle_protocol_line(
        &wave1_line(
            104,
            methods::COMPOSER_SET,
            serde_json::json!({"parent_request_id": 1, "text": "@/tmp/x.png"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("composer event") {
        ExtensionEvent::ComposerRequested {
            request_id,
            generation,
            owner,
            operation,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(104));
            assert_eq!(generation, 1);
            assert_eq!(owner.expect("owner").session_id, "session-a");
            assert_eq!(
                operation,
                ExtensionComposerOperation::Set {
                    text: "@/tmp/x.png".to_owned()
                }
            );
        }
        other => panic!("expected composer request, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "admitted requests are not answered locally"
    );
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);
}

#[test]
fn wave2_terminal_acquire_is_gated_owned_and_stays_registered() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Feature gate precedes the owner check and stays non-fatal.
    handle_protocol_line(
        &wave1_line(
            300,
            methods::TERMINAL_ACQUIRE,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    wave1_negotiate(&state, &[EXTENSION_FEATURE_TERMINAL_HANDOFF]);

    // deny_unknown_fields: neither op carries a parameter beyond the
    // owner envelope, so an extra field is a refusal and never a coercion.
    handle_protocol_line(
        &wave1_line(
            301,
            methods::TERMINAL_ACQUIRE,
            serde_json::json!({"parent_request_id": 1, "mode": "fullscreen"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            302,
            methods::TERMINAL_ACQUIRE,
            serde_json::json!({"parent_request_id": 2}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    // Happy path fans out with the authoritative owner and generation.
    handle_protocol_line(
        &wave1_line(
            303,
            methods::TERMINAL_ACQUIRE,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("terminal request") {
        ExtensionEvent::TerminalRequested {
            request_id,
            generation,
            owner,
            operation,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(303));
            assert_eq!(generation, 1);
            assert_eq!(owner.expect("owner").session_id, "session-a");
            assert_eq!(operation, ExtensionTerminalOperation::Acquire);
        }
        other => panic!("expected terminal acquire, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "an admitted acquire is answered by the frontend, never locally"
    );
    assert_eq!(
        lock_std_mutex(&state.child_requests).len(),
        1,
        "the acquire child request stays registered until the frontend answers"
    );
}

#[test]
fn wave2_terminal_release_dispatch_stays_typed_and_bounded() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Release is gated by the same negotiated feature.
    handle_protocol_line(
        &wave1_line(
            400,
            methods::TERMINAL_RELEASE,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );

    wave1_negotiate(&state, &[EXTENSION_FEATURE_TERMINAL_HANDOFF]);

    handle_protocol_line(
        &wave1_line(
            401,
            methods::TERMINAL_RELEASE,
            serde_json::json!({"parent_request_id": 1, "grant_id": "g-1"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            402,
            methods::TERMINAL_RELEASE,
            serde_json::json!({"parent_request_id": 2}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    handle_protocol_line(
        &wave1_line(
            403,
            methods::TERMINAL_RELEASE,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("terminal release") {
        ExtensionEvent::TerminalRequested {
            request_id,
            generation,
            owner,
            operation,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(403));
            assert_eq!(generation, 1);
            assert_eq!(owner.expect("owner").session_id, "session-a");
            assert_eq!(operation, ExtensionTerminalOperation::Release);
        }
        other => panic!("expected terminal release, got {other:?}"),
    }
    assert!(frames.try_recv().is_err());
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);
}

#[test]
fn wave3_context_snapshot_dispatch_requires_feature_owner_and_bounds() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Feature gate precedes every other check and stays non-fatal.
    handle_protocol_line(
        &wave1_line(
            600,
            methods::CONTEXT_SESSION_MANAGER,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    wave1_negotiate(&state, &[EXTENSION_FEATURE_SESSION_CONTEXT]);

    // deny_unknown_fields on the shared owner envelope.
    handle_protocol_line(
        &wave1_line(
            601,
            methods::CONTEXT_SESSION_MANAGER,
            serde_json::json!({"parent_request_id": 1, "extra": true}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            602,
            methods::CONTEXT_PENDING_MESSAGES,
            serde_json::json!({"parent_request_id": 2}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    // Happy path fans out with the authoritative owner and generation.
    handle_protocol_line(
        &wave1_line(
            603,
            methods::CONTEXT_SESSION_MANAGER,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("context snapshot") {
        ExtensionEvent::ContextSnapshotRequested {
            request_id,
            generation,
            owner,
            operation,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(603));
            assert_eq!(generation, 1);
            assert_eq!(owner.expect("owner").session_id, "session-a");
            assert_eq!(operation, ExtensionContextOperation::SessionManager);
        }
        other => panic!("expected context snapshot, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "admitted requests are not answered locally"
    );
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);

    // Pending messages shares the feature but stays a distinct arm.
    handle_protocol_line(
        &wave1_line(
            604,
            methods::CONTEXT_PENDING_MESSAGES,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("pending messages") {
        ExtensionEvent::ContextSnapshotRequested { operation, .. } => {
            assert_eq!(operation, ExtensionContextOperation::PendingMessages);
        }
        other => panic!("expected context snapshot, got {other:?}"),
    }
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 2);
}

#[test]
fn wave3_model_view_dispatch_requires_feature_owner_and_bounds() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Feature gate precedes every other check and stays non-fatal.
    handle_protocol_line(
        &wave1_line(
            800,
            methods::CONTEXT_MODEL,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );

    wave1_negotiate(&state, &[EXTENSION_FEATURE_MODEL_CATALOG]);

    // deny_unknown_fields on the request body.
    handle_protocol_line(
        &wave1_line(
            801,
            methods::CONTEXT_MODEL,
            serde_json::json!({"parent_request_id": 1, "unexpected": true}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            802,
            methods::CONTEXT_MODEL_CATALOG,
            serde_json::json!({"parent_request_id": 2}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    // Happy paths fan out with the authoritative owner and operation.
    handle_protocol_line(
        &wave1_line(
            803,
            methods::CONTEXT_MODEL,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("model view event") {
        ExtensionEvent::ModelViewRequested {
            operation, owner, ..
        } => {
            assert_eq!(operation, ExtensionModelOperation::Current);
            assert_eq!(owner.expect("owner").session_id, "session-a");
        }
        other => panic!("expected model view request, got {other:?}"),
    }

    handle_protocol_line(
        &wave1_line(
            804,
            methods::CONTEXT_MODEL_CATALOG,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("catalog event") {
        ExtensionEvent::ModelViewRequested { operation, .. } => {
            assert_eq!(operation, ExtensionModelOperation::Catalog);
        }
        other => panic!("expected catalog request, got {other:?}"),
    }
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 2);
}

#[test]
fn wave3_model_view_results_are_bounded_and_deny_unknown_fields() {
    // One shape serves both mechanisms: the pushed host state / `model/selected`
    // event and the pulled `context/model` / `context/model_catalog` answer.
    let view = ExtensionModelView {
        id: "anthropic/claude-sonnet-4".into(),
        name: Some("Claude Sonnet 4".into()),
        base_url: None,
        api: "anthropic-messages".into(),
        provider: "anthropic".into(),
        reasoning: true,
        input: vec!["text".into(), "image".into()],
        cost: Some(ExtensionModelCost {
            input: 3_000_000,
            output: 15_000_000,
            cache_read: 300_000,
            cache_write: 3_750_000,
        }),
        context_window: 200_000,
        max_tokens: 64_000,
    };
    view.validate().expect("bounded view");
    let encoded = serde_json::to_value(&view).expect("encode");
    assert_eq!(encoded["provider"], serde_json::json!("anthropic"));
    assert_eq!(
        encoded["reasoning"],
        serde_json::json!(true),
        "capability, not level"
    );
    assert!(encoded.get("base_url").is_none(), "endpoints stay withheld");
    assert_eq!(
        serde_json::from_value::<ExtensionModelView>(encoded.clone()).unwrap(),
        view
    );
    let mut extra = encoded;
    extra["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ExtensionModelView>(extra).is_err());

    let mut oversized = view.clone();
    oversized.id = "x".repeat(MAX_EXTENSION_MODEL_FIELD_BYTES + 1);
    assert!(oversized.validate().is_err());
    let mut empty = view.clone();
    empty.provider = String::new();
    assert!(empty.validate().is_err());
    let mut bad_api = view.clone();
    bad_api.api = "x".repeat(MAX_EXTENSION_MODEL_API_BYTES + 1);
    assert!(bad_api.validate().is_err());
    let mut bad_input = view.clone();
    bad_input.input = vec!["audio".into()];
    assert!(bad_input.validate().is_err());

    let catalog = ContextModelCatalogResult {
        models: vec![view.clone()],
        truncated: false,
    };
    catalog.validate().expect("bounded catalog");
    let over_bound = ContextModelCatalogResult {
        models: vec![view; MAX_EXTENSION_MODEL_CATALOG_ROWS + 1],
        truncated: true,
    };
    assert!(over_bound.validate().is_err());
}

#[test]
fn wave3_system_prompt_disclosure_requires_a_declared_capability() {
    // A disclosure surface is not in the blanket optional list: an extension
    // must declare `capabilities.system_prompt` before the feature is even
    // offered, exactly like `capabilities.secrets`.
    assert!(
        !API_0_2_OPTIONAL_FEATURES.contains(&EXTENSION_FEATURE_SYSTEM_PROMPT_READ),
        "prompt disclosure must not be offered to every API 0.2 extension"
    );

    fn disclosure_manifest(system_prompt: bool) -> ExtensionManifest {
        ExtensionManifest::parse(&format!(
            r#"name = "disclosure-probe"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "probe.sh"
[capabilities]
system_prompt = {system_prompt}
"#
        ))
        .expect("disclosure manifest")
    }

    fn echoing_response() -> InitializeResponse {
        let mut features = API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<Vec<_>>();
        features.push(EXTENSION_FEATURE_SYSTEM_PROMPT_READ.to_owned());
        InitializeResponse {
            api_version: EXTENSION_API_VERSION_0_2.to_owned(),
            tools: Vec::new(),
            commands: Vec::new(),
            tool_renderers: Vec::new(),
            shortcuts: Vec::new(),
            protocol: Some(ExtensionProtocolResponse {
                session_snapshot_transport_v1: None,
                version: EXTENSION_API_VERSION_0_2.to_owned(),
                features,
                limits: ExtensionProtocolLimits {
                    max_message_bytes: None,
                    resource_refs_v1: None,
                    max_concurrent_requests: 4,
                },
                lifecycle_events: Vec::new(),
            }),
        }
    }

    let no_services = OfferedHostServices {
        transcript_render: false,
        provider_proxy: false,
        resource_paths: false,
        provider_pipeline: false,
        bulk_objects: false,
        remote_ui: false,
        agent_sessions: false,
        tool_composition: false,
        session_lifecycle: false,
        session_compaction: false,
        approvals: false,
        secrets: false,
        max_message_bytes: 0,
    };

    // An undeclared extension that echoes the feature fails negotiation, so
    // it can never negotiate a disclosure it never declared.
    let refused = negotiate_contributions_with_host_services(
        &disclosure_manifest(false),
        echoing_response(),
        4,
        no_services,
    )
    .expect_err("an undeclared disclosure must not negotiate");
    assert!(refused.to_string().contains("unknown feature"), "{refused}");

    // Declared: the feature is allowed and lands in the negotiated set.
    let (_, negotiated) = negotiate_contributions_with_host_services(
        &disclosure_manifest(true),
        echoing_response(),
        4,
        no_services,
    )
    .expect("a declared disclosure negotiates");
    assert!(negotiated.supports(EXTENSION_FEATURE_SYSTEM_PROMPT_READ));
}

#[test]
fn wave3_system_prompt_read_is_its_own_disclosure_feature() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Negotiating `session_context` does NOT disclose the system prompt.
    wave1_negotiate(&state, &[EXTENSION_FEATURE_SESSION_CONTEXT]);
    handle_protocol_line(
        &wave1_line(
            700,
            methods::CONTEXT_SYSTEM_PROMPT,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    wave1_negotiate(&state, &[EXTENSION_FEATURE_SYSTEM_PROMPT_READ]);

    // deny_unknown_fields on the shared owner envelope.
    handle_protocol_line(
        &wave1_line(
            701,
            methods::CONTEXT_SYSTEM_PROMPT,
            serde_json::json!({"parent_request_id": 1, "mode": "raw"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            702,
            methods::CONTEXT_SYSTEM_PROMPT,
            serde_json::json!({"parent_request_id": 2}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    // Happy path fans out with the authoritative owner and generation.
    handle_protocol_line(
        &wave1_line(
            703,
            methods::CONTEXT_SYSTEM_PROMPT,
            serde_json::json!({"parent_request_id": 1}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("system prompt") {
        ExtensionEvent::ContextSnapshotRequested {
            request_id,
            generation,
            owner,
            operation,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(703));
            assert_eq!(generation, 1);
            assert_eq!(owner.expect("owner").session_id, "session-a");
            assert_eq!(operation, ExtensionContextOperation::SystemPrompt);
        }
        other => panic!("expected context snapshot, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "admitted requests are not answered locally"
    );
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);
}

#[test]
fn wave4_provider_credentials_require_a_declared_capability() {
    // Credential disclosure is a review-time grant: an extension must declare
    // `capabilities.provider_credentials` before the feature is even offered,
    // exactly like `capabilities.system_prompt` and `capabilities.secrets`.
    assert!(
        !API_0_2_OPTIONAL_FEATURES.contains(&EXTENSION_FEATURE_PROVIDER_CREDENTIALS),
        "credential disclosure must not be offered to every API 0.2 extension"
    );

    fn disclosure_manifest(provider_credentials: bool) -> ExtensionManifest {
        ExtensionManifest::parse(&format!(
            r#"name = "credential-probe"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "probe.sh"
[capabilities]
provider_credentials = {provider_credentials}
"#
        ))
        .expect("credential manifest")
    }

    fn echoing_response() -> InitializeResponse {
        let mut features = API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<Vec<_>>();
        features.push(EXTENSION_FEATURE_PROVIDER_CREDENTIALS.to_owned());
        InitializeResponse {
            api_version: EXTENSION_API_VERSION_0_2.to_owned(),
            tools: Vec::new(),
            commands: Vec::new(),
            tool_renderers: Vec::new(),
            shortcuts: Vec::new(),
            protocol: Some(ExtensionProtocolResponse {
                session_snapshot_transport_v1: None,
                version: EXTENSION_API_VERSION_0_2.to_owned(),
                features,
                limits: ExtensionProtocolLimits {
                    max_message_bytes: None,
                    resource_refs_v1: None,
                    max_concurrent_requests: 4,
                },
                lifecycle_events: Vec::new(),
            }),
        }
    }

    let no_services = OfferedHostServices {
        transcript_render: false,
        provider_proxy: false,
        resource_paths: false,
        provider_pipeline: false,
        bulk_objects: false,
        remote_ui: false,
        agent_sessions: false,
        tool_composition: false,
        session_lifecycle: false,
        session_compaction: false,
        approvals: false,
        secrets: false,
        max_message_bytes: 0,
    };

    // An undeclared extension that echoes the feature fails negotiation, so a
    // manifest the user never reviewed can never reach the credential path.
    let refused = negotiate_contributions_with_host_services(
        &disclosure_manifest(false),
        echoing_response(),
        4,
        no_services,
    )
    .expect_err("an undeclared credential disclosure must not negotiate");
    assert!(refused.to_string().contains("unknown feature"), "{refused}");

    // Declared: the feature is allowed and lands in the negotiated set.
    let (_, negotiated) = negotiate_contributions_with_host_services(
        &disclosure_manifest(true),
        echoing_response(),
        4,
        no_services,
    )
    .expect("a declared credential disclosure negotiates");
    assert!(negotiated.supports(EXTENSION_FEATURE_PROVIDER_CREDENTIALS));
}

#[test]
fn wave4_provider_credentials_dispatch_is_owner_and_bounds_gated() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));

    // Feature gate precedes every other check and stays non-fatal.
    handle_protocol_line(
        &wave1_line(
            800,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({"parent_request_id": 1, "provider": "test", "model": "test-model"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("typed refusal")),
        (-32601, "unsupported_feature".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    wave1_negotiate(&state, &[EXTENSION_FEATURE_PROVIDER_CREDENTIALS]);

    // A credential-shaped field is refused, never silently dropped or echoed.
    handle_protocol_line(
        &wave1_line(
            801,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({"parent_request_id": 1, "provider": "test", "model": "test-model", "apiKey": "planted-secret"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("unknown-field refusal")),
        (-32602, "invalid_request".to_owned())
    );

    // The requested identity is one exact, bounded native provider/model pair.
    for (id, params, expected) in [
        (
            802,
            serde_json::json!({"parent_request_id": 1, "provider": "", "model": "test-model"}),
            (-32602, "invalid_request"),
        ),
        (
            803,
            serde_json::json!({"parent_request_id": 1, "provider": "test", "model": "x".repeat(257)}),
            (-32602, "bounds_exceeded"),
        ),
        (
            804,
            serde_json::json!({"parent_request_id": 1, "provider": "te\u{1b}st", "model": "test-model"}),
            (-32602, "invalid_request"),
        ),
    ] {
        handle_protocol_line(
            &wave1_line(id, methods::PROVIDER_CREDENTIALS, params),
            &state,
        )
        .expect("reader loop stays alive");
        assert_eq!(
            wave1_error(&frames.try_recv().expect("bounded refusal")),
            (expected.0, expected.1.to_owned())
        );
    }
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    // A parent without a durable session owner is never coerced.
    insert_test_parent(&state, 2, None);
    handle_protocol_line(
        &wave1_line(
            805,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({"parent_request_id": 2, "provider": "test", "model": "test-model"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    // Happy path fans out the exact identity plus authoritative owner and
    // generation. The host, not the process, resolves the credential.
    handle_protocol_line(
        &wave1_line(
            806,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({"parent_request_id": 1, "provider": "test", "model": "test-model"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("credential request") {
        ExtensionEvent::ProviderCredentialsRequested {
            request_id,
            generation,
            owner,
            provider,
            model,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(806));
            assert_eq!(generation, 1);
            assert_eq!(owner.session_id, "session-a");
            assert_eq!(provider, "test");
            assert_eq!(model, "test-model");
        }
        other => panic!("expected provider credentials, got {other:?}"),
    }

    // A retained request whose parent already settled is admitted only for an
    // owner this host issued, never for an invented session: the Pi bridge's
    // retained auto-review timer cannot widen its owner.
    handle_protocol_line(
        &wave1_line(
            807,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({
                "parent_request_id": 987_654,
                "resource_owner": test_resource_owner("session-intruder"),
                "provider": "test",
                "model": "test-model",
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("foreign owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );
    handle_protocol_line(
        &wave1_line(
            808,
            methods::PROVIDER_CREDENTIALS,
            serde_json::json!({
                "parent_request_id": 987_654,
                "resource_owner": test_resource_owner("session-a"),
                "provider": "test",
                "model": "test-model",
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("retained credential request") {
        ExtensionEvent::ProviderCredentialsRequested {
            request_id,
            generation,
            owner,
            provider,
            model,
        } => {
            assert_eq!(request_id, ExtensionRequestId::Number(808));
            assert_eq!(generation, 1);
            assert_eq!(owner.session_id, "session-a");
            assert_eq!(provider, "test");
            assert_eq!(model, "test-model");
        }
        other => panic!("expected retained provider credentials, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "admitted requests are not answered locally"
    );
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 2);
}

#[test]
fn wave3_context_structs_round_trip_bound_and_deny_unknown_fields() {
    fn assert_round_trip<T>(value: T, expected: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_value(&value).expect("encode");
        assert_eq!(encoded, expected);
        let decoded: T = serde_json::from_value(expected.clone()).expect("decode");
        assert_eq!(decoded, value);
        let mut extra = expected.clone();
        extra["unexpected"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<T>(extra).is_err(),
            "{expected} must deny unknown fields"
        );
    }

    assert_round_trip(
        ContextSnapshotRequest {
            parent_request_id: 7,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7}),
    );
    assert_round_trip(
        ContextPendingMessagesResult { pending: 3 },
        serde_json::json!({"pending": 3}),
    );
    assert_round_trip(
        ContextSessionManagerResult {
            session_id: "s-1".into(),
            name: Some("Planning".into()),
            model: Some("model-a".into()),
            reasoning: Some("high".into()),
            active_skills: vec![ContextSkillSummary {
                id: "typesafe-ai".into(),
                name: "TypeSafe AI".into(),
            }],
            cwd: "/workspace".into(),
        },
        serde_json::json!({
            "session_id": "s-1",
            "name": "Planning",
            "model": "model-a",
            "reasoning": "high",
            "active_skills": [{"id": "typesafe-ai", "name": "TypeSafe AI"}],
            "cwd": "/workspace"
        }),
    );
    // Absent optional fields stay absent on the wire.
    assert_round_trip(
        ContextSessionManagerResult {
            session_id: "s-1".into(),
            name: None,
            model: None,
            reasoning: None,
            active_skills: Vec::new(),
            cwd: "/workspace".into(),
        },
        serde_json::json!({
            "session_id": "s-1",
            "active_skills": [],
            "cwd": "/workspace"
        }),
    );

    // The disclosed prompt text is bounded, never truncated silently.
    ContextSystemPromptResult {
        text: "system prompt".into(),
    }
    .validate()
    .expect("bounded prompt");
    assert!(matches!(
        ContextSystemPromptResult {
            text: "x".repeat(MAX_EXTENSION_SYSTEM_PROMPT_BYTES + 1),
        }
        .validate(),
        Err(message) if message.contains("limit is")
    ));
    assert_round_trip(
        ContextSystemPromptResult {
            text: "system prompt".into(),
        },
        serde_json::json!({"text": "system prompt"}),
    );
}

#[test]
fn wave2_terminal_structs_round_trip_and_deny_unknown_fields() {
    fn assert_round_trip_strict<T>(value: T, expected: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_value(&value).expect("encode");
        assert_eq!(encoded, expected);
        let decoded: T = serde_json::from_value(expected.clone()).expect("decode");
        assert_eq!(decoded, value);
        let mut extra = expected.clone();
        extra["unexpected"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<T>(extra).is_err(),
            "{expected} must deny unknown fields"
        );
    }

    // Serde ignores `deny_unknown_fields` on internally tagged enums (`tag =
    // "operation"`), which every Wave-1 request enum shares. The tag itself
    // is what is dispatch-relevant, so an ignored extra field cannot change
    // the operation; the strictness claim therefore lives on the request,
    // result, and notification structs.
    fn assert_round_trip_tagged<T>(value: T, expected: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_value(&value).expect("encode");
        assert_eq!(encoded, expected);
        let decoded: T = serde_json::from_value(expected.clone()).expect("decode");
        assert_eq!(decoded, value);
    }

    assert_round_trip_strict(
        TerminalAcquireRequest {
            parent_request_id: 7,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7}),
    );
    assert_round_trip_strict(
        TerminalReleaseRequest {
            parent_request_id: 7,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7}),
    );
    assert_round_trip_strict(
        TerminalAcquireResult {
            grant_id: "g-1".into(),
            columns: 120,
            rows: 40,
        },
        serde_json::json!({"grant_id": "g-1", "columns": 120, "rows": 40}),
    );
    assert_round_trip_strict(TerminalReleaseResult {}, serde_json::json!({}));
    assert_round_trip_strict(
        TerminalGrantLost {
            reason: "holder exited".into(),
        },
        serde_json::json!({"reason": "holder exited"}),
    );
    assert_round_trip_tagged(
        ExtensionTerminalOperation::Acquire,
        serde_json::json!({"operation": "acquire"}),
    );
    assert_round_trip_tagged(
        ExtensionTerminalOperation::Release,
        serde_json::json!({"operation": "release"}),
    );
}

#[cfg(unix)]
#[tokio::test]
async fn wave2_terminal_grant_lost_is_a_no_op_when_unnegotiated() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("terminal-grant-lost.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$OCTET_WORKSPACE/terminal-grant-lost.log"
  case "$line" in
    *'"method":"shutdown"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}' ;;
  esac
done
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("terminal-grant-lost", "terminal-grant-lost.sh"),
    );
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();
    assert!(
        !read_std_lock(&connection.protocol).supports(EXTENSION_FEATURE_TERMINAL_HANDOFF),
        "an API 0.1 generation does not negotiate terminal handoff"
    );

    process
        .notify_terminal_grant_lost("holder exited")
        .expect("an unnegotiated emitter is a best-effort no-op");

    // The only frame this generation may observe afterwards is the shutdown
    // request, so the no-op really wrote nothing to the wire. The fixture
    // script may already have exited, so shutdown is best-effort here: the
    // frame log below is the evidence, not the shutdown acknowledgement.
    let _ = process.shutdown().await;
    let frames = std::fs::read_to_string(temp.path().join("terminal-grant-lost.log"))
        .expect("fixture frame log");
    let methods = frames
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).expect("frame JSON")["method"]
                .as_str()
                .expect("frame method")
                .to_owned()
        })
        .collect::<Vec<_>>();
    assert_eq!(methods, vec!["shutdown".to_owned()]);
}

#[test]
fn wave1_session_tools_and_injection_dispatch_stay_typed_and_bounded() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));
    wave1_negotiate(
        &state,
        &[
            EXTENSION_FEATURE_SESSION_ENTRIES,
            EXTENSION_FEATURE_MESSAGE_INJECTION,
            EXTENSION_FEATURE_ACTIVE_TOOLS,
        ],
    );

    // Pi sendMessage has no role; a role-shaped request is refused.
    handle_protocol_line(
        &wave1_line(
            200,
            methods::SESSION_SEND_MESSAGE,
            serde_json::json!({"parent_request_id": 1, "role": "user", "text": "hi"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("role refusal")),
        (-32602, "invalid_request".to_owned())
    );

    handle_protocol_line(
        &wave1_line(
            201,
            methods::SESSION_SEND_MESSAGE,
            serde_json::json!({"parent_request_id": 1, "custom_type": "job", "content": "note", "display": true, "deliver_as": "follow_up", "trigger_turn": true}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("injection event") {
        ExtensionEvent::MessageInjectionRequested { injection, .. } => assert_eq!(
            injection,
            ExtensionMessageInjection::Custom {
                custom_type: "job".to_owned(),
                content: crate::session::CustomMessageContent::Text("note".to_owned()),
                display: true,
                details: None,
                deliver_as: Some(ExtensionMessageDelivery::FollowUp),
                trigger_turn: Some(true),
            }
        ),
        other => panic!("expected message injection, got {other:?}"),
    }

    handle_protocol_line(
        &wave1_line(
            202,
            methods::SESSION_SEND_USER_MESSAGE,
            serde_json::json!({"parent_request_id": 1, "text": "question"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("user injection event") {
        ExtensionEvent::MessageInjectionRequested { injection, .. } => assert_eq!(
            injection,
            ExtensionMessageInjection::User {
                content: None,
                text: "question".to_owned(),
                deliver_as: None,
            }
        ),
        other => panic!("expected user injection, got {other:?}"),
    }

    // Bounded durable entry append and host-owned naming.
    handle_protocol_line(
        &wave1_line(
            203,
            methods::SESSION_APPEND_ENTRY,
            serde_json::json!({
                "parent_request_id": 1,
                "entry_type": "todo",
                "data": {"items": ["one"]},
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("entry event") {
        ExtensionEvent::SessionEntryRequested { operation, .. } => assert!(matches!(
            operation,
            ExtensionSessionEntryOperation::Append { ref entry_type, .. } if entry_type == "todo"
        )),
        other => panic!("expected session entry request, got {other:?}"),
    }

    handle_protocol_line(
        &wave1_line(
            204,
            methods::SESSION_SET_NAME,
            serde_json::json!({"parent_request_id": 1, "name": "planning"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("name event") {
        ExtensionEvent::SessionEntryRequested { operation, .. } => assert_eq!(
            operation,
            ExtensionSessionEntryOperation::SetName {
                name: "planning".to_owned()
            }
        ),
        other => panic!("expected session name request, got {other:?}"),
    }

    // Active tool names are validated like `tools/register`.
    handle_protocol_line(
        &wave1_line(
            205,
            methods::TOOLS_SET_ACTIVE,
            serde_json::json!({"parent_request_id": 1, "names": ["read", "not a tool"]}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("tool-name refusal")),
        (-32602, "invalid_request".to_owned())
    );

    handle_protocol_line(
        &wave1_line(
            206,
            methods::TOOLS_SET_ACTIVE,
            serde_json::json!({"parent_request_id": 1, "names": ["read", "search"]}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("active tools event") {
        ExtensionEvent::ActiveToolsRequested { names, .. } => {
            assert_eq!(names, vec!["read".to_owned(), "search".to_owned()]);
        }
        other => panic!("expected active tools request, got {other:?}"),
    }
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn wave1_deferred_requests_need_a_genuine_owner() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    // The pi-draw `/draw` shape: the command request settles and the insert
    // happens later in an HTTP callback.
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));
    lock_std_mutex(&state.pending)
        .get(&1)
        .expect("parent")
        .terminal
        .store(REQUEST_COMPLETED, Ordering::Release);
    wave1_negotiate(&state, &[EXTENSION_FEATURE_COMPOSER]);
    let owner = |session_id: &str, generation: u64| {
        serde_json::json!({
            "session_id": session_id,
            "extension_instance_id": "instance-test",
            "process_generation": generation,
        })
    };

    // A settled parent without an explicit owner stays refused.
    handle_protocol_line(
        &wave1_line(
            300,
            methods::COMPOSER_SET,
            serde_json::json!({"parent_request_id": 1, "text": "@/tmp/x.png"}),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("no-owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    // A stale generation is refused.
    handle_protocol_line(
        &wave1_line(
            301,
            methods::COMPOSER_SET,
            serde_json::json!({
                "parent_request_id": 1,
                "text": "@/tmp/x.png",
                "resource_owner": owner("session-a", 9),
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("stale-owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );

    // An owner this process never issued is refused.
    handle_protocol_line(
        &wave1_line(
            302,
            methods::COMPOSER_SET,
            serde_json::json!({
                "parent_request_id": 1,
                "text": "@/tmp/x.png",
                "resource_owner": owner("session-b", 1),
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    assert_eq!(
        wave1_error(&frames.try_recv().expect("foreign-owner refusal")),
        (-32002, "not_foreground_owner".to_owned())
    );
    assert!(matches!(
        received.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));

    // The genuine owner observed earlier on this wire is admitted, and the
    // request is answered on its own lifetime rather than a dead parent's.
    handle_protocol_line(
        &wave1_line(
            303,
            methods::COMPOSER_SET,
            serde_json::json!({
                "parent_request_id": 1,
                "text": "@/tmp/x.png",
                "resource_owner": owner("session-a", 1),
            }),
        ),
        &state,
    )
    .expect("reader loop stays alive");
    match received.try_recv().expect("deferred composer event") {
        ExtensionEvent::ComposerRequested {
            owner: Some(owner),
            operation,
            ..
        } => {
            assert_eq!(owner.session_id, "session-a");
            assert_eq!(
                operation,
                ExtensionComposerOperation::Set {
                    text: "@/tmp/x.png".to_owned()
                }
            );
        }
        other => panic!("expected deferred composer request, got {other:?}"),
    }
    assert_eq!(lock_std_mutex(&state.child_requests).len(), 1);
}

#[test]
fn wave1_typed_failures_use_contract_codes_and_bounded_names() {
    for (failure, code, name) in [
        (
            ExtensionRequestFailure::UnsupportedFeature,
            -32601,
            "unsupported_feature",
        ),
        (
            ExtensionRequestFailure::NotForegroundOwner,
            -32002,
            "not_foreground_owner",
        ),
        (
            ExtensionRequestFailure::InvalidRequest,
            -32602,
            "invalid_request",
        ),
        (
            ExtensionRequestFailure::BoundsExceeded,
            -32602,
            "bounds_exceeded",
        ),
    ] {
        assert_eq!(failure.code(), code);
        assert_eq!(failure.name(), name);
        let message = failure.message("detail");
        assert_eq!(message, format!("{name}: detail"));
        assert!(message.len() <= name.len() + 2 + MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES);
        // A hostile detail can never inflate the frame beyond the bound.
        let oversized = failure.message(&"é".repeat(4096));
        assert!(
            oversized.len() <= name.len() + 2 + MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES,
            "{oversized:?} exceeded the bounded detail"
        );
        assert!(oversized.starts_with(name));
        assert_eq!(failure.message(""), name.to_owned());
    }
}

#[test]
fn wave1_request_structs_round_trip_and_deny_unknown_fields() {
    fn assert_round_trip<T>(value: T, expected: serde_json::Value)
    where
        T: Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let encoded = serde_json::to_value(&value).expect("encode");
        assert_eq!(encoded, expected);
        let decoded: T = serde_json::from_value(expected.clone()).expect("decode");
        assert_eq!(decoded, value);
        let mut extra = expected.clone();
        extra["unexpected"] = serde_json::json!(true);
        assert!(
            serde_json::from_value::<T>(extra).is_err(),
            "{expected} must deny unknown fields"
        );
    }

    assert_round_trip(
        ComposerGetRequest {
            parent_request_id: 7,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7}),
    );
    assert_round_trip(
        ComposerTextRequest {
            parent_request_id: 7,
            text: "draft".into(),
            resource_owner: None,
            editor_checkpoint: None,
        },
        serde_json::json!({"parent_request_id": 7, "text": "draft"}),
    );
    assert_round_trip(
        ComposerTextResult {
            text: "draft".into(),
        },
        serde_json::json!({"text": "draft"}),
    );
    assert_round_trip(
        ShortcutRegisterRequest {
            parent_request_id: 7,
            id: "draw".into(),
            key: "ctrl+shift+c".into(),
            description: "Describe".into(),
            resource_owner: None,
        },
        serde_json::json!({
            "parent_request_id": 7,
            "id": "draw",
            "key": "ctrl+shift+c",
            "description": "Describe"
        }),
    );
    assert_round_trip(
        SessionAppendEntryRequest {
            parent_request_id: 7,
            entry_type: "todo".into(),
            data: serde_json::json!({"items": ["one"]}),
            resource_owner: None,
        },
        serde_json::json!({
            "parent_request_id": 7,
            "entry_type": "todo",
            "data": {"items": ["one"]}
        }),
    );
    assert_round_trip(
        SessionAppendEntryResult {
            entry_id: "e-1".into(),
        },
        serde_json::json!({"entry_id": "e-1"}),
    );
    assert_round_trip(
        SessionSetNameRequest {
            parent_request_id: 7,
            name: "planning".into(),
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7, "name": "planning"}),
    );
    assert_round_trip(
        SessionSetLabelRequest {
            parent_request_id: 7,
            entry_id: "e-1".into(),
            label: "done".into(),
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7, "entry_id": "e-1", "label": "done"}),
    );
    assert_round_trip(
        SessionSendMessageRequest {
            parent_request_id: 7,
            custom_type: "job".into(),
            content: crate::session::CustomMessageContent::Text("hello".into()),
            display: false,
            details: None,
            deliver_as: Some(ExtensionMessageDelivery::NextTurn),
            trigger_turn: None,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7, "custom_type": "job", "content": "hello", "display": false, "deliver_as": "next_turn"}),
    );
    assert_round_trip(
        SessionSendUserMessageRequest {
            parent_request_id: 7,
            text: "hello".into(),
            content: None,
            deliver_as: None,
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7, "text": "hello"}),
    );
    assert_round_trip(
        ToolsSetActiveRequest {
            parent_request_id: 7,
            names: vec!["read".into()],
            resource_owner: None,
        },
        serde_json::json!({"parent_request_id": 7, "names": ["read"]}),
    );
}

#[test]
fn wave1_message_delta_coalescer_batches_and_stays_bounded() {
    let now = Instant::now();
    let mut coalescer = MessageDeltaCoalescer::default();
    coalescer.begin_message("m-1");

    // Many small pushes inside one interval produce no notification at all.
    let mut batches = Vec::new();
    for _ in 0..32 {
        batches.extend(coalescer.push("tok ", now));
    }
    assert!(batches.is_empty(), "per-delta fan-out is forbidden");
    let flushed = coalescer.flush(now);
    assert_eq!(flushed.len(), 1, "one flush is one notification");
    assert_eq!(flushed[0].deltas, 32);
    assert_eq!(flushed[0].delta, "tok ".repeat(32));
    assert_eq!(
        flushed[0]
            .clone()
            .into_updated(coalescer.active_message_id()),
        ExtensionMessageUpdated {
            message_id: Some("m-1".to_owned()),
            delta: "tok ".repeat(32),
            deltas: 32,
        }
    );
    assert!(
        coalescer.flush(now).is_empty(),
        "a flush never repeats a batch"
    );

    // The byte bound flushes the pending batch before it could overflow,
    // and every emitted batch stays inside the per-notification cap.
    let mut coalescer = MessageDeltaCoalescer::default();
    coalescer.begin_message("m-2");
    assert!(coalescer.push(&"a".repeat(4000), now).is_empty());
    let overflow = coalescer.push(&"b".repeat(5000), now);
    assert_eq!(
        overflow.len(),
        2,
        "the pending batch flushes before overflow"
    );
    assert_eq!(overflow[0].delta.len(), 4000);
    assert_eq!(overflow[0].deltas, 1);
    assert_eq!(overflow[1].delta.len(), 5000);
    for batch in &overflow {
        assert!(batch.delta.len() <= MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES);
    }
    assert!(coalescer.flush(now).is_empty());

    // The elapsed-time boundary is honored without any background task.
    let start = Instant::now();
    assert!(coalescer.push("late", start).is_empty());
    let later = start + MESSAGE_DELTA_FLUSH_INTERVAL;
    let time_flush = coalescer.push("late", later);
    assert_eq!(time_flush.len(), 1);
    assert_eq!(time_flush[0].delta, "latelate");
    assert_eq!(time_flush[0].deltas, 2);

    // 64 deltas force a flush even inside one interval.
    let mut count = 0;
    for _ in 0..MESSAGE_DELTA_FLUSH_DELTAS {
        count += coalescer.push("d", now).len();
    }
    assert_eq!(count, 1);
}

#[test]
fn progress_decoration_dispatch_requires_feature_active_parent_and_safe_bounded_fields() {
    let (events, mut diagnostics) = broadcast::channel(16);
    let (state, _frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session")));
    let (sink, mut progress) = ToolProgressSink::bounded_channel();
    lock_std_mutex(&state.pending).get_mut(&1).unwrap().progress = Some(sink);
    let notification = |request_id, sequence, label: String| ExtensionProgressNotification {
        request_id,
        sequence,
        event: ExtensionProgressEvent::Decoration {
            label,
            detail: None,
        },
    };
    assert!(dispatch_progress(&state, notification(1, 1, "unnegotiated".into())).is_err());
    {
        let mut protocol = write_std_lock(&state.protocol);
        protocol.version = EXTENSION_API_VERSION_0_2.into();
        protocol
            .features
            .insert(EXTENSION_FEATURE_PROGRESS_DECORATION.into());
    }
    dispatch_progress(&state, notification(1, 2, "é".repeat(128))).unwrap();
    let crate::tool::ToolProgress::Decoration(decoration) = progress.try_recv().unwrap() else {
        panic!("expected decoration");
    };
    assert_eq!(decoration.label().len(), 256);
    dispatch_progress(&state, notification(1, 2, "duplicate".into())).unwrap();
    dispatch_progress(&state, notification(99, 1, "foreign".into())).unwrap();
    assert!(progress.try_recv().is_err());
    for (sequence, label) in [
        (3, "é".repeat(129)),
        (4, "bad\u{001b}[31m".into()),
        (5, String::new()),
    ] {
        assert!(dispatch_progress(&state, notification(1, sequence, label)).is_err());
        assert!(progress.try_recv().is_err());
    }
    lock_std_mutex(&state.pending).remove(&1);
    dispatch_progress(&state, notification(1, 6, "late".into())).unwrap();
    assert!(progress.try_recv().is_err());
    let mut ignored = 0;
    while let Ok(ExtensionEvent::Diagnostic { .. }) = diagnostics.try_recv() {
        ignored += 1;
    }
    assert_eq!(ignored, 3);
}

#[test]
fn pending_cancellation_preserves_original_method_and_reason() {
    for expected_method in [
        methods::TOOL_CALL,
        methods::COMMAND_EXECUTE,
        methods::HOOK_RUN,
    ] {
        for expected_reason in ["shutdown", "reload drain deadline", "user"] {
            let error = pending_error(
                PendingError::Cancelled(expected_reason.into()),
                expected_method,
            );
            match error {
                ExtensionRuntimeError::Cancelled { method, reason } => {
                    assert_eq!(method, expected_method);
                    assert_eq!(reason, expected_reason);
                }
                other => panic!("expected local cancellation, got {other:?}"),
            }
        }
    }
}

#[test]
fn pending_remote_cancellation_remains_a_remote_error() {
    let data = serde_json::json!({"terminal": "cancelled", "reason": "remote"});
    let error = pending_error(
        PendingError::Remote {
            code: -32800,
            message: "request cancelled".into(),
            data: Some(data.clone()),
        },
        methods::TOOL_CALL,
    );
    match error {
        ExtensionRuntimeError::Remote {
            code,
            message,
            data: actual_data,
        } => {
            assert_eq!(code, -32800);
            assert_eq!(message, "request cancelled");
            assert_eq!(actual_data, Some(data));
        }
        other => panic!("remote terminal error was reclassified: {other:?}"),
    }
}

#[test]
fn api_v03_theme_selection_is_omitted_without_a_host_handler() {
    for session_lifecycle in [false, true] {
        let offer = api_v03_host_offer_for_services(1024, 4, session_lifecycle, false).unwrap();
        assert!(!offer
            .optional_capabilities
            .iter()
            .any(|capability| capability == "theme_selection"));
        assert!(!offer
            .optional_methods
            .iter()
            .any(|method| method == "theme/select"));
        api_v03::validate_offer(&offer).unwrap();
    }
}

#[test]
fn api_v03_session_lifecycle_offer_is_conditional_on_a_bound_driver() {
    let unavailable = api_v03_host_offer_for_services(1024, 4, false, false).unwrap();
    assert!(unavailable
        .optional_capabilities
        .iter()
        .all(|capability| capability != "session_lifecycle"));
    assert!(unavailable.optional_methods.iter().all(|method| {
        api_v03::method_spec(method)
            .is_none_or(|specification| specification.capability != "session_lifecycle")
    }));
    api_v03::validate_offer(&unavailable).unwrap();

    let available = api_v03_host_offer_for_services(1024, 4, true, false).unwrap();
    assert!(available
        .optional_capabilities
        .iter()
        .any(|capability| capability == "session_lifecycle"));
    assert_eq!(
        available
            .optional_methods
            .iter()
            .filter(|method| {
                api_v03::method_spec(method)
                    .is_some_and(|specification| specification.capability == "session_lifecycle")
            })
            .count(),
        4
    );
}

#[tokio::test]
async fn session_lifecycle_service_is_bounded_and_epoch_fenced() {
    assert!(ExtensionSessionLifecycleService::channel(0).is_err());
    assert!(
        ExtensionSessionLifecycleService::channel(MAX_EXTENSION_SESSION_LIFECYCLE_QUEUE + 1)
            .is_err()
    );

    let (service, mut receiver) = ExtensionSessionLifecycleService::channel(1).unwrap();
    assert!(matches!(
        service.try_submit(ExtensionSessionLifecycleOperation::Create),
        Err(SessionLifecycleSubmitError::Unavailable)
    ));

    service.activate();
    let stale = service
        .try_submit(ExtensionSessionLifecycleOperation::Fork {
            entry_id: None,
            at: false,
        })
        .unwrap();
    assert!(matches!(
        service.try_submit(ExtensionSessionLifecycleOperation::Reload),
        Err(SessionLifecycleSubmitError::Full)
    ));
    service.deactivate();
    assert!(receiver.try_next().is_none());
    assert_eq!(
        stale.await.unwrap(),
        Err(ExtensionSessionLifecycleError::Unavailable)
    );

    service.activate();
    let completed = service
        .try_submit(ExtensionSessionLifecycleOperation::Switch {
            session_id: "safe-session".into(),
        })
        .unwrap();
    let request = receiver.try_next().expect("active request");
    assert_eq!(
        request.operation(),
        &ExtensionSessionLifecycleOperation::Switch {
            session_id: "safe-session".into(),
        }
    );
    request.respond(Ok("safe-session".into()));
    assert_eq!(completed.await.unwrap(), Ok("safe-session".into()));
}

#[tokio::test]
async fn api_v03_session_lifecycle_dispatch_validates_and_settles_canonically() {
    let (events, _receiver) = broadcast::channel(8);
    let (mut state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    let (service, mut receiver) = ExtensionSessionLifecycleService::channel(1).unwrap();
    service.activate();
    let offer = api_v03_host_offer_for_services(1024, 4, true, false).unwrap();
    let mut selection = api_v03::select_required(&offer).unwrap();
    selection.capabilities.push("session_lifecycle".into());
    selection.methods.extend(
        [
            methods::SESSION_CREATE,
            methods::SESSION_FORK,
            methods::SESSION_RELOAD,
            methods::SESSION_SWITCH,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    let contract = api_v03::negotiate(&offer, &selection).unwrap();
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_3.into(),
        features: contract.capabilities.clone(),
        max_concurrent_requests: contract.limits.max_concurrent_requests,
        lifecycle_events: BTreeSet::new(),
    };
    *write_std_lock(&state.api_v03_contract) = Some(contract);
    state.session_lifecycle = Some(service);

    let cases = vec![
        (
            "create-request",
            methods::SESSION_CREATE,
            serde_json::json!({}),
            ExtensionSessionLifecycleOperation::Create,
            "created-session",
        ),
        (
            "fork-request",
            methods::SESSION_FORK,
            serde_json::json!({}),
            ExtensionSessionLifecycleOperation::Fork {
                entry_id: None,
                at: false,
            },
            "forked-session",
        ),
        (
            "reload-request",
            methods::SESSION_RELOAD,
            serde_json::json!({}),
            ExtensionSessionLifecycleOperation::Reload,
            "reloaded-session",
        ),
        (
            "switch-request",
            methods::SESSION_SWITCH,
            serde_json::json!({"session_id": "target-session"}),
            ExtensionSessionLifecycleOperation::Switch {
                session_id: "target-session".into(),
            },
            "target-session",
        ),
    ];
    for (request_id, method, params, expected_operation, result_session_id) in cases {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        });
        let request = api_v03::canonical_json(&request).unwrap();
        handle_protocol_line(request.as_bytes(), &state).unwrap();
        let request = receiver.try_next().expect("session lifecycle request");
        assert_eq!(request.operation(), &expected_operation);
        request.respond(Ok(result_session_id.into()));

        let mut frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("response frame deadline")
            .expect("response frame");
        let response =
            serde_json::from_slice::<serde_json::Value>(frame.line.strip_suffix(b"\n").unwrap())
                .unwrap();
        assert_eq!(response["result"]["session_id"], result_session_id);
        frame
            .completion
            .take()
            .expect("response completion")
            .send(Ok(()))
            .unwrap();
    }

    let malformed = serde_json::json!({
        "jsonrpc": "2.0",
        "id": "bad-switch-request",
        "method": methods::SESSION_SWITCH,
        "params": {},
    });
    let malformed = api_v03::canonical_json(&malformed).unwrap();
    let error = handle_protocol_line(malformed.as_bytes(), &state).unwrap_err();
    assert!(error.contains("invalid API 0.3 session/switch request"));
}

#[derive(Clone)]
struct RecordingSecretBroker {
    requests: Arc<StdMutex<Vec<ExtensionSecretRequest>>>,
}

struct UnavailableSecretBroker {
    fail: bool,
}

#[async_trait::async_trait]
impl ExtensionSecretBroker for UnavailableSecretBroker {
    async fn get_secret(
        &self,
        _request: ExtensionSecretRequest,
    ) -> Result<
        Option<crate::extension_secret::ExtensionSecretValue>,
        crate::extension_secret::ExtensionSecretError,
    > {
        if self.fail {
            Err(crate::extension_secret::ExtensionSecretError::Provider(
                "provider detail must not cross the wire".into(),
            ))
        } else {
            Ok(None)
        }
    }
}

#[async_trait::async_trait]
impl ExtensionSecretBroker for RecordingSecretBroker {
    async fn get_secret(
        &self,
        request: ExtensionSecretRequest,
    ) -> Result<
        Option<crate::extension_secret::ExtensionSecretValue>,
        crate::extension_secret::ExtensionSecretError,
    > {
        lock_std_mutex(&self.requests).push(request);
        Ok(Some(crate::extension_secret::ExtensionSecretValue::new(
            "host-secret",
        )?))
    }
}

#[cfg(unix)]
#[test]
fn bundled_subagent_unicode_summary_publishes_through_the_host_reader() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-subagents");
    let output = std::process::Command::new("python3")
        .current_dir(root)
        .args([
            "-c",
            r#"
import json
from tests.test_presentation import PresentationTests
from octet_subagents.model import sanitize_document
from octet_subagents.presentation import build_snapshot
worker = PresentationTests().worker('done', summary=sanitize_document(
    'joined \U0001f469\u200d\U0001f4bb hidden\u200b bidi\u202e done', 8192))
snapshot = build_snapshot([worker], selected_agent_id=worker.agent_id, now_ms=1700000001000)
snapshot['revision'] = 1
print(json.dumps({'jsonrpc':'2.0', 'method':'presentation/update', 'params':{'snapshot':snapshot}}))
"#,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (events, mut receiver) = broadcast::channel(8);
    let (state, _frames) = protocol_read_state_for_test(
        ManifestContributions {
            presentation: true,
            commands: vec!["subagents".into()],
            ..ManifestContributions::default()
        },
        events,
    );
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_4.into();
    handle_protocol_line(&output.stdout, &state).expect("bundled snapshot passes host validation");
    let ExtensionEvent::PresentationUpdated { snapshot, .. } = receiver.try_recv().unwrap() else {
        panic!("host must publish the accepted snapshot");
    };
    let body = &snapshot.collection.unwrap().detail.unwrap().body;
    assert!(body.contains("\\u200d"));
    assert!(body.contains("\\u200b"));
    assert!(body.contains("\\u202e"));
    assert!(!body.contains('\u{200d}'));
}

#[test]
fn semantic_presentation_is_api_0_2_only_bounded_and_declared() {
    let (events, mut receiver) = broadcast::channel(8);
    let mut declared = ManifestContributions {
        presentation: true,
        commands: vec!["workers".into()],
        ..ManifestContributions::default()
    };
    let (mut state, _frames) = protocol_read_state_for_test(declared.clone(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: BTreeSet::from([
            EXTENSION_FEATURE_REQUEST_CANCELLATION.into(),
            EXTENSION_FEATURE_CONTENT_PARTS.into(),
        ]),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let update = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "presentation/update",
        "params": {
            "snapshot": {
                "revision": 4,
                "status": {"state": "active", "label": "1 worker"},
                "activities": [{
                    "id": "worker:1",
                    "kind": "delegation",
                    "state": "running",
                    "summary": "Reviewing tests"
                }],
                "actions": [{
                    "id": "stop",
                    "label": "Stop worker",
                    "command": "workers",
                    "arguments": ["stop", "worker:1"],
                    "destructive": true
                }]
            }
        }
    });
    handle_protocol_line(&serde_json::to_vec(&update).unwrap(), &state).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        ExtensionEvent::PresentationUpdated {
            generation: 1,
            resource_owner: None,
            snapshot: ExtensionPresentationSnapshot { revision: 4, .. }
        }
    ));

    let owner = test_resource_owner("owner-a");
    insert_test_parent(&state, 7, Some(owner.clone()));
    let mut owner_update = update.clone();
    owner_update["params"]["parent_request_id"] = serde_json::json!(7);
    owner_update["params"]["snapshot"]["revision"] = serde_json::json!(5);
    handle_protocol_line(&serde_json::to_vec(&owner_update).unwrap(), &state).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        ExtensionEvent::PresentationUpdated {
            generation: 1,
            resource_owner: Some(observed),
            snapshot: ExtensionPresentationSnapshot { revision: 5, .. }
        } if observed == owner
    ));

    let mut background_update = update.clone();
    background_update["params"]["resource_owner"] = serde_json::to_value(&owner).unwrap();
    background_update["params"]["snapshot"]["revision"] = serde_json::json!(6);
    handle_protocol_line(&serde_json::to_vec(&background_update).unwrap(), &state).unwrap();
    assert!(matches!(
        receiver.try_recv().unwrap(),
        ExtensionEvent::PresentationUpdated {
            resource_owner: Some(observed),
            snapshot: ExtensionPresentationSnapshot { revision: 6, .. },
            ..
        } if observed == owner
    ));
    let forged_owner = test_resource_owner("never-issued-owner");
    background_update["params"]["resource_owner"] = serde_json::to_value(&forged_owner).unwrap();
    assert_eq!(
        handle_protocol_line(&serde_json::to_vec(&background_update).unwrap(), &state).unwrap_err(),
        "presentation update resource owner is stale or foreign"
    );
    background_update["params"]["resource_owner"] = serde_json::to_value(&owner).unwrap();
    background_update["params"]["resource_owner"]["process_generation"] = serde_json::json!(2);
    assert!(
        handle_protocol_line(&serde_json::to_vec(&background_update).unwrap(), &state)
            .unwrap_err()
            .contains("stale or foreign")
    );

    declared.commands.clear();
    state.declared = declared;
    let error = handle_protocol_line(&serde_json::to_vec(&update).unwrap(), &state)
        .expect_err("actions cannot route to undeclared commands");
    assert!(error.contains("undeclared command"));

    state.declared.commands = vec!["workers".into()];
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_1.into();
    let error = handle_protocol_line(&serde_json::to_vec(&update).unwrap(), &state)
        .expect_err("presentation is not backported to API 0.1");
    assert!(error.contains("requires extension API 0.2"));
}

#[tokio::test]
async fn semantic_presentation_dispatch_coalesces_bursts_without_losing_terminal_snapshot() {
    let (events, mut receiver) = broadcast::channel(128);
    let (updates, update_rx) = watch::channel(None);
    let snapshot = |revision| ExtensionPresentationSnapshot {
        revision,
        status: None,
        activities: Vec::new(),
        collection: None,
        actions: Vec::new(),
    };
    let dispatch = tokio::spawn(dispatch_presentation_updates(update_rx, events, 9));
    for revision in 0..MAX_PRESENTATION_UPDATES_PER_SECOND {
        updates.send_replace(Some((revision as u64 + 1, None, snapshot(revision as u64))));
        loop {
            if matches!(
                receiver.recv().await.unwrap(),
                ExtensionEvent::PresentationUpdated { .. }
            ) {
                break;
            }
        }
    }
    for revision in MAX_PRESENTATION_UPDATES_PER_SECOND..=40 {
        updates.send_replace(Some((revision as u64 + 1, None, snapshot(revision as u64))));
    }

    let terminal = tokio::time::timeout(Duration::from_millis(1_500), async {
        loop {
            if let ExtensionEvent::PresentationUpdated { snapshot, .. } =
                receiver.recv().await.unwrap()
            {
                if snapshot.revision == 40 {
                    return snapshot.revision;
                }
            }
        }
    })
    .await
    .expect("coalesced terminal snapshot");
    assert_eq!(terminal, 40);
    drop(updates);
    dispatch.await.unwrap();
}

#[test]
fn semantic_presentation_update_rate_is_bounded_per_generation() {
    let (events, mut receiver) = broadcast::channel(128);
    let declared = ManifestContributions {
        presentation: true,
        ..ManifestContributions::default()
    };
    let (state, _frames) = protocol_read_state_for_test(declared, events);
    write_std_lock(&state.protocol).version = EXTENSION_API_VERSION_0_2.into();
    for revision in 0..(MAX_PRESENTATION_UPDATES_PER_SECOND + 5) {
        let update = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "presentation/update",
            "params": {
                "snapshot": {
                    "revision": revision,
                    "activities": [],
                    "actions": [],
                }
            }
        });
        handle_protocol_line(&serde_json::to_vec(&update).unwrap(), &state).unwrap();
    }

    let mut accepted = 0;
    let mut diagnostics = 0;
    while let Ok(event) = receiver.try_recv() {
        match event {
            ExtensionEvent::PresentationUpdated { .. } => accepted += 1,
            ExtensionEvent::Diagnostic { message } => {
                diagnostics += 1;
                assert!(message.contains("update rate exceeded"));
            }
            _ => {}
        }
    }
    assert_eq!(accepted, MAX_PRESENTATION_UPDATES_PER_SECOND);
    assert_eq!(diagnostics, 1);
}

#[test]
fn answered_confirmations_cover_exactly_the_buffered_event_window() {
    let generation = 7;
    let mut answered = AnsweredConfirmations::default();

    for id in 0..ANSWERED_CONFIRMATION_CAPACITY {
        assert!(answered.insert(generation, ExtensionRequestId::Number(id as u64)));
    }
    assert_eq!(answered.len(), EXTENSION_EVENT_CAPACITY);
    assert!(answered.contains(generation, &ExtensionRequestId::Number(0)));

    assert!(!answered.insert(generation, ExtensionRequestId::Number(0)));
    assert_eq!(answered.len(), ANSWERED_CONFIRMATION_CAPACITY);

    assert!(answered.insert(
        generation,
        ExtensionRequestId::Number(ANSWERED_CONFIRMATION_CAPACITY as u64)
    ));
    assert_eq!(answered.len(), ANSWERED_CONFIRMATION_CAPACITY);
    assert!(!answered.contains(generation, &ExtensionRequestId::Number(0)));
    assert!(answered.contains(generation, &ExtensionRequestId::Number(1)));

    answered.retain_generation(generation + 1);
    assert_eq!(answered.len(), 0);
    assert!(answered.insert(generation + 1, ExtensionRequestId::Number(0)));
}

#[test]
fn old_operation_rejects_reused_parent_id_from_replacement_generation() {
    let old_operation = ExtensionOperationToken {
        generation: 4,
        parent_request_id: 2,
    };
    let replacement_event = ExtensionEvent::InputRequested {
        request_id: ExtensionRequestId::String("replacement-input".into()),
        generation: 5,
        parent_request_id: 2,
        request: ExtensionInputRequest {
            parent_request_id: 2,
            prompt: "replacement prompt".into(),
            secret: false,
        },
    };

    let ExtensionEvent::InputRequested {
        generation,
        parent_request_id,
        ..
    } = replacement_event
    else {
        unreachable!();
    };
    assert!(old_operation.owns(4, 2));
    assert!(!old_operation.owns(generation, parent_request_id));
}

fn child_request(parent_request_id: u64, state: u8) -> ChildRequest {
    ChildRequest {
        exec_cancelled: false,
        remote_ui: None,
        parent_request_id,
        response_state: Arc::new(ChildResponseState {
            state: AtomicU8::new(state),
            changed: Notify::new(),
            cancel_on_response_abort: StdMutex::new(None),
            composition_cancellation: StdMutex::new(None),
            session_leaf_cancel: StdMutex::new(None),
        }),
        policy_intent: None,
    }
}

#[test]
fn parent_settlement_cancels_only_active_child_requests() {
    let children: ChildRequests = Arc::new(StdMutex::new(HashMap::new()));
    let active_id = ExtensionRequestId::String("active".into());
    let responding_id = ExtensionRequestId::String("responding".into());
    let unrelated_id = ExtensionRequestId::String("unrelated".into());
    lock_std_mutex(&children).insert(active_id.clone(), child_request(7, CHILD_ACTIVE));
    lock_std_mutex(&children).insert(responding_id.clone(), child_request(7, CHILD_RESPONDING));
    lock_std_mutex(&children).insert(unrelated_id.clone(), child_request(8, CHILD_ACTIVE));

    assert_eq!(
        cancel_active_children(&children, 7, "parent settled"),
        vec![active_id.clone()]
    );
    let children = lock_std_mutex(&children);
    assert!(!children.contains_key(&active_id));
    assert_eq!(
        children[&responding_id]
            .response_state
            .state
            .load(Ordering::Acquire),
        CHILD_RESPONDING
    );
    assert!(children.contains_key(&unrelated_id));
}

#[test]
fn child_response_claim_restores_before_admission_and_settles_after() {
    let children: ChildRequests = Arc::new(StdMutex::new(HashMap::new()));
    let id = ExtensionRequestId::String("claim".into());
    let child = child_request(7, CHILD_RESPONDING);
    let response_state = Arc::clone(&child.response_state);
    lock_std_mutex(&children).insert(id.clone(), child);
    drop(ChildResponseClaim {
        child_requests: Arc::clone(&children),
        id: id.clone(),
        response_state: Arc::clone(&response_state),
        admitted: false,
        abort_cancel: None,
    });
    assert_eq!(response_state.state.load(Ordering::Acquire), CHILD_ACTIVE);
    assert!(lock_std_mutex(&children).contains_key(&id));

    response_state
        .state
        .store(CHILD_RESPONDING, Ordering::Release);
    let mut claim = ChildResponseClaim {
        child_requests: Arc::clone(&children),
        id: id.clone(),
        response_state: Arc::clone(&response_state),
        admitted: false,
        abort_cancel: None,
    };
    claim.mark_admitted();
    drop(claim);
    assert_eq!(response_state.state.load(Ordering::Acquire), CHILD_SETTLED);
    assert!(!lock_std_mutex(&children).contains_key(&id));
}

#[test]
fn parent_cancel_during_response_claim_is_deferred_until_abort() {
    let children: ChildRequests = Arc::new(StdMutex::new(HashMap::new()));
    let id = ExtensionRequestId::String("deferred".into());
    let child = child_request(7, CHILD_RESPONDING);
    let response_state = Arc::clone(&child.response_state);
    lock_std_mutex(&children).insert(id.clone(), child);
    let (writer, mut frames) = mpsc::channel(1);
    let claim = ChildResponseClaim {
        child_requests: Arc::clone(&children),
        id: id.clone(),
        response_state: Arc::clone(&response_state),
        admitted: false,
        abort_cancel: Some((
            writer,
            Arc::new(ProtocolFrameLimit::new(
                DEFAULT_EXTENSION_MESSAGE_BYTES,
                false,
            )),
        )),
    };

    assert!(cancel_active_children(&children, 7, "parent settled").is_empty());
    assert_eq!(
        response_state.state.load(Ordering::Acquire),
        CHILD_RESPONDING
    );
    drop(claim);

    assert_eq!(response_state.state.load(Ordering::Acquire), CHILD_SETTLED);
    assert!(!lock_std_mutex(&children).contains_key(&id));
    let frame = frames.try_recv().expect("deferred cancel frame");
    let cancel: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    assert_eq!(cancel["method"], methods::CANCEL_REQUEST);
    assert_eq!(cancel["params"]["id"], "deferred");
}

#[test]
fn cancelled_artifact_child_rolls_back_published_bytes() {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrollback-fixture";
    let (events, _events_rx) = broadcast::channel(4);
    let (state, _frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    let id = ExtensionRequestId::String("artifact:cancelled".into());
    insert_child_request(&state, id.clone(), Some(7), None).expect("register child");
    let published = state
        .artifact_store
        .publish(
            1,
            ArtifactPublication {
                source: ArtifactSource::Inline(bytes::Bytes::from_static(PNG)),
                mime_type: "image/png".into(),
                size: PNG.len() as u64,
                sha256: crate::tool::content_hash(PNG),
            },
        )
        .expect("publish before cancellation wins");

    assert_eq!(
        cancel_active_children(&state.child_requests, 7, "parent cancelled"),
        vec![id.clone()]
    );
    let delivery = try_queue_child_response(
        &state.child_requests,
        &id,
        &state.writer,
        state.max_message_bytes(),
        serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "result":{"artifact_id":published.id.to_string()},
        }),
    );
    assert_eq!(delivery, Ok(ChildResponseAdmission::AlreadySettled));
    rollback_undelivered_artifact(&state.artifact_store, 1, Some(&published.id), &delivery);
    assert!(matches!(
        state.artifact_store.resolve_artifact(1, &published.id),
        Err(crate::artifact::ArtifactError::UnknownArtifact)
    ));
    state
        .artifact_store
        .publish(
            1,
            ArtifactPublication {
                source: ArtifactSource::Inline(bytes::Bytes::from_static(PNG)),
                mime_type: "image/png".into(),
                size: PNG.len() as u64,
                sha256: crate::tool::content_hash(PNG),
            },
        )
        .expect("rollback recovers publication capacity");
}

#[test]
fn approval_token_retry_is_single_use_and_emits_no_second_policy_event() {
    let (events, mut events_rx) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .copied()
            .chain([
                EXTENSION_FEATURE_POLICY_INTENTS,
                EXTENSION_FEATURE_APPROVALS,
            ])
            .map(str::to_owned)
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    insert_test_parent(&state, 7, Some(test_resource_owner("owner-a")));
    let intent = ExtensionActionIntent {
        kind: "external_side_effect".into(),
        operation: "browser.submit_form".into(),
        target: serde_json::json!({"origin":"https://example.com"}),
        data_classes: vec!["user_text".into()],
        adapter_hints: Default::default(),
    };
    let token = state
        .approval_store
        .issue(
            &intent,
            1,
            ExtensionRequestId::Number(7),
            Duration::from_secs(30),
        )
        .expect("issue token");
    for (id, expected) in [("approved", "allow"), ("reused", "deny")] {
        let line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":methods::POLICY_EVALUATE,
            "params":{
                "parent_request_id":7,
                "intent":intent,
                "approval_token":token,
            },
        }))
        .unwrap();
        handle_protocol_line(&line, &state).expect("token retry is handled");
        let frame = frames.try_recv().expect("policy response");
        let response: serde_json::Value =
            serde_json::from_slice(&frame.line).expect("JSON response");
        assert_eq!(response["result"]["decision"], expected);
    }
    assert!(matches!(
        events_rx.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn secret_lookup_forwards_host_owner_and_requires_owner() {
    let (events, _events_rx) = broadcast::channel(8);
    let (mut state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .copied()
            .chain([EXTENSION_FEATURE_SECRETS])
            .map(str::to_owned)
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let requests = Arc::new(StdMutex::new(Vec::new()));
    state.secret_broker = Some(Arc::new(RecordingSecretBroker {
        requests: Arc::clone(&requests),
    }));
    state.allowed_secrets = Arc::new(BTreeSet::from(["browser.api_token".into()]));
    let owner = test_resource_owner("owner-a");
    insert_test_parent(&state, 7, Some(owner.clone()));
    let line = serde_json::to_vec(&serde_json::json!({
        "jsonrpc":"2.0",
        "id":"secret-1",
        "method":methods::SECRET_GET,
        "params":{"parent_request_id":7,"name":"browser.api_token"},
    }))
    .unwrap();
    handle_protocol_line(&line, &state).expect("secret request accepted");
    let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
        .await
        .expect("secret response timeout")
        .expect("secret response frame");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON response");
    assert_eq!(response["result"]["value"], "host-secret");
    assert_eq!(
        lock_std_mutex(&requests).as_slice(),
        &[ExtensionSecretRequest {
            extension: state.extension_identity.clone(),
            resource_owner: owner,
            parent_request_id: 7,
            name: "browser.api_token".into(),
        }]
    );

    insert_test_parent(&state, 8, None);
    let ownerless = serde_json::to_vec(&serde_json::json!({
        "jsonrpc":"2.0",
        "id":"secret-ownerless",
        "method":methods::SECRET_GET,
        "params":{"parent_request_id":8,"name":"browser.api_token"},
    }))
    .unwrap();
    handle_protocol_line(&ownerless, &state).expect("ownerless request receives an error");
    let frame = frames.recv().await.expect("ownerless response");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    assert_eq!(response["error"]["code"], -32002);
}

#[tokio::test]
async fn unavailable_and_failed_secret_lookups_are_wire_indistinguishable() {
    let (events, _events_rx) = broadcast::channel(8);
    let (mut state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .copied()
            .chain([EXTENSION_FEATURE_SECRETS])
            .map(str::to_owned)
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    state.allowed_secrets = Arc::new(BTreeSet::from(["browser.api_token".into()]));
    let mut errors = Vec::new();
    for (parent, id, fail) in [(7, "missing", false), (8, "failed", true)] {
        insert_test_parent(&state, parent, Some(test_resource_owner("owner-a")));
        state.secret_broker = Some(Arc::new(UnavailableSecretBroker { fail }));
        let line = serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":methods::SECRET_GET,
            "params":{"parent_request_id":parent,"name":"browser.api_token"},
        }))
        .unwrap();
        handle_protocol_line(&line, &state).expect("lookup receives bounded response");
        let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .expect("secret error timeout")
            .expect("secret error frame");
        let response: serde_json::Value =
            serde_json::from_slice(&frame.line).expect("JSON response");
        errors.push(response["error"].clone());
    }
    assert_eq!(errors[0], errors[1]);
    assert_eq!(errors[0]["code"], -32004);
    assert_eq!(errors[0]["message"], "secret is unavailable");
}

#[tokio::test]
async fn artifact_publication_requires_and_preserves_the_host_owner() {
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nowner-fixture";
    let (events, _events_rx) = broadcast::channel(8);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .copied()
            .chain([EXTENSION_FEATURE_ARTIFACTS])
            .map(str::to_owned)
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let publication = |id: &str, parent_request_id| {
        serde_json::to_vec(&serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":methods::ARTIFACT_PUBLISH,
            "params":{
                "parent_request_id":parent_request_id,
                "mime_type":"image/png",
                "size":PNG.len(),
                "sha256":crate::tool::content_hash(PNG),
                "data":{
                    "encoding":"base64",
                    "data":base64::engine::general_purpose::STANDARD.encode(PNG),
                },
            },
        }))
        .unwrap()
    };

    insert_test_parent(&state, 7, Some(test_resource_owner("owner-a")));
    handle_protocol_line(&publication("artifact-a", 7), &state)
        .expect("owner-scoped publication accepted");
    let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
        .await
        .expect("artifact response timeout")
        .expect("artifact response frame");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    let artifact_id: ArtifactId =
        serde_json::from_value(response["result"]["artifact_id"].clone()).expect("artifact id");
    assert!(state
        .artifact_store
        .resolve_artifact_for_owner(1, "owner-a", &artifact_id)
        .is_ok());
    assert!(matches!(
        state
            .artifact_store
            .resolve_artifact_for_owner(1, "owner-b", &artifact_id),
        Err(crate::artifact::ArtifactError::UnknownArtifact)
    ));

    insert_test_parent(&state, 8, None);
    handle_protocol_line(&publication("artifact-ownerless", 8), &state)
        .expect("ownerless publication receives an error");
    let frame = frames.recv().await.expect("ownerless artifact response");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    assert_eq!(response["error"]["code"], -32002);
}

#[test]
fn child_request_ids_are_unique_for_the_process_generation() {
    let (events, _events_rx) = broadcast::channel(4);
    let (state, _frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    let id = ExtensionRequestId::String("py:1".into());
    register_child_request(&state, id.clone(), Some(1), methods::INPUT_REQUEST).expect("first ID");
    assert!(settle_child_request(&state.child_requests, &id));
    let error = register_child_request(&state, id, Some(1), methods::INPUT_REQUEST)
        .err()
        .expect("ID reuse must fail");
    assert_eq!(error, "reused extension-originated request id");
}

#[test]
fn child_arriving_after_parent_cancellation_is_terminal_not_fatal() {
    let (events, _events_rx) = broadcast::channel(4);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let (reply, _reply_rx) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        7,
        PendingRequest {
            method: "tool/call".into(),
            sender: reply,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: None,
            child_interaction_progress: None,
            resource_owner: None,
            last_progress_sequence: None,
            tool_call_policy_digest: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
    lock_std_mutex(&state.pending).remove(&7);

    let line = br#"{"jsonrpc":"2.0","id":"late-input","method":"input/request","params":{"parent_request_id":7,"prompt":"Too late","secret":false}}"#;
    handle_protocol_line(line, &state).expect("late child is a normal cancellation race");
    let frame = frames.try_recv().expect("terminal child response");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    assert_eq!(response["id"], "late-input");
    assert_eq!(response["error"]["code"], JSON_RPC_REQUEST_CANCELLED);
    assert!(lock_std_mutex(&state.child_requests).is_empty());
    assert!(lock_std_mutex(&state.seen_child_request_ids)
        .contains(&ExtensionRequestId::String("late-input".into())));

    let reuse = handle_protocol_line(line, &state)
        .expect_err("terminal child IDs remain consumed for the generation");
    assert_eq!(reuse, "reused extension-originated request id");
}

#[test]
fn parent_settlement_cannot_overtake_child_registration() {
    let (events, _events_rx) = broadcast::channel(4);
    let (state, _frames) = protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let (reply, _reply_rx) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        7,
        PendingRequest {
            method: "tool/call".into(),
            sender: reply,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: None,
            child_interaction_progress: None,
            resource_owner: None,
            last_progress_sequence: None,
            tool_call_policy_digest: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
    let state = Arc::new(state);
    let id = ExtensionRequestId::String("input:cancel-race".into());
    let child_map = lock_std_mutex(&state.child_requests);
    let register_state = Arc::clone(&state);
    let register_id = id.clone();
    let registration = std::thread::spawn(move || {
        register_child_request(
            &register_state,
            register_id,
            Some(7),
            methods::INPUT_REQUEST,
        )
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        match state.pending.try_lock() {
            Err(std::sync::TryLockError::WouldBlock) => break,
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("pending lock poisoned"),
            Ok(pending) => drop(pending),
        }
        assert!(
            Instant::now() < deadline,
            "registration did not hold the parent lock while awaiting child insertion"
        );
        std::thread::yield_now();
    }

    let cancel_state = Arc::clone(&state);
    let cancellation = std::thread::spawn(move || {
        let mut pending = lock_std_mutex(&cancel_state.pending);
        pending.remove(&7);
        drop(pending);
        cancel_active_children(&cancel_state.child_requests, 7, "parent settled")
    });
    drop(child_map);

    let response_state = registration
        .join()
        .expect("registration thread")
        .expect("register child")
        .expect("parent remains active through registration")
        .response_state;
    let cancelled = cancellation.join().expect("cancellation thread");

    assert_eq!(cancelled, vec![id.clone()]);
    assert!(!lock_std_mutex(&state.child_requests).contains_key(&id));
    assert_eq!(response_state.state.load(Ordering::Acquire), CHILD_SETTLED);
}

#[test]
fn non_tool_input_is_delivered_to_an_event_consumer() {
    let (events, mut events_rx) = broadcast::channel(4);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let (reply, _reply_rx) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        7,
        PendingRequest {
            method: "tool/call".into(),
            sender: reply,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: None,
            child_interaction_progress: None,
            resource_owner: None,
            last_progress_sequence: None,
            tool_call_policy_digest: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
    handle_protocol_line(
            br#"{"jsonrpc":"2.0","id":"py:1","method":"input/request","params":{"parent_request_id":7,"prompt":"Token?","secret":true}}"#,
            &state,
        )
        .expect("input event delivery");
    assert!(frames.try_recv().is_err());
    assert!(matches!(
        events_rx.try_recv(),
        Ok(ExtensionEvent::InputRequested {
            request_id: ExtensionRequestId::String(id),
            generation: 1,
            parent_request_id: 7,
            request: ExtensionInputRequest { prompt, secret: true, .. },
        }) if id == "py:1" && prompt == "Token?"
    ));
    assert!(lock_std_mutex(&state.child_requests)
        .contains_key(&ExtensionRequestId::String("py:1".into())));
}

#[test]
fn non_tool_input_fails_closed_without_an_event_consumer() {
    let (events, events_rx) = broadcast::channel(4);
    drop(events_rx);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_2.into(),
        features: API_0_2_REQUIRED_FEATURES
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };
    let (reply, _reply_rx) = oneshot::channel();
    lock_std_mutex(&state.pending).insert(
        7,
        PendingRequest {
            method: "tool/call".into(),
            sender: reply,
            terminal: Arc::new(AtomicU8::new(REQUEST_ACTIVE)),
            frame_state: Arc::new(AtomicU8::new(FRAME_WRITTEN)),
            cancellation_sent: Arc::new(AtomicBool::new(false)),
            progress: None,
            child_interaction_progress: None,
            resource_owner: None,
            last_progress_sequence: None,
            tool_call_policy_digest: None,
            composition_files: Arc::new(CompositionFiles::default()),
        },
    );
    handle_protocol_line(
            br#"{"jsonrpc":"2.0","id":"py:1","method":"input/request","params":{"parent_request_id":7,"prompt":"Token?","secret":true}}"#,
            &state,
        )
        .expect("fail-closed input response");
    let frame = frames.try_recv().expect("null response frame");
    let response: serde_json::Value = serde_json::from_slice(&frame.line).expect("JSON");
    assert_eq!(response["id"], "py:1");
    assert!(response["result"]["value"].is_null());
    assert!(lock_std_mutex(&state.child_requests).is_empty());
}

#[test]
fn prospective_tool_catalog_has_one_input_and_output_schema_byte_budget() {
    let tool = |name: &str, bytes: usize| ToolDefinition {
        default_active: None,
        nested_execution: false,
        prepare_arguments: false,
        prompt_snippet: None,
        prompt_guidelines: Vec::new(),
        operation: None,
        name: name.into(),
        description: "bounded definition".into(),
        parameters: serde_json::json!({"type": "object", "description": "x".repeat(bytes)}),
        output_schema: Some(
            serde_json::json!({"type": "object", "description": "y".repeat(bytes)}),
        ),
        composition: None,
        constrained_sampling: None,
    };
    let mut catalog = Vec::new();
    for index in 0..6 {
        let next = tool(&format!("tool_{index}"), 300_000);
        validate_tool_definitions(std::slice::from_ref(&next), EXTENSION_API_VERSION_0_2).unwrap();
        catalog.push(next);
        validate_tool_definitions(&catalog, EXTENSION_API_VERSION_0_2).unwrap();
    }
    let addition = tool("overflow", 300_000);
    validate_tool_definitions(std::slice::from_ref(&addition), EXTENSION_API_VERSION_0_2).unwrap();
    let mut prospective = catalog.clone();
    prospective.push(addition);
    assert!(
        validate_tool_definitions(&prospective, EXTENSION_API_VERSION_0_2)
            .unwrap_err()
            .to_string()
            .contains("aggregate schema bytes")
    );
    validate_tool_definitions(&catalog, EXTENSION_API_VERSION_0_2).unwrap();
    prospective[0] = tool("tool_0", 1);
    validate_tool_definitions(&prospective, EXTENSION_API_VERSION_0_2).unwrap();
    // Inclusive exact byte boundary, without allocating a serialized copy.
    let mut exact = tool("exact", 0);
    exact.output_schema = None;
    let overhead = serde_json::to_vec(&exact.parameters).unwrap().len();
    exact.parameters["description"] =
        serde_json::Value::String("z".repeat(MAX_TOOL_CATALOG_SCHEMA_BYTES - overhead));
    validate_tool_definitions(std::slice::from_ref(&exact), EXTENSION_API_VERSION_0_2).unwrap();
    exact.output_schema = Some(serde_json::json!({}));
    assert!(validate_tool_definitions(&[exact], EXTENSION_API_VERSION_0_2).is_err());
}

#[test]
fn structured_validation_has_a_total_operation_budget() {
    let schema = serde_json::json!({
        "allOf": vec![serde_json::json!({}); MAX_SCHEMA_VALIDATION_STEPS + 1]
    });
    assert!(
        validate_structured_content(&schema, &serde_json::Value::Null)
            .expect_err("validation must be bounded")
            .contains("budget")
    );
}

#[test]
fn lifecycle_reasons_are_clipped_on_a_utf8_boundary() {
    let mut reason = "🦀".repeat(MAX_LIFECYCLE_REASON_BYTES);
    truncate_utf8(&mut reason, MAX_LIFECYCLE_REASON_BYTES);
    assert!(reason.len() <= MAX_LIFECYCLE_REASON_BYTES);
    assert!(reason.is_char_boundary(reason.len()));

    let health = StdRwLock::new(ConnectionHealth {
        state: ExtensionHealthState::Ready,
        last_error: None,
    });
    update_health(
        &health,
        ExtensionHealthState::Degraded,
        Some("🦀".repeat(MAX_LIFECYCLE_REASON_BYTES)),
    );
    let health = read_std_lock(&health);
    let last_error = health.last_error.as_deref().expect("last error");
    assert!(last_error.len() <= MAX_LIFECYCLE_REASON_BYTES);
    assert!(last_error.is_char_boundary(last_error.len()));
}

#[test]
fn confirmation_request_string_ids_are_bounded_before_event_delivery() {
    let (events, mut receiver) = broadcast::channel(2);
    let declared = ManifestContributions {
        confirmations: true,
        ..ManifestContributions::default()
    };
    let accepted_id = "x".repeat(MAX_CONFIRMATION_REQUEST_ID_BYTES);
    let (state, _writer_frames) = protocol_read_state_for_test(declared, events);
    let accepted = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": accepted_id,
        "method": methods::CONFIRMATION_REQUEST,
        "params": {"prompt": "Continue?"},
    }))
    .expect("serialize accepted confirmation");
    handle_protocol_line(&accepted, &state)
        .expect("maximum-size confirmation id should be accepted");
    assert!(matches!(
        receiver.try_recv(),
        Ok(ExtensionEvent::ConfirmationRequested {
            request_id: ExtensionRequestId::String(id),
            generation: 1,
            ..
        }) if id.len() == MAX_CONFIRMATION_REQUEST_ID_BYTES
    ));

    let oversized = serde_json::to_vec(&serde_json::json!({
        "jsonrpc": "2.0",
        "id": "x".repeat(MAX_CONFIRMATION_REQUEST_ID_BYTES + 1),
        "method": methods::CONFIRMATION_REQUEST,
        "params": {"prompt": "Continue?"},
    }))
    .expect("serialize oversized confirmation");
    let error = handle_protocol_line(&oversized, &state)
        .expect_err("oversized confirmation id should be rejected");
    assert!(error.contains(&format!("limit is {MAX_CONFIRMATION_REQUEST_ID_BYTES}")));
    assert!(matches!(
        receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn manifest_parses_the_minimum_product_boundary() {
    let manifest = ExtensionManifest::parse(VALID_MANIFEST).expect("valid manifest");
    assert_eq!(manifest.name, "git-tools");
    assert_eq!(
        manifest.capabilities.filesystem,
        ExtensionFilesystemAccess::Workspace
    );
    assert_eq!(manifest.contributes.tools, vec!["git_status"]);
    assert_eq!(
        manifest.contributes.hooks,
        vec![ExtensionHook::AfterToolCall]
    );
    assert_eq!(manifest.contributes.ui, vec![ExtensionUiSurface::Status]);
    assert!(manifest.contributes.context);
    assert!(manifest.contributes.confirmations);
    assert_eq!(manifest.runtime, ExtensionRuntimeSettings::default());
}

#[test]
fn compaction_strategy_manifest_is_api_v04_only() {
    let source = include_str!("../../../../extensions/octet-snap-compact/extension.toml");
    let manifest = ExtensionManifest::parse(source).expect("source extension manifest");
    assert_eq!(manifest.api_version, EXTENSION_API_VERSION_0_4);
    assert_eq!(
        manifest.contributes.hooks,
        vec![ExtensionHook::CompactionStrategy]
    );
    let legacy = source.replace("api_version = \"0.4\"", "api_version = \"0.3\"");
    assert!(matches!(
        ExtensionManifest::parse(&legacy),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("compaction_strategy requires extension API 0.4")
    ));
}

// Also the Windows proof that a `#!/usr/bin/env python3` entrypoint starts
// through a resolved interpreter and completes a protocol round trip.
#[cfg(any(unix, windows))]
#[tokio::test]
async fn compaction_strategy_negotiates_and_renders_through_host() {
    let temp = TempDir::new().expect("tempdir");
    write_executable_script(
        &temp.path().join("extension.py"),
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


init = receive()
assert init["params"]["api_version"] == "0.4"
assert init["params"]["contributes"]["hooks"] == ["compaction_strategy"]
protocol = init["params"]["protocol"]
assert protocol["version"] == "0.4", protocol
assert "compaction_strategy" in protocol["optional_features"]
send({"jsonrpc": "2.0", "id": init["id"], "result": {
    "api_version": "0.4", "tools": [], "commands": [],
    "protocol": {"version": "0.4", "features":
        protocol["required_features"] + ["compaction_strategy"],
        "limits": {"max_concurrent_requests": 1}},
}})
request = receive()
assert request["method"] == "hook/run"
assert request["params"]["hook"] == "compaction_strategy"
assert request["params"]["payload"] == {"model_id": "vision-model", "text": "history"}
send({"jsonrpc": "2.0", "id": request["id"], "result": {
    "disposition": {"action": "continue"}, "context": [], "notifications": [],
    "compaction_frames": ["iVBORw0KGgo="],
}})
shutdown = receive()
assert shutdown["method"] == "shutdown"
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {"terminal": "shutdown"}})
"#,
    );
    let manifest = ExtensionManifest::parse(include_str!(
        "../../../../extensions/octet-snap-compact/extension.toml"
    ))
    .expect("source extension manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start API 0.4 compaction process");
    assert!(process.supports_feature(EXTENSION_FEATURE_COMPACTION_STRATEGY));
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    assert!(host.compaction_strategy.is_some());
    let frames = host
        .compaction_strategy
        .unwrap()
        .render("vision-model", "history", "session-owner")
        .await
        .expect("render through host");
    assert_eq!(frames, vec![b"\x89PNG\r\n\x1a\n".to_vec()]);
    assert!(process.shutdown().await);
}

#[test]
fn manifest_runtime_profiles_require_explicit_safe_sharing() {
    let workspace_service = VALID_MANIFEST
            .replace("api_version = \"0.1\"", "api_version = \"0.2\"")
            .replace(
                "\n[entrypoint]",
                "\n[runtime]\nlifecycle = \"workspace_service\"\nsharing = \"workspace\"\n\n[entrypoint]",
            );
    assert!(ExtensionManifest::parse(&workspace_service).is_ok());

    let legacy_api = workspace_service.replace("api_version = \"0.2\"", "api_version = \"0.1\"");
    assert!(matches!(
        ExtensionManifest::parse(&legacy_api),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("requires extension API 0.2")
    ));

    let isolated_service =
        workspace_service.replace("sharing = \"workspace\"", "sharing = \"isolated\"");
    assert!(matches!(
        ExtensionManifest::parse(&isolated_service),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("requires explicit sharing")
    ));

    let shared_legacy = workspace_service.replace(
        "lifecycle = \"workspace_service\"",
        "lifecycle = \"legacy_resident\"",
    );
    assert!(matches!(
        ExtensionManifest::parse(&shared_legacy),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("must use sharing = \"isolated\"")
    ));
}

#[test]
fn manifest_rejects_api_mismatch_duplicate_names_and_unknown_keys() {
    let mismatch = VALID_MANIFEST.replace("api_version = \"0.1\"", "api_version = \"9\"");
    assert!(matches!(
        ExtensionManifest::parse(&mismatch),
        Err(ExtensionRuntimeError::UnsupportedApiVersion { .. })
    ));

    let duplicate = VALID_MANIFEST.replace(
        "tools = [\"git_status\"]",
        "tools = [\"git_status\", \"git_status\"]",
    );
    assert!(matches!(
        ExtensionManifest::parse(&duplicate),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("duplicate tool")
    ));

    let unknown = VALID_MANIFEST.replace("network = false", "network = false\nshell = true");
    assert!(matches!(
        ExtensionManifest::parse(&unknown),
        Err(ExtensionRuntimeError::ManifestParse(_))
    ));
}

#[test]
fn session_hooks_are_typed_and_available_to_every_stateful_api() {
    let base = r#"name = "session-hooks"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "session-hooks"
[contributes]
hooks = ["session_start", "session_end"]
"#;
    let manifest = ExtensionManifest::parse(base).expect("paired API 0.3 hooks");
    assert_eq!(
        manifest.contributes.hooks,
        vec![ExtensionHook::SessionStart, ExtensionHook::SessionEnd]
    );
    // API 0.4 is the union of every earlier capability: a partial pair and a
    // mix with non-session hooks are ordinary declarations, exactly as in Pi.
    let missing_end = base.replace(
        "hooks = [\"session_start\", \"session_end\"]",
        "hooks = [\"session_start\"]",
    );
    ExtensionManifest::parse(&missing_end).expect("a single session hook is a valid declaration");
    let legacy = base.replace("api_version = \"0.3\"", "api_version = \"0.2\"");
    ExtensionManifest::parse(&legacy).expect("API 0.2 validates the contribution union");
    let newest = base.replace("api_version = \"0.3\"", "api_version = \"0.4\"");
    ExtensionManifest::parse(&newest).expect("API 0.4 validates the contribution union");
    let deferred = base.replace(
        "hooks = [\"session_start\", \"session_end\"]",
        "hooks = [\"session_start\", \"session_end\", \"before_prompt\"]",
    );
    ExtensionManifest::parse(&deferred).expect("mixed hook surfaces are a valid declaration");
    let frozen = base.replace("api_version = \"0.3\"", "api_version = \"0.1\"");
    assert!(matches!(
        ExtensionManifest::parse(&frozen),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("require extension API 0.2 or later")
    ));
}

#[test]
fn manifest_cli_flags_are_typed_bounded_and_require_api_v0_2() {
    let source = r#"
name = "flag-fixture"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "flag-fixture"
[contributes]
flags = [
  { name = "enabled", type = "boolean", default = true, description = "Enable the fixture" },
  { name = "label", type = "string", default = "default" },
  { name = "count", type = "integer", default = 2 },
]
"#;
    let manifest = ExtensionManifest::parse(source).expect("typed API 0.3 flags");
    assert_eq!(manifest.contributes.flags.len(), 3);
    assert_eq!(
        manifest.contributes.flags[0].kind,
        ExtensionFlagType::Boolean
    );
    assert_eq!(manifest.contributes.flags[2].default, serde_json::json!(2));

    let legacy = source.replace("api_version = \"0.3\"", "api_version = \"0.2\"");
    ExtensionManifest::parse(&legacy).expect("API 0.2 validates the contribution union");
    let newest = source.replace("api_version = \"0.3\"", "api_version = \"0.4\"");
    ExtensionManifest::parse(&newest).expect("API 0.4 validates the contribution union");
    let frozen = source.replace("api_version = \"0.3\"", "api_version = \"0.1\"");
    assert!(matches!(
        ExtensionManifest::parse(&frozen),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("CLI flags require extension API 0.2 or later")
    ));

    let wrong_type = source.replace("default = 2", "default = \"two\"");
    assert!(matches!(
        ExtensionManifest::parse(&wrong_type),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("value must be an integer")
    ));

    let duplicate = source.replace(
        "{ name = \"count\", type = \"integer\", default = 2 }",
        "{ name = \"enabled\", type = \"integer\", default = 2 }",
    );
    assert!(matches!(
        ExtensionManifest::parse(&duplicate),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("duplicate CLI flag")
    ));

    let mut oversized = manifest.clone();
    oversized.contributes.flags = vec![
        ExtensionFlag {
            name: "flag".into(),
            kind: ExtensionFlagType::Boolean,
            default: serde_json::json!(false),
            description: None,
        };
        MAX_EXTENSION_FLAGS + 1
    ];
    assert!(matches!(
        oversized.validate(),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("limit is")
    ));
}

#[test]
fn runtime_cli_flag_values_fill_defaults_and_reject_undeclared_values() {
    let manifest = ExtensionManifest::parse(
        r#"
name = "flag-runtime"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "flag-runtime"
[contributes]
flags = [
  { name = "enabled", type = "boolean", default = false },
  { name = "label", type = "string", default = "default" },
  { name = "count", type = "integer", default = 2 },
]
"#,
    )
    .expect("manifest");
    let supplied = BTreeMap::from([
        ("enabled".to_owned(), serde_json::json!(true)),
        ("count".to_owned(), serde_json::json!(7)),
    ]);
    let values = resolve_extension_flag_values(&manifest, &supplied).expect("resolved values");
    assert_eq!(
        values,
        BTreeMap::from([
            ("count".to_owned(), serde_json::json!(7)),
            ("enabled".to_owned(), serde_json::json!(true)),
            ("label".to_owned(), serde_json::json!("default")),
        ])
    );

    let undeclared = BTreeMap::from([("surprise".to_owned(), serde_json::json!(true))]);
    assert!(matches!(
        resolve_extension_flag_values(&manifest, &undeclared),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("undeclared CLI flag")
    ));
    let invalid = BTreeMap::from([("count".to_owned(), serde_json::json!("seven"))]);
    assert!(matches!(
        resolve_extension_flag_values(&manifest, &invalid),
        Err(ExtensionRuntimeError::InvalidManifest(message)) if message.contains("value must be an integer")
    ));
}

#[test]
fn api_v02_initialize_projects_flag_values_and_api_v01_stays_unchanged() {
    let values = BTreeMap::from([
        ("count".to_owned(), serde_json::json!(7)),
        ("enabled".to_owned(), serde_json::json!(true)),
        ("label".to_owned(), serde_json::json!("host-resolved")),
    ]);

    // API 0.2 carries the declared flag values; they are ordered by name.
    let projected = projected_initialize_flag_values(EXTENSION_API_VERSION_0_2, &values)
        .expect("bounded projection")
        .expect("API 0.2 carries flag values");
    assert_eq!(
        projected,
        vec![
            api_v03::InitializeFlagValue {
                name: "count".into(),
                value: serde_json::json!(7),
            },
            api_v03::InitializeFlagValue {
                name: "enabled".into(),
                value: serde_json::json!(true),
            },
            api_v03::InitializeFlagValue {
                name: "label".into(),
                value: serde_json::json!("host-resolved"),
            },
        ]
    );

    // API 0.1 and API 0.3 never use the 0.2 projection.
    assert!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_1, &values)
            .expect("ok")
            .is_none()
    );
    assert!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_3, &values)
            .expect("ok")
            .is_none()
    );

    // Over-bound string value is refused, never truncated silently.
    let oversized = BTreeMap::from([(
        "label".to_owned(),
        serde_json::json!("x".repeat(MAX_EXTENSION_FLAG_STRING_BYTES + 1)),
    )]);
    assert!(matches!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_2, &oversized),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("exceeds")
    ));

    // An unmodeled value kind is refused rather than coerced.
    let invalid = BTreeMap::from([("count".to_owned(), serde_json::json!(["seven"]))]);
    assert!(matches!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_2, &invalid),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("cannot be projected")
    ));

    // Too many flags are refused up front.
    let many = (0..=MAX_EXTENSION_FLAGS)
        .map(|index| (format!("flag{index}"), serde_json::json!(true)))
        .collect::<BTreeMap<_, _>>();
    assert!(matches!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_2, &many),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("limit is")
    ));

    // The initialize field is additive and optional: absent when there are
    // no flags (and always for API 0.1), present when there are.
    let base = || InitializeRequest {
        api_version: EXTENSION_API_VERSION_0_2.to_owned(),
        octet_version: "0.0.0".to_owned(),
        extension: ExtensionIdentity {
            name: "flag-fixture".into(),
            version: "0.1.0".into(),
            manifest_path: PathBuf::from("/test/extension.toml"),
            source: ExtensionSource::Explicit,
        },
        workspace: PathBuf::from("/test/workspace"),
        capabilities: ExtensionCapabilities::default(),
        contributes: ManifestContributions::default(),
        host: ExtensionHostState::default(),
        flag_values: None,
        protocol: None,
    };
    let absent = serde_json::to_value(base()).expect("serialize");
    assert!(
        absent.get("flag_values").is_none(),
        "an API 0.2 host with no flags stays wire-identical: {absent}"
    );
    let present = serde_json::to_value(InitializeRequest {
        flag_values: Some(vec![api_v03::InitializeFlagValue {
            name: "enabled".into(),
            value: serde_json::json!(true),
        }]),
        ..base()
    })
    .expect("serialize");
    assert_eq!(
        present["flag_values"],
        serde_json::json!([{"name": "enabled", "value": true}])
    );
}

#[test]
fn extension_provider_protocols_reject_native_unmodeled_routes() {
    assert_eq!(
        provider_protocol_name(Protocol::OpenAiChat),
        Some("openai_chat")
    );
    assert_eq!(
        provider_protocol_name(Protocol::OpenAiResponses),
        Some("openai_responses")
    );
    assert_eq!(
        provider_protocol_name(Protocol::AnthropicMessages),
        Some("anthropic_messages")
    );
    assert_eq!(provider_protocol_name(Protocol::BedrockConverse), None);
    assert_eq!(provider_protocol_name(Protocol::GoogleGenerativeAi), None);
    assert_eq!(provider_protocol_name(Protocol::MistralConversations), None);
}

#[test]
fn manifest_accepts_matching_optional_octet_requirement_and_rejects_mismatch() {
    let matching = VALID_MANIFEST.replace(
        "api_version = \"0.1\"",
        &format!(
            "api_version = \"0.1\"\nrequires_octet = \"={}\"",
            env!("CARGO_PKG_VERSION")
        ),
    );
    let manifest = ExtensionManifest::parse(&matching).expect("matching octet requirement");
    assert_eq!(
        manifest.requires_octet.as_deref(),
        Some(concat!("=", env!("CARGO_PKG_VERSION")))
    );

    let old_name = matching.replace("requires_octet", "requires_ygg");
    assert!(ExtensionManifest::parse(&old_name).is_err());

    let mismatch = matching.replace(
        &format!("requires_octet = \"={}\"", env!("CARGO_PKG_VERSION")),
        "requires_octet = \"=99.0.0\"",
    );
    assert!(matches!(
        ExtensionManifest::parse(&mismatch),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("requires octet")
    ));
}

#[test]
fn brokered_environment_is_explicit_narrow_and_not_in_default_subprocesses() {
    let declared = VALID_MANIFEST
            .replace("api_version = \"0.1\"", "api_version = \"0.2\"")
            .replace(
                "network = false",
                "network = false\nenvironment = [\"SSH_AUTH_SOCK\", \"DISPLAY\", \"WAYLAND_DISPLAY\", \"XDG_RUNTIME_DIR\", \"DBUS_SESSION_BUS_ADDRESS\", \"XAUTHORITY\", \"USERPROFILE\", \"APPDATA\", \"LOCALAPPDATA\"]",
            );
    let manifest = ExtensionManifest::parse(&declared).expect("reviewed environment names");
    assert_eq!(
        manifest.capabilities.environment,
        [
            "SSH_AUTH_SOCK",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XDG_RUNTIME_DIR",
            "DBUS_SESSION_BUS_ADDRESS",
            "XAUTHORITY",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ]
    );
    for name in &manifest.capabilities.environment {
        assert!(!sanitized_subprocess_environment().contains_key(std::ffi::OsStr::new(name)));
    }
    let brokered = brokered_extension_environment_from(
        &manifest.capabilities.environment,
        |name| match name {
            "DISPLAY" => Some(":42".into()),
            "XAUTHORITY" => Some("/private/session/auth".into()),
            "AWS_SECRET_ACCESS_KEY" => Some("must-not-pass".into()),
            _ => None,
        },
    );
    assert_eq!(
        brokered.get(std::ffi::OsStr::new("DISPLAY")),
        Some(&":42".into())
    );
    assert_eq!(
        brokered.get(std::ffi::OsStr::new("XAUTHORITY")),
        Some(&"/private/session/auth".into())
    );
    assert!(!brokered.contains_key(std::ffi::OsStr::new("AWS_SECRET_ACCESS_KEY")));

    let unsupported = declared.replace("XAUTHORITY", "AWS_SECRET_ACCESS_KEY");
    assert!(matches!(
        ExtensionManifest::parse(&unsupported),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("unsupported brokered environment variable")
    ));
    // A Windows-native driver cannot resolve its own runtime without the
    // system root, so a manifest declaring them must be installable. This
    // pins the whole allowlist: a name the host rejects is a manifest that
    // cannot be installed at all, on any host.
    for name in BROKERED_EXTENSION_ENVIRONMENT {
        assert!(
                ExtensionManifest::parse(&declared.replace(
                    "environment = [\"SSH_AUTH_SOCK\", \"DISPLAY\", \"WAYLAND_DISPLAY\", \"XDG_RUNTIME_DIR\", \"DBUS_SESSION_BUS_ADDRESS\", \"XAUTHORITY\", \"USERPROFILE\", \"APPDATA\", \"LOCALAPPDATA\"]",
                    &format!("environment = [\"{name}\"]"),
                ))
                .is_ok(),
                "{name} is allowlisted but a manifest declaring only it is rejected"
            );
    }
    let legacy = declared.replace("api_version = \"0.2\"", "api_version = \"0.1\"");
    assert!(matches!(
        ExtensionManifest::parse(&legacy),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("require extension API 0.2")
    ));
}

#[test]
fn bounded_manifest_load_rejects_oversized_files() {
    let temp = TempDir::new().expect("tempdir");
    let path = temp.path().join(EXTENSION_MANIFEST_FILENAME);
    std::fs::write(&path, VALID_MANIFEST).expect("write manifest");
    let error = ExtensionManifest::load_bounded(&path, 10).expect_err("must be bounded");
    assert!(matches!(
        error,
        ExtensionRuntimeError::ManifestTooLarge { limit: 10, .. }
    ));
}

#[test]
fn discovery_is_sorted_and_catalog_precedence_is_caller_owned() {
    let temp = TempDir::new().expect("tempdir");
    let project = temp.path().join("project");
    let global = temp.path().join("home/.octet/extensions");
    write_manifest(&project.join(".octet/extensions/z-last"), "z-last", "z");
    write_manifest(
        &project.join(".octet/extensions/git-tools"),
        "git-tools",
        "project",
    );
    write_manifest(&global.join("git-tools"), "git-tools", "global");

    let roots = default_extension_roots(&project, Some(&temp.path().join("home")));
    let (inputs, diagnostics) = discover_extension_manifests(&roots);
    assert!(diagnostics.is_empty());
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[0].source, ExtensionSource::Project);
    assert!(inputs[0].path.to_string_lossy().contains("git-tools"));

    let mut policy = ExtensionPolicy::default();
    policy.enable("git-tools");
    policy.trust("git-tools");
    let catalog = ExtensionCatalog::load_resolved(inputs, &policy, 64 * 1024);
    assert_eq!(catalog.extensions.len(), 2);
    assert_eq!(catalog.extensions[0].manifest.name, "git-tools");
    assert_eq!(
        catalog.extensions[0].manifest.description.as_deref(),
        Some("project")
    );
    assert_eq!(
        catalog.extensions[0].activation.trust,
        ExtensionTrust::Untrusted
    );
    assert!(catalog.extensions[0].activation.enabled);
    assert_eq!(catalog.diagnostics.len(), 1);
    assert!(catalog.diagnostics[0].message.contains("shadowed"));
}

#[test]
fn full_access_extension_trust_never_enables_or_records_grants() {
    let path = Path::new("/selected/extensions/fixture/extension.toml");
    for effect_policy in [
        EffectPolicy::UnsafeHost,
        EffectPolicy::Controlled,
        EffectPolicy::ControlledBashApproval,
    ] {
        for source in [
            ExtensionSource::Global,
            ExtensionSource::Project,
            ExtensionSource::Explicit,
        ] {
            let mut policy = ExtensionPolicy::for_effect_policy(effect_policy);
            let expected_trust = if effect_policy == EffectPolicy::UnsafeHost
                || source == ExtensionSource::Explicit
            {
                ExtensionTrust::Trusted
            } else {
                ExtensionTrust::Untrusted
            };
            assert_eq!(
                policy.activation("fixture", path, source),
                ExtensionActivation {
                    enabled: false,
                    trust: expected_trust,
                }
            );
            policy.enable("fixture");
            assert_eq!(
                policy.activation("fixture", path, source),
                ExtensionActivation {
                    enabled: true,
                    trust: expected_trust,
                }
            );
            assert!(policy.trusted_global.is_empty());
            assert!(policy.trusted_sources.is_empty());
            assert!(policy.trusted_for_invocation.is_empty());
        }
    }
    assert_eq!(
        ExtensionPolicy::for_effect_policy(EffectPolicy::ControlledBashApproval),
        ExtensionPolicy::default()
    );
}

#[test]
fn executable_trust_is_bound_to_global_or_exact_selected_source() {
    let project = PathBuf::from("/workspace/.octet/extensions/git-tools/extension.toml");
    let global = PathBuf::from("/home/user/.octet/extensions/git-tools/extension.toml");
    let mut policy = ExtensionPolicy::default();
    policy.enable("git-tools");
    policy.trust("git-tools");

    assert_eq!(
        policy.activation("git-tools", &global, ExtensionSource::Global),
        ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        }
    );
    assert_eq!(
        policy.activation("git-tools", &project, ExtensionSource::Project),
        ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Untrusted,
        }
    );

    policy.trust_source("git-tools", project.clone());
    assert_eq!(
        policy
            .activation("git-tools", &project, ExtensionSource::Project)
            .trust,
        ExtensionTrust::Trusted
    );
    assert_eq!(
        policy
            .activation(
                "git-tools",
                Path::new("/other/project/extension.toml"),
                ExtensionSource::Project,
            )
            .trust,
        ExtensionTrust::Untrusted
    );

    policy.revoke_trust("git-tools");
    policy.trust_for_invocation("git-tools");
    assert_eq!(
        policy
            .activation(
                "git-tools",
                Path::new("/one-shot/extension.toml"),
                ExtensionSource::Project,
            )
            .trust,
        ExtensionTrust::Trusted
    );
}

#[test]
fn host_authority_startup_truth_table() {
    use ExtensionStartDecision::{Allowed, Disabled, NeedsHostAuthority, NeedsWorkspaceTrust};
    let path = Path::new("/selected/extension.toml");
    // enabled, source, policy, persistent grant, trusted workspace, decision
    let cases = [
        (
            false,
            ExtensionSource::Global,
            EffectPolicy::UnsafeHost,
            false,
            true,
            Disabled,
        ),
        (
            true,
            ExtensionSource::Global,
            EffectPolicy::UnsafeHost,
            false,
            true,
            Allowed,
        ),
        (
            true,
            ExtensionSource::Global,
            EffectPolicy::Controlled,
            true,
            true,
            Allowed,
        ),
        (
            true,
            ExtensionSource::Global,
            EffectPolicy::ControlledBashApproval,
            false,
            true,
            NeedsHostAuthority,
        ),
        (
            true,
            ExtensionSource::Project,
            EffectPolicy::UnsafeHost,
            false,
            false,
            NeedsWorkspaceTrust,
        ),
        (
            true,
            ExtensionSource::Project,
            EffectPolicy::UnsafeHost,
            false,
            true,
            Allowed,
        ),
        (
            true,
            ExtensionSource::Project,
            EffectPolicy::Controlled,
            true,
            true,
            Allowed,
        ),
        (
            true,
            ExtensionSource::Project,
            EffectPolicy::ControlledBashApproval,
            false,
            true,
            NeedsHostAuthority,
        ),
        (
            true,
            ExtensionSource::Explicit,
            EffectPolicy::ControlledBashApproval,
            false,
            false,
            Allowed,
        ),
    ];
    for (enabled, source, effect_policy, grant, workspace_trusted, expected) in cases {
        let mut policy = ExtensionPolicy::for_effect_policy(effect_policy);
        if enabled {
            policy.enable("fixture");
        }
        if grant {
            if source == ExtensionSource::Global {
                policy.trust("fixture");
            } else {
                policy.trust_source("fixture", path);
            }
        }
        let actual = policy
            .activation("fixture", path, source)
            .start_decision(source, workspace_trusted);
        assert_eq!(
            actual, expected,
            "{enabled:?} {source:?} {effect_policy:?} {grant:?} {workspace_trusted:?}"
        );
    }
}

#[tokio::test]
async fn launch_requires_both_enablement_and_trust() {
    let temp = TempDir::new().expect("tempdir");
    let manifest = minimal_manifest("policy-test", "does-not-exist");
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Untrusted,
        },
    };
    let error =
        match ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path())).await {
            Ok(_) => panic!("untrusted process unexpectedly started"),
            Err(error) => error,
        };
    assert!(matches!(error, ExtensionRuntimeError::Untrusted(name) if name == "policy-test"));
}

#[test]
fn handshake_must_exactly_match_manifest_contribution_names() {
    let manifest = ExtensionManifest::parse(VALID_MANIFEST).expect("valid manifest");
    let response = InitializeResponse {
        api_version: manifest.api_version.clone(),
        tools: vec![ToolDefinition {
            default_active: None,
            nested_execution: false,
            prepare_arguments: false,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
            operation: None,
            name: "surprise".into(),
            description: "Undeclared".into(),
            parameters: serde_json::json!({"type": "object"}),
            output_schema: None,
            composition: None,
            constrained_sampling: None,
        }],
        commands: vec![CommandDefinition {
            name: "checkpoint".into(),
            description: "Checkpoint".into(),
            usage: None,
        }],
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: None,
    };
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response,
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("do not match")
    ));
}

#[test]
fn api_0_4_is_the_union_version_on_the_legacy_wire() {
    // API 0.4 validates the whole contribution union in one manifest:
    // tools, commands, shortcuts, mixed hooks, UI surfaces, notifications,
    // confirmations, presentation, provider catalogs, and CLI flags.
    let source = r#"name = "union-fixture"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "union-fixture"
[contributes]
tools = ["echo"]
commands = ["checkpoint"]
shortcuts = [{ key = "ctrl+shift+p", name = "open_panel", description = "Open the panel" }]
hooks = ["session_start", "provider_retry", "post_mutation", "before_prompt"]
ui = ["status", "header", "footer"]
context = true
tool_renderers = ["echo"]
notifications = true
confirmations = true
presentation = true
providers = true
flags = [{ name = "enabled", type = "boolean", default = true }]
"#;
    let manifest = ExtensionManifest::parse(source).expect("API 0.4 accepts the union");
    assert_eq!(manifest.api_version, EXTENSION_API_VERSION_0_4);
    assert!(manifest.contributes.providers);
    assert!(manifest.contributes.presentation);
    assert_eq!(manifest.contributes.ui.len(), 3);

    // API 0.4 rides the API 0.2 feature-negotiation wire: every version above
    // the frozen API 0.1 text contract is stateful, API 0.4 is not canonical,
    // and only API 0.3 keeps the canonical wire.
    assert!(is_stateful_api(EXTENSION_API_VERSION_0_4));
    assert!(uses_api_0_2_capabilities(EXTENSION_API_VERSION_0_4));
    assert!(!is_canonical_api(EXTENSION_API_VERSION_0_4));
    assert!(!is_stateful_api(EXTENSION_API_VERSION_0_1));
    assert!(!uses_api_0_2_capabilities(EXTENSION_API_VERSION_0_1));
    assert!(is_canonical_api(EXTENSION_API_VERSION_0_3));

    // Real host flag projection reaches an API 0.4 process.
    let values = BTreeMap::from([("enabled".to_owned(), serde_json::json!(true))]);
    let projected = projected_initialize_flag_values(EXTENSION_API_VERSION_0_4, &values)
        .expect("API 0.4 flag projection is bounded")
        .expect("API 0.4 projects declared flags");
    assert_eq!(projected.len(), 1);
    assert!(
        projected_initialize_flag_values(EXTENSION_API_VERSION_0_1, &values)
            .expect("API 0.1 has no projection")
            .is_none()
    );

    // The legacy negotiator accepts an API 0.4 handshake that echoes 0.4 and
    // records the union under a 0.4 negotiated version.
    let response = InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_4.into(),
        tools: vec![ToolDefinition {
            default_active: None,
            nested_execution: false,
            prepare_arguments: false,
            prompt_snippet: None,
            prompt_guidelines: Vec::new(),
            operation: None,
            name: "echo".into(),
            description: "Echo".into(),
            parameters: serde_json::json!({"type": "object"}),
            output_schema: Some(serde_json::json!({"type": "object"})),
            composition: None,
            constrained_sampling: None,
        }],
        commands: vec![CommandDefinition {
            name: "checkpoint".into(),
            description: "Checkpoint".into(),
            usage: None,
        }],
        tool_renderers: Vec::new(),
        shortcuts: manifest.contributes.shortcuts.clone(),
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_4.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 4,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    let (contributions, protocol) = negotiate_contributions_with_host_services(
        &manifest,
        response.clone(),
        DEFAULT_PENDING_REQUESTS,
        OfferedHostServices::default(),
    )
    .expect("API 0.4 negotiates on the legacy wire");
    assert_eq!(protocol.version, EXTENSION_API_VERSION_0_4);
    assert!(contributions.presentation);

    // A handshake that claims a different version than its manifest is refused.
    let mut mismatched = response;
    mismatched.api_version = EXTENSION_API_VERSION_0_2.into();
    if let Some(protocol) = mismatched.protocol.as_mut() {
        protocol.version = EXTENSION_API_VERSION_0_2.into();
    }
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            mismatched,
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::UnsupportedApiVersion { .. })
    ));
}

#[test]
fn shortcut_contributions_must_match_initialize_and_respect_bounds() {
    let manifest_source = r#"name = "shortcut-test"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "shortcut-test"
[contributes]
shortcuts = [{ key = "ctrl+shift+p", name = "open_panel", description = "Open the panel" }]
"#;
    let manifest = ExtensionManifest::parse(manifest_source).unwrap();
    let unsupported = manifest_source.replace("api_version = \"0.2\"", "api_version = \"0.1\"");
    assert!(matches!(
        ExtensionManifest::parse(&unsupported),
        Err(ExtensionRuntimeError::InvalidManifest(message))
            if message.contains("shortcuts require extension API 0.2")
    ));
    // API 0.4 folds API 0.3, so shortcuts are an ordinary declaration on
    // every version above the frozen API 0.1 text contract.
    for version in ["0.2", "0.3", "0.4"] {
        let supported = manifest_source.replace(
            "api_version = \"0.2\"",
            &format!("api_version = \"{version}\""),
        );
        ExtensionManifest::parse(&supported)
            .unwrap_or_else(|error| panic!("API {version} must accept shortcuts: {error}"));
    }
    let response = |shortcuts: Vec<ShortcutDefinition>| InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_2.into(),
        tools: Vec::new(),
        commands: Vec::new(),
        tool_renderers: Vec::new(),
        shortcuts,
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_2.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 1,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    let declared = manifest.contributes.shortcuts.clone();
    let (contributions, _) = negotiate_contributions_with_host_services(
        &manifest,
        response(declared.clone()),
        DEFAULT_PENDING_REQUESTS,
        OfferedHostServices::default(),
    )
    .unwrap();
    assert_eq!(contributions.shortcuts, declared);

    let mismatch = vec![ShortcutDefinition {
        key: "ctrl+shift+o".into(),
        name: "open_panel".into(),
        description: "Open the panel".into(),
    }];
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(mismatch),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("shortcuts do not match")
    ));

    let oversized = (0..=MAX_EXTENSION_SHORTCUTS)
        .map(|index| ShortcutDefinition {
            key: format!("ctrl+alt+{index}"),
            name: format!("shortcut-{index}"),
            description: "bounded shortcut".into(),
        })
        .collect::<Vec<_>>();
    assert!(validate_shortcut_definitions(&oversized)
        .unwrap_err()
        .contains("limit is"));
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_process_uses_canonical_contracts_and_unknown_method_errors() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("api-v03.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
assert contract["schema"] == "octet.extension.api/0.3", contract
assert initialize["params"]["flag_values"] == [
    {"name": "api-v03-count", "value": 7},
    {"name": "api-v03-enabled", "value": True},
    {"name": "api-v03-label", "value": "default"},
], initialize
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"],
    "methods": contract["required_methods"],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.3",
        "tools": [{
            "name": "echo",
            "description": "Canonical echo",
            "parameters": {"type": "object"},
        }],
        "contract": selection,
    },
})

send({
    "jsonrpc": "2.0",
    "id": "unavailable-method",
    "method": "tools/register",
    "params": {},
})
saw_unknown_method_error = False
saw_tool_call = False
while not (saw_unknown_method_error and saw_tool_call):
    message = receive()
    if message.get("id") == "unavailable-method":
        assert message["error"]["code"] == -32601, message
        assert message["error"]["message"] == "unknown or unnegotiated method", message
        saw_unknown_method_error = True
    else:
        assert message["method"] == "tool/call", message
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "content": [{"type": "text", "text": "canonical API 0.3"}],
                "is_error": False,
                "metadata": {},
            },
        })
        saw_tool_call = True

shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {"terminal": "shutdown"}})
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "api-v03.py"
[contributes]
tools = ["echo"]
flags = [
  { name = "api-v03-enabled", type = "boolean", default = false },
  { name = "api-v03-label", type = "string", default = "default" },
  { name = "api-v03-count", type = "integer", default = 2 },
]
"#,
    )
    .expect("API 0.3 manifest");
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.flag_values = BTreeMap::from([
        ("api-v03-enabled".to_owned(), serde_json::json!(true)),
        ("api-v03-count".to_owned(), serde_json::json!(7)),
    ]);
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), runtime)
        .await
        .expect("start API 0.3 process");
    assert_eq!(process.api_version(), EXTENSION_API_VERSION_0_3);
    let protocol = process.negotiated_protocol();
    assert_eq!(protocol.version, EXTENSION_API_VERSION_0_3);
    assert!(protocol.supports(EXTENSION_FEATURE_REQUEST_CANCELLATION));
    assert!(protocol.supports(EXTENSION_FEATURE_CONTENT_PARTS));
    let connection = read_std_lock(&process.inner.connection).clone();
    match connection.require_api_v03_host_method(methods::COMMAND_EXECUTE) {
        Err(ExtensionRuntimeError::Protocol(message)) => {
            assert_eq!(
                message,
                concat!(
                    "API 0.3 contract error -32601: unknown or unnegotiated method: ",
                    "unknown method \"command/execute\""
                )
            );
        }
        other => panic!("expected unavailable API 0.3 command rejection, got {other:?}"),
    }
    let mut events = process.subscribe();
    let output = process
        .call_tool("echo", serde_json::json!({}), process.current_context())
        .await;
    if let Err(ref error) = output {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let diagnostics = std::iter::from_fn(|| events.try_recv().ok())
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>();
        panic!(
            "canonical API 0.3 tool response failed: {error}; health={:?}; events={diagnostics:?}",
            process.health_snapshot()
        );
    }
    assert_eq!(output.expect("checked above").content, "canonical API 0.3");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn extension_provider_stream_revalidates_a_route_changed_during_acceptance() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("provider-route-race.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


def provider(max_output_tokens):
    return {
        "provider": {
            "id": "fixture-provider",
            "label": "Fixture provider",
            "auth": {"kind": "none"},
        },
        "models": [{
            "id": "fixture-model",
            "api_name": "fixture-model",
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": max_output_tokens,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }],
    }


def request(identifier, method, params):
    send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
    response = receive()
    assert response.get("id") == identifier and "result" in response, response
    return response["result"]


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
provider_capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
provider_methods = {
    "providers/complete",
    "providers/register",
    "providers/update",
    "providers/unregister",
    "provider/stream",
    "provider/event",
    "provider/cancel",
    "provider/auth/request",
    "provider/auth/revoke",
}
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": [
        capability
        for capability in contract["required_capabilities"] + contract["optional_capabilities"]
        if capability in contract["required_capabilities"] or capability in provider_capabilities
    ],
    "methods": [
        method
        for method in contract["required_methods"] + contract["optional_methods"]
        if method in contract["required_methods"] or method in provider_methods
    ],
    "limits": contract["limits"],
}
assert provider_capabilities.issubset(selection["capabilities"]), selection
assert provider_methods.issubset(selection["methods"]), selection
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})
request("register", "providers/register", provider(1024))
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})

while True:
    message = receive()
    if message["method"] == "provider/stream":
        params = message["params"]
        # The update is accepted before this request's stale acceptance. This
        # is the exact post-acceptance race the transport must fence.
        request("replace-route", "providers/update", provider(2048))
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"stream_id": params["stream_id"], "accepted": True},
        })
    elif message["method"] == "provider/cancel":
        assert message["params"]["reason"] == "provider route changed before admission", message
    elif message["method"] == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
    else:
        raise AssertionError(message)
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "provider-route-race"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "provider-route-race.py"
[contributes]
providers = true
"#,
    )
    .expect("API 0.3 provider manifest");
    let registry = Arc::new(ExtensionProviderRegistry::new());
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.provider_registry = Some(Arc::clone(&registry));
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), runtime)
        .await
        .expect("start API 0.3 provider process");
    let route = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(route) = registry.resolve("fixture-provider", "fixture-model") {
                break route;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("initial provider route");
    let transport = process.provider_stream_transport("fixture-provider", "fixture-model");
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        transport.stream(
            HostStreamModel {
                id: octet_ai::ModelId("fixture-provider/fixture-model".into()),
                protocol: Protocol::OpenAiChat,
                pricing: None,
            },
            octet_ai::Request {
                system: None,
                messages: Vec::new(),
                tools: Vec::new(),
                tool_choice: Default::default(),
                max_output_tokens: None,
                temperature: None,
                stop: Vec::new(),
                reasoning: Default::default(),
                reasoning_mode: Default::default(),
                responses: None,
                output_format: Default::default(),
                output_modalities: Default::default(),
                compatibility: Default::default(),
                cache_retention: Default::default(),
                session_id: None,
            },
            Vec::new(),
        ),
    )
    .await
    .expect("provider acceptance response");
    assert!(matches!(
        result,
        Err(AiError::Provider(ProviderError {
            kind: Some(kind),
            message,
            ..
        })) if kind == "extension_provider" && message == "extension provider is unavailable"
    ));
    assert!(
        !registry.route_is_active(&route),
        "the original route must be stale after the extension update"
    );
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn late_provider_registration_does_not_disturb_an_in_flight_request() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("late-provider-stream.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


def provider(provider_id, model_id):
    return {
        "provider": {
            "id": provider_id,
            "label": provider_id + " provider",
            "auth": {"kind": "none"},
        },
        "models": [{
            "id": model_id,
            "api_name": model_id,
            "protocol": "openai_chat",
            "context_window": 8192,
            "max_output_tokens": 1024,
            "capabilities": {
                "tools": False,
                "parallel_tool_calls": False,
                "structured_output": False,
                "reasoning": False,
            },
        }],
    }


def reverse_request(identifier, method, params):
    send({"jsonrpc": "2.0", "id": identifier, "method": method, "params": params})
    response = receive()
    assert response.get("id") == identifier and "result" in response, response
    return response["result"]


def stream(stream_id, text):
    events = [
        ("started", {"response_id": stream_id + "-response"}),
        ("text_start", {"index": 0}),
        ("text_delta", {"index": 0, "delta": text}),
        ("text_end", {"index": 0}),
        ("finished", {"stop_reason": "stop"}),
    ]
    for sequence, (kind, payload) in enumerate(events):
        send({
            "jsonrpc": "2.0",
            "method": "provider/event",
            "params": {
                "stream_id": stream_id,
                "sequence": sequence,
                "kind": kind,
                "payload": payload,
            },
        })


initialize = receive()
assert initialize["method"] == "initialize", initialize
contract = initialize["params"]["contract"]
provider_capabilities = {"provider_catalog", "provider_stream", "provider_auth"}
provider_methods = {
    "providers/complete",
    "providers/register",
    "providers/update",
    "providers/unregister",
    "provider/stream",
    "provider/event",
    "provider/cancel",
    "provider/auth/request",
    "provider/auth/revoke",
}
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": [
        capability
        for capability in contract["required_capabilities"] + contract["optional_capabilities"]
        if capability in contract["required_capabilities"] or capability in provider_capabilities
    ],
    "methods": [
        method
        for method in contract["required_methods"] + contract["optional_methods"]
        if method in contract["required_methods"] or method in provider_methods
    ],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})
reverse_request("initial-register", "providers/register", provider("alpha", "alpha-model"))
send({"jsonrpc": "2.0", "method": "providers/complete", "params": {}})

while True:
    message = receive()
    method = message.get("method")
    if method == "provider/stream":
        params = message["params"]
        if params["provider_id"] == "alpha":
            # Publish a second provider while alpha's request is already in
            # flight. The accepted request must keep streaming.
            reverse_request("late-register", "providers/register", provider("beta", "beta-model"))
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"stream_id": params["stream_id"], "accepted": True},
        })
        stream(params["stream_id"], params["provider_id"] + " is live")
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
    else:
        raise AssertionError(message)
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "late-provider-stream"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "late-provider-stream.py"
[contributes]
providers = true
"#,
    )
    .expect("API 0.3 provider manifest");
    let registry = Arc::new(ExtensionProviderRegistry::new());
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.provider_registry = Some(Arc::clone(&registry));
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), runtime)
        .await
        .expect("start late-provider fixture");
    let route = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if let Some(route) = registry.resolve("alpha", "alpha-model") {
                break route;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("initial provider route");

    let transport = process.provider_stream_transport("alpha", "alpha-model");
    let mut response = transport
        .stream(
            HostStreamModel {
                id: octet_ai::ModelId("alpha/alpha-model".into()),
                protocol: Protocol::OpenAiChat,
                pricing: None,
            },
            octet_ai::Request {
                system: None,
                messages: Vec::new(),
                tools: Vec::new(),
                tool_choice: Default::default(),
                max_output_tokens: None,
                temperature: None,
                stop: Vec::new(),
                reasoning: Default::default(),
                reasoning_mode: Default::default(),
                responses: None,
                output_format: Default::default(),
                output_modalities: Default::default(),
                compatibility: Default::default(),
                cache_retention: Default::default(),
                session_id: None,
            },
            Vec::new(),
        )
        .await
        .expect("the in-flight request is accepted while beta is registered");

    let mut text = String::new();
    let finished = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match futures_util::StreamExt::next(&mut response).await {
                Some(Ok(StreamEvent::TextDelta { delta, .. })) => text.push_str(&delta),
                Some(Ok(StreamEvent::Finished(response))) => break Some(response),
                Some(Ok(_)) => {}
                Some(Err(error)) => panic!("canonical stream event failed: {error}"),
                None => break None,
            }
        }
    })
    .await
    .expect("the in-flight stream settled")
    .expect("the in-flight stream finished");
    assert_eq!(text, "alpha is live");
    assert_eq!(finished.stop_reason, StopReason::EndTurn);
    assert!(
        registry.route_is_active(&route),
        "an unrelated late registration must not invalidate the in-flight route"
    );
    assert!(
        registry.resolve("beta", "beta-model").is_some(),
        "the late declaration is available to the same session"
    );

    // The late provider is immediately usable: the next request routes to it.
    let beta_transport = process.provider_stream_transport("beta", "beta-model");
    let mut beta_response = beta_transport
        .stream(
            HostStreamModel {
                id: octet_ai::ModelId("beta/beta-model".into()),
                protocol: Protocol::OpenAiChat,
                pricing: None,
            },
            octet_ai::Request {
                system: None,
                messages: Vec::new(),
                tools: Vec::new(),
                tool_choice: Default::default(),
                max_output_tokens: None,
                temperature: None,
                stop: Vec::new(),
                reasoning: Default::default(),
                reasoning_mode: Default::default(),
                responses: None,
                output_format: Default::default(),
                output_modalities: Default::default(),
                compatibility: Default::default(),
                cache_retention: Default::default(),
                session_id: None,
            },
            Vec::new(),
        )
        .await
        .expect("the late provider accepts a request");
    let mut beta_text = String::new();
    tokio::time::timeout(Duration::from_secs(2), async {
        while let Some(event) = futures_util::StreamExt::next(&mut beta_response).await {
            match event.expect("canonical stream event") {
                StreamEvent::TextDelta { delta, .. } => beta_text.push_str(&delta),
                StreamEvent::Finished(_) => break,
                _ => {}
            }
        }
    })
    .await
    .expect("the late provider stream settled");
    assert_eq!(beta_text, "beta is live");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_declared_session_hooks_are_typed_sanitized_and_exact_once() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("session-hooks.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
contract = initialize["params"]["contract"]
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"] + ["lifecycle_events"],
    "methods": contract["required_methods"] + ["hook/run"],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})

seen = []
while True:
    message = receive()
    if message["method"] == "hook/run":
        params = message["params"]
        assert set(params) == {"hook", "payload"}, params
        payload = params["payload"]
        assert set(payload) == ({"binding"} if params["hook"] == "session_start" else {"binding", "outcome", "reason", "duration_ms"}), payload
        binding = payload["binding"]
        assert set(binding) == {"session_id", "extension_instance_id", "process_generation"}, binding
        assert binding["session_id"] == "session-owner-1", binding
        assert binding["process_generation"] == 1, binding
        assert 0 < len(binding["extension_instance_id"]) <= 256, binding
        assert all(character.isalnum() or character in "-_" for character in binding["extension_instance_id"]), binding
        if params["hook"] == "session_end":
            assert payload["outcome"] == "completed", payload
            assert payload["reason"] == "shutdown", payload
            assert isinstance(payload["duration_ms"], int), payload
        seen.append(params["hook"])
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"disposition": {"kind": "continue"}},
        })
    else:
        assert message["method"] == "shutdown", message
        assert seen == ["session_start", "session_end"], seen
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03-session-hooks"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "session-hooks.py"
[contributes]
hooks = ["session_start", "session_end"]
"#,
    )
    .expect("API 0.3 session hook manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start API 0.3 session hook process");
    assert!(process.declares_session_hooks());
    assert!(matches!(
        process
            .run_hook(
                ExtensionHook::SessionStart,
                serde_json::json!({}),
                process.current_context(),
            )
            .await,
        Err(ExtensionRuntimeError::Protocol(message))
            if message.contains("host-owned API 0.3/0.4 lifecycle hooks")
    ));
    assert!(matches!(
        process.start_session_hook_binding("session/with/a/path").await,
        Err(ExtensionRuntimeError::Protocol(message))
            if message.contains("bounded opaque ASCII tokens")
    ));
    process
        .start_session_hook_binding("session-owner-1")
        .await
        .expect("start declared hook");
    process
        .start_session_hook_binding("session-owner-1")
        .await
        .expect("idempotent start");
    process
        .settle_session_hook_binding("session-owner-1", ExtensionLifecycleOutcome::Completed)
        .await
        .expect("settle declared hook");
    process
        .settle_session_hook_binding("session-owner-1", ExtensionLifecycleOutcome::Completed)
        .await
        .expect("idempotent settlement");
    assert!(process.shutdown().await);
}

/// Pi awaits session_start; a Pi fleet's startup work takes longer than the
/// 250 ms end-of-session cap. Only session_end keeps that short deadline.
#[cfg(unix)]
#[tokio::test]
async fn slow_session_start_completes_within_the_request_deadline() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("slow-start.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys
import time


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    return json.loads(line)


def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":"), sort_keys=True) + "\n")
    sys.stdout.flush()


initialize = receive()
contract = initialize["params"]["contract"]
send({"jsonrpc": "2.0", "id": initialize["id"], "result": {"api_version": "0.3", "tools": [], "contract": {
    "schema": contract["schema"], "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"] + ["lifecycle_events"],
    "methods": contract["required_methods"] + ["hook/run"], "limits": contract["limits"]}}})
while True:
    message = receive()
    if message.get("method") == "hook/run":
        if message["params"]["hook"] == "session_start":
            time.sleep(0.6)
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"disposition": {"kind": "continue"}}})
    elif message.get("method") == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "slow-session-start"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "slow-start.py"
[contributes]
hooks = ["session_start", "session_end"]
"#,
    )
    .expect("slow session_start manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start slow session_start process");
    process
        .start_session_hook_binding("session-owner-1")
        .await
        .expect("a 600 ms session_start is not a timeout");
    process
        .settle_session_hook_binding("session-owner-1", ExtensionLifecycleOutcome::Completed)
        .await
        .expect("settle");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_session_hooks_settle_and_rebind_per_reload_generation() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("session-hook-reload.py");
    let wire_log = temp.path().join("session-hook-reload.jsonl");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import os
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
contract = initialize["params"]["contract"]
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"] + ["lifecycle_events"],
    "methods": contract["required_methods"] + ["hook/run"],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})

while True:
    message = receive()
    if message["method"] == "hook/run":
        with open(os.path.join(os.environ["OCTET_WORKSPACE"], "session-hook-reload.jsonl"), "a", encoding="utf-8") as wire_log:
            wire_log.write(canonical(message) + "\n")
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {"disposition": {"kind": "continue"}},
        })
    else:
        assert message["method"] == "shutdown", message
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03-session-hook-reload"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "session-hook-reload.py"
[contributes]
hooks = ["session_start", "session_end"]
"#,
    )
    .expect("API 0.3 reload manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start API 0.3 session hook process");
    process
        .start_session_hook_binding("session-owner-reload")
        .await
        .expect("initial session start");
    let report = process.reload().await.expect("reload session hook process");
    assert_eq!(report.generation, 2);
    process
        .settle_session_hook_binding("session-owner-reload", ExtensionLifecycleOutcome::Completed)
        .await
        .expect("final session settlement");
    assert!(process.shutdown().await);

    let hooks = std::fs::read_to_string(wire_log)
        .expect("read hook log")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("hook frame"))
        .collect::<Vec<_>>();
    assert_eq!(hooks.len(), 4);
    assert_eq!(hooks[0]["params"]["hook"], "session_start");
    assert_eq!(
        hooks[0]["params"]["payload"]["binding"]["process_generation"],
        1
    );
    assert_eq!(hooks[1]["params"]["hook"], "session_end");
    assert_eq!(
        hooks[1]["params"]["payload"]["binding"]["process_generation"],
        1
    );
    assert_eq!(hooks[1]["params"]["payload"]["outcome"], "interrupted");
    assert_eq!(hooks[1]["params"]["payload"]["reason"], "reload");
    assert_eq!(hooks[2]["params"]["hook"], "session_start");
    assert_eq!(
        hooks[2]["params"]["payload"]["binding"]["process_generation"],
        2
    );
    assert_eq!(hooks[3]["params"]["hook"], "session_end");
    assert_eq!(
        hooks[3]["params"]["payload"]["binding"]["process_generation"],
        2
    );
    assert_eq!(hooks[3]["params"]["payload"]["outcome"], "completed");
    assert_eq!(hooks[3]["params"]["payload"]["reason"], "shutdown");
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_session_hooks_recover_a_crashed_generation_on_replacement() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("session-hook-crash.py");
    let wire_log = temp.path().join("session-hook-crash.jsonl");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import os
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip("\n") == canonical(value), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
contract = initialize["params"]["contract"]
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"] + ["lifecycle_events"],
    "methods": contract["required_methods"] + ["hook/run"],
    "limits": contract["limits"],
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})

while True:
    message = receive()
    if message["method"] == "hook/run":
        with open(os.path.join(os.environ["OCTET_WORKSPACE"], "session-hook-crash.jsonl"), "a", encoding="utf-8") as wire_log:
            wire_log.write(canonical(message) + "\n")
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"disposition": {"kind": "continue"}}})
    else:
        assert message["method"] == "shutdown", message
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03-session-hook-crash"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "session-hook-crash.py"
[contributes]
hooks = ["session_start", "session_end"]
"#,
    )
    .expect("API 0.3 crash manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start API 0.3 session hook process");
    process
        .start_session_hook_binding("session-owner-crash")
        .await
        .expect("initial session start");
    let crashed = read_std_lock(&process.inner.connection).clone();
    crashed.terminate().await;
    let report = process.reload().await.expect("recover crashed process");
    assert_eq!(report.generation, 2);
    process
        .settle_session_hook_binding("session-owner-crash", ExtensionLifecycleOutcome::Cancelled)
        .await
        .expect("final session settlement");
    assert!(process.shutdown().await);

    let hooks = std::fs::read_to_string(wire_log)
        .expect("read hook log")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("hook frame"))
        .collect::<Vec<_>>();
    assert_eq!(hooks.len(), 4);
    assert_eq!(hooks[0]["params"]["hook"], "session_start");
    assert_eq!(
        hooks[0]["params"]["payload"]["binding"]["process_generation"],
        1
    );
    assert_eq!(hooks[1]["params"]["hook"], "session_end");
    assert_eq!(
        hooks[1]["params"]["payload"]["binding"]["process_generation"],
        1
    );
    assert_eq!(hooks[1]["params"]["payload"]["outcome"], "interrupted");
    assert_eq!(hooks[1]["params"]["payload"]["reason"], "crash");
    assert_eq!(hooks[2]["params"]["hook"], "session_start");
    assert_eq!(
        hooks[2]["params"]["payload"]["binding"]["process_generation"],
        2
    );
    assert_eq!(hooks[3]["params"]["hook"], "session_end");
    assert_eq!(
        hooks[3]["params"]["payload"]["binding"]["process_generation"],
        2
    );
    assert_eq!(hooks[3]["params"]["payload"]["outcome"], "cancelled");
    assert_eq!(hooks[3]["params"]["payload"]["reason"], "cancelled");
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_runtime_frame_limit_accepts_exact_payload_and_rejects_oversized_queue() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("frame-limit.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"), sort_keys=True)


def receive():
    line = sys.stdin.buffer.readline()
    assert line, "host closed stdin"
    value = json.loads(line)
    assert line.rstrip(b"\n") == canonical(value).encode(), line
    return value


def send(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


initialize = receive()
contract = initialize["params"]["contract"]
limits = dict(contract["limits"])
limits["max_frame_bytes"] = 512
selection = {
    "schema": contract["schema"],
    "encoding": contract["encoding"],
    "capabilities": contract["required_capabilities"],
    "methods": contract["required_methods"],
    "limits": limits,
}
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {"api_version": "0.3", "tools": [], "contract": selection},
})
message = receive()
assert len(canonical(message).encode()) == 512, len(canonical(message).encode())
send({"jsonrpc": "2.0", "id": message["id"], "result": {"terminal": "shutdown"}})
sys.stdin.buffer.readline()
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03-frame-limit"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "frame-limit.py"
"#,
    )
    .expect("API 0.3 manifest");
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.max_message_bytes = 2048;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .expect("start API 0.3 process");
    let connection = read_std_lock(&process.inner.connection).clone();
    assert_eq!(connection.max_frame_bytes(), 512);
    assert_eq!(connection.max_message_bytes(), 513);

    let id = connection.next_id.load(Ordering::Acquire);
    let empty_params = serde_json::json!({"padding": ""});
    let empty_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "shutdown",
        "params": empty_params,
    });
    let empty_line = connection
        .serialize_message(&empty_message)
        .expect("empty frame fits");
    let padding = 512 - (empty_line.len() - 1);
    let params = serde_json::json!({"padding": "x".repeat(padding)});
    let exact_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "shutdown",
        "params": params,
    });
    assert_eq!(
        connection
            .serialize_message(&exact_message)
            .expect("exact frame fits")
            .len(),
        513
    );
    assert_eq!(
        connection
            .request(
                "shutdown",
                exact_message["params"].clone(),
                Duration::from_secs(5)
            )
            .await
            .expect("exactly bounded request")["terminal"],
        "shutdown"
    );

    // A pre-serialized line has to be checked again by the writer after
    // negotiation; it cannot bypass the selected payload limit in its queue.
    let (completion_tx, completion_rx) = oneshot::channel();
    let mut oversized = vec![b' '; 513];
    oversized.push(b'\n');
    connection
        .writer
        .send(WriterFrame {
            line: oversized,
            state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
            completion: Some(completion_tx),
            bus_delivery: None,
        })
        .await
        .expect("queue oversized buffered frame");
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), completion_rx)
            .await
            .expect("writer completion")
            .expect("writer retained completion"),
        Err(PendingError::Protocol(message))
            if message == "outbound message exceeded negotiated 513 byte limit"
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_process_rejects_crlf_at_stdout_boundary() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("crlf.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '{"id":1,"jsonrpc":"2.0","result":{}}\r\n'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "crlf-api-v03"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "crlf.sh"
"#,
    )
    .expect("API 0.3 manifest");
    match ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    {
        Err(ExtensionRuntimeError::Closed(message)) => {
            assert_eq!(message, "API 0.3 frames must end with exactly one LF");
        }
        Err(error) => panic!("expected CRLF rejection, got {error}"),
        Ok(_) => panic!("CRLF API 0.3 frame initialized successfully"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_process_rejects_empty_lf_frame_at_stdout_boundary() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("empty-frame.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '\n'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "empty-frame-api-v03"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "empty-frame.sh"
"#,
    )
    .expect("API 0.3 manifest");
    match ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    {
        Err(ExtensionRuntimeError::Closed(message)) => assert_eq!(
            message,
            "API 0.3 frames must contain one canonical JSON value per LF delimiter"
        ),
        Err(error) => panic!("expected empty-frame rejection, got {error}"),
        Ok(_) => panic!("empty API 0.3 frame initialized successfully"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn api_v03_process_rejects_noncanonical_wire_frames() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("noncanonical.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf ' {"jsonrpc":"2.0","id":1,"result":{}}\n'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "noncanonical-api-v03"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "noncanonical.sh"
"#,
    )
    .expect("API 0.3 manifest");
    match ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    {
        Err(ExtensionRuntimeError::Closed(message)) => {
            assert_eq!(message, "API 0.3 frame is not canonical JSON");
        }
        Err(error) => panic!("expected canonical-wire rejection, got {error}"),
        Ok(_) => panic!("noncanonical API 0.3 frame initialized successfully"),
    }
}

#[test]
fn api_v03_frame_limit_excludes_the_delimiter_and_rechecks_buffered_frames() {
    let limit = Arc::new(ProtocolFrameLimit::new(257, true));
    assert_eq!(limit.max_frame_bytes(), 256);
    assert_eq!(limit.max_message_bytes(), 257);
    assert!(
        limit.accepts_message_bytes(257),
        "a 256-byte payload plus LF fits"
    );
    assert!(!limit.accepts_message_bytes(258));

    // A frame serialized before selection must be rechecked by the shared
    // writer authority after the selected bound is atomically installed.
    let buffered_line_bytes = 257;
    limit.install_selected_api_v03(128);
    assert_eq!(limit.max_message_bytes(), 129);
    assert!(!limit.accepts_message_bytes(buffered_line_bytes));
    assert!(limit.accepts_message_bytes(129));

    let reader = Arc::clone(&limit);
    std::thread::scope(|scope| {
        scope.spawn(move || {
            for _ in 0..1_000 {
                assert!(matches!(reader.max_message_bytes(), 129 | 65));
            }
        });
        limit.install_selected_api_v03(64);
    });
    assert_eq!(limit.max_frame_bytes(), 64);
    assert_eq!(limit.max_message_bytes(), 65);
}

#[test]
fn api_v03_raw_boundary_rejects_duplicate_keys_and_crlf() {
    let (mut state, _frames) =
        protocol_read_state_for_test(ManifestContributions::default(), broadcast::channel(8).0);
    state.frame_limit = Arc::new(ProtocolFrameLimit::new(1025, true));
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_3.to_owned(),
        features: BTreeSet::new(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };

    assert!(matches!(
        handle_protocol_line(br#"{"jsonrpc":"2.0","id":1,"id":1,"result":{}}"#, &state),
        Err(message) if message.contains("duplicate JSON object key \"id\"")
    ));
    assert!(matches!(
        handle_protocol_line(br#"{"id":1,"jsonrpc":"2.0","result":{"x":1,"x":2}}"#, &state),
        Err(message) if message.contains("duplicate JSON object key \"x\"")
    ));
    assert_eq!(
        handle_protocol_line(b"{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{}}\r", &state),
        Err("API 0.3 frame is not canonical JSON".into())
    );
}

#[test]
fn api_v03_queue_writer_value_canonicalizes_and_validates_child_frames() {
    let (mut state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), broadcast::channel(8).0);
    state.frame_limit = Arc::new(ProtocolFrameLimit::new(1025, true));
    *write_std_lock(&state.protocol) = ExtensionNegotiatedProtocol {
        version: EXTENSION_API_VERSION_0_3.to_owned(),
        features: BTreeSet::new(),
        max_concurrent_requests: 1,
        lifecycle_events: BTreeSet::new(),
    };

    queue_writer_value(
        &state.writer,
        &state.frame_limit,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "child",
            "error": {
                "code": -32601,
                "message": "unknown or unnegotiated method",
            },
        }),
    )
    .expect("canonical API 0.3 child response");
    assert_eq!(
            frames.try_recv().expect("queued response").line,
            br#"{"error":{"code":-32601,"message":"unknown or unnegotiated method"},"id":"child","jsonrpc":"2.0"}
"#
        );
    assert!(queue_writer_value(
        &state.writer,
        &state.frame_limit,
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "child",
            "error": {"code": -32601, "message": "method not found"},
        }),
    )
    .is_err());
}

#[tokio::test]
async fn api_v03_message_limit_reserves_the_newline_delimiter() {
    let temp = TempDir::new().expect("tempdir");
    let manifest = ExtensionManifest::parse(
        r#"name = "api-v03-small-frame"
version = "0.3.0"
api_version = "0.3"
[entrypoint]
command = "must-not-spawn"
"#,
    )
    .expect("API 0.3 manifest");
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.max_message_bytes = 1;
    match ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config).await {
        Err(ExtensionRuntimeError::Protocol(message)) => assert_eq!(
            message,
            "API 0.3 message limit must leave room for a non-empty frame and newline"
        ),
        Err(error) => panic!("expected API 0.3 frame-limit rejection, got {error}"),
        Ok(_) => panic!("API 0.3 process initialized without a frame payload limit"),
    }
}

#[test]
fn runtime_command_catalog_is_duplicate_free_and_bounded() {
    let manifest = ExtensionManifest::parse(
        r#"name = "runtime-command-validation"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "runtime-command-validation"
"#,
    )
    .unwrap();
    let response = |commands: Vec<CommandDefinition>| InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_2.into(),
        tools: Vec::new(),
        commands,
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_2.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .copied()
                .chain([EXTENSION_FEATURE_RUNTIME_COMMANDS])
                .map(str::to_owned)
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 1,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    let command = |name: String| CommandDefinition {
        name,
        description: "Runtime command".into(),
        usage: None,
    };
    let duplicate = vec![command("same".into()), command("same".into())];
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(duplicate),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("duplicate command")
    ));

    let oversized = (0..=MAX_EXTENSION_COMMANDS)
        .map(|index| command(format!("command-{index}")))
        .collect();
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(oversized),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("limit is 256")
    ));
}

#[test]
fn agent_sessions_must_be_explicitly_offered_by_the_host() {
    let manifest = ExtensionManifest::parse(
        r#"name = "agent-service"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "agent-service"
"#,
    )
    .unwrap();
    let response = || InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_2.into(),
        tools: Vec::new(),
        commands: Vec::new(),
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_2.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .copied()
                .chain([EXTENSION_FEATURE_AGENT_SESSIONS])
                .map(str::to_owned)
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 1,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("agent_sessions")
    ));
    let mut routing_response = response();
    routing_response
        .protocol
        .as_mut()
        .unwrap()
        .features
        .push(EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1.into());
    let (_, protocol) = negotiate_contributions_with_host_services(
        &manifest,
        routing_response.clone(),
        DEFAULT_PENDING_REQUESTS,
        OfferedHostServices {
            agent_sessions: true,
            ..OfferedHostServices::default()
        },
    )
    .unwrap();
    assert!(protocol.supports(EXTENSION_FEATURE_AGENT_SESSIONS));
    assert!(protocol.supports(EXTENSION_FEATURE_AGENT_MODEL_SELECTION_V1));
    routing_response
        .protocol
        .as_mut()
        .unwrap()
        .features
        .retain(|f| f != EXTENSION_FEATURE_AGENT_SESSIONS);
    assert!(matches!(negotiate_contributions_with_host_services(
            &manifest, routing_response, DEFAULT_PENDING_REQUESTS,
            OfferedHostServices { agent_sessions: true, ..OfferedHostServices::default() },
        ), Err(ExtensionRuntimeError::Protocol(message)) if message.contains("requires agent_sessions")));
}

#[test]
fn first_party_subagents_requires_native_telemetry_contract() {
    let manifest = ExtensionManifest::parse(
        r#"name = "octet-subagents"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "octet-subagents"
"#,
    )
    .unwrap();
    let response = InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_2.into(),
        tools: Vec::new(),
        commands: Vec::new(),
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_2.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .copied()
                .chain([EXTENSION_FEATURE_AGENT_SESSIONS])
                .map(str::to_owned)
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 1,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    let error = negotiate_contributions_with_host_services(
        &manifest,
        response,
        DEFAULT_PENDING_REQUESTS,
        OfferedHostServices {
            agent_sessions: true,
            ..OfferedHostServices::default()
        },
    )
    .unwrap_err();
    assert!(matches!(error, ExtensionRuntimeError::Protocol(message)
            if message.contains("delegation_telemetry_v1")
                && message.contains("reinstall")));
}

#[test]
fn agent_spawn_requires_host_policy_and_defaults_tokens_to_parent_inheritance() {
    let missing = serde_json::json!({
        "parent_request_id": 7,
        "task_name": "inspect",
        "message": "inspect safely",
        "idempotency_key": "inspect-1",
    });
    assert!(serde_json::from_value::<AgentSessionSpawnRequest>(missing).is_err());

    let valid = serde_json::json!({
        "parent_request_id": 7,
        "task_name": "inspect",
        "message": "inspect safely",
        "idempotency_key": "inspect-1",
        "policy": {
            "tools": ["read", "search"],
            "max_depth": 1,
            "max_concurrent_children": 2,
            "max_turns": 8,
            "max_cost_microdollars": 200000,
            "max_output_bytes": 8192,
            "timeout_ms": 300000
        }
    });
    let request: AgentSessionSpawnRequest = serde_json::from_value(valid).unwrap();
    let policy: ExtensionAgentSessionPolicy = request.policy.into();
    assert!(policy.validate().is_ok());
    assert_eq!(policy.max_tokens, None);
    let mut invalid_tokens = policy.clone();
    invalid_tokens.max_tokens = Some(999);
    assert!(invalid_tokens
        .validate()
        .unwrap_err()
        .contains("max_tokens"));

    let mut invalid = policy.clone();
    invalid.tools.push("browser".into());
    assert!(invalid
        .validate()
        .unwrap_err()
        .contains("duplicate-free subset of read, search, edit, write, and bash"));

    let mut elevated = policy.clone();
    elevated.tools = vec!["read".into(), "bash".into()];
    assert!(
        elevated.validate().is_ok(),
        "standard mutating tools are admissible child tools"
    );
}

#[test]
fn approvals_and_secrets_must_be_explicitly_offered() {
    let manifest = ExtensionManifest::parse(
        r#"name = "host-services"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "host-services"
[capabilities]
secrets = ["browser.api_token"]
"#,
    )
    .unwrap();
    let response = |features: &[&str]| InitializeResponse {
        api_version: EXTENSION_API_VERSION_0_2.into(),
        tools: Vec::new(),
        commands: Vec::new(),
        tool_renderers: Vec::new(),
        shortcuts: Vec::new(),
        protocol: Some(ExtensionProtocolResponse {
            session_snapshot_transport_v1: None,
            version: EXTENSION_API_VERSION_0_2.into(),
            features: API_0_2_REQUIRED_FEATURES
                .iter()
                .copied()
                .chain(features.iter().copied())
                .map(str::to_owned)
                .collect(),
            limits: ExtensionProtocolLimits {
                max_message_bytes: None,
                resource_refs_v1: None,
                max_concurrent_requests: 1,
            },
            lifecycle_events: Vec::new(),
        }),
    };
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(&[EXTENSION_FEATURE_APPROVALS, EXTENSION_FEATURE_POLICY_INTENTS]),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("approvals")
    ));
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(&[EXTENSION_FEATURE_SECRETS]),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices::default(),
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("secrets")
    ));
    assert!(matches!(
        negotiate_contributions_with_host_services(
            &manifest,
            response(&[EXTENSION_FEATURE_APPROVALS]),
            DEFAULT_PENDING_REQUESTS,
            OfferedHostServices {
                approvals: true,
                ..OfferedHostServices::default()
            },
        ),
        Err(ExtensionRuntimeError::Protocol(message)) if message.contains("requires policy_intents")
    ));

    let (_, protocol) = negotiate_contributions_with_host_services(
        &manifest,
        response(&[
            EXTENSION_FEATURE_POLICY_INTENTS,
            EXTENSION_FEATURE_APPROVALS,
            EXTENSION_FEATURE_SECRETS,
        ]),
        DEFAULT_PENDING_REQUESTS,
        OfferedHostServices {
            approvals: true,
            secrets: true,
            ..OfferedHostServices::default()
        },
    )
    .unwrap();
    assert!(protocol.supports(EXTENSION_FEATURE_APPROVALS));
    assert!(protocol.supports(EXTENSION_FEATURE_SECRETS));
}

#[cfg(unix)]
async fn lifecycle_v02_fixture(temp: &TempDir) -> (ExtensionProcess, PathBuf) {
    let script_path = temp.path().join("lifecycle-v02.py");
    let log_path = temp.path().join("lifecycle-v02.jsonl");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import os
import sys

log_path = os.path.join(os.environ["OCTET_WORKSPACE"], "lifecycle-v02.jsonl")
lifecycle = {
    "session/started", "session/settled", "turn/started", "turn/settled",
    "tool/started", "tool/settled",
}

def send(value):
    sys.stdout.write(json.dumps(value, separators=(",", ":")) + "\n")
    sys.stdout.flush()

def record(message):
    value = {
        "pid": os.getpid(),
        "method": message["method"],
        "params": message.get("params"),
    }
    line = (json.dumps(value, separators=(",", ":")) + "\n").encode()
    descriptor = os.open(log_path, os.O_APPEND | os.O_CREAT | os.O_WRONLY, 0o600)
    try:
        os.write(descriptor, line)
    finally:
        os.close(descriptor)

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": {
                "api_version": "0.2",
                "tools": [],
                "commands": [],
                "protocol": {
                    "version": "0.2",
                    "features": ["request_cancellation", "content_parts", "lifecycle_events"],
                    "limits": {"max_concurrent_requests": 1},
                    "lifecycle_events": sorted(lifecycle),
                },
            },
        })
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
        break
    elif method in lifecycle:
        record(message)
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"
name = "lifecycle-v02"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "lifecycle-v02.py"
"#,
    )
    .expect("manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start lifecycle fixture");
    (process, log_path)
}

#[cfg(unix)]
fn lifecycle_records(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).expect("lifecycle JSON"))
        .collect()
}

#[cfg(unix)]
fn lifecycle_methods_for_pid(records: &[serde_json::Value], pid: u32) -> Vec<&str> {
    records
        .iter()
        .filter(|record| record["pid"].as_u64() == Some(u64::from(pid)))
        .map(|record| record["method"].as_str().expect("lifecycle method"))
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn reload_preserves_each_generation_lifecycle_order() {
    let temp = TempDir::new().expect("tempdir");
    let (process, log_path) = lifecycle_v02_fixture(&temp).await;
    let old_connection = read_std_lock(&process.inner.connection).clone();
    let old_pid = old_connection.child.lock().await.id().expect("old pid");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionStarted {
            session_id: "session".into(),
            run_id: Some("run".into()),
        })
        .await
        .expect("session start");
    process.set_active_lifecycle_turn("owner", "session", "run", "turn");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnStarted {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "turn".into(),
        })
        .await
        .expect("turn start");
    process.on_event(&AgentEvent::ToolStarted {
        id: octet_ai::ToolCallId("tool-call".into()),
        name: "observed".into(),
        args: serde_json::json!({}),
    });

    let admission = old_connection
        .acquire_request_admission()
        .expect("hold admitted request in drain window");
    let reload_process = process.clone();
    let reload = tokio::spawn(async move { reload_process.reload().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !old_connection.draining.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reload never began drain");
    process.on_event(&AgentEvent::ToolFinished {
        id: octet_ai::ToolCallId("tool-call".into()),
        result: Ok(ToolOutput::new("natural completion during drain")),
        duration: Duration::ZERO,
    });
    drop(admission);
    tokio::time::timeout(Duration::from_secs(3), reload)
        .await
        .expect("reload timed out")
        .expect("reload task failed")
        .expect("reload");
    let new_connection = read_std_lock(&process.inner.connection).clone();
    let new_pid = new_connection.child.lock().await.id().expect("new pid");
    assert_ne!(old_pid, new_pid);
    process.on_event(&AgentEvent::ToolFinished {
        id: octet_ai::ToolCallId("tool-call".into()),
        result: Ok(ToolOutput::new("duplicate late completion")),
        duration: Duration::ZERO,
    });
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnSettled {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "turn".into(),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 1,
            reason: None,
        })
        .await
        .expect("replacement turn terminal");
    process.clear_active_lifecycle_turn("owner", "turn");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionSettled {
            session_id: "session".into(),
            run_id: Some("run".into()),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 2,
            reason: None,
        })
        .await
        .expect("replacement session terminal");
    assert!(process.shutdown().await);

    let records = lifecycle_records(&log_path);
    assert_eq!(
        lifecycle_methods_for_pid(&records, old_pid),
        [
            "session/started",
            "turn/started",
            "tool/started",
            "tool/settled",
            "turn/settled",
            "session/settled",
        ]
    );
    assert_eq!(
        lifecycle_methods_for_pid(&records, new_pid),
        [
            "session/started",
            "turn/started",
            "turn/settled",
            "session/settled",
        ]
    );
    let old_terminal_outcomes = records
        .iter()
        .filter(|record| {
            record["pid"].as_u64() == Some(u64::from(old_pid))
                && record["method"]
                    .as_str()
                    .is_some_and(|method| method.ends_with("/settled"))
        })
        .map(|record| record["params"]["outcome"].as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        old_terminal_outcomes,
        [Some("completed"), Some("interrupted"), Some("interrupted")]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn delayed_turn_start_after_reload_is_delivered_once_to_replacement() {
    let temp = TempDir::new().expect("tempdir");
    let (process, log_path) = lifecycle_v02_fixture(&temp).await;
    let old_connection = read_std_lock(&process.inner.connection).clone();
    let old_pid = old_connection.child.lock().await.id().expect("old pid");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionStarted {
            session_id: "session".into(),
            run_id: Some("run".into()),
        })
        .await
        .expect("session start");
    process.set_active_lifecycle_turn("owner", "session", "run", "delayed-turn");

    process
        .reload()
        .await
        .expect("reload before start delivery");
    let new_connection = read_std_lock(&process.inner.connection).clone();
    let new_pid = new_connection.child.lock().await.id().expect("new pid");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnStarted {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "delayed-turn".into(),
        })
        .await
        .expect("delayed start is once-suppressed");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnSettled {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "delayed-turn".into(),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 1,
            reason: None,
        })
        .await
        .expect("replacement turn terminal");
    process.clear_active_lifecycle_turn("owner", "delayed-turn");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionSettled {
            session_id: "session".into(),
            run_id: Some("run".into()),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 2,
            reason: None,
        })
        .await
        .expect("replacement session terminal");
    assert!(process.shutdown().await);

    let records = lifecycle_records(&log_path);
    assert_eq!(
        lifecycle_methods_for_pid(&records, old_pid),
        ["session/started", "session/settled"],
        "old generation must not receive an unmatched turn terminal"
    );
    assert_eq!(
        lifecycle_methods_for_pid(&records, new_pid),
        [
            "session/started",
            "turn/started",
            "turn/settled",
            "session/settled",
        ],
        "replacement must receive exactly one turn start"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn settled_turn_is_consumed_before_reload_can_handoff_lifecycle() {
    let temp = TempDir::new().expect("tempdir");
    let (process, log_path) = lifecycle_v02_fixture(&temp).await;
    let old_connection = read_std_lock(&process.inner.connection).clone();
    let old_pid = old_connection.child.lock().await.id().expect("old pid");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionStarted {
            session_id: "session".into(),
            run_id: Some("run".into()),
        })
        .await
        .expect("session start");
    process.set_active_lifecycle_turn("owner", "session", "run", "settled-turn");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnStarted {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "settled-turn".into(),
        })
        .await
        .expect("turn start");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::TurnSettled {
            session_id: "session".into(),
            run_id: "run".into(),
            turn_id: "settled-turn".into(),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 1,
            reason: None,
        })
        .await
        .expect("turn terminal");

    process.reload().await.expect("reload after terminal");
    let new_connection = read_std_lock(&process.inner.connection).clone();
    let new_pid = new_connection.child.lock().await.id().expect("new pid");
    process.clear_active_lifecycle_turn("owner", "settled-turn");
    process
        .notify_lifecycle(&ExtensionLifecycleEvent::SessionSettled {
            session_id: "session".into(),
            run_id: Some("run".into()),
            outcome: ExtensionLifecycleOutcome::Completed,
            duration_ms: 2,
            reason: None,
        })
        .await
        .expect("replacement session terminal");
    assert!(process.shutdown().await);

    let records = lifecycle_records(&log_path);
    assert_eq!(
        lifecycle_methods_for_pid(&records, old_pid),
        [
            "session/started",
            "turn/started",
            "turn/settled",
            "session/settled",
        ]
    );
    assert_eq!(
        lifecycle_methods_for_pid(&records, new_pid),
        ["session/started", "session/settled"],
        "reload must not duplicate or resurrect a settled turn"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn process_transport_registers_tools_and_routes_events_and_confirmation() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("fixture.sh");
    std::fs::write(&script_path, protocol_fixture_script()).expect("write fixture");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script_path, permissions).expect("chmod");

    let manifest = ExtensionManifest::parse(
        r#"
name = "fixture"
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "fixture.sh"
[contributes]
tools = ["echo"]
notifications = true
confirmations = true
"#,
    )
    .expect("manifest");
    let descriptor = trusted_descriptor(temp.path(), manifest);
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");

    let mut host = ExtensionHost::new();
    host.load(&process);
    assert_eq!(host.tool_definitions()[0].name, "echo");

    let mut events = process.subscribe();
    let result = process
        .call_tool(
            "echo",
            serde_json::json!({"text": "hello"}),
            process.current_context(),
        )
        .await
        .expect("tool result");
    assert_eq!(result.content, "from extension");
    assert!(!result.is_error);

    let notification = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("notification timeout")
        .expect("notification event");
    assert!(matches!(
        notification,
        ExtensionEvent::Notification { notification }
            if notification.message == "tool called"
    ));
    let confirmation = tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .expect("confirmation timeout")
        .expect("confirmation event");
    let (request_id, generation) = match confirmation {
        ExtensionEvent::ConfirmationRequested {
            request_id,
            generation,
            request,
            ..
        } => {
            assert_eq!(request.prompt, "Continue?");
            (request_id, generation)
        }
        event => panic!("unexpected event: {event:?}"),
    };
    process
        .respond_to_confirmation(
            request_id.clone(),
            generation,
            ConfirmationResponse { confirmed: true },
        )
        .await
        .expect("confirmation response");
    assert!(process.confirmation_answered(&request_id, generation));
    process
        .respond_to_confirmation(
            request_id,
            generation,
            ConfirmationResponse { confirmed: false },
        )
        .await
        .expect("duplicate confirmation response is suppressed");
    assert!(process.shutdown().await);
    assert!(!process.is_running());
}

#[cfg(unix)]
#[tokio::test]
async fn process_diagnostics_remain_separate_from_tool_progress() {
    use crate::tool::ToolProgress;

    let temp = TempDir::new().unwrap();
    write_executable_script(
        &temp.path().join("diagnostics.sh"),
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[{"name":"held","description":"Held tool","parameters":{"type":"object"}}],"commands":[]}}'
IFS= read -r tool_call
printf '%s\n' '{"jsonrpc":"2.0","id":999,"result":{}}'
printf '%s\n' '{"jsonrpc":"2.0","method":"notification","params":{"level":"info","message":"diagnostic barrier"}}'
IFS= read -r release
case "$release" in *'"method":"test/release"'*) ;; *) exit 91 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":"released","is_error":false,"metadata":null,"structured_content":null}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"
name = "diagnostic-routing"
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "diagnostics.sh"
[contributes]
tools = ["held"]
notifications = true
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    let mut diagnostics = process.subscribe();
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();
    let (_, tools) = host.tool_snapshot();
    let sandbox = crate::SandboxConfig::new(temp.path());
    let registered = vec!["held".to_owned()];
    let (progress, mut callbacks) = ToolProgressSink::bounded_channel();
    let context = ToolContext {
        workspace: temp.path(),
        sandbox: &sandbox,
        execution_scope: "diagnostic-test",
        resource_owner: "diagnostic-owner",
        active_skills: &[],
        registered_tools: &registered,
        progress,
        cancellation: crate::CancellationToken::default(),
    };
    let call = tools[0].execute(serde_json::json!({}), &context);
    tokio::pin!(call);
    // Both events use the same FIFO process broadcast. Observing the ordinary
    // notification in the tool stream proves the earlier diagnostic was consumed;
    // the fixture cannot send the terminal until the explicit release below.
    let observed = tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            event = callbacks.recv() => event.unwrap(),
            result = &mut call => panic!("tool completed before release: {result:?}"),
        }
    })
    .await
    .unwrap();
    assert!(matches!(observed, ToolProgress::Status(text)
        if text == "extension notification: diagnostic barrier"));
    let diagnostic = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let ExtensionEvent::Diagnostic { message } = diagnostics.recv().await.unwrap() {
                break message;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(diagnostic, "ignored response for unknown request 999");
    // Fixture-only release over the existing writer, not a product RPC/profile.
    assert!(read_std_lock(&process.inner.connection)
        .queue_notification("test/release", serde_json::json!({})));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), &mut call)
            .await
            .unwrap()
            .unwrap()
            .text,
        "released"
    );
    assert!(callbacks.try_recv().is_err());
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn explicit_null_structured_content_is_validated_and_preserved() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("null-structured.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[{"name":"null_result","description":"Return explicit null","parameters":{"type":"object"},"output_schema":{"type":"null"}}],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
IFS= read -r tool_call
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"explicit null"}],"is_error":false,"structured_content":null,"metadata":null}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"
name = "null-structured"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "null-structured.sh"
[contributes]
tools = ["null_result"]
"#,
    )
    .expect("manifest");
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .expect("start process");

    let output = process
        .call_tool(
            "null_result",
            serde_json::json!({}),
            process.current_context(),
        )
        .await
        .expect("null output satisfies type:null schema");
    assert_eq!(output.structured_content, Some(serde_json::Value::Null));
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn reload_swaps_only_after_a_compatible_handshake() {
    use std::os::unix::fs::PermissionsExt;

    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("reload.sh");
    std::fs::write(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
"#,
    )
    .expect("write fixture");
    let mut permissions = std::fs::metadata(&script_path)
        .expect("metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&script_path, permissions).expect("chmod");

    let descriptor = trusted_descriptor(temp.path(), minimal_manifest("reloadable", "reload.sh"));
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");
    assert!(!process.supports_feature("old-generation-sentinel"));
    // A synthetic feature on the old connection proves the query does not
    // capture initialization or an old generation's cloned feature set.
    {
        let connection = read_std_lock(&process.inner.connection);
        write_std_lock(&connection.protocol)
            .features
            .insert("old-generation-sentinel".into());
    }
    assert!(process.supports_feature("old-generation-sentinel"));
    let report = process.reload().await.expect("reload");
    assert!(!process.supports_feature("old-generation-sentinel"));
    assert_eq!(report.generation, 2);
    assert!(report.previous_shutdown_graceful);
    assert!(process.is_running());
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn drain_waits_for_request_between_admission_gate_and_pending_insert() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("admission-drain.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("admission-drain", "admission-drain.sh"),
    );
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();
    let admission = connection
        .acquire_request_admission()
        .expect("admit before drain");
    let drain_connection = Arc::clone(&connection);
    let mut drain = tokio::spawn(async move {
        drain_connection
            .drain(Duration::from_secs(1), "reload drain deadline")
            .await
    });

    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut drain)
            .await
            .is_err(),
        "drain returned while an admitted request had not inserted pending state"
    );
    drop(admission);
    assert!(tokio::time::timeout(Duration::from_secs(1), drain)
        .await
        .expect("drain did not observe admission release")
        .expect("drain task failed"));
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_shutdown_preserves_each_pending_method_and_wire_envelope() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("cancel-methods.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r first
IFS= read -r second
printf '%s\n%s\n' "$first" "$second" > "$OCTET_WORKSPACE/requests.jsonl"
IFS= read -r shutdown
case "$shutdown" in
  *'"method":"shutdown"'*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}' ;;
  *) exit 23 ;;
esac
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("cancel-methods", "cancel-methods.sh"),
    );
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();
    let methods = [methods::TOOL_CALL, methods::COMMAND_EXECUTE];
    let calls = methods.map(|method| {
        let connection = Arc::clone(&connection);
        tokio::spawn(async move {
            connection
                .request(
                    method,
                    serde_json::json!({"marker": method}),
                    Duration::from_secs(5),
                )
                .await
        })
    });
    // Wait for both writes, not a scheduling delay or just queued frames:
    // cancellation may legitimately skip a frame not yet sent to the child.
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let written = {
                let pending = lock_std_mutex(&connection.pending);
                pending.len() == 2
                    && pending
                        .values()
                        .all(|request| request.frame_state.load(Ordering::Acquire) == FRAME_WRITTEN)
            };
            if written {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("both requests must be written before shutdown");

    assert!(process.shutdown().await);
    for (expected_method, call) in methods.into_iter().zip(calls) {
        match call.await.expect("request task") {
            Err(ExtensionRuntimeError::Cancelled { method, reason }) => {
                assert_eq!(method, expected_method);
                assert_eq!(reason, "shutdown");
            }
            other => panic!("expected pending cancellation, got {other:?}"),
        }
    }
    assert!(lock_std_mutex(&connection.pending).is_empty());

    let captured = std::fs::read_to_string(temp.path().join("requests.jsonl"))
        .expect("captured legacy requests");
    let frames = captured
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("request envelope"))
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0]["id"], 2);
    assert_eq!(frames[1]["id"], 3);
    for method in methods {
        let frame = frames
            .iter()
            .find(|frame| frame["method"] == method)
            .expect("original method on the wire");
        assert_eq!(
            *frame,
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": frame["id"],
                "method": method,
                "params": {"marker": method},
            })
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn hung_process_rpc_is_bounded_and_removes_its_pending_slot() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("hung-rpc.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r request
sleep 30
"#,
    );
    let descriptor = trusted_descriptor(temp.path(), minimal_manifest("hung-rpc", "hung-rpc.sh"));
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.shutdown_timeout = Duration::from_millis(50);
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();

    let started = Instant::now();
    let error = connection
        .request(
            "probe/hang",
            serde_json::json!({}),
            Duration::from_millis(100),
        )
        .await
        .expect_err("hung request must time out");
    assert!(
        matches!(error, ExtensionRuntimeError::Timeout { ref method }
                if method == "probe/hang"),
        "{error:?}"
    );
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "request exceeded its bounded deadline: {:?}",
        started.elapsed()
    );
    assert!(lock_std_mutex(&connection.pending).is_empty());
    assert!(!process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn timed_out_framed_write_does_not_corrupt_the_connection() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("blocked-stdin.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
sleep 30
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("blocked-stdin", "blocked-stdin.sh"),
    );
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.max_message_bytes = 5 * 1024 * 1024;
    config.shutdown_timeout = Duration::from_millis(50);
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();

    let error = connection
        .request(
            "probe/large",
            serde_json::json!({"payload": "x".repeat(4 * 1024 * 1024)}),
            Duration::from_millis(50),
        )
        .await
        .expect_err("blocked framed write must time out");
    assert!(matches!(error, ExtensionRuntimeError::Timeout { .. }));
    assert!(!connection.closed.load(Ordering::Acquire));
    assert!(lock_std_mutex(&connection.pending).is_empty());
    connection.terminate().await;
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_process_rpc_releases_its_pending_slot() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("dropped-rpc.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r request
sleep 30
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("dropped-rpc", "dropped-rpc.sh"),
    );
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.shutdown_timeout = Duration::from_millis(50);
    let process = ExtensionProcess::start(descriptor, config)
        .await
        .expect("start process");
    let connection = read_std_lock(&process.inner.connection).clone();
    let request_connection = Arc::clone(&connection);
    let request = tokio::spawn(async move {
        request_connection
            .request("probe/drop", serde_json::json!({}), Duration::from_secs(5))
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if lock_std_mutex(&connection.pending).len() == 1 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("request never registered");
    request.abort();
    let _ = request.await;
    assert!(lock_std_mutex(&connection.pending).is_empty());
    connection.terminate().await;
}

#[cfg(unix)]
#[test]
fn process_scan_does_not_mutate_a_later_registration() {
    let process_group_id = 42;
    let root = ProcessIdentity {
        pid: process_group_id,
        start_time: 200,
    };
    let descendant = ProcessIdentity {
        pid: 43,
        start_time: 201,
    };
    let mut registered = BTreeMap::from([(
        process_group_id,
        RegisteredProcessGroup {
            kind: RegisteredProcessKind::Bash,
            registration_id: 8,
            root: Some(root),
            original_group_active: true,
            descendants: BTreeMap::from([(descendant.pid, descendant)]),
            detached_bash: None,
        },
    )]);
    let registrations_at_snapshot_start = BTreeMap::from([(
        process_group_id,
        RegisteredProcessScanState {
            registration_id: 7,
            direct_bash_child_owned: true,
        },
    )]);
    let unrelated = ProcessSnapshot {
        identity: ProcessIdentity {
            pid: 900,
            start_time: 1,
        },
        parent_pid: 1,
        process_group_id: 900,
        is_zombie: false,
    };
    let snapshots_by_pid = BTreeMap::from([(unrelated.identity.pid, unrelated)]);

    apply_process_snapshots(
        &mut registered,
        &registrations_at_snapshot_start,
        &snapshots_by_pid,
    );

    let entry = registered.get(&process_group_id).expect("registration");
    assert!(entry.original_group_active);
    assert_eq!(entry.root, Some(root));
    assert_eq!(entry.descendants.get(&descendant.pid), Some(&descendant));
}

#[cfg(unix)]
#[test]
fn process_scan_keeps_a_directly_owned_group_bound_during_handoff() {
    let process_group_id = 42;
    let root = ProcessIdentity {
        pid: process_group_id,
        start_time: 200,
    };
    let mut registered = BTreeMap::from([(
        process_group_id,
        RegisteredProcessGroup {
            kind: RegisteredProcessKind::Bash,
            registration_id: 8,
            root: Some(root),
            original_group_active: true,
            descendants: BTreeMap::new(),
            detached_bash: Some(DetachedBashSupervision {
                deadline: Instant::now() + Duration::from_secs(1),
                cancellation: CancellationToken::default(),
            }),
        },
    )]);
    let registrations_at_snapshot_start = BTreeMap::from([(
        process_group_id,
        RegisteredProcessScanState {
            registration_id: 8,
            direct_bash_child_owned: true,
        },
    )]);
    let unrelated = ProcessSnapshot {
        identity: ProcessIdentity {
            pid: 900,
            start_time: 1,
        },
        parent_pid: 1,
        process_group_id: 900,
        is_zombie: false,
    };
    let snapshots_by_pid = BTreeMap::from([(unrelated.identity.pid, unrelated)]);

    apply_process_snapshots(
        &mut registered,
        &registrations_at_snapshot_start,
        &snapshots_by_pid,
    );

    let entry = registered.get(&process_group_id).expect("registration");
    assert!(entry.original_group_active);
}

#[cfg(unix)]
#[test]
fn process_scan_does_not_keep_an_extension_group_bound_without_a_member() {
    let process_group_id = 42;
    let root = ProcessIdentity {
        pid: process_group_id,
        start_time: 200,
    };
    let mut registered = BTreeMap::from([(
        process_group_id,
        RegisteredProcessGroup {
            kind: RegisteredProcessKind::Extension,
            registration_id: 8,
            root: Some(root),
            original_group_active: true,
            descendants: BTreeMap::new(),
            detached_bash: None,
        },
    )]);
    let registrations_at_snapshot_start = BTreeMap::from([(
        process_group_id,
        RegisteredProcessScanState {
            registration_id: 8,
            direct_bash_child_owned: false,
        },
    )]);
    let unrelated = ProcessSnapshot {
        identity: ProcessIdentity {
            pid: 900,
            start_time: 1,
        },
        parent_pid: 1,
        process_group_id: 900,
        is_zombie: false,
    };
    let snapshots_by_pid = BTreeMap::from([(unrelated.identity.pid, unrelated)]);

    apply_process_snapshots(
        &mut registered,
        &registrations_at_snapshot_start,
        &snapshots_by_pid,
    );

    let entry = registered.get(&process_group_id).expect("registration");
    assert!(!entry.original_group_active);
    assert!(entry.descendants.is_empty());
}

#[cfg(unix)]
#[test]
fn pid_start_time_prevents_signaling_a_different_process_identity() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exec sleep 30"])
        .spawn()
        .expect("spawn identity fixture");
    let pid = i32::try_from(child.id()).expect("fixture pid");
    let identity = process_identity(pid).expect("read fixture identity");
    let stale_identity = ProcessIdentity {
        start_time: identity.start_time.saturating_add(1),
        ..identity
    };

    signal_identity(stale_identity, libc::SIGKILL);
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        child.try_wait().expect("inspect fixture").is_none(),
        "a stale PID identity signaled the replacement process"
    );

    signal_identity(identity, libc::SIGKILL);
    child.wait().expect("reap fixture");
}

#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios"
))]
#[test]
fn zombie_processes_are_not_live_identities() {
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .spawn()
        .expect("spawn zombie fixture");
    let pid = i32::try_from(child.id()).expect("fixture pid");
    let wait_id = libc::id_t::try_from(pid).expect("fixture wait id");
    let mut exit = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
    // SAFETY: `exit` is writable siginfo storage and `pid` names our child.
    // WNOWAIT observes its exit while preserving the state for `Child::wait`.
    let wait_result = unsafe {
        libc::waitid(
            libc::P_PID,
            wait_id,
            exit.as_mut_ptr(),
            libc::WEXITED | libc::WNOWAIT,
        )
    };
    let wait_error = (wait_result == -1).then(std::io::Error::last_os_error);

    // Capture liveness before reaping. Linux exposes a zombie snapshot;
    // Darwin may instead stop serving PROC_PIDTBSDINFO for the zombie.
    let snapshot = process_snapshot(pid);
    let identity = process_identity(pid);
    let is_live = process_is_live_for_test(pid);
    let exit_status = child.wait().expect("reap zombie fixture");

    assert_eq!(
        wait_result, 0,
        "waitid did not observe fixture exit: {wait_error:?}"
    );
    assert!(
        exit_status.success(),
        "zombie fixture failed: {exit_status}"
    );
    #[cfg(any(target_os = "linux", target_os = "android"))]
    assert!(
        snapshot.is_some_and(|snapshot| !snapshot.is_live()),
        "an exited, unreaped process lacked a zombie snapshot: {snapshot:?}"
    );
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    assert!(
        snapshot.is_none_or(|snapshot| !snapshot.is_live()),
        "an exited, unreaped process had a live snapshot: {snapshot:?}"
    );
    assert_eq!(identity, None);
    assert!(
        !is_live,
        "zombies must not keep lifecycle diagnostics alive"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn graceful_shutdown_request_reaches_extension_before_exit() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("graceful.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
IFS= read -r shutdown
printf '%s\n' graceful > "$OCTET_WORKSPACE/graceful.marker"
descendant_marker="$OCTET_WORKSPACE/graceful-descendant.pid"
python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1], "w").write(str(os.getpid())); time.sleep(30)' "$descendant_marker" &
attempts=0
while [ ! -s "$descendant_marker" ]; do
  attempts=$((attempts + 1))
  [ "$attempts" -lt 100 ] || exit 24
  sleep 0.01
done
sleep 0.1
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
"#,
    );
    let descriptor = trusted_descriptor(temp.path(), minimal_manifest("graceful", "graceful.sh"));
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("start process");

    assert!(process.shutdown().await);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("graceful.marker")).expect("shutdown marker"),
        "graceful\n"
    );
    let descendant = std::fs::read_to_string(temp.path().join("graceful-descendant.pid"))
        .expect("descendant marker")
        .trim()
        .parse::<i32>()
        .expect("descendant pid");
    let deadline = Instant::now() + Duration::from_millis(500);
    while process_id_exists(descendant) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !process_id_exists(descendant),
        "extension descendant survived graceful shutdown"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn fatal_stdout_protocol_error_terminates_and_reaps_the_child_group() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("fatal-stdout.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
sleep 0.05
printf '%s\n' 'not-json'
sleep 30
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("fatal-stdout", "fatal-stdout.sh"),
    );
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("initial handshake completes before fatal frame");
    let connection = read_std_lock(&process.inner.connection).clone();
    let pid = connection.child.lock().await.id().expect("child pid");

    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let reaped = connection
                .child
                .lock()
                .await
                .try_wait()
                .expect("inspect child")
                .is_some();
            if reaped {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fatal child was not reaped");
    assert!(connection.closed.load(Ordering::Acquire));
    assert!(!process_group_registered_for_test(pid as i32));
    assert!(!process_id_exists(pid as i32));
    process.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn supervisor_restarts_after_a_crash_and_explicit_shutdown_stops_revival() {
    let temp = TempDir::new().expect("tempdir");
    let script_path = temp.path().join("restart-once.sh");
    write_executable_script(
        &script_path,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[],"commands":[]}}'
marker="$OCTET_WORKSPACE/restarted.marker"
if [ ! -f "$marker" ]; then
  printf '%s\n' first > "$marker"
  sleep 0.05
  exit 17
fi
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'
"#,
    );
    let descriptor = trusted_descriptor(
        temp.path(),
        minimal_manifest("restart-once", "restart-once.sh"),
    );
    let process = ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(temp.path()))
        .await
        .expect("initial generation");
    let first = read_std_lock(&process.inner.connection).clone();

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let health = process.health_snapshot();
            if health.generation >= 2 && health.state == ExtensionHealthState::Ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("supervisor did not activate a replacement");
    assert!(first.closed.load(Ordering::Acquire));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("restarted.marker")).unwrap(),
        "first\n"
    );

    assert!(process.shutdown().await);
    let stopped_generation = process.health_snapshot().generation;
    tokio::time::sleep(SUPERVISOR_BASE_BACKOFF + SUPERVISOR_POLL).await;
    assert_eq!(process.health_snapshot().generation, stopped_generation);
    assert_eq!(
        process.health_snapshot().state,
        ExtensionHealthState::Stopped
    );
}

#[cfg(unix)]
#[tokio::test]
async fn runtime_commands_may_be_discovered_during_initialization() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("runtime-commands.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(90)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


initialize = receive()
assert initialize["params"]["protocol"]["version"] == "0.2", initialize
assert "runtime_commands" in initialize["params"]["protocol"]["optional_features"], initialize
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.2",
        "tools": [],
        "commands": [{
            "name": "runtime-hello",
            "description": "Discovered after loading a compatibility runtime",
            "usage": "/runtime-hello [name]",
        }],
        "protocol": {
            "version": "0.2",
            "features": ["request_cancellation", "content_parts", "runtime_commands"],
            "limits": {"max_concurrent_requests": 1},
        },
    },
})

command = receive()
assert command["method"] == "command/execute", command
assert command["params"]["name"] == "runtime-hello", command
assert command["params"]["arguments"] == ["octet"], command
send({
    "jsonrpc": "2.0",
    "id": command["id"],
    "result": {
        "text": "hello octet",
        "notifications": [],
        "context": [],
    },
})

shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "runtime-commands"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "runtime-commands.py"
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();

    assert_eq!(
        process.contributions().commands,
        vec![CommandDefinition {
            name: "runtime-hello".into(),
            description: "Discovered after loading a compatibility runtime".into(),
            usage: Some("/runtime-hello [name]".into()),
        }]
    );
    let output = process
        .execute_command(
            "runtime-hello",
            vec!["octet".into()],
            process.current_context(),
        )
        .await
        .unwrap();
    assert_eq!(output.text, "hello octet");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn declared_menus_are_collected_typed_and_routed_to_declared_commands() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("menu.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(90)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


initialize = receive()
assert initialize["params"]["contributes"]["menu"] is True, initialize
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.2",
        "tools": [],
        "commands": [{"name": "tool", "description": "Tool actions"}],
        "protocol": {
            "version": "0.2",
            "features": ["request_cancellation", "content_parts"],
            "limits": {"max_concurrent_requests": 1},
        },
    },
})

request = receive()
assert request["method"] == "menu/collect", request
assert "context" in request["params"], request
send({"jsonrpc": "2.0", "id": request["id"], "result": {
    "title": "Tool",
    "status": {"state": "active", "label": "Ready"},
    "items": [
        {"id": "setup", "label": "Set up", "command": "tool",
         "arguments": ["setup"], "recommended": True},
        {"id": "servers", "label": "Servers", "detail": "One server",
         "items": [{"id": "restart", "label": "Restart", "command": "tool",
                    "arguments": ["restart", "one"], "destructive": True}]},
    ],
}})

request = receive()
assert request["method"] == "menu/collect", request
send({"jsonrpc": "2.0", "id": request["id"], "result": {
    "items": [{"id": "escape", "label": "Escape", "command": "elsewhere"}],
}})

shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "menu-fixture"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "menu.py"
[contributes]
commands = ["tool"]
menu = true
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    assert!(process.contributions().menu);

    let menu = process
        .collect_menu(process.current_context())
        .await
        .unwrap();
    assert_eq!(menu.title.as_deref(), Some("Tool"));
    assert!(menu.items[0].recommended);
    assert_eq!(menu.items[0].arguments, ["setup"]);
    let servers = menu.items[1].items.as_ref().unwrap();
    assert!(servers[0].destructive);
    assert_eq!(servers[0].arguments, ["restart", "one"]);

    match process.collect_menu(process.current_context()).await {
        Err(ExtensionRuntimeError::Protocol(message)) => {
            assert!(message.contains("undeclared command"), "{message}");
        }
        other => panic!("expected an undeclared-command rejection, got {other:?}"),
    }
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn attended_commands_may_outlast_the_ordinary_request_deadline() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("slow.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys
import time


def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


initialize = receive()
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.2",
        "tools": [],
        "commands": [{"name": "tool", "description": "Slow setup"}],
        "protocol": {
            "version": "0.2",
            "features": ["request_cancellation", "content_parts", "request_progress"],
            "limits": {"max_concurrent_requests": 2},
        },
    },
})
while True:
    message = receive()
    if message.get("method") == "command/execute":
        time.sleep(0.8)
        send({"jsonrpc": "2.0", "id": message["id"], "result": {"text": "installed"}})
    elif message.get("method") == "shutdown":
        send({"jsonrpc": "2.0", "id": message["id"], "result": {}})
        break
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "slow-fixture"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "slow.py"
[contributes]
commands = ["tool"]
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.request_timeout = Duration::from_millis(300);
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();

    let (started, _) = oneshot::channel();
    let ordinary = process
        .execute_command_controlled_with_progress(
            "tool",
            Vec::new(),
            process.current_context(),
            CancellationToken::default(),
            ToolProgressSink::null(),
            started,
        )
        .await;
    assert!(
        matches!(ordinary, Err(ExtensionRuntimeError::Timeout { .. })),
        "{ordinary:?}"
    );

    let (started, _) = oneshot::channel();
    let attended = process
        .execute_attended_command_with_progress(
            "tool",
            Vec::new(),
            process.current_context(),
            CancellationToken::default(),
            ToolProgressSink::null(),
            started,
            Duration::from_secs(20),
        )
        .await
        .unwrap();
    assert_eq!(attended.text, "installed");
    assert!(process.shutdown().await);
}

#[test]
fn menus_need_api_0_2_and_a_declared_command() {
    for (source, expected) in [
        (
            "name = \"legacy\"\nversion = \"0.1.0\"\napi_version = \"0.1\"\n[entrypoint]\ncommand = \"x\"\n[contributes]\ncommands = [\"tool\"]\nmenu = true\n",
            "require extension API 0.2",
        ),
        (
            "name = \"bare\"\nversion = \"0.2.0\"\napi_version = \"0.2\"\n[entrypoint]\ncommand = \"x\"\n[contributes]\nmenu = true\n",
            "at least one declared command",
        ),
    ] {
        match ExtensionManifest::parse(source) {
            Err(ExtensionRuntimeError::InvalidManifest(message)) => {
                assert!(message.contains(expected), "{message}");
            }
            other => panic!("expected {expected:?}, got {other:?}"),
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn declared_shortcuts_execute_after_initialization() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("shortcuts.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys


def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(90)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


initialize = receive()
assert initialize["params"]["contributes"]["shortcuts"] == [{
    "key": "ctrl+shift+p",
    "name": "toggle-panel",
    "description": "Toggle the extension panel",
}], initialize
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.2",
        "tools": [],
        "commands": [],
        "shortcuts": [{
            "key": "ctrl+shift+p",
            "name": "toggle-panel",
            "description": "Toggle the extension panel",
        }],
        "protocol": {
            "version": "0.2",
            "features": ["request_cancellation", "content_parts"],
            "limits": {"max_concurrent_requests": 1},
        },
    },
})

shortcut = receive()
assert shortcut["method"] == "shortcut/execute", shortcut
assert shortcut["params"]["name"] == "toggle-panel", shortcut
send({
    "jsonrpc": "2.0",
    "id": shortcut["id"],
    "result": {
        "text": "shortcut executed",
        "notifications": [],
        "context": [],
    },
})

shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let manifest = ExtensionManifest::parse(
            r#"name = "shortcut-extension"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "shortcuts.py"
[contributes]
shortcuts = [{ key = "ctrl+shift+p", name = "toggle-panel", description = "Toggle the extension panel" }]
"#,
        )
        .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();

    let output = process
        .execute_shortcut("toggle-panel", process.current_context())
        .await
        .unwrap();
    assert_eq!(output.text, "shortcut executed");
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn live_catalog_rejects_prospective_schema_overflow_without_publication() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("schema-budget.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import os
import sys

def receive():
    line = sys.stdin.readline()
    assert line, "host closed stdin"
    return json.loads(line)

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

initialize = receive()
send({"jsonrpc":"2.0", "id":initialize["id"], "result": {
    "api_version":"0.2", "tools":[], "commands":[],
    "protocol":{"version":"0.2", "features":["request_cancellation", "content_parts", "dynamic_tools"],
                "limits":{"max_concurrent_requests":1}}
}})
for index in range(7):
    schema = {"type":"object", "description":"x" * 300000}
    tool = {"name":"bulk_" + str(index), "description":"bounded", "parameters":schema, "output_schema":schema}
    send({"jsonrpc":"2.0", "id":"catalog-" + str(index), "method":"tools/register", "params":{"tools":[tool]}})
    ack = receive()
    if index < 6:
        assert ack["result"]["revision"] == index + 1, ack
        assert ack["result"]["tools"] == ["bulk_" + str(i) for i in range(index + 1)], ack
    else:
        assert ack["error"]["code"] == -32602, ack
        assert "aggregate schema bytes" in ack["error"]["message"], ack
with open(os.path.join(os.environ["OCTET_WORKSPACE"], "catalog-checked"), "w") as marker:
    marker.write("checked")
shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc":"2.0", "id":shutdown["id"], "result":{}})
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "schema-budget"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "schema-budget.py"
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !temp.path().join("catalog-checked").exists() {
            assert!(
                process.is_running(),
                "fixture exited before verifying acknowledgements"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("bounded catalog mutation completion");
    assert_eq!(host.tool_definitions().len(), 6);
    {
        let connection = read_std_lock(&process.inner.connection);
        assert_eq!(connection.catalog_revision.load(Ordering::Acquire), 6);
    }
    assert!(process.shutdown().await);
}

#[cfg(unix)]
#[tokio::test]
async fn live_subprocess_catalog_registers_replaces_and_unregisters_while_host_is_idle() {
    let temp = TempDir::new().unwrap();
    let script_path = temp.path().join("dynamic.py");
    write_executable_script(
        &script_path,
        r#"#!/usr/bin/env python3
import json
import sys

def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(90)
    return json.loads(line)

def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)

initialize = receive()
send({
    "jsonrpc": "2.0",
    "id": initialize["id"],
    "result": {
        "api_version": "0.2",
        "tools": [],
        "commands": [],
        "protocol": {
            "version": "0.2",
            "features": ["request_cancellation", "content_parts", "dynamic_tools"],
            "limits": {"max_concurrent_requests": 1},
        },
    },
})

def definition(description):
    return {
        "name": "live_echo",
        "description": description,
        "parameters": {"type": "object", "additionalProperties": False},
    }

send({
    "jsonrpc": "2.0",
    "id": "catalog-1",
    "method": "tools/register",
    "params": {"tools": [definition("revision one")]},
})
ack = receive()
assert ack["result"] == {"revision": 1, "tools": ["live_echo"]}, ack

first = receive()
assert first["method"] == "tool/call", first
assert first["params"]["catalog_revision"] == 1, first
send({
    "jsonrpc": "2.0",
    "id": first["id"],
    "result": {
        "content": [{"type": "text", "text": "revision one"}],
        "is_error": False,
        "metadata": {},
    },
})

send({
    "jsonrpc": "2.0",
    "id": "catalog-2",
    "method": "tools/register",
    "params": {"tools": [definition("revision two")]},
})
ack = receive()
assert ack["result"] == {"revision": 2, "tools": ["live_echo"]}, ack

second = receive()
assert second["method"] == "tool/call", second
assert second["params"]["catalog_revision"] == 2, second
send({
    "jsonrpc": "2.0",
    "id": second["id"],
    "result": {
        "content": [{"type": "text", "text": "revision two"}],
        "is_error": False,
        "metadata": {},
    },
})

send({
    "jsonrpc": "2.0",
    "id": "catalog-3",
    "method": "tools/unregister",
    "params": {"names": ["live_echo"]},
})
ack = receive()
assert ack["result"] == {"revision": 3, "tools": []}, ack

shutdown = receive()
assert shutdown["method"] == "shutdown", shutdown
send({"jsonrpc": "2.0", "id": shutdown["id"], "result": {}})
"#,
    );
    let manifest = ExtensionManifest::parse(
        r#"name = "dynamic"
version = "0.2.0"
api_version = "0.2"
[entrypoint]
command = "dynamic.py"
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();

    let initial_publication = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let definitions = host.tool_definitions();
            if definitions
                .iter()
                .any(|definition| definition.description == "revision one")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    if initial_publication.is_err() {
        let mut events = process.subscribe();
        let mut diagnostics = Vec::new();
        while let Ok(event) = events.try_recv() {
            diagnostics.push(format!("{event:?}"));
        }
        panic!(
            "initial live catalog did not publish while the host was idle; health={:?}; events={diagnostics:?}",
            process.health_snapshot()
        );
    }
    assert_eq!(
        process
            .call_tool(
                "live_echo",
                serde_json::json!({}),
                process.current_context()
            )
            .await
            .unwrap()
            .content,
        "revision one"
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let definitions = host.tool_definitions();
            if definitions
                .iter()
                .any(|definition| definition.description == "revision two")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("replacement live catalog did not publish");
    assert_eq!(
        process
            .call_tool(
                "live_echo",
                serde_json::json!({}),
                process.current_context()
            )
            .await
            .unwrap()
            .content,
        "revision two"
    );

    tokio::time::timeout(Duration::from_secs(5), async {
        while !host.tool_definitions().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("live tool did not unregister");
    assert!(process.tool_definitions().is_empty());
    assert!(process.shutdown().await);
}

#[test]
fn semantic_ui_transport_rejects_terminal_controls_and_oversized_widgets() {
    assert!(ExtensionUiContribution::Status {
        key: "pi.status".into(),
        text: Some("ready".into()),
        style_role: Some("extension.pi.status".into()),
        priority: 0,
    }
    .validate()
    .is_ok());
    assert!(ExtensionUiContribution::Status {
        key: "pi.status".into(),
        text: Some("\u{1b}[31munsafe".into()),
        style_role: Some("extension.pi.status".into()),
        priority: 0,
    }
    .validate()
    .is_err());
    assert!(ExtensionUiContribution::Widget {
        key: "pi.widget".into(),
        lines: Some(vec!["row".into(); MAX_EXTENSION_UI_LINES + 1]),
        placement: ExtensionWidgetPlacement::AboveEditor,
        style_role: None,
        priority: 0,
    }
    .validate()
    .is_err());
}

#[test]
fn autocomplete_transport_requires_a_character_boundary_and_plain_choices() {
    assert!(ExtensionAutocompleteRequest {
        text: "é".into(),
        cursor: 1,
        revision: 1,
    }
    .validate()
    .is_err());
    assert!(ExtensionAutocompleteResponse {
        prefix: "@".into(),
        items: vec![ExtensionAutocompleteItem {
            value: "file".into(),
            label: "file".into(),
            description: Some("\u{1b}[31munsafe".into()),
            replace_after_bytes: None,
            cursor_offset_bytes: None,
        }],
    }
    .validate()
    .is_err());
}

#[test]
fn rendered_tool_transport_is_bounded_and_plain_text() {
    assert!(RenderedToolCall {
        segments: vec![ToolRenderSegment {
            text: "\u{1b}[31munsafe".into(),
            style_role: None,
        }],
    }
    .validate()
    .is_err());
    assert!(RenderedToolCall {
        segments: vec![
            ToolRenderSegment {
                text: "row".into(),
                style_role: None,
            };
            MAX_EXTENSION_UI_LINES.saturating_mul(4) + 1
        ],
    }
    .validate()
    .is_err());
}

fn write_manifest(directory: &Path, name: &str, description: &str) {
    std::fs::create_dir_all(directory).expect("create extension directory");
    std::fs::write(
        directory.join(EXTENSION_MANIFEST_FILENAME),
        format!(
            r#"name = "{name}"
version = "0.1.0"
api_version = "0.1"
description = "{description}"
[entrypoint]
command = "test"
"#
        ),
    )
    .expect("write manifest");
}

/// Windows has no executable bit; a script entrypoint is launched through
/// its interpreter instead (see `windows_script_launch`).
#[cfg(windows)]
pub(super) fn write_executable_script(path: &Path, source: &str) {
    std::fs::write(path, source).expect("write fixture");
}

#[cfg(windows)]
#[test]
fn windows_python_entrypoints_launch_through_an_interpreter() {
    let temp = TempDir::new().expect("tempdir");
    let script = temp.path().join("extension");
    std::fs::write(&script, "#!/usr/bin/env python3\nprint('ok')\n").expect("script");
    let (interpreter, arguments) = windows_script_launch(&script)
        .expect("the Windows runner provides Python 3")
        .expect("a python shebang is a script");
    let name = interpreter
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    assert!(
        ["py.exe", "python3.exe", "python.exe"].contains(&name.as_str()),
        "{interpreter:?}"
    );
    if name == "py.exe" {
        assert_eq!(arguments, vec![std::ffi::OsString::from("-3")]);
    }
    let native = temp.path().join("native.exe");
    std::fs::write(&native, b"MZ\x90\x00").expect("native fixture");
    assert!(windows_script_launch(&native).expect("inspect").is_none());
}

#[test]
fn python_script_detection_uses_the_extension_or_interpreter_line() {
    assert!(is_python_script(Path::new("extension.py"), b""));
    assert!(is_python_script(Path::new("EXTENSION.PY"), b""));
    assert!(is_python_script(
        Path::new("extension"),
        b"#!/usr/bin/env python3"
    ));
    assert!(is_python_script(
        Path::new("run"),
        b"#!C:\\Python312\\python.exe"
    ));
    assert!(!is_python_script(Path::new("extension.sh"), b"#!/bin/sh"));
    assert!(!is_python_script(Path::new("extension.exe"), b"MZ\x90\x00"));
    assert!(!is_python_script(
        Path::new("notes"),
        b"python is mentioned"
    ));
}

#[cfg(unix)]
pub(super) fn write_executable_script(path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, source).expect("write fixture");
    let mut permissions = std::fs::metadata(path).expect("metadata").permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions).expect("chmod");
}

#[cfg(unix)]
fn process_id_exists(pid: i32) -> bool {
    process_is_live_for_test(pid)
}

fn minimal_manifest(name: &str, command: &str) -> ExtensionManifest {
    ExtensionManifest::parse(&format!(
        r#"name = "{name}"
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "{command}"
"#
    ))
    .expect("minimal manifest")
}

pub(super) fn trusted_descriptor(
    directory: &Path,
    manifest: ExtensionManifest,
) -> DiscoveredExtension {
    DiscoveredExtension {
        manifest,
        manifest_path: directory.join(EXTENSION_MANIFEST_FILENAME),
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    }
}

/// POSIX shell fixture exercising the API `0.1` transport: tool
/// registration, notifications, a confirmation round trip, and shutdown.
#[cfg(unix)]
fn protocol_fixture_script() -> &'static str {
    r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.1","tools":[{"name":"echo","description":"Echo a value","parameters":{"type":"object","properties":{"text":{"type":"string"}}}}],"commands":[]}}'
IFS= read -r tool_call
printf '%s\n' '{"jsonrpc":"2.0","method":"notification","params":{"level":"info","message":"tool called"}}'
printf '%s\n' '{"jsonrpc":"2.0","id":"confirm-1","method":"confirmation/request","params":{"prompt":"Continue?","destructive":false,"default":false}}'
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"content":"from extension","is_error":false,"metadata":null,"structured_content":null}}'
IFS= read -r confirmation_response
IFS= read -r shutdown
case "$shutdown" in
  *'"method":"shutdown"'*) printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}' ;;
  *) exit 23 ;;
esac
"#
}
