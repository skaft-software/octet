//! Rebuild and fork
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[cfg(any(unix, windows))]
#[test]
fn explicit_rebuild_reasoning_clears_a_persisted_legacy_pro_mode() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let base = app
        .catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut spec = (*base.spec).clone();
    spec.id = ModelId("rebuild-ultra-test".into());
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.max_effort = octet_ai::ReasoningEffort::Ultra;
    spec.capabilities.responses_lite = true;
    spec.capabilities.agent_delegation = Some(octet_ai::AgentDelegation::V2);
    app.catalog.register_model(spec).unwrap();

    let target = directory.path().join("legacy-pro-target.jsonl");
    let mut session = Session::create(&target).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("rebuild-ultra-test".into()),
            reasoning: Some("max".into()),
            reasoning_mode: Some("pro".into()),
        })
        .unwrap();
    drop(session);

    let rebuilt = rebuild_app(
        app,
        None,
        Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)),
        None,
        Some(SessionSelection::OpenExisting(target)),
    )
    .unwrap();
    assert_eq!(
        rebuilt.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert_eq!(rebuilt.reasoning_mode, ReasoningMode::Standard);
}

#[test]
fn rebuild_same_session_preserves_history_without_redundant_config_write() {
    use octet_ai::{Message, UserMessage, UserPart};

    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let entry = app
        .agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("keep me".into())],
        })))
        .unwrap();
    let path = app.agent.session().path().to_owned();
    let entries_before = app.agent.session().entries().len();
    let bytes_before = std::fs::metadata(&path).unwrap().len();
    let app = rebuild_app(app, None, None, None, None).unwrap();
    assert!(app.agent.session().entry(&entry).is_some());
    assert_eq!(app.agent.session().entries().len(), entries_before);
    assert_eq!(std::fs::metadata(path).unwrap().len(), bytes_before);
}

#[test]
fn rebuild_restores_the_target_sessions_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let app = fresh_app(directory.path());
    let target = directory.path().join("target.jsonl");
    let mut session = Session::create(&target).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("gpt-5.4-mini-responses".to_string()),
            reasoning: Some("medium".to_string()),
            reasoning_mode: None,
        })
        .unwrap();
    drop(session);

    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(target)),
    )
    .unwrap();
    assert_eq!(app.model.spec.id.0, "gpt-5.4-mini-responses");
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium)
    );
}

#[test]
fn rebuild_new_session_has_empty_context_and_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let app = fresh_app(directory.path());
    let new_path = directory.path().join("new.jsonl");
    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::CreateNew(new_path)),
    )
    .unwrap();
    assert!(app.agent.session().context().unwrap().is_empty());
    assert_eq!(app.agent.session().entries().len(), 1);
    assert!(matches!(
        app.agent.session().entries()[0].value,
        EntryValue::Config { .. }
    ));
}

#[test]
fn rebuild_validates_native_compaction_against_the_candidate_model() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), Some("gpt-5.4-mini-responses"));
    config.compaction.mode = CompactionMode::NativeResponses;
    let boot = bootstrap(config).unwrap();
    let launch = resolve_launch_print(&boot, "native-rebuild").unwrap();
    let app = build_app(boot, launch, "system".into()).unwrap();
    let chat = app.catalog.resolve(&ModelId("gpt-4o-mini".into())).unwrap();

    let error = match rebuild_app(app, Some(chat), None, None, None) {
        Ok(_) => panic!("candidate Chat model must fail native compaction validation"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("native Responses compaction requires an OpenAI Responses route"),
        "{error:#}"
    );
}

