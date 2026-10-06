use super::*;
use pretty_assertions::assert_eq;

#[test]
fn composer_history_dispatch_requires_feature_owner_and_bounds() {
    let (events, mut received) = broadcast::channel(16);
    let (state, mut frames) =
        protocol_read_state_for_test(ManifestContributions::default(), events);
    insert_test_parent(&state, 1, Some(test_resource_owner("session-a")));
    insert_test_parent(&state, 2, None);
    let params = serde_json::json!({"parent_request_id": 1, "entries": ["newer", "older"]});
    handle_protocol_line(&wave1_line(500, "composer/history", params.clone()), &state).unwrap();
    assert_eq!(
        wave1_error(&frames.try_recv().unwrap()),
        (-32601, "unsupported_feature".into())
    );
    wave1_negotiate(&state, &[EXTENSION_FEATURE_COMPOSER]);

    for (id, params, token) in [
        (
            501,
            serde_json::json!({"parent_request_id": 1, "entries": [], "extra": true}),
            "invalid_request",
        ),
        (
            502,
            serde_json::json!({"parent_request_id": 1, "entries": vec!["x"; 101]}),
            "bounds_exceeded",
        ),
        (
            503,
            serde_json::json!({"parent_request_id": 1, "entries": ["bad\u{1b}text"]}),
            "invalid_request",
        ),
        (
            504,
            serde_json::json!({"parent_request_id": 1, "entries": ["x".repeat(MAX_EXTENSION_COMPOSER_TEXT_BYTES), "y"]}),
            "bounds_exceeded",
        ),
        (
            505,
            serde_json::json!({"parent_request_id": 2, "entries": ["x"]}),
            "not_foreground_owner",
        ),
    ] {
        handle_protocol_line(&wave1_line(id, "composer/history", params), &state).unwrap();
        assert_eq!(wave1_error(&frames.try_recv().unwrap()).1, token);
        assert!(
            received.try_recv().is_err(),
            "invalid history reached the host"
        );
    }
    handle_protocol_line(&wave1_line(506, "composer/history", params), &state).unwrap();
    match received.try_recv().expect("history dispatched") {
        ExtensionEvent::ComposerRequested {
            owner,
            generation,
            operation,
            ..
        } => {
            assert_eq!(owner.unwrap().session_id, "session-a");
            assert_eq!(generation, 1);
            assert_eq!(
                serde_json::to_value(operation).unwrap(),
                serde_json::json!({"operation": "history", "entries": ["newer", "older"]})
            );
        }
        other => panic!("expected history, got {other:?}"),
    }
    assert!(
        frames.try_recv().is_err(),
        "native receipt is deferred to the frontend"
    );
}
