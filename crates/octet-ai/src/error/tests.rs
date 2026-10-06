//! Unit tests for `crate::error`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::error`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use http::StatusCode;

#[test]
fn test_error_display_is_secret_free() {
    let err = AiError::Auth(AuthError::Resolve);
    let display = format!("{}", err);
    assert!(display.contains("Credential resolution failed"));
    assert!(!display.contains("Bearer "));

    let debug = format!("{:?}", err);
    assert!(debug.contains("Resolve"));
}

#[test]
fn test_environment_limit_errors_do_not_expose_values() {
    let sample = "oversized-secret-value";
    let config = ConfigError::EnvironmentValueTooLarge {
        var: "OCTET_API_KEY".to_owned(),
        max_bytes: 4096,
    };
    let auth = AuthError::EnvironmentValueTooLarge {
        var: "OCTET_API_KEY".to_owned(),
        max_bytes: 4096,
    };
    assert_eq!(
        config.to_string(),
        "Environment variable OCTET_API_KEY exceeds the 4096-byte limit"
    );
    assert_eq!(
        auth.to_string(),
        "Credential environment variable OCTET_API_KEY exceeds the 4096-byte limit"
    );
    assert!(!config.to_string().contains(sample));
    assert!(!auth.to_string().contains(sample));
}

#[test]
fn test_transport_error_preserves_phase() {
    let err = AiError::Transport(TransportError {
        phase: TransportPhase::ResponseHeaders,
        timeout: true,
        message: "Sanitized error message".to_string(),
    });
    let AiError::Transport(transport) = &err else {
        unreachable!()
    };
    assert_eq!(transport.phase, TransportPhase::ResponseHeaders);
    assert!(transport.timeout);
    let display = err.to_string();
    assert_eq!(display.matches("Transport error").count(), 1, "{display}");
    assert!(!display.contains("http://secret-url.com"));
}

#[test]
fn test_http_error_no_secret_headers() {
    let err = HttpError {
        status: StatusCode::UNAUTHORIZED,
        request_id: Some("req_123".to_string()),
        retry_after: None,
        provider_code: None,
        body_snippet: Some("Access denied".to_string()),
        retryable: false,
    };
    // Verify we don't have header fields in the struct
    let display = format!("{}", err);
    assert!(display.contains("401"));
    assert!(!display.contains("Authorization"));
}

#[test]
fn qualified_server_status_hint_keeps_permanent_veto_and_legacy_whitelist() {
    for status in 500..600 {
        let mut error = HttpError {
            status: StatusCode::from_u16(status).unwrap(),
            request_id: None,
            retry_after: None,
            provider_code: None,
            body_snippet: None,
            retryable: false,
        };
        assert!(error.is_transient_server_error(), "{status}");
        assert!(!error.is_safe_to_retry());
        error.retryable = true;
        if status == 520 {
            assert!(!error.is_safe_to_retry());
        }
        for code in [
            "invalid_prompt",
            "bio_policy",
            "cyber_policy",
            "misalignment_policy_violation",
            "insufficient_quota",
            "usage_not_included",
            "context_length_exceeded",
            "401",
            "server_is_overloaded",
            "slow_down",
        ] {
            error.provider_code = Some(code.into());
            assert!(!error.is_transient_server_error(), "{status} {code}");
            assert!(!error.is_safe_to_retry(), "{status} {code}");
        }
    }
}

#[test]
fn test_is_safe_to_retry() {
    let err_429 = HttpError {
        status: StatusCode::TOO_MANY_REQUESTS,
        request_id: None,
        retry_after: None,
        provider_code: None,
        body_snippet: None,
        retryable: true,
    };
    assert!(err_429.is_safe_to_retry());

    for status in [
        StatusCode::REQUEST_TIMEOUT,
        StatusCode::BAD_GATEWAY,
        StatusCode::GATEWAY_TIMEOUT,
    ] {
        let transient = HttpError {
            status,
            request_id: None,
            retry_after: None,
            provider_code: None,
            body_snippet: None,
            retryable: true,
        };
        assert!(transient.is_safe_to_retry(), "{status} should be retryable");
    }

    let err_500 = HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        request_id: None,
        retry_after: None,
        provider_code: None,
        body_snippet: None,
        retryable: true,
    };
    assert!(err_500.is_safe_to_retry());
}

#[test]
fn test_stream_failure_display_delegates_to_inner() {
    let err = AiError::StreamFailure {
        inner: Box::new(AiError::Decode(DecodeError::Json(
            "unterminated string at line 1 column 20".into(),
        ))),
        progress: StreamProgress {
            provider_events: 412,
            decoded_events: 38,
            content_bytes: 18204,
            buffered_bytes: 96,
            first_body_seen: true,
            elapsed_ms: 97321,
            last_event_ms: Some(97_000),
        },
    };
    // Display delegates to the inner error so existing log lines stay
    // stable; the progress data travels in the struct, not the text.
    assert_eq!(
        err.to_string(),
        "Decode error: JSON decode error: unterminated string at line 1 column 20"
    );
}

#[test]
fn test_stream_progress_serde_roundtrip() {
    let progress = StreamProgress {
        provider_events: 7,
        decoded_events: 3,
        content_bytes: 100,
        buffered_bytes: 4,
        first_body_seen: true,
        elapsed_ms: 1200,
        last_event_ms: Some(1_100),
    };
    let json = serde_json::to_string(&progress).unwrap();
    let back: StreamProgress = serde_json::from_str(&json).unwrap();
    assert_eq!(progress, back);
}
