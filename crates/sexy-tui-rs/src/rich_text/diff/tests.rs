//! Unit tests for `crate::rich_text::diff`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::diff`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn parses_headers_hunks_rows_renames_and_binary_notices() {
    let diff = UnifiedDiff::parse(
        "diff --git a/old b/new\nrename from old\nrename to new\n--- a/old\n+++ b/new\n@@ -2,2 +2,3 @@ fn x\n same\n-old\n+new\n+extra\nBinary files a/a.png and b/a.png differ",
    );
    assert_eq!(diff.lines[5].kind, DiffLineKind::HunkHeader);
    assert_eq!(diff.lines[6].old_number, Some(2));
    assert_eq!(diff.lines[7].old_number, Some(3));
    assert_eq!(diff.lines[8].new_number, Some(3));
    assert_eq!(diff.lines[9].new_number, Some(4));
    assert!(diff
        .lines
        .iter()
        .any(|line| line.kind == DiffLineKind::Binary));
    assert!(diff.lines.iter().any(|line| line.text == "rename to new"));
}

#[test]
fn incomplete_diff_is_safe() {
    let diff = UnifiedDiff::parse("@@ -1 +\n+partial\n\\ No newline at end");
    assert_eq!(diff.lines.len(), 3);
    assert_eq!(diff.lines[1].kind, DiffLineKind::Addition);
    assert!(diff.plain_text().contains("+partial"));
}
