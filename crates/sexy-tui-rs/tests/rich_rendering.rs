use std::path::PathBuf;

use sexy_tui_rs::{
    parse_markdown, CapabilityOverrides, ColorDepth, RenderOptions, RichRenderer,
    StreamingMarkdown, SupportLevel, TerminalCapabilities, Theme, WidthPolicy,
};

const FIXTURE: &str = include_str!("fixtures/rich.md");
const WIDTH_GOLDEN: &str = include_str!("goldens/width-matrix.txt");
const CAPABILITY_GOLDEN: &str = include_str!("goldens/capability-matrix.txt");

fn renderer(capabilities: TerminalCapabilities) -> RichRenderer {
    RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions {
            syntax_highlighting: false,
            code_borders: true,
            ..RenderOptions::default()
        },
    )
}

#[test]
fn static_width_matrix_matches_golden_and_cell_bounds() {
    let document = parse_markdown(FIXTURE);
    let renderer = renderer(TerminalCapabilities::plain());
    let mut actual = String::new();
    for width in [20u16, 40, 60, 80, 120, 160] {
        let rendered = renderer.render(&document, width);
        actual.push_str(&format!("===== width {width} =====\n"));
        actual.push_str(&rendered.plain_text());
        actual.push('\n');
        assert!(rendered
            .lines
            .iter()
            .all(|line| { WidthPolicy::default().line_width(&line.plain) <= usize::from(width) }));
    }
    assert_or_update("width-matrix.txt", &actual, WIDTH_GOLDEN);
}

#[test]
fn capability_matrix_matches_golden() {
    let document = parse_markdown(
        "# Heading\n\n**strong** *emphasis* [docs](https://example.com) `code`\n\n> quote\n\n- item",
    );
    let profiles = [
        ("plain-ascii", TerminalCapabilities::plain()),
        (
            "no-color-unicode",
            TerminalCapabilities::interactive(ColorDepth::None, true),
        ),
        (
            "ansi16-ascii",
            TerminalCapabilities::interactive(ColorDepth::Ansi16, false),
        ),
        (
            "ansi256-unicode",
            TerminalCapabilities::interactive(ColorDepth::Ansi256, true),
        ),
        (
            "truecolor-link",
            TerminalCapabilities::interactive(ColorDepth::TrueColor, true).with_overrides(
                &CapabilityOverrides {
                    italics: Some(SupportLevel::Supported),
                    hyperlinks: Some(true),
                    ..CapabilityOverrides::default()
                },
            ),
        ),
    ];
    let mut actual = String::new();
    for (name, capabilities) in profiles {
        actual.push_str(&format!("===== {name} =====\n"));
        let output = renderer(capabilities).render(&document, 60).styled_text();
        actual.push_str(&visualize_controls(&output));
        actual.push('\n');
    }
    assert_or_update("capability-matrix.txt", &actual, CAPABILITY_GOLDEN);
}

#[test]
fn deterministic_adversarial_bytes_never_break_width_or_static_equivalence() {
    let hostile = b"# title\n\ntext \x1b]52;c;Y2xpcA==\x07 **open \xf0\x9f\x92\xa1**\n\n```rs\nfn x() {}\n```\n";
    for seed in 0..64u64 {
        let mut state = seed.wrapping_add(1);
        let mut offset = 0usize;
        let mut stream = StreamingMarkdown::new();
        while offset < hostile.len() {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let chunk = 1 + (state as usize % 9);
            let end = (offset + chunk).min(hostile.len());
            stream.push_bytes(&hostile[offset..end]);
            offset = end;
        }
        let expected = parse_markdown(stream.raw_text());
        assert_eq!(stream.finish(), &expected);
        for width in [1u16, 2, 20, 40, 160] {
            let rendered =
                renderer(TerminalCapabilities::plain()).render(stream.committed(), width);
            assert!(rendered.lines.iter().all(|line| {
                !line.styled.contains('\x1b')
                    && WidthPolicy::default().line_width(&line.plain) <= usize::from(width)
            }));
        }
    }
}

