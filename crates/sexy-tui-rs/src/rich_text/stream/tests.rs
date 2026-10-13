//! Unit tests for `crate::rich_text::stream`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::rich_text::stream`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::rich_text::render::RichRenderer;
use crate::{ColorDepth, TerminalCapabilities, Theme};

const ADVERSARIAL: &str = "# Heading\n\nA **strong** link to [docs](https://example.com) and `code`.\n\n- first\n  - nested\n- second\n\n```rust\nfn main() {\n    println!(\"界\");\n}\n```\n";

#[test]
fn finalized_constructor_parses_once_without_streaming_work() {
    let cases = [
        "",
        ADVERSARIAL,
        "[late][ref]\n\n| a | b |\n|---|---|\n| 界 | 👩‍💻 |\n\n[ref]: https://example.com\n",
        "```mermaid\ngraph LR\n A --> B\n```\n\n$\\frac{1}{2}$\n",
        "unterminated ```\r\n\t\x1b[2J\u{202e}e\u{301}",
    ];
    for text in cases {
        let mut stream = StreamingMarkdown::from_finalized_text(text);
        assert_eq!(stream.raw_bytes(), text.as_bytes());
        assert_eq!(stream.raw_text(), text);
        assert_eq!(stream.committed(), &markdown::parse(text));
        assert!(stream.is_finished());
        assert!(stream.unstable_source().is_empty());
        let stats = stream.stats();
        assert_eq!(stats.parse_passes, 1);
        assert_eq!(stats.reparsed_bytes, text.len() as u64);
        assert_eq!(stats.fence_scanned_bytes, 0);
        assert_eq!(stats.preview_scanned_bytes, 0);
        assert_eq!(stats.preview_copied_bytes, 0);
        stream.finish();
        stream.push_str("ignored after completion");
        assert_eq!(stream.stats(), stats);
        assert_eq!(stream.raw_text(), text);
    }
}

fn prior_list_marker(line: &str) -> bool {
    line.starts_with("- ")
        || line.starts_with("* ")
        || line.starts_with("+ ")
        || line
            .split_once(". ")
            .is_some_and(|(number, _)| number.chars().all(|character| character.is_ascii_digit()))
}

fn prior_lexical_stable_offset(source: &str) -> Option<usize> {
    let offset = source.rfind("\n\n")?.saturating_add(2);
    if offset >= source.len() {
        return None;
    }
    let first = source.lines().next().unwrap_or_default().trim_start();
    let candidate = source[offset..]
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start();
    let same_quote = first.starts_with('>') && candidate.starts_with('>');
    let possible_list_continuation = prior_list_marker(first)
        && (prior_list_marker(candidate)
            || candidate.starts_with("  ")
            || candidate.starts_with('\t'));
    let possible_indented_code = (first.starts_with("    ") || first.starts_with('\t'))
        && (candidate.starts_with("    ") || candidate.starts_with('\t'));
    let possible_html_block = first.starts_with('<');
    if same_quote || possible_list_continuation || possible_indented_code || possible_html_block {
        None
    } else {
        Some(offset)
    }
}

#[test]
fn lexical_prefix_checks_preserve_prior_semantics_without_scanning_line_bodies() {
    let cases = [
        "",
        " ",
        "\t\u{2003}",
        "\n- x",
        "\r\n> x",
        "\u{2003}\n<tag>",
        "> quote",
        "<tag>",
        "- item",
        "* item",
        "+ item",
        ". item",
        "1. item",
        "12345678901234567890. item",
        "1.. item",
        "  plain text",
        "\t- item",
        "1.\n item",
    ];
    for first in cases {
        assert_eq!(is_list_marker(first, &mut 0), prior_list_marker(first));
        for candidate in cases {
            let mut source = format!("{first}\n\n{candidate}");
            let mut cache = None;
            for suffix in [
                "",
                " more",
                "\n\n- item",
                "\n\n. item",
                "\n\n\t> quote",
                "\n\n",
            ] {
                source.push_str(suffix);
                assert_eq!(
                    lexical_stable_offset(&source, &mut cache, &mut 0),
                    prior_lexical_stable_offset(&source),
                    "{source:?}"
                );
            }
        }
    }
    let mut seed = 71u64;
    let alphabet = [
        'a', '1', '9', '.', ' ', '\t', '\n', '\r', '\u{2003}', '-', '*', '+', '>', '<', '界',
    ];
    for _ in 0..2_000 {
        let mut source = String::new();
        for _ in 0..40 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            source.push(alphabet[seed as usize % alphabet.len()]);
        }
        assert_eq!(is_list_marker(&source, &mut 0), prior_list_marker(&source));
        source.push_str("\n\n- item");
        assert_eq!(
            lexical_stable_offset(&source, &mut None, &mut 0),
            prior_lexical_stable_offset(&source)
        );
    }
    // A rejected marker must not search a long ordinary line for a later
    // '. '. An empty numeric prefix remains accepted, matching the old rule.
    let ordinary = format!("word{}. item", "a".repeat(100_000));
    let mut scanned = 0;
    assert!(!is_list_marker(&ordinary, &mut scanned));
    assert_eq!(scanned, 1);
    assert!(is_list_marker(". item", &mut 0));
}

