//! Unit tests for `crate::rich_text::render`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::render`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::capabilities::{CapabilityOverrides, ColorDepth};
use crate::rich_text::markdown;
use crate::rich_text::{DetailBlock, Inline, StatusKind};
use crate::style::{BlockRole, Color};

fn renderer(color: ColorDepth, unicode: bool) -> RichRenderer {
    let capabilities =
        TerminalCapabilities::interactive(color, unicode).with_overrides(&CapabilityOverrides {
            hyperlinks: Some(true),
            ..CapabilityOverrides::default()
        });
    RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions::default(),
    )
}

#[test]
fn whitespace_and_blank_suffixes_keep_only_append_local_layout_work() {
    let renderer = RichRenderer::plain();
    for chunk in [" ", "\n", "abcdefgh"] {
        let mut block = Block::Plain("start ".to_owned());
        let mut cache = AppendOnlyTail::default();
        let mut stats = StreamingLayoutStats::default();
        let mut output = Vec::new();
        let mut visible = 0;
        for _ in 0..8_000 {
            let Block::Plain(source) = &mut block else {
                unreachable!()
            };
            source.push_str(chunk);
            (_, visible) = cache
                .update(&block, &renderer, 12, &mut output, &mut stats)
                .unwrap();
        }
        assert!(stats.laid_out_bytes < 8_000 * 40, "{chunk:?}: {stats:?}");
        assert!(stats.encoded_rows < 8_000 * 4, "{chunk:?}: {stats:?}");
        assert_eq!(
            output[..visible],
            renderer
                .render_unstable(&Document::new(vec![block.clone()]), 12)
                .lines
        );
        // A combining suffix can turn skipped whitespace into a visible
        // grapheme. It must remain recoverable at the mutable checkpoint.
        for suffix in ["\u{301}", "next", "\n", "after"] {
            let Block::Plain(source) = &mut block else {
                unreachable!()
            };
            source.push_str(suffix);
            (_, visible) = cache
                .update(&block, &renderer, 12, &mut output, &mut stats)
                .unwrap();
            assert_eq!(
                output[..visible],
                renderer
                    .render_unstable(&Document::new(vec![block.clone()]), 12)
                    .lines
            );
        }
    }
}

#[test]
fn append_only_literal_rows_match_full_layout_at_every_scalar_boundary() {
    let source = concat!(
        "alpha beta gamma delta epsilon longidentifierabcdefghijklmno ",
        "界界 e\u{301} 👩\u{200d}💻 🇦🇧 ",
        "\n\nsecond line  with    spaces\n\n\n",
        "ending \u{301}♥\u{fe0f} and more text for wrapping"
    );
    for width in [0, 1, 2, 3, 7, 16, 40] {
        for code in [false, true] {
            for borders in [false, true] {
                for overflow in [CodeOverflow::Clip, CodeOverflow::Wrap] {
                    let capabilities =
                        TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
                    let mut renderer = RichRenderer::new(
                        Theme::with_capabilities(capabilities),
                        capabilities,
                        RenderOptions {
                            code_borders: borders,
                            code_overflow: overflow,
                            ..RenderOptions::default()
                        },
                    );
                    let mut style = renderer.theme().block_style(BlockRole::Code);
                    style.background = Some(Color::Rgb(20, 30, 40));
                    style.padding_top = 1;
                    style.padding_bottom = 1;
                    renderer
                        .theme_mut()
                        .override_block_style(BlockRole::Code, style);
                    let mut cache = AppendOnlyTail::default();
                    let mut stats = StreamingLayoutStats::default();
                    let mut output = Vec::new();
                    let mut prior: Vec<RenderedLine> = Vec::new();
                    for end in source.char_indices().map(|(i, c)| i + c.len_utf8()) {
                        let block = if code {
                            Block::CodeBlock(CodeBlock::with_language("rust", &source[..end]))
                        } else {
                            Block::Plain(source[..end].to_owned())
                        };
                        let (stable, visible) = cache
                            .update(&block, &renderer, width, &mut output, &mut stats)
                            .unwrap();
                        let expected = renderer
                            .render_unstable(&Document::new(vec![block]), width)
                            .lines;
                        assert_eq!(output[..visible], expected, "end={end} width={width} code={code} borders={borders} overflow={overflow:?}");
                        let stable = stable.min(prior.len());
                        assert_eq!(prior[..stable], output[..stable]);
                        prior = expected;
                    }
                }
            }
        }
    }
}

