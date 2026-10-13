//! Unit tests for `crate::rich_text::latex`.
//!
//! Covers render performance across nested math and table layouts.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::latex`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn repeated_relations_preserve_spacing() {
    let source = "x=".repeat(1024) + "y";
    assert_eq!(
        render_latex(&source, RenderLatexOptions::default()),
        Some("x = ".repeat(1024) + "y")
    );
}

#[test]
fn amplified_matrix_falls_back_before_padding() {
    let source = format!(
        "\\begin{{matrix}}{}{}\\end{{matrix}}",
        "x".repeat(4096),
        "\\\\y".repeat(128)
    );
    assert!(source.len() < super::super::markdown::MAX_DIAGRAM_FENCE_BYTES);
    assert_eq!(render_latex(&source, RenderLatexOptions::display()), None);
}