#[test]
fn huge_lexical_first_line_is_classified_once_including_whitespace_and_digits() {
    for first in [
        format!("- {}", "a".repeat(100_000)),
        format!("{}- item", "\u{2003}".repeat(40_000)),
        format!("{}. item", "1".repeat(100_000)),
        format!(". {}", "a".repeat(100_000)),
    ] {
        let mut stream = StreamingMarkdown::from_text(&first);
        for step in 0..1_000 {
            let before = stream.stats().lexical_scanned_bytes;
            stream.push_str("\n\n- x");
            let work = stream.stats().lexical_scanned_bytes - before;
            if step > 0 {
                assert_eq!(work, 9, "step={step}");
            } else {
                assert!(work <= first.len() as u64 + 16);
            }
        }
        let stats = stream.stats();
        assert!(
            stats.lexical_scanned_bytes <= first.len() as u64 + 9_016,
            "{stats:?}"
        );
        assert!(stream.committed().blocks.is_empty());
        let expected = first + &"\n\n- x".repeat(1_000);
        assert_eq!(stream.raw_bytes(), expected.as_bytes());
        let [Block::Plain(text)] = stream.preview().blocks.as_slice() else {
            panic!("literal preview");
        };
        assert_eq!(text, &expected);
        assert_eq!(stream.copy_text(), expected.clone() + "\n");
        assert_eq!(stream.finish(), &markdown::parse(&expected));
        eprintln!(
            "lexical source={} scan_bytes={}",
            expected.len(),
            stats.lexical_scanned_bytes
        );
    }
}

#[test]
fn lexical_first_line_cache_is_reset_when_the_tail_prefix_commits() {
    let mut stream = StreamingMarkdown::from_text(&format!("- {}", "a".repeat(100_000)));
    stream.push_str("\n\n- x");
    assert!(stream.lexical_first.is_some());
    stream.push_str("\n\nordinary");
    assert!(stream.lexical_first.is_none());
    let committed = stream.committed().blocks.len();
    stream.push_str(&"b".repeat(100_000));
    stream.push_str("\n\n- new list");
    assert!(stream.committed().blocks.len() > committed);
    let expected = markdown::parse(stream.raw_text());
    assert_eq!(stream.finish(), &expected);
}

#[test]
fn fence_search_and_literal_preview_copy_are_append_local() {
    for chunk in ["abcdefgh", "word line\n", "界 e\u{301} "] {
        let mut stream = StreamingMarkdown::new();
        let mut total = 0;
        for _ in 0..30_000 {
            stream.push_str(chunk);
            total += chunk.len();
        }
        let stats = stream.stats();
        assert!(stats.fence_scanned_bytes <= 2 * total as u64, "{stats:?}");
        assert!(stats.preview_copied_bytes <= 3 * total as u64, "{stats:?}");
        assert!(
            stats.preview_classified_bytes <= 3 * total as u64,
            "{stats:?}"
        );
        let expected = chunk.repeat(30_000);
        assert_eq!(stream.raw_bytes(), expected.as_bytes());
        if chunk == "word line\n" {
            let [Block::Paragraph(content)] = stream.preview().blocks.as_slice() else {
                panic!("expected canonical prose preview");
            };
            assert!(matches!(content.as_slice(), [Inline::Raw(_)]));
            assert_eq!(stream.copy_text(), markdown::parse(&expected).plain_text());
        } else {
            let [Block::Plain(text)] = stream.preview().blocks.as_slice() else {
                panic!("expected literal preview");
            };
            assert_eq!(text, stream.unstable_source());
        }
        assert_eq!(stream.finish(), &markdown::parse(&expected));
    }
}

