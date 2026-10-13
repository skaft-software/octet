//! The `terminal/grant` arbitration the host hands to extensions.
//!
//! `ExecutableExtensions` may only lend one terminal to one instance at a time,
//! and a grant is only valid for the generation it was minted against. These
//! tests cover the happy path (a bounded grant plus the host size), the refusal
//! paths (a second acquire, a foreign or stale release), revocation of a dead or
//! reloaded holder, and the bound on minted grant ids.

use super::*;

fn terminal_holder(instance_id: &str, name: &str, generation: u64) -> TerminalHolder {
    TerminalHolder {
        owner: Some("session-owner".to_owned()),
        instance_id: instance_id.to_owned(),
        generation,
        name: name.to_owned(),
    }
}

#[test]
fn terminal_acquire_hands_out_a_bounded_grant_and_the_host_size() {
    let mut arbiter = TerminalGrantArbiter::default();
    let granted = arbiter
        .acquire(terminal_holder("instance-a", "fixture-a", 7), 120, 40)
        .expect("first acquire is granted");
    assert_eq!(granted.columns, 120);
    assert_eq!(granted.rows, 40);
    assert!(!granted.grant_id.is_empty());
    assert!(granted.grant_id.len() <= MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES);
    assert!(granted
        .grant_id
        .chars()
        .all(|character| !character.is_control()));
    assert_eq!(
        arbiter.active().map(|active| active.grant_id.as_str()),
        Some(granted.grant_id.as_str())
    );
}

#[test]
fn a_second_terminal_acquire_is_refused_with_a_typed_failure() {
    let mut arbiter = TerminalGrantArbiter::default();
    arbiter
        .acquire(terminal_holder("instance-a", "fixture-a", 7), 80, 24)
        .expect("first acquire is granted");
    let (failure, message) = arbiter
        .acquire(terminal_holder("instance-b", "fixture-b", 7), 80, 24)
        .expect_err("a second acquire must be refused");
    assert_eq!(failure, ExtensionRequestFailure::InvalidRequest);
    assert_eq!(failure.code(), -32602);
    assert!(message.contains("already granted"), "{message}");
    // The refusal changed nothing: the original holder still owns it.
    assert_eq!(
        arbiter.active().map(|active| active.holder.name.as_str()),
        Some("fixture-a")
    );
}

#[test]
fn a_foreign_or_stale_terminal_release_is_refused_with_a_typed_failure() {
    let mut arbiter = TerminalGrantArbiter::default();
    arbiter
        .acquire(terminal_holder("instance-a", "fixture-a", 7), 80, 24)
        .expect("first acquire is granted");

    // A different extension instance cannot release the live grant.
    let (failure, message) = arbiter
        .release(&terminal_holder("instance-b", "fixture-b", 7))
        .expect_err("a foreign release must be refused");
    assert_eq!(failure, ExtensionRequestFailure::InvalidRequest);
    assert!(message.contains("does not hold"), "{message}");

    // A holder that reloaded is a different generation of the same instance.
    let (failure, _) = arbiter
        .release(&terminal_holder("instance-a", "fixture-a", 8))
        .expect_err("a stale release must be refused");
    assert_eq!(failure, ExtensionRequestFailure::InvalidRequest);

    // A release with no live grant is refused too, never a silent success.
    let (failure, message) = TerminalGrantArbiter::default()
        .release(&terminal_holder("instance-a", "fixture-a", 7))
        .expect_err("a release without a grant must be refused");
    assert_eq!(failure, ExtensionRequestFailure::InvalidRequest);
    assert!(
        message.contains("no foreground terminal grant"),
        "{message}"
    );

    // The holder itself still releases cleanly.
    arbiter
        .release(&terminal_holder("instance-a", "fixture-a", 7))
        .expect("the holder releases its own grant");
    assert!(arbiter.active().is_none());
}

#[test]
fn a_dead_or_reloaded_holder_is_revoked_without_waiting_on_it() {
    let mut arbiter = TerminalGrantArbiter::default();
    arbiter
        .acquire(terminal_holder("instance-a", "fixture-a", 7), 80, 24)
        .expect("first acquire is granted");

    // Still valid: the host must not touch a live grant.
    assert!(arbiter.revoke_if(|_| true).is_none());
    assert!(arbiter.active().is_some());

    // Holder death: the host takes the grant back with no cooperation.
    let revoked = arbiter
        .revoke_if(|_| false)
        .expect("a dead holder is revoked");
    assert_eq!(revoked.holder.name, "fixture-a");
    assert!(arbiter.active().is_none());

    // Revoking again is a no-op rather than a second handback.
    assert!(arbiter.revoke_if(|_| false).is_none());
}

#[test]
fn minted_terminal_grant_ids_stay_bounded_for_a_long_instance_id() {
    let instance = "i".repeat(512);
    let minted = mint_terminal_grant_id(&instance);
    assert!(minted.len() <= MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES);
    assert_ne!(minted, mint_terminal_grant_id(&instance));
}
