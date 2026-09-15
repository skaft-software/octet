use super::*;
use crate::tui::terminal::ColorMode as FacadeColorMode;

#[test]
fn color_mode_facade_keeps_explicit_policy_values() {
    assert_eq!(FacadeColorMode::parse("auto").unwrap(), ColorMode::Auto);
    assert_eq!(FacadeColorMode::parse("always").unwrap(), ColorMode::Always);
    assert_eq!(FacadeColorMode::parse("on").unwrap(), ColorMode::Always);
    assert_eq!(FacadeColorMode::parse("never").unwrap(), ColorMode::Never);
    assert_eq!(FacadeColorMode::parse("off").unwrap(), ColorMode::Never);
    assert!(FacadeColorMode::parse("maybe").is_err());
}

fn probe(term: Option<&str>) -> CapabilityProbe {
    CapabilityProbe {
        stdin_tty: true,
        stdout_tty: true,
        term: term.map(str::to_owned),
        term_program: None,
        colorterm: None,
        locale: Some("en_US.UTF-8".into()),
        no_color: false,
        explicit_plain: false,
    }
}

#[test]
fn redirected_dumb_and_unknown_terminals_are_plain() {
    let mut redirected = probe(Some("xterm-256color"));
    redirected.stdout_tty = false;
    let caps = TerminalCapabilities::from_probe(ColorMode::Auto, redirected);
    assert!(!caps.interactive);
    assert_eq!(caps.color, ColorDepth::None);

    let dumb = TerminalCapabilities::from_probe(ColorMode::Always, probe(Some("dumb")));
    assert!(!dumb.interactive);
    assert!(!dumb.unicode);
    assert_eq!(dumb.color, ColorDepth::None);

    let unknown = TerminalCapabilities::from_probe(ColorMode::Auto, probe(None));
    assert!(!unknown.interactive);
    assert!(!unknown.unicode);
    assert_eq!(unknown.color, ColorDepth::None);

    for name in ["unknown", "mystery-terminal"] {
        let unknown = TerminalCapabilities::from_probe(ColorMode::Auto, probe(Some(name)));
        assert!(!unknown.interactive, "{name}");
        assert!(!unknown.unicode, "{name}");
        assert_eq!(unknown.color, ColorDepth::None, "{name}");
    }
}

#[test]
fn apple_terminal_supports_italics_without_hyperlinks() {
    let mut apple = probe(Some("xterm-256color"));
    apple.term_program = Some("Apple_Terminal".into());
    let caps = TerminalCapabilities::from_probe(ColorMode::Auto, apple);
    assert!(caps.italics);
    assert!(!caps.hyperlinks);
}

#[test]
fn capability_detection_degrades_truecolour_to_256_and_16() {
    let mut rich = probe(Some("xterm-256color"));
    rich.colorterm = Some("truecolor".into());
    assert_eq!(
        TerminalCapabilities::from_probe(ColorMode::Auto, rich).color,
        ColorDepth::TrueColor
    );
    assert_eq!(
        TerminalCapabilities::from_probe(ColorMode::Auto, probe(Some("screen-256color"))).color,
        ColorDepth::Ansi256
    );
    assert_eq!(
        TerminalCapabilities::from_probe(ColorMode::Auto, probe(Some("xterm"))).color,
        ColorDepth::Ansi16
    );
}

#[test]
fn ssh_does_not_downgrade_term_or_colorterm_colour_depth() {
    // SSH does not participate in this mapping: the remote TERM and COLORTERM
    // values are the negotiated terminal contract.
    let mut ansi256 = probe(Some("xterm-256color"));
    assert_eq!(
        TerminalCapabilities::from_probe(ColorMode::Auto, ansi256.clone()).color,
        ColorDepth::Ansi256
    );
    ansi256.colorterm = Some("truecolor".into());
    assert_eq!(
        TerminalCapabilities::from_probe(ColorMode::Auto, ansi256).color,
        ColorDepth::TrueColor
    );
}

#[test]
fn no_color_and_explicit_plain_override_forced_colour() {
    let mut no_color = probe(Some("xterm-256color"));
    no_color.no_color = true;
    let caps = TerminalCapabilities::from_probe(ColorMode::Always, no_color);
    assert_eq!(caps.color, ColorDepth::None);
    assert!(!caps.animation, "no-color startup must be static");
    let mut plain = probe(Some("xterm-256color"));
    plain.explicit_plain = true;
    let caps = TerminalCapabilities::from_probe(ColorMode::Always, plain);
    assert!(!caps.interactive);
    assert!(!caps.unicode);
    assert_eq!(caps.color, ColorDepth::None);
}

#[test]
fn renderer_profile_uses_frame_delimiters_and_octet_capabilities() {
    let renderer = sexy_terminal_capabilities(
        TerminalCapabilities::test(true, true, ColorDepth::Ansi256),
        (132, 47),
    );
    assert!(renderer.interactive);
    assert!(!renderer.plain);
    assert_eq!(renderer.color_depth, SexyColorDepth::Ansi256);
    assert!(renderer.cursor_addressing);
    assert!(renderer.line_clearing);
    assert!(renderer.synchronized_output);
    assert!(renderer.sync_output);
    assert_eq!(
        renderer.dimensions,
        Some(SexyTerminalSize {
            columns: 132,
            rows: 47,
        })
    );
}
