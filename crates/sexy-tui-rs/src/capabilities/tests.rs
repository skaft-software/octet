//! Unit tests for `crate::capabilities`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `capabilities.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::capabilities`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn probe(term: &str) -> CapabilityProbe {
    CapabilityProbe {
        stdin_tty: true,
        stdout_tty: true,
        term: Some(term.into()),
        locale: Some("en_US.UTF-8".into()),
        ..CapabilityProbe::default()
    }
}

#[test]
fn dumb_redirected_and_unknown_are_plain() {
    assert!(
        TerminalCapabilities::detect_from(&probe("dumb"), &CapabilityOverrides::default()).plain
    );
    let mut redirected = probe("xterm-256color");
    redirected.stdout_tty = false;
    assert!(TerminalCapabilities::detect_from(&redirected, &CapabilityOverrides::default()).plain);
    assert!(TerminalCapabilities::detect_from(&probe(""), &CapabilityOverrides::default()).plain);
}

#[test]
fn color_depth_degrades_conservatively() {
    let mut truecolor = probe("xterm-256color");
    truecolor.colorterm = Some("truecolor".into());
    assert_eq!(
        TerminalCapabilities::detect_from(&truecolor, &Default::default()).color_depth,
        ColorDepth::TrueColor
    );
    assert_eq!(
        TerminalCapabilities::detect_from(&probe("screen-256color"), &Default::default())
            .color_depth,
        ColorDepth::Ansi256
    );
    assert_eq!(
        TerminalCapabilities::detect_from(&probe("xterm"), &Default::default()).color_depth,
        ColorDepth::Ansi16
    );
}

#[test]
fn no_color_and_tmux_are_respected() {
    let mut no_color = probe("xterm-256color");
    no_color.no_color = true;
    assert_eq!(
        TerminalCapabilities::detect_from(&no_color, &Default::default()).color_depth,
        ColorDepth::None
    );

    let mut tmux = probe("wezterm");
    tmux.term_program = Some("WezTerm".into());
    tmux.tmux = true;
    let caps = TerminalCapabilities::detect_from(&tmux, &Default::default());
    assert!(!caps.hyperlinks);
    assert!(!caps.synchronized_output);
    assert!(!caps.kitty_graphics);

    let mut iterm_tmux = probe("xterm-256color");
    iterm_tmux.term_program = Some("iTerm.app".into());
    iterm_tmux.tmux = true;
    assert!(!TerminalCapabilities::detect_from(&iterm_tmux, &Default::default()).iterm2_images);
}

#[test]
fn bounded_probe_values_and_cell_pixels_are_normalized() {
    let mut hostile = probe("xterm-256color");
    hostile.term = Some("x".repeat(MAX_CAPABILITY_VALUE_BYTES + 1));
    assert!(TerminalCapabilities::detect_from(&hostile, &Default::default()).plain);

    let cell = CellPixelSize::new(8, 16).unwrap();
    let mut measured = probe("xterm-256color");
    measured.dimensions = Some(TerminalSize {
        columns: 0,
        rows: 24,
    });
    measured.cell_pixel_size = Some(cell);
    let measured = TerminalCapabilities::detect_from(&measured, &Default::default());
    assert_eq!(measured.dimensions, None);
    assert_eq!(measured.cell_pixel_size, Some(cell));

    let plain = measured.with_overrides(&CapabilityOverrides {
        plain: Some(true),
        ..CapabilityOverrides::default()
    });
    assert_eq!(plain.cell_pixel_size, None);
    assert_eq!(CellPixelSize::new(0, 16), None);
    assert_eq!(CellPixelSize::new(MAX_CELL_PIXEL_DIMENSION + 1, 16), None);
}

#[test]
fn explicit_overrides_win() {
    let mut no_color = probe("dumb");
    no_color.no_color = true;
    let caps = TerminalCapabilities::detect_from(
        &no_color,
        &CapabilityOverrides {
            interactive: Some(true),
            plain: Some(false),
            color_depth: Some(ColorDepth::TrueColor),
            hyperlinks: Some(true),
            ..CapabilityOverrides::default()
        },
    );
    assert!(caps.interactive);
    assert!(!caps.plain);
    assert_eq!(caps.color_depth, ColorDepth::TrueColor);
    assert!(caps.hyperlinks);
}