#[test]
fn append_only_code_transforms_match_every_scalar_boundary() {
    for (color, unicode) in [(ColorDepth::TrueColor, true), (ColorDepth::None, false)] {
        for overflow in [CodeOverflow::Clip, CodeOverflow::Wrap] {
            for width in [1, 7, 16] {
                let mut renderer = renderer(color, unicode);
                let mut options = renderer.options();
                options.code_overflow = overflow;
                renderer.set_options(options);
                for source in [
                    "12345♥\u{fe0f}\t👩\u{200d}💻\t🇦🇧e\u{301}\tend\nnext\trow",
                    "abc\t\x1b[31mred\x1b[0m\n\x1b]52;c;Y2xpcA==\x07\u{009b}\u{202e}\tend",
                ] {
                    let mut cache = AppendOnlyTail::default();
                    let mut stats = StreamingLayoutStats::default();
                    let mut output = Vec::new();
                    let mut prior: Vec<RenderedLine> = Vec::new();
                    for end in source.char_indices().map(|(i, c)| i + c.len_utf8()) {
                        let block =
                            Block::CodeBlock(CodeBlock::with_language("rust", &source[..end]));
                        let (stable, visible) = cache
                            .update(&block, &renderer, width, &mut output, &mut stats)
                            .unwrap();
                        let expected = renderer
                            .render_unstable(&Document::new(vec![block]), width)
                            .lines;
                        assert_eq!(
                            output[..visible],
                            expected,
                            "end={end} width={width} overflow={overflow:?}"
                        );
                        let stable = stable.min(prior.len());
                        assert_eq!(prior[..stable], output[..stable]);
                        prior = expected;
                    }
                    assert_eq!(stats.literal_transform_fallbacks, 0);
                }
            }
        }
    }
}

#[test]
fn append_only_rich_paragraph_matches_every_scalar_boundary() {
    let prefixes = [
        vec![Inline::strong("bold"), Inline::text(" prefix e")],
        vec![Inline::strong("bold e")],
        vec![Inline::strong("bold"), Inline::text(" 🇦")],
        vec![Inline::strong("bold"), Inline::text(" 👩")],
        vec![Inline::strong("bold"), Inline::text("  \n\n")],
        vec![Inline::strong("bold"), Inline::text("          ")],
        vec![
            Inline::strong("bold"),
            Inline::emphasis(""),
            Inline::text(""),
        ],
        vec![
            Inline::status(StatusKind::Success, "ready"),
            Inline::SoftBreak,
            Inline::link("docs", "https://example.com/界"),
            Inline::HardBreak,
            Inline::Code("e\t界\x1b[31m\r\n".into()),
            Inline::text(" 👩"),
        ],
    ];
    let source = concat!(
        "\u{301}\u{200d}💻🇧 alpha beta gamma longidentifierabcdefghij ",
        "界界 e\u{301} ♥\u{fe0f} 👩\u{200d}💻 🇦🇧 ",
        "\n\nsecond line with     spaces\n\n\n",
        "                  \u{301}more text for wrapping"
    );
    for renderer in [RichRenderer::plain(), renderer(ColorDepth::TrueColor, true)] {
        for width in [0, 1, 2, 3, 7, 16, 40] {
            for prefix in &prefixes {
                let mut cache = AppendOnlyTail::default();
                let mut stats = StreamingLayoutStats::default();
                let mut output = Vec::new();
                let mut frame = Vec::new();
                for end in
                    std::iter::once(0).chain(source.char_indices().map(|(i, c)| i + c.len_utf8()))
                {
                    let mut content = prefix.clone();
                    content.push(Inline::Raw(source[..end].to_owned()));
                    let block = Block::Paragraph(content);
                    let (stable, visible) = cache
                        .update(&block, &renderer, width, &mut output, &mut stats)
                        .unwrap();
                    let stable = stable.min(frame.len());
                    frame.truncate(stable);
                    frame.extend_from_slice(&output[stable..visible]);
                    let expected = renderer.render_unstable(&Document::new(vec![block]), width);
                    assert_eq!(
                        frame, expected.lines,
                        "end={end} width={width} prefix={prefix:?}"
                    );
                    assert_eq!(output[..visible], expected.lines);
                }
                assert_eq!(stats.rich_prefix_layouts, 1);
                assert_eq!(stats.literal_transform_fallbacks, 0);
            }
        }
    }
}

#[test]
fn rich_literal_transform_fallback_is_named_and_checked_once() {
    for suffix in ["\t", "\r\n", "\x1b[31m", "\u{202e}"] {
        let renderer = RichRenderer::plain();
        let mut block =
            Block::Paragraph(vec![Inline::strong("bold"), Inline::Raw(" prefix".into())]);
        let mut cache = AppendOnlyTail::default();
        let mut stats = StreamingLayoutStats::default();
        let mut output = Vec::new();
        cache
            .update(&block, &renderer, 12, &mut output, &mut stats)
            .unwrap();
        let Block::Paragraph(content) = &mut block else {
            unreachable!()
        };
        let Some(Inline::Raw(raw)) = content.last_mut() else {
            unreachable!()
        };
        raw.push_str(suffix);
        assert!(cache
            .update(&block, &renderer, 12, &mut output, &mut stats)
            .is_none());
        assert_eq!(stats.literal_transform_fallbacks, 1);
        let before = stats;
        assert!(cache
            .update(&block, &renderer, 12, &mut output, &mut stats)
            .is_none());
        assert_eq!(stats, before);
    }
}

