//! Unit tests for `crate::protocol`.
//!
//! Covers tool-call ID normalisation and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::{normalize_tool_call_id, normalize_tool_call_id_owned, MAX_TOOL_CALL_ID_LEN};

fn is_wire_valid(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TOOL_CALL_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[test]
fn already_valid_ids_are_untouched() {
    for id in ["call_abc123", "toolu_01A", "a", "AZ-_09"] {
        assert_eq!(normalize_tool_call_id(id), id);
    }
}

#[test]
fn already_valid_owned_id_keeps_its_string_allocation() {
    let id = String::from("call_abc123");
    let allocation = id.as_ptr();
    let normalized = normalize_tool_call_id_owned(id);
    assert_eq!(normalized, "call_abc123");
    assert_eq!(normalized.as_ptr(), allocation);
}

#[test]
fn long_responses_id_is_normalized_into_charset_and_length() {
    // OpenAI Responses `call_…|item_…` shape, over-length (Pi pi-ai.md).
    let raw = format!("call_{}|item_{}", "a".repeat(240), "b".repeat(240));
    let out = normalize_tool_call_id(&raw);
    assert!(
        is_wire_valid(&out),
        "normalized id must be wire-valid: {out}"
    );
    assert_ne!(out, raw);
}

#[test]
fn normalization_is_deterministic_so_call_and_result_pair() {
    // A call and its result share the same canonical id; the pure transform
    // must map both to the same wire id (design §11).
    let raw = "call_x|item_y/with:invalid.chars";
    assert_eq!(normalize_tool_call_id(raw), normalize_tool_call_id(raw));
}

#[test]
fn distinct_ids_do_not_collide_via_hash() {
    let a = normalize_tool_call_id(&"z".repeat(100));
    let b = normalize_tool_call_id(&"z".repeat(101));
    assert_ne!(a, b);
}
