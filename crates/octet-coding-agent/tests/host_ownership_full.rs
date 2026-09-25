//! Source-level ownership checks for the native host facade and its focused modules.

use std::collections::BTreeMap;

const FACADE: &str = include_str!("../src/host.rs");

fn source(path: &str) -> &'static str {
    match path {
        "protocol" => include_str!("../src/host/protocol.rs"),
        "framing" => include_str!("../src/host/framing.rs"),
        "transport" => include_str!("../src/host/transport.rs"),
        "routing" => include_str!("../src/host/routing.rs"),
        "policy" => include_str!("../src/host/policy.rs"),
        "run" => include_str!("../src/host/run.rs"),
        "events" => include_str!("../src/host/events.rs"),
        "media" => include_str!("../src/host/media.rs"),
        "sessions" => include_str!("../src/host/sessions.rs"),
        _ => unreachable!("the ownership map names every host child module"),
    }
}

#[test]
fn host_facade_retains_only_process_and_dispatch_ownership() {
    assert!(FACADE.contains("pub async fn run_stdio()"));
    assert!(FACADE.contains("framing::read_frame"));
    assert!(FACADE.contains("parse_request"));
    assert!(FACADE.contains("match request.command"));

    for declaration in [
        "mod events;",
        "mod framing;",
        "mod media;",
        "mod policy;",
        "mod protocol;",
        "mod routing;",
        "mod run;",
        "mod sessions;",
        "mod transport;",
    ] {
        assert!(FACADE.contains(declaration), "facade lost {declaration}");
    }

    for moved_definition in [
        "struct HostRequest",
        "enum HostCommand",
        "struct RunRequest",
        "fn parse_strict_json",
        "fn read_frame",
        "fn serialize_bounded",
        "fn host_config",
        "fn validate_run_request",
        "fn load_user_input",
        "fn session_selection",
        "fn translate(",
    ] {
        assert!(
            !FACADE.contains(moved_definition),
            "facade still owns moved definition {moved_definition}"
        );
    }
}

#[test]
fn each_boundary_has_a_single_named_owner() {
    let mut owners = BTreeMap::new();
    for (owner, definitions) in [
        (
            "protocol",
            &["struct HostRequest", "enum HostCommand", "fn parse_request"][..],
        ),
        (
            "framing",
            &["fn serialize_bounded", "async fn read_frame"][..],
        ),
        (
            "transport",
            &[
                "struct HostEvent",
                "struct Emitter",
                "fn cleanup_host_processes",
            ][..],
        ),
        ("routing", &["fn emit_hello", "fn emit_models"][..]),
        (
            "policy",
            &[
                "fn host_config",
                "fn validate_run_request",
                "fn register_inline_model",
            ][..],
        ),
        ("run", &["async fn run_request"][..]),
        ("events", &["async fn translate", "fn clip_text"][..]),
        (
            "media",
            &["fn load_user_input", "fn image_mime", "fn audio_format"][..],
        ),
        (
            "sessions",
            &[
                "fn session_selection",
                "fn seed_history",
                "fn valid_session_id",
            ][..],
        ),
    ] {
        for definition in definitions {
            let matches = [
                "protocol",
                "framing",
                "transport",
                "routing",
                "policy",
                "run",
                "events",
                "media",
                "sessions",
            ]
            .into_iter()
            .filter(|candidate| source(candidate).contains(definition))
            .collect::<Vec<_>>();
            assert_eq!(matches, vec![owner], "{definition} has the wrong owner");
            assert!(owners.insert(definition, owner).is_none());
        }
    }
}

#[test]
fn run_translates_with_disjoint_metadata_before_dropping_the_live_run() {
    let events = source("events");
    assert!(events.contains("endpoint_id: &str"));
    assert!(events.contains("model_id: &str"));
    assert!(!events.contains("crate::app"));
    assert!(!events.contains("app.agent"));

    let run = source("run");
    let prompt = run.find("app.agent.prompt(input).await").unwrap();
    let translation = run.find("events::translate(").unwrap();
    let translation_await = translation + run[translation..].find(".await?").unwrap();
    let arguments = &run[translation..translation_await];
    assert!(arguments.contains("&app.model.endpoint.id.0"));
    assert!(arguments.contains("&app.model.spec.id.0"));
    let release = run.find("drop(run)").unwrap();
    let settlement = run.find(".settle_turn(extension_turn, &outcome)").unwrap();
    assert!(prompt < translation);
    assert!(translation_await < release);
    assert!(release < settlement);
}

#[test]
fn host_children_do_not_reintroduce_protocol_orchestration_into_the_facade() {
    assert!(!FACADE.contains("crate::app::bootstrap"));
    assert!(!FACADE.contains("AsyncWriteExt"));
    assert!(!FACADE.contains("AgentEvent"));
    assert!(!FACADE.contains("ModelCatalog"));
    assert!(!FACADE.contains("PathBuf"));
}
