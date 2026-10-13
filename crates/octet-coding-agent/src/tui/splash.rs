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
    // Explicit splash colours retain precedence at every capability tier. On
    // animation-capable truecolor the splash colour additionally shades into a
    // column gradient with the travelling sweep instead of a flat block, so a
    // custom theme keeps a living mark; every other tier stays solid.
    let animated_gradient = solid_color.filter(|_| {
        theme.capabilities().color == crate::tui::terminal::ColorDepth::TrueColor
            && theme.capabilities().animation
    });
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
                let mut color = match (animated_gradient, solid_color) {
                    (Some(solid), _) => gradient_stop(solid, column),
                    (None, Some(solid)) => solid,
                    (None, None) => model_accent
                        .map_or(COLORS[column], |accent| mix(COLORS[column], accent, 0.58)),
                };
                if (solid_color.is_none() || animated_gradient.is_some())
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

/// Retained native mark at the canonical 256×128 master size. Tern 0.4.0
/// flattens the SVG silhouette; a transparent PNG preserves all eight bars.
/// Encode only this fixed RGBA8 raster, using the existing zlib dependency.
/// Logical display dimensions remain independent of this bounded pixel buffer.
pub(crate) fn native_png(
    model_accent: Option<(u8, u8, u8)>,
    solid: Option<(u8, u8, u8)>,
) -> std::io::Result<Vec<u8>> {
    use flate2::{write::ZlibEncoder, Compression, Crc};
    use std::io::Write;

    const WIDTH: usize = 256;
    const HEIGHT: usize = 128;
    const COLUMN_WIDTH: usize = WIDTH / BYTE.len();
    let colors = std::array::from_fn::<_, 8, _>(|column| {
        solid.map_or_else(
            || model_accent.map_or(COLORS[column], |accent| mix(COLORS[column], accent, 0.58)),
            |solid| gradient_stop(solid, column),
        )
    });
    let mut pixels = Vec::with_capacity(HEIGHT * (1 + WIDTH * 4));
    for y in 0..HEIGHT {
        pixels.push(0); // PNG's unfiltered scanline selector.
        for x in 0..WIDTH {
            let column = x / COLUMN_WIDTH;
            if BYTE[column] == b'0' && y < HEIGHT / 2 {
                pixels.extend_from_slice(&[0, 0, 0, 0]);
            } else {
                let (r, g, b) = colors[column];
                pixels.extend_from_slice(&[r, g, b, 255]);
            }
        }
    }
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&pixels)?;
    let compressed = encoder.finish()?;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8; 4], data: &[u8]| {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(data);
        png.extend_from_slice(&crc.sum().to_be_bytes());
    };
    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&(WIDTH as u32).to_be_bytes());
    header.extend_from_slice(&(HEIGHT as u32).to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]); // RGBA8, deflate, no interlace.
    chunk(b"IHDR", &header);
    chunk(b"IDAT", &compressed);
    chunk(b"IEND", &[]);
    Ok(png)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), amount: f32) -> (u8, u8, u8) {
    let channel = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * amount) as u8;
    (channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// Shade one gradient stop from an explicit splash colour: darkened on the
/// left columns, lifted on the right, keeping the theme's hue across the
/// byte mark.
fn gradient_stop(solid: (u8, u8, u8), column: usize) -> (u8, u8, u8) {
    let position = column as f32 / 7.0;
    mix(
        mix(solid, (0, 0, 0), 0.45),
        mix(solid, (255, 255, 255), 0.30),
        position,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::terminal::{ColorDepth, TerminalCapabilities};
    use sexy_tui_rs::{strip_terminal_sequences, visible_width};

    fn decode_native_png(png: &[u8]) -> Vec<u8> {
        use flate2::{read::ZlibDecoder, Crc};
        use std::io::Read;

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut offset = 8;
        let mut kinds = Vec::new();
        let mut pixels = Vec::new();
        while offset < png.len() {
            let len = u32::from_be_bytes(png[offset..offset + 4].try_into().unwrap()) as usize;
            let kind = &png[offset + 4..offset + 8];
            let data = &png[offset + 8..offset + 8 + len];
            let crc =
                u32::from_be_bytes(png[offset + 8 + len..offset + 12 + len].try_into().unwrap());
            let mut expected_crc = Crc::new();
            expected_crc.update(kind);
            expected_crc.update(data);
            assert_eq!(crc, expected_crc.sum());
            kinds.push(kind);
            match kind {
                b"IHDR" => assert_eq!(data, &[0, 0, 1, 0, 0, 0, 0, 128, 8, 6, 0, 0, 0]),
                b"IDAT" => {
                    ZlibDecoder::new(data).read_to_end(&mut pixels).unwrap();
                }
                b"IEND" => assert!(data.is_empty()),
                _ => panic!("unexpected native PNG chunk"),
            }
            offset += len + 12;
        }
        assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);
        assert_eq!(pixels.len(), 128 * (1 + 256 * 4));
        pixels
    }

    #[test]
    fn native_png_is_valid_bounded_and_has_the_exact_canonical_raster_silhouette() {
        use octet_ai::{
            media::{prepare_user_image, ImageInputLimits},
            types::{ImageMedia, ImageSource},
        };

        let png = native_png(None, None).unwrap();
        assert_eq!(png, native_png(None, None).unwrap(), "immutable bytes");
        assert!(
            png.len() < 4096,
            "a small native blob, not an arbitrary image"
        );
        // Also use the independent image decoder already behind octet's media
        // boundary, rather than validating only against our chunk parser.
        let image = ImageMedia {
            source: ImageSource::Inline(bytes::Bytes::from(png.clone())),
            media_type: Some(mime::IMAGE_PNG),
            detail: None,
        };
        prepare_user_image(
            &image,
            ImageInputLimits {
                max_width: 256,
                max_height: 128,
                max_bytes: 4096,
            },
        )
        .unwrap();
        let pixels = decode_native_png(&png);
        for y in 0..128 {
            assert_eq!(pixels[y * (1 + 256 * 4)], 0);
            for x in 0..256 {
                let offset = y * (1 + 256 * 4) + 1 + x * 4;
                let column = x / 32;
                let expected = if (column == 0 || column == 3) && y < 64 {
                    [0, 0, 0, 0]
                } else {
                    let (r, g, b) = COLORS[column];
                    [r, g, b, 255]
                };
                assert_eq!(&pixels[offset..offset + 4], &expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn native_png_preserves_settled_model_blend_and_explicit_theme_gradient() {
        let base = native_png(None, None).unwrap();
        let model = native_png(Some((255, 0, 0)), None).unwrap();
        assert_ne!(base, model);
        let pixels = decode_native_png(&model);
        let bottom_left = 127 * (1 + 256 * 4) + 1;
        assert_eq!(&pixels[bottom_left..bottom_left + 4], &[179, 59, 107, 255]);
        let theme = native_png(Some((255, 0, 0)), Some((217, 119, 87))).unwrap();
        assert_eq!(
            theme,
            native_png(None, Some((217, 119, 87))).unwrap(),
            "theme overrides model"
        );
        let pixels = decode_native_png(&theme);
        assert_eq!(&pixels[bottom_left..bottom_left + 4], &[119, 65, 47, 255]);
        let bottom_right = bottom_left + 255 * 4;
        assert_eq!(
            &pixels[bottom_right..bottom_right + 4],
            &[228, 159, 137, 255]
        );
        for (plain, adaptive) in decode_native_png(&base)
            .chunks_exact(1025)
            .zip(decode_native_png(&model).chunks_exact(1025))
        {
            for (plain, adaptive) in plain[1..]
                .chunks_exact(4)
                .zip(adaptive[1..].chunks_exact(4))
            {
                assert_eq!(plain[3], adaptive[3], "colors never change silhouette");
            }
        }
    }

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
    fn themed_solid_renders_truecolor_gradient_with_sweep_and_static_solid_elsewhere() {
        let theme = crate::tui::theme::test_theme();
        let settled = render_logo(
            &theme,
            8,
            2,
            DURATION,
            Some((255, 0, 0)),
            Some((217, 119, 87)),
        );
        // Column gradient derived from the splash colour, settled past the sweep.
        assert!(settled[1].contains(&theme.rgb_fg((119, 65, 47), "█")));
        assert!(settled[1].contains(&theme.rgb_fg((228, 159, 137), "█")));
        let during = render_logo(&theme, 8, 2, 0.5, Some((255, 0, 0)), Some((217, 119, 87)));
        assert_ne!(during, settled, "sweep still travels over themed gradients");
        let plain = |rows: &[String]| {
            rows.iter()
                .map(|line| strip_terminal_sequences(line))
                .collect::<Vec<_>>()
        };
        assert_eq!(plain(&during), plain(&settled), "not geometry");
        // Without animation the explicit colour stays one flat block.
        let mut capabilities = TerminalCapabilities::test(true, true, ColorDepth::TrueColor);
        capabilities.animation = false;
        let reduced = crate::tui::theme::test_theme_with(capabilities);
        let flat = render_logo(&reduced, 8, 2, 0.5, Some((255, 0, 0)), Some((217, 119, 87)));
        assert!(flat.iter().all(|row| row.contains("38;2;217;119;87")));
        assert_eq!(
            flat,
            render_logo(&reduced, 8, 2, DURATION, None, Some((217, 119, 87)))
        );
    }
}