fn tail_work(chunks: usize, fence: bool, multiline: bool, wrap: bool) -> StreamingLayoutStats {
    let mut stream = StreamingMarkdown::new();
    let mut renderer = RichRenderer::plain();
    let mut options = renderer.options();
    options.code_overflow = if wrap {
        super::super::render::CodeOverflow::Wrap
    } else {
        super::super::render::CodeOverflow::Clip
    };
    renderer.set_options(options);
    let mut cache = StreamingRenderCache::default();
    if fence {
        stream.push_str("```rust\n");
    }
    let chunk = if multiline {
        "some words and source text\n"
    } else {
        "word xyz "
    };
    let mut frame = Vec::new();
    for _ in 0..chunks {
        stream.push_str(chunk);
        let update = cache.render_line_update(&stream, &renderer, 40, false);
        assert!(update.stable_prefix <= frame.len());
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
    }
    let stats = cache.stats();
    let expected = renderer.render_unstable(stream.preview(), 40).plain_lines();
    assert_eq!(frame, expected);
    assert_eq!(
        stream.raw_text(),
        format!(
            "{}{}",
            if fence { "```rust\n" } else { "" },
            chunk.repeat(chunks)
        )
    );
    let copy = cache.render(&stream, &renderer, 40).copy_text;
    assert_eq!(copy, renderer.sanitize_copy(&stream.copy_text()));
    assert!(copy.contains(chunk.trim()));
    let raw = stream.raw_text().to_owned();
    assert_eq!(stream.finish(), &markdown::parse(&raw));
    let final_update = cache.render_line_update(&stream, &renderer, 40, false);
    assert_eq!(final_update.stable_prefix, 0);
    assert_eq!(
        final_update.replacement,
        renderer.render(stream.committed(), 40).plain_lines()
    );
    stats
}

#[test]
fn open_paragraph_rows_are_provisional_until_a_following_block_commits_them() {
    let renderer = RichRenderer::plain();
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    let mut frame = Vec::new();

    stream.push_str("first paragraph has enough words to wrap across rows\n");
    let update = cache.render_line_update(&stream, &renderer, 40, false);
    assert_eq!(update.stable_prefix, 0);
    frame.extend(update.replacement);
    assert!(frame.len() >= 2);
    assert!(stream.committed().blocks.is_empty());
    assert_eq!(cache.committed_rows(), 0);

    let previous = frame.clone();
    stream.push_str("continuation adds more words\n");
    let update = cache.render_line_update(&stream, &renderer, 40, false);
    assert!(update.stable_prefix < previous.len());
    assert_eq!(cache.committed_rows(), 0);
    frame.truncate(update.stable_prefix);
    frame.extend(update.replacement);
    assert_ne!(frame, previous);
    assert_eq!(
        frame,
        renderer
            .render(&markdown::parse(stream.raw_text()), 40)
            .plain_lines()
    );

    // A trailing blank line proves the paragraph boundary, but the
    // paragraph is not committed until a following block is present.
    stream.push_str("\n");
    let update = cache.render_line_update(&stream, &renderer, 40, false);
    frame.truncate(update.stable_prefix);
    frame.extend(update.replacement);
    assert!(stream.committed().blocks.is_empty());
    assert_eq!(cache.committed_rows(), 0);
    assert_eq!(
        frame,
        renderer
            .render(&markdown::parse(stream.raw_text()), 40)
            .plain_lines()
    );

    let previous = frame.clone();
    stream.push_str("next paragraph starts here\n");
    let update = cache.render_line_update(&stream, &renderer, 40, false);
    frame.truncate(update.stable_prefix);
    frame.extend(update.replacement);
    assert!(!stream.committed().blocks.is_empty());
    assert!(cache.committed_rows() > 0);
    assert!(cache.committed_rows() <= previous.len());
    assert_eq!(
        &frame[..cache.committed_rows()],
        &previous[..cache.committed_rows()]
    );
    assert_eq!(
        frame,
        renderer
            .render(&markdown::parse(stream.raw_text()), 40)
            .plain_lines()
    );
}

#[test]
fn tabbed_open_code_layout_work_grows_linearly() {
    fn work(chunks: usize, multiline: bool, wrap: bool) -> StreamingLayoutStats {
        let mut stream = StreamingMarkdown::new();
        let mut renderer = RichRenderer::plain();
        let mut options = renderer.options();
        options.code_overflow = if wrap {
            super::super::render::CodeOverflow::Wrap
        } else {
            super::super::render::CodeOverflow::Clip
        };
        renderer.set_options(options);
        let mut cache = StreamingRenderCache::default();
        let mut frame = Vec::new();
        stream.push_str("```rust\n");
        for _ in 0..chunks {
            // Split an EGC before a tab; tab stops must use its final width.
            for chunk in [
                "\t界 e",
                "\u{301}\t\x1b[31m",
                if multiline { "\n" } else { " " },
            ] {
                stream.push_str(chunk);
                let update = cache.render_line_update(&stream, &renderer, 40, false);
                frame.truncate(update.stable_prefix);
                frame.extend(update.replacement);
            }
        }
        assert_eq!(
            frame,
            renderer.render_unstable(stream.preview(), 40).plain_lines()
        );
        let stats = cache.stats();
        assert_eq!(stats.literal_transform_fallbacks, 0, "{stats:?}");
        assert_eq!(stats.full_tail_layouts, 0, "{stats:?}");
        let source = stream.raw_text().to_owned();
        stream.finish();
        assert_eq!(
            cache
                .render_line_update(&stream, &renderer, 40, false)
                .replacement,
            renderer.render(&markdown::parse(&source), 40).plain_lines()
        );
        stats
    }
    for multiline in [false, true] {
        for wrap in [false, true] {
            let small = work(256, multiline, wrap);
            let large = work(512, multiline, wrap);
            let processed = |s: StreamingLayoutStats| {
                s.checked_bytes
                    + s.measured_bytes
                    + s.laid_out_bytes
                    + s.copied_bytes
                    + s.fallback_source_bytes
            };
            assert!(
                processed(large) <= processed(small) * 5 / 2,
                "{small:?} -> {large:?}"
            );
        }
    }
}

