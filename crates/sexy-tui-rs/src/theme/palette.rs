//! Colour quantization and backend-independent ANSI encoding.

use crate::capabilities::ColorDepth;
use crate::style::Color;

const ANSI16_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 49, 49),
    (13, 188, 121),
    (229, 229, 16),
    (36, 114, 200),
    (188, 63, 188),
    (17, 168, 205),
    (229, 229, 229),
    (102, 102, 102),
    (241, 76, 76),
    (35, 209, 139),
    (245, 245, 67),
    (59, 142, 234),
    (214, 112, 214),
    (41, 184, 219),
    (255, 255, 255),
];

/// Apply a parsed foreground colour at a selected precision.
pub fn apply_foreground(color: Color, depth: ColorDepth, text: &str) -> String {
    let Some(open) = foreground_sequence(color, depth) else {
        return text.to_owned();
    };
    format!("{open}{text}\x1b[39m")
}

/// Apply a parsed background colour at a selected precision.
pub fn apply_background(color: Color, depth: ColorDepth, text: &str) -> String {
    let Some(open) = background_sequence(color, depth) else {
        return text.to_owned();
    };
    format!("{open}{text}\x1b[49m")
}

pub fn foreground_sequence(color: Color, depth: ColorDepth) -> Option<String> {
    sequence(color, depth, false)
}

pub fn background_sequence(color: Color, depth: ColorDepth) -> Option<String> {
    sequence(color, depth, true)
}

fn sequence(color: Color, depth: ColorDepth, background: bool) -> Option<String> {
    if depth == ColorDepth::None || color == Color::Default {
        return None;
    }
    let prefix = if background { 48 } else { 38 };
    match (color, depth) {
        (Color::Default, _) | (_, ColorDepth::None) => None,
        (Color::Ansi16(index), ColorDepth::Ansi16) => {
            Some(format!("\x1b[{}m", ansi16_sgr(index, background)))
        }
        (Color::Indexed(index), ColorDepth::Ansi16) => {
            let (red, green, blue) = ansi256_rgb(index);
            let index = nearest_ansi16(red, green, blue);
            Some(format!("\x1b[{}m", ansi16_sgr(index, background)))
        }
        (Color::Rgb(red, green, blue), ColorDepth::Ansi16) => {
            let index = nearest_ansi16(red, green, blue);
            Some(format!("\x1b[{}m", ansi16_sgr(index, background)))
        }
        (Color::Ansi16(index), ColorDepth::Ansi256 | ColorDepth::TrueColor) => {
            let (red, green, blue) = ANSI16_RGB[usize::from(index.min(15))];
            if depth == ColorDepth::TrueColor {
                Some(format!("\x1b[{prefix};2;{red};{green};{blue}m"))
            } else {
                Some(format!("\x1b[{prefix};5;{}m", index.min(15)))
            }
        }
        (Color::Indexed(index), ColorDepth::Ansi256) => Some(format!("\x1b[{prefix};5;{index}m")),
        (Color::Indexed(index), ColorDepth::TrueColor) => {
            let (red, green, blue) = ansi256_rgb(index);
            Some(format!("\x1b[{prefix};2;{red};{green};{blue}m"))
        }
        (Color::Rgb(red, green, blue), ColorDepth::Ansi256) => Some(format!(
            "\x1b[{prefix};5;{}m",
            nearest_ansi256(red, green, blue)
        )),
        (Color::Rgb(red, green, blue), ColorDepth::TrueColor) => {
            Some(format!("\x1b[{prefix};2;{red};{green};{blue}m"))
        }
    }
}

fn ansi16_sgr(index: u8, background: bool) -> u8 {
    let index = index.min(15);
    match (background, index < 8) {
        (false, true) => 30 + index,
        (false, false) => 90 + index - 8,
        (true, true) => 40 + index,
        (true, false) => 100 + index - 8,
    }
}

