//! Unit tests for `crate::faux`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::faux`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn deferred_status_requires_an_owned_handle() {
    let provider = FauxProvider::new(FauxOptions {
        pending_fetches: 1,
        ..FauxOptions::default()
    });
    provider.set_responses(vec![
        FauxResponse::Message(FauxMessage::new("ready")),
        FauxResponse::Failure("boom".to_owned()),
    ]);
    assert_eq!(provider.pending_response_count(), 2);

    // No deferred submission happened yet, so no handle is known.
    let handle = DeferredHandle::new("faux", "faux-1", "faux", "faux-call-1");
    assert_eq!(provider.deferred_status(&handle), None);

    let state = provider.state();
    assert_eq!(state.call_count, 0);
    assert_eq!(state.deferred_submission_count, 0);
    assert_eq!(state.deferred_fetch_count, 0);
    assert!(state.cancelled_deferred.is_empty());
}
