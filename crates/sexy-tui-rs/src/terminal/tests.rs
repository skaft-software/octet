//! Unit tests for `crate::terminal`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `terminal.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::terminal`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn pi_keyboard_negotiation_and_apple_return_normalization() {
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?7u"),
        Some(KeyboardProtocolNegotiationSequence::KittyFlags(7))
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?62;4;52c"),
        Some(KeyboardProtocolNegotiationSequence::DeviceAttributes)
    );
    assert_eq!(
        normalize_apple_terminal_input("\r", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
    assert_eq!(normalize_apple_terminal_input("a", true, true), "a");
}

#[test]
fn process_terminal_emits_text_and_paste_semantically() {
    assert_eq!(
        input_from_key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)),
        TerminalInput::Text("A".into())
    );
    assert_eq!(
        input_from_key(KeyEvent::new(KeyCode::Char('é'), KeyModifiers::ALT)),
        TerminalInput::Text("é".into())
    );
    assert_eq!(
        input_from_key(KeyEvent::new(
            KeyCode::Char('€'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        )),
        TerminalInput::Text("€".into())
    );
    assert_eq!(
        input_from_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        TerminalInput::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
    );
    assert_eq!(
        TerminalInput::Paste("one\r\ntwo".into()).legacy_data(),
        Some("one\r\ntwo".into())
    );
}

#[test]
fn process_terminal_repeats_only_editing_and_navigation_keys() {
    let key = |code, modifiers, kind| KeyEvent::new_with_kind(code, modifiers, kind);
    assert!(forwards_key_event(&key(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Press,
    )));
    assert!(forwards_key_event(&key(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )));
    assert!(forwards_key_event(&key(
        KeyCode::Left,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )));
    assert!(!forwards_key_event(&key(
        KeyCode::Enter,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )));
    assert!(!forwards_key_event(&key(
        KeyCode::Char('o'),
        KeyModifiers::CONTROL,
        KeyEventKind::Repeat,
    )));
    assert!(!forwards_key_event(&key(
        KeyCode::Backspace,
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )));
}
