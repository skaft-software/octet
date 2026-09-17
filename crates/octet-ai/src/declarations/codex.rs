//! Per-request Codex Responses transport declaration.
//!
//! Upstream Pi selects the Codex transport per request (`sse`, `websocket`,
//! `websocket-cached`, or `auto`), bounds the WebSocket connection attempt with
//! `websocketConnectTimeoutMs` (default 15 s), and records per-session debug
//! statistics. Octet's endpoint transport is declared data
//! ([`crate::EndpointTransport::WebSocketPreferred`]); this module owns the
//! *request-local* selection value, the pure resolution policy, the timeout
//! normalization and the debug-stats shape so the client does not have to
//! invent them.
//!
//! Everything here is pure data and policy. It performs no I/O and never
//! retries: a resolution only says which transport one attempt should use.

use serde::{Deserialize, Serialize};

use super::DeclarationError;
use crate::types::EndpointTransport;

/// Pi's default WebSocket connect deadline in milliseconds.
pub const DEFAULT_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 15_000;

/// Upper bound for a caller-supplied Codex connect deadline (five minutes).
pub const MAX_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS: u64 = 300_000;

/// Upper bound for one retained debug error string.
pub const MAX_CODEX_DEBUG_ERROR_BYTES: usize = 256;

/// Request-local Codex transport selection.
///
/// The serde spelling of every variant is the exact wire/config spelling
/// returned by [`CodexTransport::as_str`] and accepted by
/// [`CodexTransport::parse`] (`websocket`, `websocket-cached`), matching Pi's
/// `configuredTransport`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CodexTransport {
    /// Prefer the declared endpoint transport, with HTTP/SSE fallback.
    #[default]
    #[serde(rename = "auto")]
    Auto,
    /// Force ordinary HTTP/SSE.
    #[serde(rename = "sse")]
    Sse,
    /// Force the Responses WebSocket with the full local body.
    #[serde(rename = "websocket")]
    WebSocket,
    /// WebSocket with a connection-scoped cached continuation
    /// (`previous_response_id` delta) when the cached connection is available.
    #[serde(rename = "websocket-cached")]
    WebSocketCached,
}

impl CodexTransport {
    /// Exact wire/config spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Sse => "sse",
            Self::WebSocket => "websocket",
            Self::WebSocketCached => "websocket-cached",
        }
    }

    /// Parse an exact spelling; unknown values fail closed.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "sse" => Some(Self::Sse),
            "websocket" => Some(Self::WebSocket),
            "websocket-cached" => Some(Self::WebSocketCached),
            _ => None,
        }
    }

    /// Whether the selection permits a WebSocket attempt.
    pub const fn may_use_websocket(self) -> bool {
        !matches!(self, Self::Sse)
    }

    /// Whether the selection may send a cached continuation delta instead of
    /// the full local body.
    pub const fn uses_cached_context(self) -> bool {
        matches!(self, Self::Auto | Self::WebSocketCached)
    }
}

/// Why one attempt resolved to a transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexTransportReason {
    /// SSE was requested explicitly.
    SseRequested,
    /// The endpoint does not declare `WebSocketPreferred`.
    EndpointDeclaresHttp,
    /// The session already fell back to SSE after a WebSocket failure.
    SessionSseFallback,
    /// No cache session is available for a connection-scoped continuation.
    NoSession,
    /// The declared WebSocket transport is used.
    WebSocketDeclared,
}

/// The resolved transport for one attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodexTransportResolution {
    /// The selection exactly as requested, retained for transport-failure
    /// diagnostics (Pi reports `configuredTransport`).
    pub requested: CodexTransport,
    /// The concrete transport this attempt must use: [`CodexTransport::Sse`] or
    /// [`CodexTransport::WebSocket`]. The cached/full-body distinction lives in
    /// [`Self::cached_context`].
    pub transport: CodexTransport,
    /// Whether the attempt may send a cached continuation delta instead of the
    /// full local body.
    pub cached_context: bool,
    /// Why this transport was selected.
    pub reason: CodexTransportReason,
}

impl CodexTransportResolution {
    /// Whether the attempt must open the Responses WebSocket.
    pub const fn uses_websocket(self) -> bool {
        matches!(self.transport, CodexTransport::WebSocket)
    }
}

/// Resolve one request-local Codex transport selection.
///
/// `session_sse_fallback` is the session-scoped latch Pi sets after a
/// WebSocket failure; while it is set, any non-`sse` selection uses SSE (and
/// the client records an SSE fallback). `has_session` is whether a
/// cache session id exists; without one a cached continuation cannot be
/// addressed, so `websocket-cached`/`auto` still use the full body.
pub fn resolve_codex_transport(
    requested: CodexTransport,
    endpoint: EndpointTransport,
    session_sse_fallback: bool,
    has_session: bool,
) -> CodexTransportResolution {
    if requested == CodexTransport::Sse {
        return CodexTransportResolution {
            requested,
            transport: CodexTransport::Sse,
            cached_context: false,
            reason: CodexTransportReason::SseRequested,
        };
    }
    if endpoint != EndpointTransport::WebSocketPreferred {
        return CodexTransportResolution {
            requested,
            transport: CodexTransport::Sse,
            cached_context: false,
            reason: CodexTransportReason::EndpointDeclaresHttp,
        };
    }
    if session_sse_fallback {
        return CodexTransportResolution {
            requested,
            transport: CodexTransport::Sse,
            cached_context: false,
            reason: CodexTransportReason::SessionSseFallback,
        };
    }
    if !has_session && requested == CodexTransport::WebSocketCached {
        return CodexTransportResolution {
            requested,
            transport: CodexTransport::WebSocket,
            cached_context: false,
            reason: CodexTransportReason::NoSession,
        };
    }
    CodexTransportResolution {
        requested,
        transport: CodexTransport::WebSocket,
        cached_context: requested.uses_cached_context() && has_session,
        reason: CodexTransportReason::WebSocketDeclared,
    }
}