fn color_distance(left: (u8, u8, u8), right: (u8, u8, u8)) -> u32 {
    let red = i32::from(left.0) - i32::from(right.0);
    let green = i32::from(left.1) - i32::from(right.1);
    let blue = i32::from(left.2) - i32::from(right.2);
    (red * red + green * green + blue * blue) as u32
}

fn nearest_ansi16(red: u8, green: u8, blue: u8) -> u8 {
    ANSI16_RGB
        .iter()
        .enumerate()
        .min_by_key(|(_, candidate)| color_distance((red, green, blue), **candidate))
        .map_or(7, |(index, _)| index as u8)
}

fn ansi256_rgb(index: u8) -> (u8, u8, u8) {
    if index < 16 {
        return ANSI16_RGB[usize::from(index)];
    }
    if index < 232 {
        let value = index - 16;
        let component = |part: u8| if part == 0 { 0 } else { 55 + part * 40 };
        return (
            component(value / 36),
            component((value % 36) / 6),
            component(value % 6),
        );
    }
    let gray = 8 + (index - 232) * 10;
    (gray, gray, gray)
}

/// Approximate RGB using the fixed xterm palette while retaining lightness.
pub fn nearest_ansi256(red: u8, green: u8, blue: u8) -> u8 {
    // The first sixteen entries belong to the user's terminal theme. Their
    // nominal RGB values cannot safely approximate an explicit RGB colour.
    // Indexed/Ansi16 inputs retain their intentional palette semantics above.
    // RGB distance alone can round every channel down and erase foreground
    // contrast (or round a dark surface up). Bound the contrast-luminance
    // error in either direction before choosing the closest remaining hue.
    // Cache palette luminances; animated rendering only linearizes its input.
    static LUMINANCES: std::sync::OnceLock<[f64; 240]> = std::sync::OnceLock::new();
    let luminances = LUMINANCES.get_or_init(|| {
        std::array::from_fn(|offset| relative_luminance(ansi256_rgb((offset + 16) as u8)) + 0.05)
    });
    let source = relative_luminance((red, green, blue)) + 0.05;
    (16u8..=255)
        .filter(|index| {
            let candidate = luminances[usize::from(*index - 16)];
            source.max(candidate) / source.min(candidate) <= 1.2
        })
        .min_by_key(|index| color_distance((red, green, blue), ansi256_rgb(*index)))
        .unwrap_or_else(|| {
            (16u8..=255)
                .min_by_key(|index| color_distance((red, green, blue), ansi256_rgb(*index)))
                .unwrap_or(16)
        })
}

fn relative_luminance((red, green, blue): (u8, u8, u8)) -> f64 {
    let linear = |channel: u8| {
        let channel = f64::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

/// Compatibility helper: apply a `#RRGGBB` truecolour foreground.
pub fn apply_fg(color: &str, text: &str) -> String {
    Color::parse(color).map_or_else(
        || text.to_owned(),
        |color| apply_foreground(color, ColorDepth::TrueColor, text),
    )
}

/// Compatibility helper: apply a `#RRGGBB` truecolour background.
pub fn apply_bg(color: &str, text: &str) -> String {
    Color::parse(color).map_or_else(
        || text.to_owned(),
        |color| apply_background(color, ColorDepth::TrueColor, text),
    )
}

/// Generate eight lightness-scaled colours from a base RGB value.
pub fn generate_palette(base: &str) -> Vec<String> {
    let Color::Rgb(red, green, blue) = Color::parse(base).unwrap_or(Color::Rgb(255, 255, 255))
    else {
        return Vec::new();
    };
    (0..8)
        .map(|index| {
            let factor = 0.5 + f64::from(index) * 0.125;
            let channel = |value: u8| (f64::from(value) * factor).min(255.0) as u8;
            format!(
                "#{:02x}{:02x}{:02x}",
                channel(red),
                channel(green),
                channel(blue)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