fn rich_stream_work(chunks: usize, rich: bool, width: u16, chunk: &str) -> u64 {
    use crate::rich_text::stream::{StreamingMarkdown, StreamingRenderCache};
    let renderer = renderer(ColorDepth::TrueColor, true);
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    let mut frame = Vec::new();
    stream.push_str(&format!(
        "{}{}",
        if rich { "**rich**" } else { "ordinary" },
        "x".repeat(8184)
    ));
    let mut replacement_bytes = 0;
    for index in 0..=chunks {
        if index > 0 {
            stream.push_str(chunk);
        }
        let update = cache.render_line_update(&stream, &renderer, width, true);
        assert!(update.stable_prefix <= frame.len());
        frame.truncate(update.stable_prefix);
        replacement_bytes += update.replacement.iter().map(String::len).sum::<usize>() as u64;
        frame.extend(update.replacement);
        // The oracle deliberately does full layout, outside measured work.
        assert_eq!(
            frame,
            renderer
                .render_unstable(stream.preview(), width)
                .styled_lines()
        );
    }
    let stats = cache.stats();
    assert_eq!(stats.rich_prefix_layouts, u64::from(rich));
    assert_eq!(stats.literal_transform_fallbacks, 0);
    assert!(stats.full_tail_layouts <= 1, "{stats:?}");
    assert!(stats.fallback_source_bytes <= 8192, "{stats:?}");
    let parser = stream.stats();
    let work = stats.checked_bytes
        + stats.measured_bytes
        + stats.laid_out_bytes
        + stats.copied_bytes
        + stats.encoded_rows
        + stats.fallback_source_bytes
        + replacement_bytes
        + parser.preview_copied_bytes;
    eprintln!(
        "rich={rich} width={width} chunk={:?} n={chunks} work={work} {stats:?}",
        &chunk[..chunk.len().min(20)]
    );
    work
}

#[test]
fn rich_and_literal_append_work_scales_linearly_with_exact_live_rows() {
    for rich in [false, true] {
        for width in [0, 40] {
            for chunk in [
                " word".repeat(50),
                "word line\n".repeat(25),
                " ".repeat(250),
            ] {
                let a = rich_stream_work(128, rich, width, &chunk);
                let b = rich_stream_work(256, rich, width, &chunk);
                assert!(b >= a * 3 / 2 && b <= a * 5 / 2, "{a} -> {b}");
            }
        }
    }
}

#[test]
fn rich_stream_reflows_and_fallbacks_reconstruct_authoritative_rows_and_copy() {
    use crate::rich_text::stream::{StreamingMarkdown, StreamingRenderCache};
    let mut renderer = renderer(ColorDepth::TrueColor, true);
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    let mut frame = Vec::new();
    let initial = format!(
        "**bold** [docs](https://example.com) {}",
        "word ".repeat(1500)
    );
    stream.push_str(&initial);
    stream.push_str(&"more ".repeat(200));
    for (step, suffix) in [
        "e", "\u{301}", "👩", "\u{200d}", "💻", "\n", "after", "\t", "tab", "\r", "\n", "\x1b",
        "[31m", "\u{202e}", " end",
    ]
    .iter()
    .enumerate()
    {
        stream.push_str(suffix);
        let width = [40, 0, 1, 7, 16][step % 5];
        if step % 3 == 0 {
            renderer.theme_mut().override_style(
                TextRole::Strong,
                TextStyle::plain().foreground(Color::Rgb(step as u8, 10, 90)),
            );
        }
        let styled = step % 2 == 0;
        let update = cache.render_line_update(&stream, &renderer, width, styled);
        assert_eq!(update.stable_prefix, 0, "resize must invalidate");
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
        let expected = renderer.render_unstable(stream.preview(), width);
        assert_eq!(
            frame,
            if styled {
                expected.styled_lines()
            } else {
                expected.plain_lines()
            }
        );
        assert_eq!(
            cache.render(&stream, &renderer, width).copy_text,
            renderer.sanitize_copy(&stream.copy_text())
        );
    }
    assert!(cache.stats().literal_transform_fallbacks > 0);
    let finalized = markdown::parse(stream.raw_text());
    assert_eq!(stream.finish(), &finalized);
    let update = cache.render_line_update(&stream, &renderer, 40, true);
    assert_eq!(update.stable_prefix, 0);
    assert_eq!(
        update.replacement,
        renderer.render(stream.committed(), 40).styled_lines()
    );
}

#[test]
fn headings_lists_quotes_links_and_code_are_semantic_and_width_bounded() {
    let document = markdown::parse(
        "# Session recovery\n\nThe invalid **final record** is removed before the next append.\n\n## Changes\n- preserves valid records without a trailing newline\n- removes invalid trailing bytes\n\n> safe quote\n\n```rust\nfn main() { println!(\"hello\"); }\n```\n\n[docs](https://example.com)",
    );
    for width in [20, 40, 60, 80, 120, 160] {
        let rendered = renderer(ColorDepth::TrueColor, true).render(&document, width);
        assert!(rendered
            .lines
            .iter()
            .all(|line| WidthPolicy::default().line_width(&line.plain) <= usize::from(width)));
        let plain = rendered.plain_text();
        assert!(!plain.contains("# "));
        assert!(!plain.contains("**"));
        assert!(plain.contains("Session recovery"));
        assert!(rendered.copy_text.contains("docs (https://example.com)"));
    }
}