#[test]
fn long_plain_and_open_code_layout_work_grows_linearly() {
    for fence in [false, true] {
        for multiline in [false, true] {
            for wrap in [false, true] {
                let small = tail_work(4_000, fence, multiline, wrap);
                let large = tail_work(8_000, fence, multiline, wrap);
                let work = |s: StreamingLayoutStats| {
                    s.checked_bytes + s.measured_bytes + s.laid_out_bytes + s.fallback_source_bytes
                };
                assert!(
                    work(large) <= work(small) * 5 / 2,
                    "{fence} {multiline} {wrap}: {small:?} -> {large:?}"
                );
                assert!(large.encoded_rows < 8_000 * 12, "{large:?}");
                assert!(large.laid_out_bytes < 8_000 * 256, "{large:?}");
                eprintln!("tail_work fence={fence} multiline={multiline} wrap={wrap}: {small:?} -> {large:?}");
            }
        }
    }
}

#[test]
fn random_byte_chunks_resize_theme_and_finalization_match_authoritative_rows() {
    use crate::rich_text::render::CodeOverflow;
    let cases = [
        "plain 界 words e\u{301} 👩\u{200d}💻 with wraps and more ordinary text ".repeat(8),
        format!(
            "```rust\n{}",
            "  let 界 = e\u{301}; // 👩\u{200d}💻\n\n".repeat(12)
        ),
        "# head\n\nplain\ttext\r\n\x1b[31m\u{202e} more\n\n```text\nhello\tworld\r\n\n```\n\nend"
            .to_owned(),
        "```text\nold\n```\n```rust\nnew\n".to_owned(),
        format!(
            "```rust\n{}",
            "\t界 e\u{301}\t👩\u{200d}💻\t\x1b[31m\u{202e}\n".repeat(12)
        ),
    ];
    for source in cases {
        for seed in 1..=4u64 {
            let caps = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
            let mut renderer = RichRenderer::new(
                Theme::with_capabilities(caps),
                caps,
                RenderOptions::default(),
            );
            let mut stream = StreamingMarkdown::new();
            let mut cache = StreamingRenderCache::default();
            let mut frame = Vec::new();
            let mut rng = seed;
            let mut offset = 0;
            let mut step = 0;
            while offset < source.len() {
                rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
                let end = (offset + 1 + (rng as usize % 23)).min(source.len());
                stream.push_bytes(&source.as_bytes()[offset..end]);
                offset = end;
                step += 1;
                let width = [1, 13, 40, 0, 80][step / 11 % 5];
                if step % 17 == 0 {
                    renderer
                        .theme_mut()
                        .set_accent(crate::Color::Rgb(1, step as u8, 90));
                }
                let mut options = renderer.options();
                options.code_overflow = if step / 13 % 2 == 0 {
                    CodeOverflow::Clip
                } else {
                    CodeOverflow::Wrap
                };
                options.code_borders = step / 19 % 2 == 0;
                renderer.set_options(options);
                let styled = step / 7 % 2 == 0;
                let update = cache.render_line_update(&stream, &renderer, width, styled);
                assert!(update.stable_prefix <= frame.len());
                frame.truncate(update.stable_prefix);
                frame.extend(update.replacement);
                let mut expected = renderer.render_blocks_only(&stream.committed().blocks, width);
                let tail = renderer.render_unstable(stream.preview(), width).lines;
                if !expected.is_empty() && !tail.is_empty() {
                    expected.push(RenderedLine::default());
                }
                expected.extend(tail);
                let expected: Vec<_> = expected
                    .into_iter()
                    .map(|line| if styled { line.styled } else { line.plain })
                    .collect();
                assert_eq!(frame, expected, "seed={seed} step={step} source={source:?}");
            }
            assert_eq!(stream.raw_bytes(), source.as_bytes());
            assert_eq!(stream.finish(), &markdown::parse(&source));
            let final_update = cache.render_line_update(&stream, &renderer, 40, true);
            assert_eq!(final_update.stable_prefix, 0);
            assert_eq!(
                final_update.replacement,
                renderer.render(stream.committed(), 40).styled_lines()
            );
        }
    }
}

