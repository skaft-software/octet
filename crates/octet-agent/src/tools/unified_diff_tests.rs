//! Unified-diff parsing and rendering regressions for the built-in tools.
//!
//! The parse/apply/round-trip cases for the diff format are kept out of
//! `mod.rs` so this module can stay a pure re-export surface: the file is read
//! as the inventory of what the tool layer publishes, and test-only
//! machinery in the middle of that inventory hides items from it.
use super::*;

#[test]
fn workspace_confinement_failures_have_a_policy_code() {
    for path in ["/private/secret", "../secret", "~/secret"] {
        let error = validate_effect_path(path, false).unwrap_err();
        assert_eq!(
            error.policy_denial_code(),
            Some(ToolPolicyDenialCode::WorkspaceConfinement),
            "{path}"
        );
    }
}

#[test]
fn small_unified_diff_output_is_unchanged() {
    let diff = format_unified_diff("file.txt", "old", "new", "before\nold\nafter\n");
    assert_eq!(
        diff,
        "--- a/file.txt\n+++ b/file.txt\n@@ -1,3 +1,3 @@\n before\n-old\n+new\n after\n"
    );
}

#[test]
fn small_creation_diff_output_is_unchanged() {
    let content = (1..=11)
        .map(|line| format!("line-{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let diff = format_unified_creation_diff("file.txt", &content);
    assert_eq!(
        diff,
        "--- /dev/null\n+++ b/file.txt\n@@ -0,0 +1,11 @@\n+line-1\n+line-2\n+line-3\n+line-4\n+line-5\n+line-6\n+line-7\n+line-8\n+line-9\n+line-10\n… 1 more line\n"
    );
}

#[test]
fn multi_megabyte_single_line_diffs_are_bounded_and_utf8_safe() {
    let old = "🙂".repeat(600_000);
    let new = "界".repeat(800_000);

    let replacement = format_unified_diff("large.txt", &old, &new, &old);
    let creation = format_unified_creation_diff("large.txt", &new);
    assert!(replacement.contains("@@ -1,1 +1,1 @@"), "{replacement}");
    assert!(creation.contains("@@ -0,0 +1,1 @@"), "{creation}");

    for diff in [replacement, creation] {
        assert!(diff.len() <= MAX_UNIFIED_DIFF_BYTES, "{}", diff.len());
        assert!(std::str::from_utf8(diff.as_bytes()).is_ok());
        assert!(
            diff.contains(UNIFIED_DIFF_TRUNCATION_MARKER.trim()),
            "{diff}"
        );
    }
}