#[test]
fn prose_lane_accounts_for_nested_indents_but_not_code_and_diff() {
    use crate::rich_text::stream::{StreamingMarkdown, StreamingRenderCache};

    let source = format!(
        "{}\n\n- {}\n  - {}\n\n> {}\n\n```text\n{}\n```\n\n```diff\n@@ -1 +1 @@\n+{}\n```",
        "paragraph word ".repeat(14),
        "list word ".repeat(14),
        "nested word ".repeat(14),
        "quote word ".repeat(14),
        "a".repeat(110),
        "b".repeat(110),
    );
    let mut limited = renderer(ColorDepth::TrueColor, true);
    let mut options = limited.options();
    options.prose_width = Some(92);
    limited.set_options(options);
    let document = markdown::parse(&source);
    let rendered = limited.render(&document, 160);
    let width = WidthPolicy::default();
    assert!(rendered
        .lines
        .iter()
        .any(|line| width.line_width(&line.plain) > 92));
    for line in &rendered.lines {
        let cells = width.line_width(&line.plain);
        assert!(cells <= 160, "viewport overflow: {:?}", line.plain);
        if line.plain.contains("word") {
            assert!(cells <= 92, "indented prose overflow: {:?}", line.plain);
        }
    }
    let ordinary = renderer(ColorDepth::TrueColor, true).render(&document, 160);
    assert_eq!(rendered.copy_text, ordinary.copy_text);

    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    let mut frame = Vec::new();
    for chunk in source.as_bytes().chunks(27) {
        stream.push_str(std::str::from_utf8(chunk).unwrap());
        let update = cache.render_line_update(&stream, &limited, 160, true);
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
        assert_eq!(frame, cache.render_lines(&stream, &limited, 160, true));
    }
    stream.finish();
    let update = cache.render_line_update(&stream, &limited, 160, true);
    frame.truncate(update.stable_prefix);
    frame.extend(update.replacement);
    assert_eq!(frame, rendered.styled_lines());
    assert_eq!(
        cache.render(&stream, &limited, 160).copy_text,
        rendered.copy_text
    );
}

#[test]
fn plain_ascii_mode_contains_no_escape_or_unicode_structure() {
    let document = markdown::parse("# Heading\n\n- item\n\n> quote\n\n[docs](https://example.com)");
    let rendered = RichRenderer::plain().render(&document, 40);
    let text = rendered.styled_text();
    assert!(!text.contains('\x1b'));
    assert!(text.contains("* item"));
    assert!(text.contains("| quote"));
    assert!(text.is_ascii());
}

#[test]
fn typed_accent_and_unknown_language_degrade_safely() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::Ansi16, true);
    let mut theme = Theme::with_capabilities(capabilities);
    theme.set_accent(Color::Rgb(1, 2, 3));
    let renderer = RichRenderer::new(theme, capabilities, RenderOptions::default());
    let document = Document::new(vec![
        Block::Heading {
            level: 1,
            content: vec![Inline::Role {
                role: TextRole::Accent,
                content: vec![Inline::Text("accent".into())],
            }],
        },
        Block::CodeBlock(CodeBlock::with_language("unknown-lang", "plain code")),
    ]);
    let rendered = renderer.render(&document, 30);
    assert!(rendered.plain_text().contains("plain code"));
    assert!(!rendered.styled_text().contains("38;2;"));
}

#[test]
fn code_surfaces_are_compact_terminal_neutral_and_have_no_phantom_row() {
    let document = Document::new(vec![Block::CodeBlock(CodeBlock::with_language(
        "rust",
        "let answer = 42;\n",
    ))]);
    let rendered = renderer(ColorDepth::TrueColor, true).render(&document, 160);
    assert_eq!(rendered.lines.len(), 2, "{}", rendered.plain_text());
    assert!(rendered
        .lines
        .iter()
        .all(|line| WidthPolicy::default().line_width(&line.plain) < 40));
    assert!(!rendered.styled_text().contains("\x1b[48;"));
    assert!(!rendered.plain_text().contains(['┌', '┐', '└', '┘', '│']));

    let plain = RichRenderer::plain().render(&document, 160);
    assert!(plain.lines[0].plain.contains("rust"));
    assert!(plain.lines[1].plain.starts_with("  let answer"));
    assert!(plain.lines.iter().all(|line| !line.plain.ends_with(' ')));
}

#[test]
fn generic_code_fence_labels_are_suppressed_but_meaningful_labels_remain() {
    for language in ["text", "TEXT", "plaintext", "PlainText"] {
        let document = Document::new(vec![Block::CodeBlock(CodeBlock::with_language(
            language, "value",
        ))]);
        let rendered = renderer(ColorDepth::TrueColor, true).render(&document, 40);
        let plain = rendered.plain_text();
        assert!(plain.contains("value"), "{plain:?}");
        assert!(!plain.to_ascii_lowercase().contains("text"), "{plain:?}");
        assert!(rendered.copy_text.contains("value"));
    }

    let rust = Document::new(vec![Block::CodeBlock(CodeBlock::with_language(
        "rust",
        "let answer = 42;",
    ))]);
    assert!(renderer(ColorDepth::TrueColor, true)
        .render(&rust, 40)
        .plain_text()
        .contains("rust"));
}