#[test]
fn ordinary_streaming_paragraph_boundaries_keep_canonical_rows_stable() {
    let renderer = renderer(TerminalCapabilities::plain());
    let mut stream = StreamingMarkdown::new();
    let mut cache = sexy_tui_rs::StreamingRenderCache::default();
    let mut frame = Vec::new();
    let mut source = String::new();
    let mut chunks = vec!["# APPEND heading\n\n".to_owned()];
    for index in 0..48 {
        let mut chunk = format!(
            "APPEND_{index:02} deterministic streamed prose with Markdown boundaries and enough words to occupy a physical row.\n"
        );
        if matches!(index, 7 | 15 | 23 | 31 | 39 | 47) {
            chunk.push('\n');
        }
        chunks.push(chunk);
    }

    let mut saw_open_paragraph_reflow = false;
    let mut saw_completed_paragraph_commit = false;

    for (chunk_index, chunk) in chunks.into_iter().enumerate() {
        source.push_str(&chunk);
        stream.push_str(&chunk);
        let previous = frame.clone();
        let update = cache.render_line_update(&stream, &renderer, 96, false);
        assert!(update.stable_prefix <= previous.len());
        frame.truncate(update.stable_prefix);
        frame.extend(update.replacement);
        assert!(
            update.stable_prefix <= frame.len(),
            "reported stable prefix exceeds the new frame for {chunk:?}"
        );
        assert_eq!(
            &frame[..update.stable_prefix],
            &previous[..update.stable_prefix],
            "only the reported stable prefix may be reused for {chunk:?}"
        );

        let committed_rows = cache.committed_rows();
        assert!(
            committed_rows <= frame.len(),
            "committed rows must be present in the rendered frame: {committed_rows} > {}",
            frame.len()
        );
        let prior_committed_rows = committed_rows.min(previous.len());
        assert_eq!(
            &frame[..prior_committed_rows],
            &previous[..prior_committed_rows],
            "parser-committed rows changed for {chunk:?}"
        );

        if chunk_index == 2 {
            // APPEND_01 continues the first paragraph. Its final visual row is
            // provisional: Markdown's soft newline can still add to it.
            assert!(
                update.stable_prefix < previous.len(),
                "an open paragraph must retain a mutable visual frontier"
            );
            assert!(
                committed_rows < previous.len(),
                "open-paragraph rows must not be reported as parser-committed"
            );
            assert_ne!(
                frame, previous,
                "the open paragraph's provisional row should be allowed to reflow"
            );
            saw_open_paragraph_reflow = true;
        }
        if chunk_index == 9 {
            // APPEND_08 follows the first explicit blank-line boundary, so the
            // first ordinary paragraph is now safe to append to native history.
            assert!(stream.committed().blocks.len() >= 2);
            assert!(committed_rows > 0);
            assert!(committed_rows <= previous.len());
            assert_eq!(&frame[..committed_rows], &previous[..committed_rows]);
            saw_completed_paragraph_commit = true;
        }
        assert_eq!(
            frame,
            renderer.render(&parse_markdown(&source), 96).plain_lines(),
            "stream geometry diverged for {chunk:?}"
        );
    }
    assert!(saw_open_paragraph_reflow);
    assert!(saw_completed_paragraph_commit);

    let previous = frame.clone();
    stream.finish();
    let update = cache.render_line_update(&stream, &renderer, 96, false);
    frame.truncate(update.stable_prefix);
    frame.extend(update.replacement);
    assert_eq!(
        &frame[..previous.len().min(frame.len())],
        &previous[..previous.len().min(frame.len())]
    );
    assert_eq!(
        frame,
        renderer.render(&parse_markdown(&source), 96).plain_lines()
    );
    assert_eq!(stream.raw_text(), source);
}

fn visualize_controls(value: &str) -> String {
    value.replace('\x1b', "<ESC>").replace('\x07', "<BEL>")
}

fn assert_or_update(name: &str, actual: &str, expected: &str) {
    if std::env::var_os("UPDATE_GOLDENS").is_some() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/goldens")
            .join(name);
        std::fs::write(path, actual).expect("write golden");
        return;
    }
    assert_eq!(
        actual, expected,
        "golden {name} differs; run UPDATE_GOLDENS=1 cargo test --test rich_rendering"
    );
}