#[test]
fn committed_blocks_are_a_monotonic_prefix_of_the_final_document() {
    let expected = markdown::parse(ADVERSARIAL);
    let mut stream = StreamingMarkdown::new();
    for byte in ADVERSARIAL.as_bytes() {
        stream.push_bytes(&[*byte]);
        assert!(expected.blocks.starts_with(&stream.committed().blocks));
    }
}

#[test]
fn every_utf8_chunk_boundary_finishes_like_static_parsing() {
    let expected = markdown::parse(ADVERSARIAL);
    for split in 0..=ADVERSARIAL.len() {
        if !ADVERSARIAL.is_char_boundary(split) {
            continue;
        }
        let mut stream = StreamingMarkdown::new();
        stream.push_str(&ADVERSARIAL[..split]);
        stream.push_str(&ADVERSARIAL[split..]);
        assert_eq!(stream.finish(), &expected, "split at {split}");
        assert_eq!(stream.raw_bytes(), ADVERSARIAL.as_bytes());
    }
}

#[test]
fn arbitrary_byte_chunks_buffer_utf8_and_never_panic() {
    let mut stream = StreamingMarkdown::new();
    for byte in ADVERSARIAL.as_bytes() {
        stream.push_bytes(&[*byte]);
    }
    assert_eq!(stream.finish(), &markdown::parse(ADVERSARIAL));
    assert_eq!(stream.stats().pending_utf8_bytes, 0);
}

#[test]
fn complete_fence_in_one_chunk_is_parsed_without_waiting_for_finish() {
    let mut stream = StreamingMarkdown::new();
    stream.push_str("```rust\nfn main() {}\n```\n");
    assert!(matches!(
        stream.preview().blocks.as_slice(),
        [Block::CodeBlock(_)]
    ));
}

#[test]
fn incomplete_inline_and_block_syntax_is_mutable_not_permanently_wrong() {
    let mut stream = StreamingMarkdown::new();
    stream.push_str("Opening **strong");
    assert!(stream.preview().plain_text().contains("**strong"));
    stream.push_str("**\n\n```ru");
    stream.push_str("st\nfn main()");
    assert!(stream.preview().plain_text().contains("fn main"));
    stream.push_str(" {}\n```\n");
    let final_document = stream.finish().clone();
    assert_eq!(final_document, markdown::parse(stream.raw_text()));
    assert!(!final_document.plain_text().contains("**"));
}

#[test]
fn complete_inline_markdown_becomes_rich_before_a_newline_or_finish() {
    let mut stream = StreamingMarkdown::new();
    stream.push_str("**Plan");
    assert!(stream.preview().plain_text().contains("**Plan"));
    stream.push_str("ning**");
    assert_eq!(stream.preview().plain_text(), "Planning\n");
    assert!(matches!(
        stream.preview().blocks.as_slice(),
        [Block::Paragraph(content)]
            if matches!(content.as_slice(), [crate::rich_text::Inline::Strong(_)])
    ));

    stream.push_str(" and `testing`");
    assert_eq!(stream.preview().plain_text(), "Planning and testing\n");
    let rendered = RichRenderer::plain()
        .render_unstable(stream.preview(), 80)
        .plain_text();
    assert!(!rendered.contains("**"));
    assert!(!rendered.contains('`'));
}

#[test]
fn malformed_utf8_and_escape_sequences_are_recoverable_and_safe() {
    let mut stream = StreamingMarkdown::new();
    stream.push_bytes(b"safe \xf0\x9f");
    assert_eq!(stream.stats().pending_utf8_bytes, 2);
    stream.push_bytes(b"\x92\xa1 \x1b]52;c;bad\x07");
    stream.finish();
    assert_eq!(
        stream.raw_bytes(),
        b"safe \xf0\x9f\x92\xa1 \x1b]52;c;bad\x07"
    );
    let rendered = RichRenderer::plain()
        .render(stream.committed(), 80)
        .styled_text();
    assert!(!rendered.contains('\x1b'));
    assert!(!rendered.contains('\x07'));
}