#[test]
fn rebuild_prevalidates_native_replay_before_replacing_the_agent() {
    let directory = tempfile::tempdir().unwrap();
    let boot = bootstrap(config(directory.path(), Some("gpt-5.4-mini-responses"))).unwrap();
    let launch = resolve_launch_print(&boot, "native-replay-rebuild").unwrap();
    let mut app = build_app(boot, launch, "system".into()).unwrap();
    app.agent
        .session_mut()
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("legacy prompt".into())],
            },
        )))
        .unwrap();
    app.agent
        .session_mut()
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("legacy answer".into())],
                model: app.model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
            },
        )))
        .unwrap();
    app.config.compaction.mode = CompactionMode::NativeResponses;

    let error = match rebuild_app(app, None, None, None, None) {
        Ok(_) => panic!("legacy Responses history must fail native replay prevalidation"),
        Err(error) => error,
    };
    assert!(
        error
            .to_string()
            .contains("requires complete route-affine opaque replay"),
        "{error:#}"
    );
}

#[test]
fn fork_launch_copies_the_source_head_and_records_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let session_root = directory.path().join("sessions");
    let store = SessionStore::new(&session_root, directory.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let source = store.new_path("source");
    let mut session = Session::create(&source).unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("source prompt".into())],
            },
        )))
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("source answer".into())],
                model: ModelId("test".into()),
                protocol: Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    let source_head = session.head().unwrap();
    drop(session);

    let destination = store.new_path("fork");
    let path = fork_session_into(&store, &source, destination).unwrap();
    let forked = Session::open_read_only(&path).unwrap();
    assert_eq!(forked.head(), Some(source_head.clone()));
    assert_eq!(forked.context().unwrap().len(), 2);
    let destination_id = path.file_stem().unwrap().to_str().unwrap();
    let metadata = store.load_metadata(destination_id).unwrap();
    assert_eq!(
        metadata.forked_from_session_id,
        source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    );
    assert_eq!(metadata.forked_from_entry_id, Some(source_head.0));
}

#[test]
fn resume_completes_a_narrowed_catalog_before_restoring_its_model() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());

    // A session on a route this launch never initialized.
    let target = directory.path().join("deferred-route-session.jsonl");
    let mut session = Session::create(&target).unwrap();
    session
        .append(EntryValue::Config {
            model: Some(DEEPSEEK_MODEL_ID.to_string()),
            reasoning: Some("high".to_string()),
            reasoning_mode: None,
        })
        .unwrap();
    drop(session);

    // Simulate the runtime narrowing a config-proven route produces: the
    // deferred provider's inventory is absent and the plan is not fleet. A
    // narrowed launch without this state resolves every built-in id, so test
    // catalogs must remove the deferred route's model by hand.
    app.readiness = CatalogReadiness::Routes(vec!["openai"]);
    let endpoint = EndpointId(crate::providers::DEEPSEEK.routes[0].endpoint_id.into());
    assert!(app
        .catalog
        .remove_model_if_endpoint(&ModelId(DEEPSEEK_MODEL_ID.into()), &endpoint));
    assert!(app
        .catalog
        .resolve(&ModelId(DEEPSEEK_MODEL_ID.into()))
        .is_err());

    // Before the fleet completion this resume failed with
    // `Unknown model: ModelId("deepseek-v4-pro")`.
    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(target)),
    )
    .unwrap();
    assert_eq!(app.model.spec.id.0, DEEPSEEK_MODEL_ID);
    assert_eq!(
        app.reasoning,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    assert!(
        app.catalog
            .resolve(&ModelId(DEEPSEEK_MODEL_ID.into()))
            .is_ok(),
        "the completed catalog must serve the restored route"
    );
    assert!(
        app.readiness.is_fleet(),
        "the completed plan must not re-discover on later rebuilds"
    );
}

#[test]
fn resume_still_fails_closed_when_the_completed_catalog_lacks_the_model() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fresh_app(directory.path());
    let target = directory.path().join("missing-route-session.jsonl");
    let mut session = Session::create(&target).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("provider-model-without-a-credential".to_string()),
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    drop(session);
    app.readiness = CatalogReadiness::Routes(vec!["openai"]);

    let error = match rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(target)),
    ) {
        Ok(_) => panic!("a model no route can serve must fail the resume closed"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("Unknown model"),
        "a model no route can serve keeps its resolution error: {error:#}"
    );
}
