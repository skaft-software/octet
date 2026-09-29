//! Unit tests for `crate::glyphs`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `glyphs.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::glyphs`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn plain_mode_never_requires_unicode_or_icon_fonts() {
    let glyphs = GlyphSet::for_capabilities(TerminalCapabilities::plain());
    assert_eq!(glyphs.vertical, "|");
    assert_eq!(glyphs.branch, "|-");
    assert!(glyphs.ellipsis.is_ascii());
}