#[test]
fn long_single_paragraph_uses_geometric_not_per_line_reparsing() {
    let mut stream = StreamingMarkdown::new();
    for _ in 0..20_000 {
        stream.push_str("word line\n");
    }
    assert!(stream.stats().reparsed_bytes < 4 * MAX_UNSTABLE_PARSE_BYTES as u64);
    stream.finish();
}

#[test]
fn unstable_reparse_is_bounded_for_huge_open_blocks() {
    let mut stream = StreamingMarkdown::new();
    stream.push_str("```text\n");
    for _ in 0..10_000 {
        stream.push_str("a long code line\n");
    }
    let before_finish = stream.stats();
    assert!(before_finish.reparsed_bytes < 2 * MAX_UNSTABLE_PARSE_BYTES as u64);
    assert!(stream.preview().plain_text().contains("a long code line"));
    stream.push_str("```\n");
    stream.finish();
    assert_eq!(stream.committed(), &markdown::parse(stream.raw_text()));
}

#[test]
fn semantic_commit_segments_remap_across_widths() {
    let renderer = RichRenderer::plain();
    for (source, expected_segments) in [
        ("- alpha item\n- beta item\n- gamma item\n", 3),
        (
            "| Name | Value |\n|---|---|\n| alpha | one |\n| beta | two |\n| gamma | three |\n",
            3,
        ),
    ] {
        let mut stream = StreamingMarkdown::from_text(source);
        stream.finish();
        let mut cache = StreamingRenderCache::default();

        cache.render(&stream, &renderer, 80);
        let wide = cache.committed_block_ends().to_vec();
        assert_eq!(wide.len(), expected_segments);
        assert_eq!(wide.last().copied(), Some(cache.committed_rows()));
        assert!(wide.windows(2).all(|ends| ends[0] < ends[1]));

        cache.render(&stream, &renderer, 12);
        let narrow = cache.committed_block_ends().to_vec();
        assert_eq!(narrow.len(), expected_segments);
        assert_eq!(narrow.last().copied(), Some(cache.committed_rows()));
        assert!(narrow.windows(2).all(|ends| ends[0] < ends[1]));
    }
}

#[test]
fn line_update_reuses_committed_rows_and_replaces_only_the_mutable_tail() {
    let renderer = RichRenderer::plain();
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();

    stream.push_str("# Stable heading\n\nmutable");
    let first = cache.render_line_update(&stream, &renderer, 40, false);
    assert_eq!(first.stable_prefix, 0);
    let mut frame = first.replacement;

    stream.push_str(" tail");
    let next = cache.render_line_update(&stream, &renderer, 40, false);
    assert!(next.stable_prefix > 0, "{next:?}");
    frame.truncate(next.stable_prefix);
    frame.extend(next.replacement);

    let mut full_cache = StreamingRenderCache::default();
    assert_eq!(
        frame,
        full_cache.render_lines(&stream, &renderer, 40, false)
    );

    let resized = cache.render_line_update(&stream, &renderer, 20, false);
    assert_eq!(resized.stable_prefix, 0);
}

#[test]
fn full_lines_then_incremental_update_retains_the_selected_prefix() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let renderer = RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions::default(),
    );
    for styled in [false, true] {
        let mut stream = StreamingMarkdown::from_text("# Stable heading\n\nmutable");
        let mut cache = StreamingRenderCache::default();
        let mut frame = cache.render_lines(&stream, &renderer, 40, styled);
        stream.push_str(" tail");
        let update = cache.render_line_update(&stream, &renderer, 40, styled);
        assert!(update.stable_prefix > 0, "{update:?}");
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
        let mut reference = StreamingRenderCache::default();
        assert_eq!(
            frame,
            reference.render_lines(&stream, &renderer, 40, styled)
        );

        // A full render also establishes which representation the next
        // delta must preserve. Switching it still invalidates every row.
        cache.render_lines(&stream, &renderer, 40, !styled);
        stream.push_str(" again");
        let changed = cache.render_line_update(&stream, &renderer, 40, styled);
        assert_eq!(changed.stable_prefix, 0);
        assert_eq!(
            changed.replacement,
            reference.render_lines(&stream, &renderer, 40, styled)
        );
    }
}

#[test]
fn lines_only_render_matches_the_selected_document_lines() {
    let capabilities = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let renderer = RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions::default(),
    );
    let mut stream = StreamingMarkdown::new();
    stream.push_str("**committed**\n\nmutable");
    let mut document_cache = StreamingRenderCache::default();
    let mut lines_cache = StreamingRenderCache::default();

    let document = document_cache.render(&stream, &renderer, 80);
    let plain = lines_cache.render_lines(&stream, &renderer, 80, false);
    assert_eq!(
        plain,
        document
            .lines
            .iter()
            .map(|line| line.plain.clone())
            .collect::<Vec<_>>()
    );

    let styled = lines_cache.render_lines(&stream, &renderer, 80, true);
    assert_eq!(
        styled,
        document
            .lines
            .iter()
            .map(|line| line.styled.clone())
            .collect::<Vec<_>>()
    );

    stream.push_str(" tail");
    let next_document = document_cache.render(&stream, &renderer, 80);
    let next_plain = lines_cache.render_lines(&stream, &renderer, 80, false);
    assert_eq!(
        next_plain,
        next_document
            .lines
            .iter()
            .map(|line| line.plain.clone())
            .collect::<Vec<_>>()
    );
}

