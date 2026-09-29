//! Fixtures shared by more than one area of the serve test suite.
//!
//! Building an `OctetHost` or a `WorkerPlan` by hand is the setup for roughly half
//! the suite, and every field of that plan is a private host detail. Keeping the
//! construction here means a change to the plan's shape surfaces as one compile
//! error in one file instead of silently drifting across a dozen test modules.

use super::*;
use octet_ai::{AssistantMessage, Protocol, UserMessage};
use std::sync::atomic::AtomicBool;

pub(super) fn serve_test_config(directory: &Path) -> Config {
    Config {
        workspace: directory.to_path_buf(),
        invocation_cwd: directory.to_path_buf(),
        model: None,
        model_explicit: false,
        system_prompt: None,
        reasoning: None,
        reasoning_explicit: false,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: false,
        cache_retention: octet_ai::CacheRetention::Short,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox: crate::config::SandboxPolicy::default(),
        theme: None,
        theme_paths: Vec::new(),
        color: crate::config::ColorMode::Auto,
        mouse: crate::config::MouseMode::Auto,
        plain: false,
        show_images: false,
        session_dir: directory.join("sessions"),
        compaction: crate::config::CompactionPolicy::default(),
        max_cost_microdollars: None,
        cost_warning_microdollars: None,
        max_turns: Some(40),
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: Vec::new(),
        mode: crate::config::Mode::Print {
            prompt: "test".into(),
        },
        resume: crate::config::ResumeSelector::New,
        skill_paths: Vec::new(),
        extension_paths: Vec::new(),
        enabled_extensions: Vec::new(),
        extension_activation_overridden: false,
        trusted_extensions: Vec::new(),
        invocation_trusted_extensions: Vec::new(),
        experimental_streamable_http_mcp: false,
        extension_flag_values: Default::default(),
        tools: crate::config::ToolPolicy::default(),
        telemetry: None,
        context_files: false,
        offline: true,
        workspace_trusted: true,
    }
}

pub(super) fn project_test_config(directory: &Path, trusted: bool) -> Config {
    let workspace = directory.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = serve_test_config(&workspace);
    config.session_dir = directory.join("sessions");
    config.workspace_trusted = trusted;
    config
}

pub(super) fn pull_request_worker_plan(directory: &Path, session_name: &str) -> WorkerPlan {
    let workspace = directory.join("workspace");
    let session_dir = directory.join("sessions");
    let state_dir = directory.join("state");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::create_dir_all(&session_dir).unwrap();
    std::fs::create_dir_all(&state_dir).unwrap();
    let mut config = serve_test_config(&workspace);
    config.workspace = workspace.clone();
    config.invocation_cwd = workspace.clone();
    config.session_dir = session_dir.clone();
    let session_id = SessionId::new(session_name).unwrap();
    WorkerPlan {
        config,
        sessions: SessionStore::new(&session_dir, &workspace),
        launch: LaunchSelection {
            model: ModelId("test-model".into()),
            session: SessionSelection::CreateNew(session_dir.join(format!("{session_name}.jsonl"))),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        prepared_session: Mutex::new(None),
        authority: AuthorityProfile::FullAccess,
        available_models: Vec::new(),
        actor_generation: 1,
        session_id,
        project_id: None,
        attachments: None,
        documents: None,
        projects: Arc::new(Mutex::new(
            ProjectRegistry::open(state_dir.join("projects")).unwrap(),
        )),
        trusted_files: Arc::new(Mutex::new(HashMap::new())),
        search_index: Arc::new(Mutex::new(TranscriptSearchIndex::new())),
        resources: None,
        goal_store: None,
        usage: Arc::new(Mutex::new(InferenceRequestStore::open(&state_dir).unwrap())),
        pull_requests: Arc::new(Mutex::new(PullRequestStore::open(&state_dir).unwrap())),
        pull_request_projection: Arc::new(Mutex::new(None)),
        pull_request_discovery_enabled: Arc::new(AtomicBool::new(false)),
        pull_request_refresh_requested: Arc::new(tokio::sync::Notify::new()),
        checkout_hooks: CheckoutTestHooks::default(),
    }
}

pub(super) fn stored_pull_request(
    session_id: &SessionId,
    number: u64,
    state: PullRequestState,
) -> StoredPullRequest {
    StoredPullRequest {
        session_id: session_id.as_str().to_owned(),
        url: format!("https://github.com/skaft-software/ygg/pull/{number}"),
        number,
        state,
        refreshed_at_ms: 1_750_000_000_000,
    }
}

pub(super) fn worker_checkout_fixture(
    directory: &Path,
    session_name: &str,
) -> (
    OctetHost,
    SessionId,
    DurableEntryId,
    DurableEntryId,
    PathBuf,
) {
    let mut config = serve_test_config(directory);
    let workspace = directory.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    config.workspace = workspace.clone();
    config.invocation_cwd = workspace;
    config.model = Some(ModelId("gpt-4o-mini".into()));
    config.model_explicit = true;
    let host = OctetHost::new(config).unwrap();
    let session_id = SessionId::new(session_name).unwrap();
    let context = host.project_context(Some(&host.launch_project_id)).unwrap();
    std::fs::create_dir_all(context.sessions.dir()).unwrap();
    let path = context.sessions.dir().join(format!("{session_name}.jsonl"));
    host.projects
        .lock()
        .unwrap()
        .bind_session(
            session_name,
            &registry_project_id(&host.launch_project_id).unwrap(),
        )
        .unwrap();
    let mut session = Session::create(&path).unwrap();
    let root = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("root prompt".into())],
        })))
        .unwrap();
    let old_head = session
        .append(EntryValue::Config {
            model: Some("gpt-4o-mini".into()),
            reasoning: Some("off".into()),
            reasoning_mode: Some("standard".into()),
        })
        .unwrap();
    session.checkout(root).unwrap();
    let target = session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("alternate prompt".into())],
        })))
        .unwrap();
    session.checkout(old_head.clone()).unwrap();
    (
        host,
        session_id,
        DurableEntryId::new(old_head.0).unwrap(),
        DurableEntryId::new(target.0).unwrap(),
        path,
    )
}

pub(super) fn png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&13u32.to_be_bytes());
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"IEND");
    bytes.extend_from_slice(&[0, 0, 0, 0]);
    bytes
}

pub(super) fn session_with_successful_tool_result(
    path: &Path,
    call_id: &str,
    name: &str,
    arguments: serde_json::Value,
    output: &str,
) -> Session {
    let mut session = Session::create(path).unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId(call_id.to_owned()),
                name: name.to_owned(),
                arguments_json: serde_json::to_string(&arguments).unwrap(),
                argument_error: None,
            })],
            model: ModelId("test-model".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(octet_ai::ToolResult {
                tool_call_id: ToolCallId(call_id.to_owned()),
                content: vec![ToolResultPart::Text(output.to_owned())],
                is_error: false,
                added_tool_names: None,
            })],
        })))
        .unwrap();
    session
}

pub(super) fn projected_tool(
    workspace: &Path,
    name: &str,
    arguments: serde_json::Value,
) -> ProjectedToolCall {
    ProjectedToolCall {
        name: name.into(),
        activity: semantic_tool_activity(name, &arguments, workspace, 1),
        arguments,
        result: None,
        turn_id: TurnId::new("turn-test").unwrap(),
    }
}