/// Normalize a caller-supplied timeout exactly like Pi's `normalizeTimeoutMs`:
/// absent stays absent, and a present value must be a finite, non-negative
/// integer below this module's ceiling (`0` disables the deadline).
pub fn normalize_codex_timeout_ms(value: Option<u64>) -> Result<Option<u64>, DeclarationError> {
    match value {
        None => Ok(None),
        Some(value) if value > MAX_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS => Err(
            DeclarationError::Invalid("Codex connect deadline exceeds its ceiling".into()),
        ),
        Some(value) => Ok(Some(value)),
    }
}

/// The connect deadline actually applied for one attempt.
pub fn effective_codex_connect_timeout_ms(requested: Option<u64>) -> Result<Option<u64>, DeclarationError> {
    Ok(match normalize_codex_timeout_ms(requested)? {
        // Pi passes `undefined` and lets the WebSocket layer use its own
        // 15-second default; making that default explicit keeps one number.
        None => Some(DEFAULT_CODEX_WEBSOCKET_CONNECT_TIMEOUT_MS),
        Some(0) => None,
        Some(value) => Some(value),
    })
}

/// Bounded, secret-free per-session Codex WebSocket debug statistics.
///
/// The field set mirrors Pi's `OpenAICodexWebSocketDebugStats`. Counters
/// saturate instead of overflowing, and the retained error string is bounded
/// and control-free: it is diagnostic text, never provider prose copied
/// unbounded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexWebSocketDebugStats {
    /// Total WebSocket-path requests attempted.
    pub requests: u64,
    /// Connections created.
    pub connections_created: u64,
    /// Connections reused.
    pub connections_reused: u64,
    /// Requests that were allowed to use a cached context delta.
    pub cached_context_requests: u64,
    /// Requests whose body carried `store: true`.
    pub store_true_requests: u64,
    /// Requests sent with the full local body.
    pub full_context_requests: u64,
    /// Requests sent as a cached continuation delta.
    pub delta_requests: u64,
    /// Input item count of the last request.
    pub last_input_items: u64,
    /// Input item count of the last delta request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_delta_input_items: Option<u64>,
    /// Continuation id of the last delta request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_previous_response_id: Option<String>,
    /// WebSocket failures observed.
    pub websocket_failures: u64,
    /// SSE fallbacks observed.
    pub sse_fallbacks: u64,
    /// Whether the session currently avoids the WebSocket path.
    pub websocket_fallback_active: bool,
    /// Bounded description of the last WebSocket failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_websocket_error: Option<String>,
}

impl CodexWebSocketDebugStats {
    /// Record one request attempt and its input/continuation shape.
    pub fn record_request(
        &mut self,
        reused_connection: bool,
        cached_context: bool,
        store_true: bool,
        input_items: usize,
        delta: Option<(&str, usize)>,
    ) {
        self.requests = self.requests.saturating_add(1);
        if reused_connection {
            self.connections_reused = self.connections_reused.saturating_add(1);
        } else {
            self.connections_created = self.connections_created.saturating_add(1);
        }
        if cached_context {
            self.cached_context_requests = self.cached_context_requests.saturating_add(1);
        }
        if store_true {
            self.store_true_requests = self.store_true_requests.saturating_add(1);
        }
        self.last_input_items = u64::try_from(input_items).unwrap_or(u64::MAX);
        match delta {
            Some((previous_response_id, items)) => {
                self.delta_requests = self.delta_requests.saturating_add(1);
                self.last_delta_input_items = Some(u64::try_from(items).unwrap_or(u64::MAX));
                self.last_previous_response_id = Some(bounded_debug_text(previous_response_id));
            }
            None => {
                self.full_context_requests = self.full_context_requests.saturating_add(1);
                self.last_delta_input_items = None;
                self.last_previous_response_id = None;
            }
        }
    }

    /// Latch a WebSocket failure: the session falls back to SSE and the
    /// failure text is retained bounded and control-free.
    pub fn record_websocket_failure(&mut self, error: &str) {
        self.websocket_failures = self.websocket_failures.saturating_add(1);
        self.websocket_fallback_active = true;
        self.last_websocket_error = Some(bounded_debug_text(error));
    }

    /// Record an SSE fallback for a session already latched to SSE.
    pub fn record_sse_fallback(&mut self) {
        self.sse_fallbacks = self.sse_fallbacks.saturating_add(1);
        self.websocket_fallback_active = true;
    }
}

fn bounded_debug_text(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_CODEX_DEBUG_ERROR_BYTES)
        .collect();
    cleaned
}

#[cfg(test)]
mod tests {
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
        assert_eq!(effective_codex_connect_timeout_ms(None).unwrap(), Some(15_000));
        assert_eq!(effective_codex_connect_timeout_ms(Some(0)).unwrap(), None);
        assert_eq!(effective_codex_connect_timeout_ms(Some(250)).unwrap(), Some(250));
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
        assert_eq!(stats.last_previous_response_id.as_deref(), Some("resp_prev"));
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
        assert!(serde_json::to_string(&stats).unwrap().contains("sse_fallbacks"));
    }
}