#[test]
fn render_cache_reflows_on_resize_and_tracks_latest_tail() {
    let mut stream = StreamingMarkdown::new();
    let renderer = RichRenderer::plain();
    let mut cache = StreamingRenderCache::default();
    stream.push_str("first paragraph\n\nsecond");
    let wide = cache.render(&stream, &renderer, 80).plain_text();
    assert!(wide.contains("second"));
    let narrow = cache.render(&stream, &renderer, 10).plain_text();
    assert!(narrow.lines().all(|line| line.len() <= 10));
}
#[test]
fn parser_thresholds_preserve_canonical_prose_geometry() {
    let mut stream = StreamingMarkdown::new();
    let renderer = RichRenderer::plain();
    let mut cache = StreamingRenderCache::default();
    let mut source = String::new();
    let mut frame = Vec::new();
    for _ in 1..=400 {
        let previous = frame.clone();
        let chunk = "word line\n";
        source.push_str(chunk);
        stream.push_str(chunk);
        let update = cache.render_line_update(&stream, &renderer, 40, false);
        assert!(update.stable_prefix <= previous.len());
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
        assert_eq!(
            &previous[..update.stable_prefix],
            &frame[..update.stable_prefix]
        );
        assert_eq!(
            frame,
            renderer.render(&markdown::parse(&source), 40).plain_lines(),
            "source length {}",
            source.len()
        );
    }
    assert_eq!(stream.finish(), &markdown::parse(&source));
}

#[test]
fn semantic_preview_does_not_demote_at_newlines_or_inline_budget() {
    let mut stream = StreamingMarkdown::from_text("**bold**");
    for chunk in ["\n", "next", "\n", "more"] {
        stream.push_str(chunk);
        assert!(!stream.preview().plain_text().contains("**"));
    }
    stream.push_str(&"x".repeat(MAX_LIVE_INLINE_PREVIEW_BYTES));
    for chunk in ["z", "\n", "after"] {
        stream.push_str(chunk);
        assert!(!stream.preview().plain_text().contains("**"));
        assert!(stream.preview().plain_text().contains(chunk.trim()));
    }
    let expected = markdown::parse(stream.raw_text());
    assert_eq!(stream.finish(), &expected);
}

#[test]
fn ambiguous_fence_lines_are_withheld_but_raw_and_copy_remain_complete() {
    for fence in ["```", "~~~"] {
        for indent in ["", " ", "  ", "   "] {
            let opening = format!("{indent}{fence}rust");
            let mut stream = StreamingMarkdown::new();
            for byte in opening.as_bytes() {
                stream.push_bytes(&[*byte]);
                assert!(stream.preview().is_empty());
                assert_eq!(stream.copy_text(), stream.raw_text());
            }
            stream.push_str("\r\nbody\n");
            let before = stream.preview().clone();
            for byte in format!("{indent}{fence}").as_bytes() {
                stream.push_bytes(&[*byte]);
                assert_eq!(stream.preview(), &before);
                assert!(stream
                    .copy_text()
                    .ends_with(&stream.unstable_source()[stream.preview_source_len..]));
            }
            stream.push_str("\r\n");
            let expected = markdown::parse(stream.raw_text());
            assert_eq!(stream.finish(), &expected);
        }
    }
}

#[test]
fn structural_candidate_classification_is_append_local_and_disambiguates() {
    for opening in ["", "```\n"] {
        let mut stream = StreamingMarkdown::from_text(opening);
        for _ in 0..1024 {
            stream.push_str("````````");
        }
        assert!(stream.stats().preview_scanned_bytes <= stream.raw_bytes().len() as u64);
        stream.push_str("x");
        if !opening.is_empty() {
            assert!(stream.preview().plain_text().contains('x'));
        }
        let expected = markdown::parse(stream.raw_text());
        assert_eq!(stream.finish(), &expected);
    }
}

