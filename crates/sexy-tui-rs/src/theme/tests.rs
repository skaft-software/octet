//! Unit tests for `crate::theme`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::theme`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn theme_quantizes_and_plain_mode_preserves_text() {
    for depth in [
        ColorDepth::Ansi16,
        ColorDepth::Ansi256,
        ColorDepth::TrueColor,
    ] {
        let theme = Theme::with_capabilities(TerminalCapabilities::interactive(depth, true));
        let styled = theme.apply_role(TextRole::Accent, "accent");
        assert!(styled.contains("accent"));
        match depth {
            ColorDepth::Ansi16 => {
                assert!(!styled.contains("38;2;") && !styled.contains("38;5;"))
            }
            ColorDepth::Ansi256 => assert!(styled.contains("38;5;")),
            ColorDepth::TrueColor => assert!(styled.contains("38;2;")),
            ColorDepth::None => unreachable!(),
        }
    }
    let theme = Theme::with_capabilities(TerminalCapabilities::plain());
    assert_eq!(theme.apply_role(TextRole::Error, "error"), "error");
}

#[test]
fn layered_toml_aliases_and_runtime_overrides_reload_predictably() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("theme.toml");
    std::fs::write(
        &path,
        "[colors]\naccent = \"#010203\"\n[spacing]\nsm = 3\n[icons]\nsuccess = \"ok\"\n",
    )
    .unwrap();
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let mut theme = Theme::load_with_capabilities(path.to_str(), capabilities);
    assert_eq!(theme.resolve_color("accent"), Some(Color::Rgb(1, 2, 3)));
    assert_eq!(theme.resolve::<u8>("spacing_sm"), Some(3));
    theme.override_token("accent", "#040506");
    std::fs::write(&path, "[colors]\naccent = \"#ffffff\"\n").unwrap();
    theme.reload();
    assert_eq!(theme.resolve_color("accent"), Some(Color::Rgb(4, 5, 6)));
}

#[test]
fn configured_icons_cannot_inject_terminal_controls() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true)
        .with_overrides(&crate::CapabilityOverrides {
            nerd_font: Some(true),
            ..crate::CapabilityOverrides::default()
        });
    let mut theme = Theme::with_capabilities(capabilities);
    theme.override_token("icon_success", "ok\x1b]52;c;bad\x07");
    let icon = theme.icon("icon_success");
    assert!(!icon.contains('\x1b'));
    assert!(!icon.contains('\x07'));
}

#[test]
fn accent_injection_changes_only_the_semantic_accent() {
    let mut theme = Theme::with_capabilities(TerminalCapabilities::interactive(
        ColorDepth::TrueColor,
        true,
    ));
    let heading = theme.style(TextRole::Heading);
    theme.set_accent(Color::Rgb(1, 2, 3));
    assert_eq!(
        theme.style(TextRole::Accent).foreground,
        Color::Rgb(1, 2, 3)
    );
    assert_eq!(theme.style(TextRole::Heading), heading);
}
