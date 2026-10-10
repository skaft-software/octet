//! Unit tests for `crate::sanitize`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `sanitize.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::sanitize`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn hostile_terminal_controls_are_neutralized() {
    let input = concat!(
        "ok\x1b[31mred\x1b[0m",
        "\x1b]0;title\x07",
        "\x1b]52;c;Y2xpcA==\x07",
        "\x1b[2J\x1b[6n"
    );
    let safe = sanitize_text(input, SanitizeOptions::default());
    assert!(!safe.contains('\x1b'));
    assert!(!safe.contains('\x07'));
    assert!(safe.contains('␛'));
    assert!(safe.contains("title"));
}

#[test]
fn tabs_and_newlines_follow_component_policy() {
    let safe = sanitize_text("a\tb\r\nc", SanitizeOptions::default());
    assert_eq!(safe, "a\tb\nc");
    assert_eq!(sanitize_line("a\tb\nc", true), "a<U+0009>b<U+000A>c");
}

#[test]
fn hyperlinks_show_the_destination_and_reject_active_content() {
    let link = safe_hyperlink("docs", "https://example.com/a b", true, false);
    assert!(link.contains("docs (https://example.com/a b)"));
    assert!(link.contains("a%20b"));
    let malicious = safe_hyperlink("click", "javascript:alert(1)", true, false);
    assert_eq!(malicious, "click (javascript:alert(1))");
    assert!(!malicious.contains('\x1b'));
    let unicode = SafeUrl::parse("https://example.com/界").unwrap();
    assert_eq!(unicode.as_str(), "https://example.com/%E7%95%8C");
}
