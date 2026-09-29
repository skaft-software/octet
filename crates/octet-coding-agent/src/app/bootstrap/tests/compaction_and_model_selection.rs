//! Compaction and model selection
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn configured_compaction_model_is_resolved_into_the_agent() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), Some("gpt-4o-mini"));
    config.compaction.compact_model = Some(ModelId("gpt-4o-mini".into()));
    let boot = bootstrap(config).unwrap();
    let app = build_app(
        boot,
        LaunchSelection {
            model: ModelId("gpt-4o-mini".into()),
            session: SessionSelection::CreateNew(directory.path().join("session.jsonl")),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
        },
        "system".into(),
    )
    .unwrap();
    assert_eq!(
        app.agent
            .compaction_model()
            .map(|model| model.spec.id.0.as_str()),
        Some("gpt-4o-mini")
    );
}

#[test]
fn native_compaction_rejects_non_responses_and_route_mismatch() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path(), Some("gpt-4o-mini"));
    let boot = bootstrap(config.clone()).unwrap();
    let chat = boot
        .catalog
        .resolve(config.model.as_ref().unwrap())
        .unwrap();
    let error =
        validate_compaction_route(CompactionMode::NativeResponses, &chat, None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("requires an OpenAI Responses route"),
        "{error}"
    );

    let mut responses_spec = (*chat.spec).clone();
    responses_spec.protocol = Protocol::OpenAiResponses;
    let responses = Model {
        spec: Arc::new(responses_spec),
        endpoint: chat.endpoint.clone(),
    };
    validate_compaction_route(
        CompactionMode::NativeResponses,
        &responses,
        Some(&responses),
    )
    .unwrap();

    let mut other_spec = (*responses.spec).clone();
    other_spec.id = ModelId("other-responses-model".into());
    let other = Model {
        spec: Arc::new(other_spec),
        endpoint: responses.endpoint.clone(),
    };
    let error =
        validate_compaction_route(CompactionMode::NativeResponses, &responses, Some(&other))
            .unwrap_err();
    assert!(
        error.to_string().contains("exact route affinity"),
        "{error}"
    );
}

#[test]
fn model_resolution_has_cli_project_global_precedence() {
    let id = |value: &str| Some(ModelId(value.into()));
    assert_eq!(
        resolve_model_id(id("cli"), id("project"), id("global")),
        id("cli")
    );
    assert_eq!(
        resolve_model_id(None, id("project"), id("global")),
        id("project")
    );
    assert_eq!(resolve_model_id(None, None, id("global")), id("global"));
    assert_eq!(resolve_model_id(None, None, None), None);
}
