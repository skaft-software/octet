//! Unit tests for `crate::deferred`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::deferred`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn permits_are_one_shot_and_generation_bound() {
    let mut permit = DeferredPollPermit::one("pass-1", 3);
    assert_eq!(permit.remaining(), 1);
    assert!(!permit.is_consumed());
    assert!(permit.consume(3).is_ok());
    assert!(permit.is_consumed());
    assert_eq!(
        permit.consume(3),
        Err(DeferredPollRefusalKind::AlreadyConsumed)
    );

    let mut stale = DeferredPollPermit::one("pass-1", 2);
    assert_eq!(
        stale.consume(3),
        Err(DeferredPollRefusalKind::StaleGeneration { permit: 2, leaf: 3 })
    );
    assert!(!stale.is_consumed());

    let mut none = DeferredPollPermit::none("pass-1", 3);
    assert_eq!(none.consume(3), Err(DeferredPollRefusalKind::NoPermit));
}

#[test]
fn handle_rejection_matches_identity_and_expiry() {
    let handle = DeferredHandle::new("anthropic", "claude", "anthropic-messages", "resp-1");
    assert_eq!(
        handle.rejection("anthropic", "claude", "anthropic-messages", 0),
        None
    );
    assert_eq!(
        DeferredHandle::new("anthropic", "claude", "anthropic-messages", "").rejection(
            "anthropic",
            "claude",
            "anthropic-messages",
            0
        ),
        Some(DeferredHandleRejection::EmptyId)
    );
    assert!(matches!(
        handle.rejection("openai", "claude", "anthropic-messages", 0),
        Some(DeferredHandleRejection::ForeignProvider { .. })
    ));
    assert!(matches!(
        handle.rejection("anthropic", "claude", "openai-chat", 0),
        Some(DeferredHandleRejection::ForeignApi { .. })
    ));
    assert!(matches!(
        handle.clone().with_expires_at_ms(10).rejection(
            "anthropic",
            "claude",
            "anthropic-messages",
            10
        ),
        Some(DeferredHandleRejection::Expired { .. })
    ));
}

#[test]
fn handle_debug_redacts_conversion_data() {
    let handle = DeferredHandle::new("anthropic", "claude", "anthropic-messages", "resp-1")
        .with_data(serde_json::json!({"secret": "sensitive"}));
    let debug = format!("{handle:?}");
    assert!(debug.contains("resp-1"));
    assert!(!debug.contains("sensitive"));
}