#[test]
fn code_and_diff_backgrounds_are_explicit_theme_opt_ins() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let mut theme = Theme::with_capabilities(capabilities);
    let document = markdown::parse("`name`\n\n```text\nvalue\n```");
    let neutral = RichRenderer::new(theme.clone(), capabilities, RenderOptions::default())
        .render(&document, 40)
        .styled_text();
    assert!(!neutral.contains("\x1b[48;"), "{neutral:?}");

    theme.override_token("md_code_bg", "#202020");
    theme.override_token("md_code_inline_bg", "#303030");
    theme.override_token("diff_added_bg", "#103010");
    let themed = RichRenderer::new(theme, capabilities, RenderOptions::default());
    assert!(themed
        .render(&document, 40)
        .styled_text()
        .contains("\x1b[48;2;"));

    let labelled = markdown::parse("```rust\nlet answer = 42;\n```");
    let labelled = themed.render(&labelled, 40);
    assert_eq!(labelled.lines.len(), 2, "{}", labelled.plain_text());
    assert!(
        labelled
            .lines
            .iter()
            .all(|line| line.styled.starts_with("\x1b[48;2;32;32;32m")),
        "{:?}",
        labelled.styled_lines()
    );
    assert_eq!(
        WidthPolicy::default().line_width(&labelled.lines[0].plain),
        WidthPolicy::default().line_width(&labelled.lines[1].plain)
    );
    assert!(labelled.lines[0].plain.ends_with(' '));

    assert!(themed
        .render_diff(
            &UnifiedDiff::parse("@@ -0,0 +1 @@\n+value"),
            40,
            DiffRenderOptions::default(),
        )
        .styled_text()
        .contains("\x1b[48;2;"));
}

#[cfg(feature = "syntax-highlighting")]
#[test]
fn public_diff_renderer_syntax_highlights_code_from_file_headers() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let mut theme = Theme::with_capabilities(capabilities);
    theme.override_token("diff_added", "#04aa05");
    theme.override_token("diff_added_marker", "#04aa05");
    theme.override_token("syntax_keyword", "#010203");
    theme.override_token("syntax_string", "#060708");
    let renderer = RichRenderer::new(theme, capabilities, RenderOptions::default());
    let rendered = renderer
        .render_diff(
            &UnifiedDiff::parse(
                "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -0,0 +1 @@\n+fn main() { let value = \"text\"; }",
            ),
            100,
            DiffRenderOptions {
                line_numbers: true,
                wrap: true,
            },
        )
        .styled_text();

    assert!(rendered.contains("\x1b[38;2;4;170;5m+"), "{rendered:?}");
    assert!(rendered.contains("\x1b[38;2;1;2;3mfn"), "{rendered:?}");
    assert!(
        rendered.contains("\x1b[38;2;6;7;8m\"text\""),
        "{rendered:?}"
    );
}

#[cfg(feature = "syntax-highlighting")]
#[test]
fn public_diff_renderer_reuses_syntax_highlighting_across_widths() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    let diff = UnifiedDiff::parse(
        "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -0,0 +1 @@\n+fn main() { println!(\"hello\"); }",
    );

    renderer.render_diff(&diff, 100, DiffRenderOptions::default());
    assert_eq!(
        renderer.syntax_cache_stats(),
        SyntaxCacheStats {
            misses: 1,
            entries: 1,
            bytes: 32,
            ..SyntaxCacheStats::default()
        }
    );

    renderer.render_diff(&diff, 40, DiffRenderOptions::default());
    assert_eq!(
        renderer.syntax_cache_stats(),
        SyntaxCacheStats {
            hits: 1,
            misses: 1,
            entries: 1,
            bytes: 32,
        }
    );
}

#[cfg(feature = "syntax-highlighting")]
#[test]
fn inline_syntax_renderer_colours_source_without_code_block_chrome() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let mut theme = Theme::with_capabilities(capabilities);
    theme.override_token("md_code_block", "#111213");
    theme.override_token("syntax_function", "#010203");
    theme.override_token("syntax_string", "#060708");
    theme.override_token("syntax_number", "#0c0d0e");
    theme.override_token("syntax_operator", "#090a0b");
    let renderer = RichRenderer::new(theme, capabilities, RenderOptions::default());
    let source =
        "find . -name \"*.rs\" 2>&1 | grep -v target && cargo test\nprintf '%s\\n' \"hello\"";
    let rendered = renderer.render_inline_syntax(source, "bash", 160);
    let styled = rendered.styled_text();

    assert_eq!(rendered.plain_text(), source);
    assert_eq!(rendered.copy_text, source);
    assert!(!styled.contains("\x1b[48;"));
    assert!(!rendered.plain_text().contains('│'));
    for program in ["find", "grep", "cargo", "printf"] {
        assert!(
            styled.contains(&format!("\x1b[38;2;1;2;3m{program}")),
            "{program} was not classified as a shell program: {styled:?}"
        );
    }
    assert!(
        !styled.contains("\x1b[38;2;1;2;3m-name") && !styled.contains("\x1b[38;2;1;2;3m-v"),
        "flags inherited the shell program role: {styled:?}"
    );
    assert!(styled.contains("\x1b[38;2;6;7;8m\"*.rs\""), "{styled:?}");
    assert!(
        styled.contains("\x1b[38;2;12;13;14m2") && styled.contains("\x1b[38;2;12;13;14m1"),
        "{styled:?}"
    );
}

