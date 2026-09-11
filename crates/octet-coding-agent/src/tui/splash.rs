//! Canonical eight-column octet byte mark used by the inline startup identity.

use crate::tui::theme::OctetTheme;

pub(crate) const DURATION: f32 = 2.2;
const BYTE: &[u8; 8] = b"01101111";
const COLORS: [(u8, u8, u8); 8] = [
    (0x4b, 0x8d, 0xff),
    (0x48, 0xad, 0xf5),
    (0x45, 0xce, 0xeb),
    (0x49, 0xdc, 0xd9),
    (0x4f, 0xe2, 0xc3),
    (0x5c, 0xe9, 0xaa),
    (0x74, 0xf4, 0x8a),
    (0x8d, 0xff, 0x6a),
];

/// Eight contiguous equal-width columns: zero is half height, one full height.
/// Uniformly scale the 8×2 terminal grid to fit the box, centered in padding.
/// All columns share a baseline. A finite colour sweep never changes geometry
/// or delays input; blank cells preserve the terminal background.
pub(crate) fn render_logo(
    theme: &OctetTheme,
    width: usize,
    symbol_rows: usize,
    elapsed: f32,
    model_accent: Option<(u8, u8, u8)>,
    solid_color: Option<(u8, u8, u8)>,
) -> Vec<String> {
    let width = width.min(42);
    let rows = symbol_rows.min(21);
    if width < BYTE.len() || rows < 2 {
        return vec![" ".repeat(width); rows];
    }
    let column_width = (width / BYTE.len()).min(rows / 2);
    let mark_rows = column_width * 2;
    let top_pad = (rows - mark_rows) / 2;
    let left_pad = (width - column_width * BYTE.len()) / 2;
    let right_pad = width - left_pad - column_width * BYTE.len();
    // Quantizing a per-column gradient can wash out individual bars. Reuse the
    // background-balanced model accent uniformly, without the brightening sweep.
    // Explicit splash colours retain precedence at every capability tier.
    let solid_color = solid_color.or_else(|| {
        (theme.capabilities().color != crate::tui::terminal::ColorDepth::TrueColor).then(|| {
            model_accent
                .or_else(|| theme.model_rgb(None))
                .unwrap_or(COLORS[0])
        })
    });
    let glyph = if theme.unicode() { "█" } else { "#" };
    let filled = glyph.repeat(column_width);
    let blank = " ".repeat(column_width);
    (0..rows)
        .map(|row| {
            if row < top_pad || row >= top_pad + mark_rows {
                return " ".repeat(width);
            }
            let mut line = " ".repeat(left_pad);
            for (column, bit) in BYTE.iter().enumerate() {
                if *bit == b'0' && row < top_pad + column_width {
                    line.push_str(&blank);
                    continue;
                }
                let mut color = solid_color.unwrap_or_else(|| {
                    model_accent.map_or(COLORS[column], |accent| mix(COLORS[column], accent, 0.58))
                });
                if solid_color.is_none()
                    && theme.capabilities().animation
                    && (0.0..DURATION).contains(&elapsed)
                {
                    let position = elapsed / DURATION * 1.4 - 0.2;
                    let distance = (column as f32 / 7.0 - position).abs();
                    let lift = (1.0 - distance / 0.2).max(0.0) * 0.2;
                    color = mix(color, (255, 255, 255), lift);
                }
                line.push_str(&theme.rgb_fg(color, &filled));
            }
            line.push_str(&" ".repeat(right_pad));
            line
        })
        .collect()
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), amount: f32) -> (u8, u8, u8) {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * amount) as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use sexy_tui_rs::{strip_terminal_sequences, visible_width};

    #[test]
    fn canonical_byte_has_eight_contiguous_columns_and_one_baseline() {
        let theme = crate::tui::theme::test_theme();
        for elapsed in [0.0, 0.5, DURATION, DURATION + 1.0] {
            let rows = render_logo(&theme, 8, 2, elapsed, None, None);
            assert_eq!(strip_terminal_sequences(&rows[0]), " ██ ████");
            assert_eq!(strip_terminal_sequences(&rows[1]), "████████");
        }
        for width in [8, 14, 16, 24, 42] {
            let rows = render_logo(&theme, width, 6, DURATION, None, None);
            assert_eq!(rows.len(), 6);
            assert!(rows.iter().all(|row| visible_width(row) == width));
        }
    }

    #[test]
    fn uniform_grid_fits_both_box_limits_with_centered_padding() {
        let theme = crate::tui::theme::test_theme();
        for (width, height, scale) in [
            (0, 0, 0),
            (7, 6, 0),
            (8, 1, 0),
            (8, 2, 1),
            (14, 6, 1),
            (16, 6, 2),
            (24, 3, 1),
            (24, 5, 2),
            (40, 21, 5),
        ] {
            let actual = render_logo(&theme, width, height, DURATION, None, None)
                .iter()
                .map(|line| strip_terminal_sequences(line))
                .collect::<Vec<_>>();
            let mut expected = vec![" ".repeat(width); height];
            if scale > 0 {
                let left = (width - 8 * scale) / 2;
                let top = (height - 2 * scale) / 2;
                for (y, row) in [" ██ ████", "████████"].iter().enumerate()
                {
                    let expanded = row
                        .chars()
                        .map(|ch| ch.to_string().repeat(scale))
                        .collect::<String>();
                    for dy in 0..scale {
                        expected[top + y * scale + dy] = format!(
                            "{}{}{}",
                            " ".repeat(left),
                            expanded,
                            " ".repeat(width - left - 8 * scale)
                        );
                    }
                }
            }
            assert_eq!(actual, expected, "box={width}x{height}, scale={scale}");
        }
    }

    #[test]
    fn uniformly_scaled_logo_preserves_model_blend_and_finite_color_sweep() {
        let theme = crate::tui::theme::test_theme();
        let render = |elapsed| render_logo(&theme, 16, 6, elapsed, Some((255, 0, 0)), None);
        let steady = render(DURATION);
        // First approved gradient column (75,141,255), blended 0.58 toward red.
        assert!(steady[3].contains(&theme.rgb_fg((179, 59, 107), "██")));
        let during = render(0.5);
        assert_ne!(during, steady, "animation changes colors");
        let plain = |rows: &[String]| {
            rows.iter()
                .map(|line| strip_terminal_sequences(line))
                .collect::<Vec<_>>()
        };
        assert_eq!(plain(&during), plain(&steady), "not geometry");
        assert_eq!(render(DURATION + 1.0), steady, "finite sweep settles");
        let mut capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
        capabilities.animation = false;
        let reduced = crate::tui::theme::test_theme_with(capabilities);
        assert_eq!(
            render_logo(&reduced, 16, 6, 0.5, Some((255, 0, 0)), None),
            steady
        );
    }

    #[test]
    fn ascii_no_color_and_reduced_motion_keep_static_identity() {
        let mut capabilities = TerminalCapabilities::test(true, false, ColorDepth::None);
        capabilities.animation = false;
        let theme = crate::tui::theme::test_theme_with(capabilities);
        let rows = render_logo(&theme, 8, 2, 0.2, Some((255, 0, 0)), None);
        assert_eq!(rows, [" ## ####", "########"]);
        assert_eq!(
            rows,
            render_logo(&theme, 8, 2, 1.5, Some((255, 0, 0)), None)
        );
    }

    #[test]
    fn limited_color_logo_uses_one_static_background_balanced_accent() {
        use crate::tui::theme::{test_theme_for, ModelLab, TerminalBackground};
        for depth in [ColorDepth::Ansi256, ColorDepth::Ansi16, ColorDepth::None] {
            for background in [
                TerminalBackground::Light,
                TerminalBackground::Dark,
                TerminalBackground::Unknown,
            ] {
                for unicode in [false, true] {
                    let theme = test_theme_for(
                        background,
                        TerminalCapabilities::test(true, unicode, depth),
                    );
                    for lab in [Some(ModelLab::OpenAi), Some(ModelLab::Anthropic), None] {
                        let accent = lab.and_then(|lab| theme.model_rgb(Some(lab)));
                        let color = accent.or_else(|| theme.model_rgb(None)).unwrap();
                        let glyph = if unicode { "█" } else { "#" };
                        for width in [8, 16, 24] {
                            let scale = width / 8;
                            let painted = theme.rgb_fg(color, &glyph.repeat(scale));
                            let expected = [
                                format!(
                                    "{}{}{}{}",
                                    " ".repeat(scale),
                                    painted.repeat(2),
                                    " ".repeat(scale),
                                    painted.repeat(4)
                                ),
                                painted.repeat(8),
                            ];
                            for elapsed in [0.0, 0.5, 1.5, DURATION, DURATION + 1.0] {
                                let rows =
                                    render_logo(&theme, width, scale * 2, elapsed, accent, None);
                                assert_eq!(rows[..scale], vec![expected[0].clone(); scale]);
                                assert_eq!(rows[scale..], vec![expected[1].clone(); scale]);
                                assert!(rows.iter().all(|row| visible_width(row) == width));
                            }
                        }
                        let custom = (217, 119, 87);
                        let rows = render_logo(&theme, 8, 2, 0.5, accent, Some(custom));
                        assert_eq!(rows[1], theme.rgb_fg(custom, glyph).repeat(8));
                        assert_eq!(
                            rows,
                            render_logo(&theme, 8, 2, DURATION, accent, Some(custom))
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn custom_solid_color_overrides_model_color_and_animation() {
        let theme = crate::tui::theme::test_theme();
        let rows = render_logo(&theme, 8, 2, 0.2, Some((255, 0, 0)), Some((217, 119, 87)));
        assert!(rows.iter().all(|row| row.contains("38;2;217;119;87")));
        assert_eq!(
            rows,
            render_logo(&theme, 8, 2, 1.5, None, Some((217, 119, 87)))
        );
    }
}
