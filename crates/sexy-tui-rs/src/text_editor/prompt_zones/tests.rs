//! Unit tests for `crate::text_editor::prompt_zones`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::text_editor::prompt_zones`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

const A_BEL: &str = "\x1b]133;A\x07";
const B_ST: &str = "\x1b]133;B\x1b\\";
const C_BEL: &str = "\x1b]133;C\x07";

#[test]
fn markers_are_parsed_at_line_start_only() {
    assert_eq!(
        zone_markers(&format!("{A_BEL}$ ls")),
        vec![PromptZone::PromptStart]
    );
    assert_eq!(zone_markers(B_ST), vec![PromptZone::CommandStart]);
    assert_eq!(
        zone_markers(&format!("{A_BEL}{B_ST}ready")),
        vec![PromptZone::PromptStart, PromptZone::CommandStart]
    );
    // Mid-line and malformed sequences are literal content.
    assert!(zone_markers("text \x1b]133;A\x07").is_empty());
    assert!(zone_markers("\x1b]133;D\x07").is_empty());
    assert!(zone_markers("\x1b]133;A").is_empty());
}

#[test]
fn stripping_removes_repeated_markers_without_touching_content() {
    assert_eq!(strip_zone_markers(&format!("{A_BEL}{C_BEL}hello")), "hello");
    assert_eq!(strip_zone_markers("plain"), "plain");
    assert_eq!(strip_zone_markers(""), "");
}

#[test]
fn scan_indexes_zone_rows_and_prompt_rows() {
    let rows = [
        format!("{A_BEL}{B_ST}prompt> hello"),
        "world".to_owned(),
        format!("{C_BEL}output"),
        format!("{A_BEL}{B_ST}prompt> again"),
    ];
    let zones = PromptZones::scan(&rows);
    assert_eq!(zones.prompt_rows(), &[0, 3]);
    assert!(zones.has_zone(0, PromptZone::CommandStart));
    assert!(!zones.has_zone(1, PromptZone::OutputStart));
    assert!(zones.has_zone(2, PromptZone::OutputStart));
}

#[test]
fn prompt_jumps_walk_semantic_prompts_in_both_directions() {
    let rows = [
        format!("{A_BEL}one"),
        "body".to_owned(),
        format!("{A_BEL}two"),
        "body".to_owned(),
        format!("{A_BEL}three"),
    ];
    let zones = PromptZones::scan(&rows);
    assert_eq!(zones.jump(3, false), Some(2));
    assert_eq!(zones.jump(3, true), Some(4));
    assert_eq!(zones.jump(0, false), None);
    assert_eq!(zones.jump(4, true), None);
    // A row that is itself a prompt never jumps to itself.
    assert_eq!(zones.jump(2, false), Some(0));
    assert_eq!(zones.jump(2, true), Some(4));
    assert_eq!(zones.prompt_at_or_before(3), Some(2));
    assert_eq!(zones.prompt_at_or_before(0), Some(0));
}

#[test]
fn empty_transcript_indexes_no_prompts() {
    let zones = PromptZones::scan(Vec::<String>::new());
    assert!(zones.prompt_rows().is_empty());
    assert_eq!(zones.jump(0, true), None);
    assert_eq!(zones.jump(0, false), None);
}
