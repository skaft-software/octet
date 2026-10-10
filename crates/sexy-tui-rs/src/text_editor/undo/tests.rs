//! Unit tests for `crate::text_editor::undo`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::text_editor::undo`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn push_snapshots_a_clone() {
    let mut stack = UndoStack::new();
    let mut state = vec!["first".to_owned()];
    stack.push(&state);
    state[0] = "mutated".to_owned();
    assert_eq!(stack.pop().unwrap(), vec!["first".to_owned()]);
}

#[test]
fn owned_snapshot_keeps_its_allocation() {
    let mut stack = UndoStack::new();
    let state = String::from("detached");
    let allocation = state.as_ptr();
    stack.push_owned(state);
    let restored = stack.pop().unwrap();
    assert_eq!(restored.as_ptr(), allocation);
}

#[test]
fn pop_is_lifo_and_empties() {
    let mut stack = UndoStack::new();
    stack.push(&1u8);
    stack.push(&2u8);
    assert_eq!(stack.len(), 2);
    assert_eq!(stack.pop(), Some(2));
    assert_eq!(stack.pop(), Some(1));
    assert_eq!(stack.pop(), None);
    assert!(stack.is_empty());
}

#[test]
fn clear_drops_every_snapshot() {
    let mut stack = UndoStack::new();
    stack.push(&());
    stack.clear();
    assert_eq!(stack.len(), 0);
}

#[test]
fn oldest_snapshots_expire_under_both_budgets() {
    let mut stack = UndoStack::with_limits(2, 5);
    stack.push_owned_with_size("a", 2);
    stack.push_owned_with_size("b", 2);
    stack.push_owned_with_size("c", 3);
    assert_eq!(stack.retained_bytes(), 5);
    assert_eq!(stack.pop(), Some("c"));
    assert_eq!(stack.pop(), Some("b"));
    assert_eq!(stack.retained_bytes(), 0);
    stack.push_owned_with_size("a", 0);
    stack.push_owned_with_size("b", 0);
    stack.push_owned_with_size("c", 0);
    assert_eq!(stack.len(), 2);
    stack.push_owned_with_size("too big", 6);
    assert!(stack.is_empty());
    stack.push_owned_with_size("new", 1);
    stack.clear();
    assert_eq!(stack.retained_bytes(), 0);
}
