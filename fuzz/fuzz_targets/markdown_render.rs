#![no_main]

use libfuzzer_sys::fuzz_target;
use sexy_tui_rs::utils::visible_width;
use sexy_tui_rs::{
    parse_markdown, CodeOverflow, ColorDepth, RenderOptions, RichRenderer, TerminalCapabilities,
    Theme,
};

fuzz_target!(|data: &[u8]| {
    if data.len() > 4096 {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let document = parse_markdown(&text);
    let selected = u16::from(data.first().copied().unwrap_or(0));
    for width in [0, 1, 2, selected] {
        for plain in [false, true] {
            let capabilities = TerminalCapabilities {
                plain,
                unicode: !plain,
                hyperlinks: !plain,
                color_depth: if plain {
                    ColorDepth::None
                } else {
                    ColorDepth::TrueColor
                },
                ..TerminalCapabilities::plain()
            };
            let renderer = RichRenderer::new(
                Theme::with_capabilities(capabilities),
                capabilities,
                RenderOptions {
                    code_overflow: if selected % 2 == 0 {
                        CodeOverflow::Clip
                    } else {
                        CodeOverflow::Wrap
                    },
                    code_borders: selected % 2 == 0,
                    ..RenderOptions::default()
                },
            );
            for row in renderer.render(&document, width).lines {
                assert!(
                    visible_width(&row.styled) <= usize::from(width),
                    "width={width}, row={:?}",
                    row.styled
                );
                assert!(visible_width(&row.plain) <= usize::from(width));
            }
        }
    }
});
