//! Tears down and reprojects the host's own view of the world.
//!
//! Covers the telemetry observer drain-then-close path and the `host_state`
//! projection that answers `session/manager` snapshots: cached-projection reuse,
//! refresh on a reasoning change, and the deliberate absence of model preset
//! headers from the projected host state.

use super::*;

#[tokio::test]
async fn telemetry_lifecycle_drains_and_closes_without_executable_processes() {
    let directory = tempfile::tempdir().unwrap();
    let observer =
        octet_agent::TelemetryObserver::new(directory.path().join("telemetry.jsonl"), "test")
            .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.set_telemetry(Some(observer.clone()));
    extensions.drain_background_updates();
    assert!(!observer.status().closed);
    extensions.release_binding().await;
    let status = observer.status();
    assert!(status.closed);
    assert_eq!(status.pending_records, 0);
    assert_eq!(status.rejected_records, 0);
    assert!(!status.drain_timed_out);
    assert!(extensions.telemetry.is_none());
    extensions.shutdown().await;
}

#[test]
fn initial_host_state_reuses_unchanged_projection_and_refreshes_on_reasoning_change() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let sessions = SessionStore::new(directory.path(), directory.path());
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let mut extensions = ExecutableExtensions::default();
    let mut initial = host_state(&session, &model, &ReasoningConfig::Off, &sessions);
    initial
        .active_skills
        .push(octet_agent::extension_process::ExtensionActiveSkill {
            id: "sentinel".into(),
            name: "sentinel".into(),
            version: None,
        });
    *extensions.host_state.lock().unwrap() = initial.clone();
    extensions.refresh_initial_host_state(&session, &model, &ReasoningConfig::Off, &sessions);
    assert_eq!(*extensions.host_state.lock().unwrap(), initial);
    extensions.refresh_initial_host_state(
        &session,
        &model,
        &ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        &sessions,
    );
    assert!(extensions
        .host_state
        .lock()
        .unwrap()
        .active_skills
        .is_empty());
}

#[test]
fn extension_host_state_does_not_project_model_preset_headers() {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let sessions = SessionStore::new(directory.path(), directory.path());
    let mut model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    Arc::make_mut(&mut model.spec).preset.headers.insert(
        "x-private-model-header".into(),
        "model-header-value-must-not-be-public".into(),
    );
    let projected = serde_json::to_value(host_state(
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
    ))
    .unwrap();
    assert_eq!(projected["model"], "gpt-4o-mini");
    let encoded = projected.to_string();
    for forbidden in [
        "preset",
        "headers",
        "x-private-model-header",
        "model-header-value-must-not-be-public",
    ] {
        assert!(!encoded.contains(forbidden), "{forbidden}");
    }
}
