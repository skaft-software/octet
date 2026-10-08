//! Unit tests for `crate::rich_text::highlight`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::highlight`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn requested_languages_load_and_unknown_is_plain() {
    for language in [
        "rust",
        "typescript",
        "javascript",
        "json",
        "toml",
        "yaml",
        "markdown",
        "bash",
        "python",
        "c",
        "c++",
        "diff",
    ] {
        assert!(
            highlight("let value = 1;\n", language).is_some(),
            "{language}"
        );
    }
    assert!(highlight("text", "definitely-unknown-language").is_none());
}

#[test]
fn rust_scopes_map_to_semantic_roles() {
    let lines = highlight("// note\nlet value = \"text\";\n", "rust").unwrap();
    assert!(lines
        .iter()
        .flatten()
        .any(|region| region.role == Some(TextRole::SyntaxComment)));
    assert!(lines
        .iter()
        .flatten()
        .any(|region| region.role == Some(TextRole::SyntaxKeyword)));
    assert!(lines
        .iter()
        .flatten()
        .any(|region| region.role == Some(TextRole::SyntaxString)));
}
