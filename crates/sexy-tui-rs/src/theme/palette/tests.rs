//! Unit tests for `crate::theme::palette`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::theme::palette`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn quantizes_without_changing_visible_text() {
    let color = Color::Rgb(22, 135, 109);
    for depth in [
        ColorDepth::Ansi16,
        ColorDepth::Ansi256,
        ColorDepth::TrueColor,
    ] {
        let rendered = apply_foreground(color, depth, "text");
        assert!(rendered.contains("text"));
    }
    assert_eq!(apply_foreground(color, ColorDepth::None, "text"), "text");
}

#[test]
fn fixed_palette_roundtrips_and_rgb_approximation_retains_lightness() {
    for index in 16..=255 {
        let (r, g, b) = ansi256_rgb(index);
        assert_eq!(ansi256_rgb(nearest_ansi256(r, g, b)), (r, g, b));
    }
    for r in (0..=255).step_by(17) {
        for g in (0..=255).step_by(17) {
            for b in (0..=255).step_by(17) {
                let source = relative_luminance((r, g, b)) + 0.05;
                let index = nearest_ansi256(r, g, b);
                let emitted = relative_luminance(ansi256_rgb(index)) + 0.05;
                assert!(index >= 16);
                assert!(
                    source.max(emitted) / source.min(emitted) <= 1.2,
                    "{r}/{g}/{b} -> {index}"
                );
            }
        }
    }
    let mut last = 0.0;
    for gray in 0..=255 {
        let emitted = relative_luminance(ansi256_rgb(nearest_ansi256(gray, gray, gray)));
        assert!(emitted >= last, "gray ramp reversed at {gray}");
        last = emitted;
    }
}

#[test]
fn rgb_approximation_avoids_theme_owned_palette_entries() {
    for (red, green, blue) in ANSI16_RGB {
        let index = nearest_ansi256(red, green, blue);
        assert!(
            index >= 16,
            "RGB ({red}, {green}, {blue}) selected theme slot {index}"
        );
    }
    assert_eq!(
        foreground_sequence(Color::Rgb(0, 0, 0), ColorDepth::Ansi256).as_deref(),
        Some("\x1b[38;5;16m")
    );
    assert_eq!(
        background_sequence(Color::Rgb(255, 255, 255), ColorDepth::Ansi256).as_deref(),
        Some("\x1b[48;5;231m")
    );
    // Explicit indexed colours still express the caller's palette choice.
    assert_eq!(
        foreground_sequence(Color::Indexed(1), ColorDepth::Ansi256).as_deref(),
        Some("\x1b[38;5;1m")
    );
    assert_eq!(
        foreground_sequence(Color::Ansi16(1), ColorDepth::Ansi16).as_deref(),
        Some("\x1b[31m")
    );
    assert_eq!(
        foreground_sequence(Color::Rgb(205, 49, 49), ColorDepth::TrueColor).as_deref(),
        Some("\x1b[38;2;205;49;49m")
    );
}