#[test]
fn shell_program_ranges_follow_lists_pipelines_and_redirections() {
    let source = "A=1 find . 2>/dev/null | grep -v target; git diff\n< input cargo test";
    let programs = shell_program_ranges(source)
        .into_iter()
        .map(|(start, end)| &source[start..end])
        .collect::<Vec<_>>();
    assert_eq!(programs, ["find", "grep", "git", "cargo"]);
}

#[test]
fn inline_syntax_renderer_degrades_to_plain_source() {
    let renderer = RichRenderer::plain();
    let source = "printf '%s\\n' \"hello\" && cargo test";
    let rendered = renderer.render_inline_syntax(source, "bash", 80);
    assert_eq!(rendered.plain_text(), source);
    assert_eq!(rendered.styled_text(), source);
}

#[test]
fn wrapped_code_retains_source_whitespace_instead_of_prose_wrapping() {
    let capabilities = TerminalCapabilities::plain();
    let options = RenderOptions {
        code_overflow: CodeOverflow::Wrap,
        ..RenderOptions::default()
    };
    let renderer = RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        options,
    );
    let rendered = renderer.render(
        &Document::new(vec![Block::CodeBlock(CodeBlock::new("a  b  cdefgh"))]),
        12,
    );
    assert!(rendered.plain_text().contains("a  b  c"), "{rendered:?}");
    assert_eq!(rendered.copy_text, "a  b  cdefgh\n");
}

#[test]
fn semantic_statuses_have_plain_non_color_markers() {
    let document = Document::new(vec![Block::Paragraph(vec![
        Inline::status(StatusKind::Success, "saved"),
        Inline::Text(" ".into()),
        Inline::status(StatusKind::Warning, "check"),
        Inline::Text(" ".into()),
        Inline::status(StatusKind::Error, "failed"),
        Inline::Text(" ".into()),
        Inline::status(StatusKind::Pending, "waiting"),
    ])]);
    let rendered = RichRenderer::plain().render(&document, 80);
    assert_eq!(rendered.plain_text(), "+ saved ! check x failed . waiting");
    assert_eq!(rendered.copy_text, "+ saved ! check x failed . waiting\n");
}

#[test]
fn detail_blocks_have_non_color_expand_markers_and_copy_hidden_content() {
    let hidden = Block::Paragraph(vec![Inline::Text("hidden content".into())]);
    let collapsed = Document::new(vec![Block::Detail(DetailBlock::new(
        "Details",
        vec![hidden.clone()],
        false,
    ))]);
    let expanded = Document::new(vec![Block::Detail(DetailBlock::new(
        "Details",
        vec![hidden],
        true,
    ))]);
    let renderer = RichRenderer::plain();
    let collapsed_output = renderer.render(&collapsed, 40);
    assert!(collapsed_output.plain_text().starts_with("[+] Details"));
    assert!(!collapsed_output.plain_text().contains("hidden content"));
    assert!(collapsed_output.copy_text.contains("hidden content"));
    let expanded_output = renderer.render(&expanded, 40).plain_text();
    assert!(expanded_output.starts_with("[-] Details"));
    assert!(expanded_output.contains("hidden content"));
}

#[test]
fn tables_fall_back_at_narrow_widths() {
    let document =
        markdown::parse("| Key | Value |\n| --- | --- |\n| language | Rust |\n| status | green |");
    let narrow = RichRenderer::plain().render(&document, 10).plain_text();
    assert!(narrow.contains("Key:"));
    assert!(narrow.contains("language"));
    let wide = renderer(ColorDepth::TrueColor, true)
        .render(&document, 60)
        .plain_text();
    assert!(wide.contains('│'));
}

#[test]
fn diff_keeps_prefixes_in_plain_mode() {
    let diff = UnifiedDiff::parse("--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n same");
    let rendered = RichRenderer::plain().render_diff(
        &diff,
        20,
        DiffRenderOptions {
            line_numbers: true,
            wrap: false,
        },
    );
    let plain = rendered.plain_text();
    assert!(plain.contains("-old"));
    assert!(plain.contains("+new"));
    assert!(plain.contains("1 | -old"), "single gutter was {plain:?}");
    assert!(plain.contains("1 | +new"), "single gutter was {plain:?}");
    assert!(plain.lines().all(|line| {
        line.split_once('|')
            .is_none_or(|(gutter, _)| gutter.split_whitespace().count() <= 1)
    }));
    assert!(!rendered.styled_text().contains('\x1b'));
}

#[cfg(feature = "syntax-highlighting")]
#[test]
fn unstable_rendering_defers_syntax_work() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    let document = Document::new(vec![Block::CodeBlock(CodeBlock::with_language(
        "rust",
        "fn partial(",
    ))]);
    renderer.render_unstable(&document, 80);
    renderer.render_unstable(&document, 80);
    assert_eq!(renderer.syntax_cache_stats(), SyntaxCacheStats::default());
    renderer.render(&document, 80);
    assert_eq!(renderer.syntax_cache_stats().misses, 1);
}

