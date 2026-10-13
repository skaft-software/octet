//! Unit tests for `crate::declarations::codex`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `codex.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations::codex`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn selection_spellings_round_trip_and_unknown_values_fail_closed() {
    for (wire, transport) in [
        ("auto", CodexTransport::Auto),
        ("sse", CodexTransport::Sse),
        ("websocket", CodexTransport::WebSocket),
        ("websocket-cached", CodexTransport::WebSocketCached),
    ] {
        assert_eq!(CodexTransport::parse(wire), Some(transport));
        assert_eq!(transport.as_str(), wire);
        assert_eq!(
            serde_json::to_value(transport).unwrap(),
            serde_json::json!(wire)
        );
    }
    assert_eq!(CodexTransport::parse("ws"), None);
    assert_eq!(CodexTransport::default(), CodexTransport::Auto);
}

#[test]
fn resolution_mirrors_the_declared_transport_and_session_latch() {
    let ws = EndpointTransport::WebSocketPreferred;
    let http = EndpointTransport::Http;
    // Explicit SSE always wins.
    let resolved = resolve_codex_transport(CodexTransport::Sse, ws, false, true);
    assert_eq!(resolved.transport, CodexTransport::Sse);
    assert!(!resolved.uses_websocket());
    // An HTTP endpoint never opens a socket, even for `websocket`.
    let resolved = resolve_codex_transport(CodexTransport::WebSocket, http, false, true);
    assert_eq!(resolved.reason, CodexTransportReason::EndpointDeclaresHttp);
    assert!(!resolved.cached_context);
    // A latched session stays on SSE.
    let resolved = resolve_codex_transport(CodexTransport::Auto, ws, true, true);
    assert_eq!(resolved.reason, CodexTransportReason::SessionSseFallback);
    // Cached continuation needs a session id.
    let resolved = resolve_codex_transport(CodexTransport::WebSocketCached, ws, false, false);
    assert_eq!(resolved.transport, CodexTransport::WebSocket);
    assert!(!resolved.cached_context);
    assert_eq!(resolved.reason, CodexTransportReason::NoSession);
    // Auto uses the cached context only with a session, and always keeps
    // the full-body fallback available.
    let resolved = resolve_codex_transport(CodexTransport::Auto, ws, false, true);
    assert!(resolved.uses_websocket());
    assert!(resolved.cached_context);
    assert_eq!(resolved.transport, CodexTransport::WebSocket);
    assert_eq!(resolved.requested, CodexTransport::Auto);
    let resolved = resolve_codex_transport(CodexTransport::Auto, ws, false, false);
    assert!(resolved.uses_websocket());
    assert!(!resolved.cached_context);
}

#[test]
fn connect_deadlines_match_pis_normalization() {
    assert_eq!(normalize_codex_timeout_ms(None).unwrap(), None);
    assert_eq!(normalize_codex_timeout_ms(Some(0)).unwrap(), Some(0));
    assert_eq!(
        effective_codex_connect_timeout_ms(None).unwrap(),
        Some(15_000)
    );
    assert_eq!(effective_codex_connect_timeout_ms(Some(0)).unwrap(), None);
    assert_eq!(
        effective_codex_connect_timeout_ms(Some(250)).unwrap(),
        Some(250)
    );
    assert!(effective_codex_connect_timeout_ms(Some(u64::MAX)).is_err());
}

#[test]
fn debug_stats_are_bounded_and_distinguish_full_from_delta_requests() {
    let mut stats = CodexWebSocketDebugStats::default();
    stats.record_request(false, true, false, 9, None);
    assert_eq!(stats.requests, 1);
    assert_eq!(stats.connections_created, 1);
    assert_eq!(stats.full_context_requests, 1);
    assert_eq!(stats.last_input_items, 9);
    stats.record_request(true, true, true, 3, Some(("resp_prev", 3)));
    assert_eq!(stats.connections_reused, 1);
    assert_eq!(stats.delta_requests, 1);
    assert_eq!(
        stats.last_previous_response_id.as_deref(),
        Some("resp_prev")
    );
    assert_eq!(stats.store_true_requests, 1);
    stats.record_websocket_failure("socket\nreset\u{7}after timeout");
    assert_eq!(stats.websocket_failures, 1);
    assert!(stats.websocket_fallback_active);
    assert_eq!(
        stats.last_websocket_error.as_deref(),
        Some("socketresetafter timeout")
    );
    stats.record_sse_fallback();
    assert_eq!(stats.sse_fallbacks, 1);
    assert!(serde_json::to_string(&stats)
        .unwrap()
        .contains("sse_fallbacks"));
}
