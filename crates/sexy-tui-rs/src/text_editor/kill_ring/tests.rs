//! Unit tests for `crate::text_editor::kill_ring`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::text_editor::kill_ring`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn empty_kill_is_ignored() {
    let mut ring = KillRing::new();
    ring.push("", true, false);
    assert!(ring.is_empty());
}

#[test]
fn accumulate_merges_and_orders_by_direction() {
    let mut ring = KillRing::new();
    ring.push("end", false, false);
    ring.push("-of-line", false, true);
    assert_eq!(ring.peek(), Some("end-of-line"));
    ring.push("start", true, false);
    ring.push("word ", true, true);
    assert_eq!(ring.peek(), Some("word start"));
    assert_eq!(ring.len(), 2);
}

#[test]
fn rotate_cycles_newest_to_oldest() {
    let mut ring = KillRing::new();
    ring.push("one", false, false);
    ring.push("two", false, false);
    ring.push("three", false, false);
    assert_eq!(ring.peek(), Some("three"));
    ring.rotate();
    assert_eq!(ring.peek(), Some("two"));
    ring.rotate();
    assert_eq!(ring.peek(), Some("one"));
    ring.rotate();
    assert_eq!(ring.peek(), Some("three"));
}

#[test]
fn rotate_leaves_single_entry_alone() {
    let mut ring = KillRing::new();
    ring.push("only", false, false);
    ring.rotate();
    assert_eq!(ring.peek(), Some("only"));
}

#[test]
fn first_accumulating_push_still_creates_an_entry() {
    let mut ring = KillRing::new();
    ring.push("fresh", true, true);
    assert_eq!(ring.peek(), Some("fresh"));
}