#[cfg(feature = "syntax-highlighting")]
#[test]
fn syntax_data_is_cached_per_renderer() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    let document = Document::new(vec![Block::CodeBlock(CodeBlock::with_language(
        "rust",
        "fn main() {}",
    ))]);
    renderer.render(&document, 80);
    renderer.render(&document, 80);
    let stats = renderer.syntax_cache_stats();
    assert_eq!(stats.misses, 1);
    assert_eq!(stats.hits, 1);
}

// The pre-optimization expansion is deliberately retained as an independent
// oracle: tab stops carry across semantic runs, while graphemes are run-local.
fn reference_expand_run_tabs(renderer: &RichRenderer, runs: &[RichRun]) -> Vec<RichRun> {
    use unicode_segmentation::UnicodeSegmentation;

    let mut column = 0usize;
    runs.iter()
        .map(|run| {
            let expanded = renderer
                .options
                .width
                .expand_tabs(&run.text, column)
                .into_owned();
            for grapheme in expanded.graphemes(true) {
                if matches!(grapheme, "\n" | "\r") {
                    column = 0;
                } else {
                    column = column
                        .saturating_add(renderer.options.width.grapheme_width(grapheme, column));
                }
            }
            RichRun::new(expanded, run.style, run.link.clone())
        })
        .collect()
}

fn identity_path_runs(parts: &[&str]) -> Vec<RichRun> {
    parts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            let style = match index % 3 {
                0 => TextStyle::plain().bold(),
                1 => TextStyle::plain()
                    .foreground(Color::Rgb(12, 34, 56))
                    .background(Color::Rgb(65, 43, 21))
                    .italic(),
                _ => TextStyle::plain().underline(),
            };
            let link = match index % 3 {
                0 => Some("https://example.com/界?q=value".to_owned()),
                1 => Some("javascript:alert(1)".to_owned()),
                _ => None,
            };
            RichRun::new((*text).to_owned(), style, link)
        })
        .collect()
}

fn assert_same_runs(actual: &[RichRun], expected: &[RichRun]) {
    assert_eq!(actual.len(), expected.len(), "{actual:?} != {expected:?}");
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert_eq!(actual.text, expected.text, "run {index}");
        assert_eq!(actual.style, expected.style, "run {index}");
        assert_eq!(actual.link, expected.link, "run {index}");
    }
}

#[test]
fn no_tab_expansion_borrows_runs_at_all_workload_sizes() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    let parts = ["plain ", "界e\u{301}👩\u{200d}💻", "", "\r\nnext", "🇦", "🇧"];
    for count in [0, 1, 1_000, 10_000] {
        let input = (0..count)
            .map(|index| parts[index % parts.len()])
            .collect::<Vec<_>>();
        let runs = identity_path_runs(&input);
        let expanded = renderer.expand_run_tabs(&runs);
        let std::borrow::Cow::Borrowed(borrowed) = expanded else {
            panic!("no-tab input copied at {count} runs");
        };
        assert!(std::ptr::eq(borrowed, runs.as_slice()));
        assert_same_runs(borrowed, &reference_expand_run_tabs(&renderer, &runs));
    }
}

#[test]
fn tab_expansion_matches_reference_across_styles_links_and_unicode() {
    use crate::width::AmbiguousWidth;

    let parts = [
        "",
        "a",
        " ",
        "界",
        "e\u{301}",
        "·",
        "👩\u{200d}💻",
        "🇦",
        "🇧",
        "\t",
        "a\tb",
        "\n",
        "\r",
        "\r\n",
        "x\r\n\ty",
        "\u{301}",
    ];
    let mut renderer = renderer(ColorDepth::TrueColor, true);
    for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
        for tab_stop in [0, 1, 4, 8] {
            let mut options = renderer.options();
            options.width = WidthPolicy {
                ambiguous,
                tab_stop,
            };
            renderer.set_options(options);
            for first in parts {
                for second in parts {
                    // The final tab must honor columns established in earlier
                    // runs, including blank runs and the original CRLF behavior.
                    let runs = identity_path_runs(&[first, second, "", "\ttail"]);
                    let expanded = renderer.expand_run_tabs(&runs);
                    assert!(matches!(expanded, std::borrow::Cow::Owned(_)));
                    assert_same_runs(&expanded, &reference_expand_run_tabs(&renderer, &runs));
                }
            }
        }
    }
}

#[test]
fn fitting_owned_lines_preserve_run_text_and_link_allocations() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    let parts = ["plain ", "界e\u{301}👩\u{200d}💻", "🇦🇧·"];
    for count in [1, 1_000, 10_000] {
        let input = (0..count)
            .map(|index| parts[index % parts.len()])
            .collect::<Vec<_>>();
        let runs = identity_path_runs(&input);
        let width = renderer.runs_width(&runs);
        let allocation = runs.as_ptr();
        let text_and_links = runs
            .iter()
            .map(|run| {
                (
                    run.text.as_ptr(),
                    run.link.as_ref().map(|link| link.as_ptr()),
                )
            })
            .collect::<Vec<_>>();
        let clipped = renderer.clip_line(RichLine { runs }, width);
        assert_eq!(
            clipped.runs.as_ptr(),
            allocation,
            "copied {count} fitting runs"
        );
        for (run, (text, link)) in clipped.runs.iter().zip(text_and_links) {
            assert_eq!(run.text.as_ptr(), text);
            assert_eq!(run.link.as_ref().map(|value| value.as_ptr()), link);
        }
    }
}

