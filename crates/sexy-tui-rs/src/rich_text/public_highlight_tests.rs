//! Unit tests for `crate::rich_text`.
//!
//! Covers the public syntax-highlighting entry points.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn public_highlighter_limits_and_unknown_language_fall_back() {
    assert!(highlight_code(&"x".repeat(64 * 1024 + 1), "rust").is_none());
    assert!(highlight_code(&"x".repeat(8 * 1024 + 1), "rust").is_none());
    assert!(highlight_code(&"x\n".repeat(4096), "rust").is_none());
    assert!(highlight_code("x", &"x".repeat(129)).is_none());
    assert!(highlight_code("<script>", "not-a-language").is_none());
}

#[test]
fn public_highlighter_is_semantic_and_preserves_unescaped_source() {
    let source = "// <script>&\nlet x = 3;\n";
    let output = highlight_code(source, "rust");
    #[cfg(feature = "syntax-highlighting")]
    {
        let output = output.unwrap();
        assert_eq!(
            output
                .iter()
                .map(|line| line.iter().map(|r| r.text.as_str()).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n"),
            source
        );
        assert!(output
            .iter()
            .flatten()
            .any(|r| r.role == Some(TextRole::SyntaxComment)));
    }
    #[cfg(not(feature = "syntax-highlighting"))]
    assert!(output.is_none());
}