#[test]
fn completed_table_rows_and_code_geometry_remain_stable() {
    let mut renderer = RichRenderer::plain();
    let mut options = renderer.options();
    options.stable_block_geometry = true;
    options.code_borders = true;
    options.code_overflow = super::super::render::CodeOverflow::Wrap;
    renderer.set_options(options);
    let mut table =
        StreamingMarkdown::from_text("| A | B |\n|---|---|\n| x | abcdefghijklmnop |\n");
    let before = renderer.render_unstable(table.preview(), 30).plain_lines();
    table.push_str("| supercalifragilistic");
    assert_eq!(
        renderer.render_unstable(table.preview(), 30).plain_lines(),
        before
    );
    assert!(table.copy_text().contains("supercalifragilistic"));
    table.push_str(" | z |\n");
    let after = renderer.render_unstable(table.preview(), 30).plain_lines();
    assert_eq!(&before[..before.len() - 1], &after[..before.len() - 1]);

    let mut code = StreamingMarkdown::from_text("```rust\nx\n");
    let mut cache = StreamingRenderCache::default();
    let before = cache.render_lines(&code, &renderer, 30, false);
    code.push_str("a much longer code line");
    let after = cache.render_lines(&code, &renderer, 30, false);
    assert_eq!(&before[..before.len() - 1], &after[..before.len() - 1]);
}
#[test]
fn completed_backtick_inline_code_disambiguates_a_possible_fence() {
    let mut stream = StreamingMarkdown::new();
    for chunk in ["```", "hello", "```"] {
        stream.push_str(chunk);
    }
    assert_eq!(stream.preview().plain_text(), "hello\n");
    stream.push_str("\n");
    assert_eq!(stream.preview().plain_text(), "hello\n");
}

#[test]
fn withheld_table_cells_do_not_invalidate_layout() {
    let mut stream = StreamingMarkdown::from_text("| A | B |\n|---|---|\n");
    let renderer = RichRenderer::plain();
    let mut cache = StreamingRenderCache::default();
    cache.render_lines(&stream, &renderer, 80, false);
    let stats = cache.stats();
    for _ in 0..100 {
        stream.push_str("x");
        cache.render_lines(&stream, &renderer, 80, false);
    }
    assert_eq!(stats, cache.stats());
    assert!(cache
        .render(&stream, &renderer, 80)
        .copy_text
        .contains(&"x".repeat(100)));
}
#[test]
fn oversized_closed_code_commits_without_painting_fence_syntax() {
    let mut stream = StreamingMarkdown::from_text("```text\n");
    stream.push_str(&"payload\n".repeat(10_000));
    stream.push_str("`");
    stream.push_str("``\n");
    assert!(!stream.copy_text().contains("```"));
    assert!(stream.committed().plain_text().contains("payload"));
    stream.push_str("next answer");
    assert!(stream.copy_text().contains("next answer"));
    let expected = markdown::parse(stream.raw_text());
    assert_eq!(stream.finish(), &expected);
}
#[test]
fn rich_literal_continuation_retains_rows_and_append_local_layout() {
    let run = |chunks: usize, newlines: bool| {
        let mut stream = StreamingMarkdown::from_text(&format!("**rich**{}", "x".repeat(8184)));
        let renderer = RichRenderer::plain();
        let mut cache = StreamingRenderCache::default();
        let mut frame = cache.render_lines(&stream, &renderer, 40, false);
        for _ in 0..chunks {
            let previous_len = frame.len();
            stream.push_str(if newlines {
                "word line\n"
            } else {
                "abcdefghijklmnopqrstuvwxyz abcdefghijklmnopqrstuvwxyz "
            });
            let update = cache.render_line_update(&stream, &renderer, 40, false);
            assert!(update.stable_prefix <= frame.len());
            frame.truncate(update.stable_prefix);
            frame.extend(update.replacement);
            assert!(frame.len() >= previous_len, "rich continuation collapsed");
        }
        assert_eq!(
            frame,
            renderer.render_unstable(stream.preview(), 40).plain_lines()
        );
        // Layout counters alone miss a full-source classification on
        // every append. Rich continuations must stay append-local too.
        assert!(stream.stats().preview_classified_bytes <= stream.raw_bytes().len() as u64 * 4);
        let stats = cache.stats();
        assert_eq!(stats.rich_prefix_layouts, 1);
        assert_eq!(stats.full_tail_layouts, 1);
        let raw = stream.raw_text().to_owned();
        assert_eq!(stream.finish(), &markdown::parse(&raw));
        stats
    };
    for newlines in [false, true] {
        let small = run(512, newlines);
        let large = run(1024, newlines);
        let work = |s: StreamingLayoutStats| {
            s.checked_bytes + s.laid_out_bytes + s.copied_bytes + s.fallback_source_bytes
        };
        assert!(work(large) <= work(small) * 5 / 2, "{small:?} -> {large:?}");
    }
}