// Encode the general clipping result directly, without routing it back through
// clip_line: an accidental second clipping must not hide a fast-path mismatch.
fn reference_encode_clipped_line(renderer: &RichRenderer, line: &RichLine) -> RenderedLine {
    let mut plain = String::new();
    let mut styled = String::new();
    for run in &line.runs {
        plain.push_str(&run.text);
        let styled_text = renderer.theme.apply_style(run.style, &run.text);
        if renderer.capabilities.hyperlinks {
            if let Some(target) = run
                .link
                .as_deref()
                .and_then(crate::sanitize::SafeUrl::parse)
            {
                styled.push_str("\x1b]8;;");
                styled.push_str(target.as_str());
                styled.push_str("\x1b\\");
                styled.push_str(&styled_text);
                styled.push_str("\x1b]8;;\x1b\\");
                continue;
            }
        }
        styled.push_str(&styled_text);
    }
    RenderedLine { styled, plain }
}

#[test]
fn owned_line_clipping_matches_general_path_and_encoded_bytes() {
    use crate::width::AmbiguousWidth;

    let cases: &[&[&str]] = &[
        &[],
        &[""],
        &["", ""],
        &["", "content", ""],
        &["plain ", "styled", " text"],
        &["longidentifierabcdefghijklmno"],
        &["\t", "a\tb", "\t"],
        &["a", "\t", "界\tend"],
        &["first\nsecond"],
        &["first\rsecond"],
        &["first\r\nsecond"],
        &["first\r", "\nsecond"],
        &["\n", ""],
        &["\r", "\t"],
        &["界", "·", "e\u{301}"],
        &["e", "\u{301}"],
        &["👩", "\u{200d}", "💻"],
        &["🇦", "🇧"],
        &["\u{301}"],
        &["\u{200d}"],
        &["♥\u{fe0f}", "ำຳ"],
    ];
    for (color, unicode) in [
        (ColorDepth::TrueColor, true),
        (ColorDepth::Ansi16, true),
        (ColorDepth::None, false),
    ] {
        for hyperlinks in [false, true] {
            let capabilities = TerminalCapabilities::interactive(color, unicode).with_overrides(
                &CapabilityOverrides {
                    hyperlinks: Some(hyperlinks),
                    ..CapabilityOverrides::default()
                },
            );
            let mut renderer = RichRenderer::new(
                Theme::with_capabilities(capabilities),
                capabilities,
                RenderOptions::default(),
            );
            for ambiguous in [AmbiguousWidth::Narrow, AmbiguousWidth::Wide] {
                for tab_stop in [0, 4, 8] {
                    let mut options = renderer.options();
                    options.width = WidthPolicy {
                        ambiguous,
                        tab_stop,
                    };
                    renderer.set_options(options);
                    for parts in cases {
                        let runs = identity_path_runs(parts);
                        for width in [0, 1, 2, 3, 7, 16, 120] {
                            let expected = renderer.clip_runs(&runs, width);
                            let actual = renderer.clip_line(RichLine { runs: runs.clone() }, width);
                            assert_same_runs(&actual.runs, &expected.runs);
                            assert_eq!(
                                renderer.encode_line(RichLine { runs: runs.clone() }, width),
                                reference_encode_clipped_line(&renderer, &expected),
                                "width={width} parts={parts:?} policy={:?}",
                                renderer.options.width,
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn retained_rich_prefix_charges_ownership_for_borrowed_and_expanded_runs() {
    let renderer = renderer(ColorDepth::TrueColor, true);
    for text in ["no tabs 界e\u{301}", "tabs\t界\tend"] {
        let mut prefix = vec![
            Inline::strong(text),
            Inline::link("docs", "https://example.com"),
        ];
        let runs = renderer.inline_runs(&prefix, renderer.theme.style(TextRole::Text));
        let expected_copy = super::lines::run_bytes(&runs)
            + super::lines::run_bytes(&reference_expand_run_tabs(&renderer, &runs));
        prefix.push(Inline::Raw(String::new()));
        let block = Block::Paragraph(prefix);
        let mut cache = AppendOnlyTail::default();
        let mut stats = StreamingLayoutStats::default();
        let mut output = Vec::new();
        // Zero width isolates prefix ownership from row encoding/copy work.
        cache
            .update(&block, &renderer, 0, &mut output, &mut stats)
            .unwrap();
        assert_eq!(stats.copied_bytes, expected_copy as u64);
        assert_eq!(stats.rich_prefix_layouts, 1);
        cache
            .update(&block, &renderer, 0, &mut output, &mut stats)
            .unwrap();
        assert_eq!(
            stats.copied_bytes, expected_copy as u64,
            "unchanged prefix recopied"
        );
        assert_eq!(stats.rich_prefix_layouts, 1);
    }
}
