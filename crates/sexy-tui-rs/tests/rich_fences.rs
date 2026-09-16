//! Behavioral goldens for fenced diagram dispatch (rows 2c.3/2c.4 consumer).
//!
//! A completed fence whose info string explicitly names a diagram language
//! renders through `rich_text::latex::render_latex` (`latex`) or
//! `rich_text::mermaid::render_mermaid` (`mermaid`/`graph`/`flowchart`). Every
//! other fence — unknown language, oversized body, renderer failure,
//! unterminated fence — stays the original code block.
//!
//! The box-drawing expectations below are the same glyphs asserted by
//! `tests/latex_render.rs` and `tests/mermaid_render.rs`; the fence layer only
//! decides *whether* those renderers run.

use sexy_tui_rs::rich_text::markdown;
use sexy_tui_rs::rich_text::stream::{StreamingMarkdown, StreamingRenderCache};
use sexy_tui_rs::{
    Block, ColorDepth, RenderOptions, RichRenderer, TerminalCapabilities, Theme,
};

fn code_block(source: &str) -> sexy_tui_rs::rich_text::CodeBlock {
    let document = markdown::parse(source);
    match document.blocks.as_slice() {
        [Block::CodeBlock(code)] => code.clone(),
        other => panic!("expected one code block for {source:?}, got {other:?}"),
    }
}

fn fence(language: &str, body: &str) -> String {
    format!("```{language}\n{body}\n```\n")
}

#[test]
fn latex_fence_renders_the_box_drawing() {
    let matrix = markdown::parse(&fence("latex", "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}"));
    assert_eq!(matrix.plain_text(), "⎛ 1 │ 2 ⎞\n⎝ 3 │ 4 ⎠\n");

    let fraction = markdown::parse(&fence("latex", "x = \\frac{-b}{2a}"));
    assert_eq!(fraction.plain_text(), "    -b\nx = ──\n    2a\n");

    // The rendered rows survive the code-block layout used for fences.
    let rendered = RichRenderer::plain().render(&matrix, 80);
    assert!(rendered.plain_text().contains("⎛ 1 │ 2 ⎞"), "{}", rendered.plain_text());
    assert!(rendered.plain_text().contains("⎝ 3 │ 4 ⎠"), "{}", rendered.plain_text());
    // Provenance is kept: the block still carries the fence language.
    assert_eq!(code_block(&fence("latex", "x = 1")).language.as_deref(), Some("latex"));
}

#[test]
fn mermaid_fence_renders_the_diagram() {
    let document = markdown::parse(&fence("mermaid", "graph LR\n  A[Start] --> B[Done]"));
    assert_eq!(
        document.plain_text(),
        "┌───────┐    ┌──────┐\n│ Start ├───▶│ Done │\n└───────┘    └──────┘\n"
    );

    let rendered = RichRenderer::plain().render(&document, 80);
    let text = rendered.plain_text();
    assert!(text.contains("│ Start ├───▶│ Done │"), "{text}");
}

#[test]
fn graph_and_flowchart_fences_carry_their_own_header() {
    // ```` ```graph TD ```` — the info line is the diagram header.
    let document = markdown::parse("```graph TD\n  A[One] --> B[Two]\n```\n");
    assert_eq!(
        document.plain_text(),
        "┌─────┐\n│ One │\n└──┬──┘\n   │\n   │\n   ▼\n┌─────┐\n│ Two │\n└─────┘\n"
    );
    // A graph body that already starts with its own header is left alone: the
    // info string is not prepended again, so this is the LR layout.
    let explicit = markdown::parse("```flowchart LR\nflowchart LR\n  A[One] --> B[Two]\n```\n");
    assert_eq!(
        explicit.plain_text(),
        "┌─────┐    ┌─────┐\n│ One ├───▶│ Two │\n└─────┘    └─────┘\n"
    );
}

#[test]
fn unknown_fences_are_never_reinterpreted() {
    // LaTeX-looking source in a non-diagram fence stays literal.
    let latex_like = "```rust\n\\frac{1}{2}\n```\n";
    let code = code_block(latex_like);
    assert_eq!(code.language.as_deref(), Some("rust"));
    assert_eq!(code.code, "\\frac{1}{2}\n");
    let rendered = RichRenderer::plain().render(&markdown::parse(latex_like), 80).plain_text();
    assert!(rendered.contains("\\frac{1}{2}"), "{rendered}");
    assert!(!rendered.contains('─'), "{rendered}");

    // Mermaid-looking source in a non-diagram fence stays literal.
    let mermaid_like = "```text\ngraph LR\n  A[One] --> B[Two]\n```\n";
    assert_eq!(code_block(mermaid_like).language.as_deref(), Some("text"));
    let rendered = RichRenderer::plain().render(&markdown::parse(mermaid_like), 80).plain_text();
    assert!(rendered.contains("graph LR"), "{rendered}");
    assert!(rendered.contains("A[One] --> B[Two]"), "{rendered}");
    assert!(!rendered.contains('┌'), "{rendered}");

    // A malformed info line (`latexx`, not `latex`) is a different language.
    assert_eq!(code_block("```latexx\nx = 1\n```\n").code, "x = 1\n");
    // An indented (four-space) block is not a fence at all.
    assert_eq!(code_block("    ```latex\n    x = 1\n    ```\n").code, "```latex\nx = 1\n```\n");
}

#[test]
fn unsupported_bodies_degrade_to_the_original_source() {
    // `\cfrac` is outside the LaTeX subset: the original source is shown.
    let latex = fence("latex", "\\cfrac{1}{x}");
    let code = code_block(&latex);
    assert_eq!(code.language.as_deref(), Some("latex"));
    assert_eq!(code.code, "\\cfrac{1}{x}\n");
    let rendered = RichRenderer::plain().render(&markdown::parse(&latex), 80).plain_text();
    assert!(rendered.contains("\\cfrac{1}{x}"), "{rendered}");

    // `pie` is outside the Mermaid subset: same degradation.
    let mermaid = fence("mermaid", "pie\n  title Pets");
    let code = code_block(&mermaid);
    assert_eq!(code.language.as_deref(), Some("mermaid"));
    assert_eq!(code.code, "pie\n  title Pets\n");
    let rendered = RichRenderer::plain().render(&markdown::parse(&mermaid), 80).plain_text();
    assert!(rendered.contains("pie"), "{rendered}");
    assert!(rendered.contains("title Pets"), "{rendered}");
    assert!(!rendered.contains('┌'), "{rendered}");
}

#[test]
fn renders_that_produce_nothing_keep_the_original_source() {
    // A renderer can succeed and still emit nothing: an empty expression, `{}`,
    // a header-only graph. That is a failed render — the fence must show its
    // original source byte-for-byte, exactly like the same body in a plain
    // code fence, never an empty block.
    for (language, body) in [
        ("latex", ""),
        ("latex", "\n"),
        ("latex", "   \n"),
        ("latex", "{}\n"),
        ("mermaid", ""),
        ("mermaid", "graph LR\n"),
        ("mermaid", "flowchart TD\n"),
    ] {
        let source = format!("```{language}\n{body}```\n");
        let plain = format!("```rust\n{body}```\n");
        let code = code_block(&source);
        assert_eq!(
            code.code,
            code_block(&plain).code,
            "degraded body for {source:?}"
        );
        assert_eq!(code.language.as_deref(), Some(language), "{source:?}");
    }

    // The empty source stays visible through the real renderer too.
    let rendered = RichRenderer::plain()
        .render(&markdown::parse("```latex\n{}\n```\n"), 80)
        .plain_text();
    assert!(rendered.contains("{}"), "{rendered}");
    assert!(!rendered.contains('│'), "{rendered}");
    assert!(!rendered.contains('┌'), "{rendered}");
}

#[test]
fn oversized_and_unterminated_fences_stay_literal() {
    let huge = "\\frac{a}{b}".repeat(2_000); // > MAX_DIAGRAM_FENCE_BYTES
    let source = fence("latex", huge.trim_end());
    let code = code_block(&source);
    assert_eq!(code.language.as_deref(), Some("latex"));
    assert_eq!(code.code, format!("{}\n", huge.trim_end()));

    let unterminated = "```latex\n\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}\n";
    let code = code_block(unterminated);
    assert_eq!(code.code, "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}\n");
    let rendered = RichRenderer::plain().render(&markdown::parse(unterminated), 80).plain_text();
    assert!(rendered.contains("\\begin{pmatrix}"), "{rendered}");
    assert!(!rendered.contains("⎛"), "{rendered}");
}

/// A body line that *looks* like a closer but is not one — indented four
/// spaces, or carrying trailing junk — must not terminate the fence. The body
/// is still source, so the whole block degrades to raw text; rendering it would
/// glue the stray body line into the art (observed before the closure check
/// mirrored the parser's own rules).
#[test]
fn pseudo_closing_fences_never_render_partial_art() {
    // Four-space indent: the parser keeps this line in the body, so the fence
    // is unterminated and the block stays source.
    let indented = "```latex\nx = \\frac{1}{2}\n    ```\n";
    let code = code_block(indented);
    assert_eq!(code.code, "x = \\frac{1}{2}\n    ```\n");
    let rendered = RichRenderer::plain()
        .render(&markdown::parse(indented), 80)
        .plain_text();
    assert!(rendered.contains("\\frac{1}{2}"), "{rendered}");
    assert!(!rendered.contains('─'), "{rendered}");

    // An info-string-like tail is not a closing fence either.
    let tail = "```latex\nx = \\frac{1}{2}\n``` not a close\n";
    let code = code_block(tail);
    assert_eq!(code.code, "x = \\frac{1}{2}\n``` not a close\n");
    let rendered = RichRenderer::plain()
        .render(&markdown::parse(tail), 80)
        .plain_text();
    assert!(rendered.contains("not a close"), "{rendered}");
    assert!(!rendered.contains('─'), "{rendered}");

    // Same for Mermaid.
    let mermaid = "```mermaid\ngraph LR\n  A[One] --> B[Two]\n    ```\n";
    let rendered = RichRenderer::plain()
        .render(&markdown::parse(mermaid), 80)
        .plain_text();
    assert!(rendered.contains("A[One] --> B[Two]"), "{rendered}");
    assert!(!rendered.contains('┌'), "{rendered}");

    // Positive controls: trailing spaces and up to three spaces of indentation
    // are valid closers, so these still dispatch.
    for closed in [
        "```latex\nx = \\frac{1}{2}\n```   \n",
        "```latex\nx = \\frac{1}{2}\n  ```\n",
    ] {
        assert_eq!(
            markdown::parse(closed).plain_text(),
            "    1\nx = ─\n    2\n",
            "{closed:?}"
        );
    }
}

#[test]
fn a_diagram_fence_inside_a_blockquote_still_dispatches() {
    let source = "> ```mermaid\n> graph LR\n>   A[X] --> B[Y]\n> ```\n";
    let document = markdown::parse(source);
    assert_eq!(
        document.plain_text(),
        "> ┌───┐    ┌───┐\n> │ X ├───▶│ Y │\n> └───┘    └───┘\n"
    );
}

/// The streaming contract: while the fence is open the raw body is shown, the
/// closing fence publishes the complete diagram exactly once, and later input
/// does not disturb the committed rows. No intermediate state may contain a
/// partial diagram.
#[test]
fn streaming_publishes_the_diagram_only_when_the_fence_closes() {
    let renderer = RichRenderer::plain();
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();

    let chunks = [
        "before\n\n",
        "```mermaid\n",
        "graph LR\n",
        "  A[Start] ",
        "--> B[Done]\n",
    ];
    for chunk in chunks {
        stream.push_str(chunk);
        let lines = cache.render_lines(&stream, &renderer, 80, false).join("\n");
        assert!(!lines.contains('┌'), "partial diagram while open:\n{lines}");
        assert!(!lines.contains('▶'), "partial diagram while open:\n{lines}");
    }
    let open = cache.render_lines(&stream, &renderer, 80, false).join("\n");
    assert!(open.contains("A[Start] --> B[Done]"), "{open}");

    stream.push_str("```\n");
    let closed = cache.render_lines(&stream, &renderer, 80, false);
    assert!(closed.join("\n").contains("│ Start ├───▶│ Done │"), "{closed:?}");
    assert!(!closed.join("\n").contains("graph LR"), "{closed:?}");

    // Re-rendering unchanged input is identical (no flicker), and trailing
    // prose leaves the diagram rows untouched.
    assert_eq!(closed, cache.render_lines(&stream, &renderer, 80, false));
    stream.push_str("\nafter\n");
    let after = cache.render_lines(&stream, &renderer, 80, false);
    let diagram_row = after
        .iter()
        .position(|line| line.contains("│ Start ├───▶│ Done │"))
        .expect("diagram row survives later input");
    assert_eq!(after[diagram_row - 1], "  ┌───────┐    ┌──────┐");
    assert_eq!(after[diagram_row + 1], "  └───────┘    └──────┘");
}

/// Every info string outside the four supported names must reach the *same*
/// plain code block as a `text` fence carrying the same body — byte for byte,
/// with the language preserved as provenance. That is the observable proof that
/// the body was never handed to either diagram renderer.
#[test]
fn unknown_and_alias_fences_never_reach_a_diagram_renderer() {
    // Bodies chosen so a mistaken dispatch would visibly change the text.
    let latex_body = "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}";
    let mermaid_body = "graph LR\n  A[One] --> B[Two]";
    for language in [
        "rust",
        "text",
        "plaintext",
        "tex",        // a common LaTeX alias, deliberately not dispatched
        "math",
        "latexish",
        "la",
        "asciimath",
        "diagram",
        "mermaids",
        "graphtd",
        "graphviz",
        "dot",
        "plantuml",
        "unlabeled",
    ] {
        for body in [latex_body, mermaid_body] {
            let got = code_block(&format!("```{language}\n{body}\n```\n"));
            let plain = code_block(&format!("```text\n{body}\n```\n"));
            assert_eq!(got.code, plain.code, "{language}/{body:?}");
            assert_eq!(got.language.as_deref(), Some(language), "{language}");
        }
    }
    // A fence with no info string is not a diagram either: the body is the same
    // bytes a `text` fence produces, only without a language label.
    let bare = code_block("```\n\\frac{1}{2}\n```\n");
    assert_eq!(bare.code, code_block("```text\n\\frac{1}{2}\n```\n").code);
    assert_eq!(bare.language, None);
    // Case folding applies to the supported names only (`LaTeX` dispatches,
    // `latexish` above does not), and a one-line info string leaves an empty
    // body that renders to nothing and stays empty.
    let upper = "```LaTeX\nx = \\frac{1}{2}\n```\n";
    assert_eq!(markdown::parse(upper).plain_text(), "    1\nx = ─\n    2\n");
    assert_eq!(code_block("```latex\tx = 1\n```\n").code, "");
}

/// `$$…$$` and `\[…\]` are **not** wired to the LaTeX renderer: the parser does
/// not enable math events (`Event::DisplayMath`/`InlineMath` are unreachable),
/// so math-looking prose stays literal text and never becomes box drawing.
#[test]
fn math_is_not_dispatched_to_the_latex_renderer() {
    for source in [
        "$$\n\\frac{1}{2}\n$$\n",
        "$$\\frac{1}{2}$$\n",
        "\\[\n\\frac{1}{2}\n\\]\n",
        "The value is $\\frac{1}{2}$ here.\n",
        "Costs $5 and $10 total.\n",
    ] {
        let rendered = RichRenderer::plain()
            .render(&markdown::parse(source), 80)
            .plain_text();
        assert!(!rendered.contains('⎛'), "{source:?} -> {rendered}");
        assert!(!rendered.contains('│'), "{source:?} -> {rendered}");
        assert!(!rendered.contains('─'), "{source:?} -> {rendered}");
    }
    // The literal text survives, and no block is lost.
    let bracket = RichRenderer::plain()
        .render(&markdown::parse("\\[\n\\frac{1}{2}\n\\]\n"), 80)
        .plain_text();
    assert!(bracket.contains("\\frac{1}{2}"), "{bracket}");
    let display = RichRenderer::plain()
        .render(&markdown::parse("$$\n\\frac{1}{2}\n$$\n"), 80)
        .plain_text();
    assert!(display.contains("$$"), "{display}");
    assert!(display.contains("\\frac{1}{2}"), "{display}");
}

/// CRLF sources and `~~~` fences reach the same dispatcher (the fence marker
/// check accepts both, and the body is trimmed for LaTeX).
#[test]
fn crlf_and_tilde_fences_dispatch_the_same_way() {
    let crlf_latex = markdown::parse("```latex\r\nx = \\frac{-b}{2a}\r\n```\r\n");
    assert_eq!(crlf_latex.plain_text(), "    -b\nx = ──\n    2a\n");
    let crlf_mermaid = markdown::parse("```mermaid\r\ngraph LR\r\n  A[One] --> B[Two]\r\n```\r\n");
    assert_eq!(
        crlf_mermaid.plain_text(),
        "┌─────┐    ┌─────┐\n│ One ├───▶│ Two │\n└─────┘    └─────┘\n"
    );
    let tilde = markdown::parse("~~~latex\nx = \\frac{1}{2}\n~~~\n");
    assert_eq!(tilde.plain_text(), "    1\nx = ─\n    2\n");
}

/// A rendered diagram is glyph art, not source code: no grammar may style it.
/// Only the language label is dimmed; every glyph row is a single unstyled run,
/// which is what keeps one colour per grapheme (and no background fills).
#[test]
fn a_rendered_diagram_is_never_syntax_styled() {
    let capabilities =
        TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    let renderer = RichRenderer::new(
        Theme::with_capabilities(capabilities),
        capabilities,
        RenderOptions {
            syntax_highlighting: true,
            code_borders: true,
            ..RenderOptions::default()
        },
    );
    let rendered = renderer.render(&markdown::parse(&fence("latex", "\\begin{pmatrix}1&2\\\\3&4\\end{pmatrix}")), 80);
    let rows: Vec<&str> = rendered
        .lines
        .iter()
        .map(|line| line.styled.as_str())
        .filter(|line| line.contains('⎛') || line.contains('⎝'))
        .collect();
    assert_eq!(rows.len(), 2, "{:?}", rendered.plain_text());
    for row in rows {
        assert!(
            !row.contains('\u{1b}'),
            "diagram row carries styling: {row:?}"
        );
    }
    assert_eq!(
        rendered.plain_text(),
        "┌─ latex ────┐\n│  ⎛ 1 │ 2 ⎞ │\n│  ⎝ 3 │ 4 ⎠ │\n└────────────┘"
    );
}

/// The opener itself can arrive in pieces. No dispatch may happen before the
/// info string *and* the closing fence are present.
#[test]
fn streaming_never_dispatches_before_the_fence_is_complete() {
    for chunks in [
        &["```late", "x\nx = \\frac{1}{2}\n```\n"][..],
        &["``", "`mermaid\ngraph LR\n  A[One] --> B[Two]\n", "```\n"][..],
        &["```graph", " TD\n  A[One] --> B[Two]\n```\n"][..],
    ] {
        let renderer = RichRenderer::plain();
        let mut stream = StreamingMarkdown::new();
        let mut cache = StreamingRenderCache::default();
        let last = chunks.len() - 1;
        for (index, chunk) in chunks.iter().enumerate() {
            stream.push_str(chunk);
            let lines = cache.render_lines(&stream, &renderer, 80, false).join("\n");
            let glyphs = ["⎛", "┌", "─", "▶"].iter().any(|glyph| lines.contains(glyph));
            assert_eq!(glyphs, index == last, "chunks={chunks:?} at {index}:\n{lines}");
        }
    }
}

/// A diagram fence that fails closed streams its raw source and keeps it after
/// the close: no partial diagram while open, no empty block afterwards, and the
/// committed rows do not move on the next chunk.
#[test]
fn streaming_keeps_failed_diagram_fences_as_source() {
    let renderer = RichRenderer::plain();
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();

    let mut saw_source = false;
    for chunk in ["```latex\n", "\\cfrac{1}", "{x}\n"] {
        stream.push_str(chunk);
        let lines = cache.render_lines(&stream, &renderer, 80, false).join("\n");
        assert!(!lines.contains('⎛'), "no partial render while open:\n{lines}");
        saw_source |= lines.contains("\\cfrac");
    }
    assert!(saw_source, "raw body visible while the fence is open");

    stream.push_str("```\n");
    let closed = cache.render_lines(&stream, &renderer, 80, false);
    let text = closed.join("\n");
    assert!(text.contains("\\cfrac{1}{x}"), "source survives the close:\n{text}");
    assert!(!text.contains('⎛'), "no diagram for a failed render:\n{text}");
    assert_eq!(closed, cache.render_lines(&stream, &renderer, 80, false));

    stream.push_str("\ntrailing prose\n");
    let after = cache.render_lines(&stream, &renderer, 80, false).join("\n");
    assert!(after.contains("\\cfrac{1}{x}"), "{after}");
    assert!(after.contains("trailing prose"), "{after}");
}

/// Independent oracle for "did the parser close this fence?", read from
/// pulldown's own source (`firstpass.rs::parse_fenced_code_block` +
/// `scanners.rs::scan_closing_code_fence`): the closing line may be indented at
/// most three spaces **relative to the container content indent** — *not*
/// relative to the opening fence — must repeat the opening marker at least as
/// many times, and may be followed by spaces only (a tab does not close a
/// fence).
fn parser_closes_fence(closer: &str, marker: char, count: usize) -> bool {
    let indent = closer.len() - closer.trim_start_matches(' ').len();
    let run = closer[indent..].chars().take_while(|c| *c == marker).count();
    let rest = &closer[indent + run..];
    indent <= 3 && run >= count && rest.chars().all(|c| c == ' ')
}

/// The closure check must be *sound* on every shape a fence can take in one
/// flat document: a fence is dispatched exactly when a valid closing line ends
/// it, and every malformed/trailing-junk closer stays the original source. This
/// is the assertion that the dispatcher is not a match arm that fires on
/// fence-shaped text inside an open block.
#[test]
fn closure_matrix_dispatches_only_complete_fences() {
    let body = "graph LR\n  A[One] --> B[Two]";
    let mut cases = 0usize;
    for opener_indent in 0..=3usize {
        for closer_indent in 0..=5usize {
            for tail in ["", " ", "\t", " not a close"] {
                for marker in ["```", "~~~"] {
                    let source = format!(
                        "{o}{marker}mermaid\n{body}\n{c}{marker}{tail}\n",
                        o = " ".repeat(opener_indent),
                        c = " ".repeat(closer_indent),
                    );
                    let closed = parser_closes_fence(
                        &format!("{}{marker}{tail}", " ".repeat(closer_indent)),
                        marker.chars().next().expect("marker"),
                        marker.len(),
                    );
                    let rendered = RichRenderer::plain()
                        .render(&markdown::parse(&source), 80)
                        .plain_text();
                    let art = rendered.contains('┌') && rendered.contains('▶');
                    assert_eq!(
                        art, closed,
                        "dispatch mismatch (closed={closed}) for {source:?}\n{rendered}"
                    );
                    if !art {
                        // The degraded case must keep the whole source, closer
                        // line and all, rather than partial art.
                        assert!(rendered.contains("A[One] --> B[Two]"), "{rendered}");
                    }
                    cases += 1;
                }
            }
        }
    }
    assert!(cases >= 100, "matrix shrank to {cases} cases");
}

/// Streaming a fence one byte at a time: the diagram is published exactly when
/// the closing fence line completes, it never disappears again (no flicker), no
/// prefix ever shows a partial diagram, and the published rows equal the rows a
/// full-document render produces. Completed rows stay put after the close.
#[test]
fn streaming_prefix_scan_publishes_the_diagram_once() {
    let source = "intro\n\n```mermaid\ngraph LR\n  A[One] --> B[Two]\n```\n\noutro\n";
    let renderer = RichRenderer::plain();
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    let closing = source.rfind("```\n").expect("closing fence");
    let mut first_art = None;
    let mut lost_art = None;
    let mut published: Option<Vec<String>> = None;

    for (index, character) in source.char_indices() {
        stream.push_str(&character.to_string());
        let end = index + character.len_utf8();
        let rows = cache.render_lines(&stream, &renderer, 80, false);
        let art = rows.join("\n").contains('▶');
        if art && first_art.is_none() {
            first_art = Some(end);
        }
        if published.is_some() && !art {
            lost_art = Some(end);
        }
        if end >= closing + "```\n".len() {
            match &published {
                None => published = Some(rows.clone()),
                Some(prefix) => assert!(
                    rows.iter().take(prefix.len()).eq(prefix.iter()),
                    "completed rows moved at byte {end}:\n{prefix:?}\n{rows:?}"
                ),
            }
        }
    }

    assert_eq!(
        first_art,
        Some(closing + "```\n".len()),
        "the diagram must appear exactly when its closing fence completes"
    );
    assert!(lost_art.is_none(), "the diagram flickered away at {lost_art:?}");
    let committed = cache.render_lines(&stream, &renderer, 80, false);
    let direct: Vec<String> = renderer
        .render(&markdown::parse(source), 80)
        .lines
        .iter()
        .map(|line| line.styled.clone())
        .collect();
    assert_eq!(committed, direct, "streamed rows diverged from the final render");
}

/// A closing line more than three spaces past the *container content* indent (a
/// nested list item whose closer is written at the item's raw indentation) is a
/// deliberate fail-closed boundary: the raw range does not reveal the content
/// indent, so the closure check cannot prove closure even though the parser
/// accepted the closer and produced a clean body. The fence therefore stays
/// literal source — no art, no panic — and streaming agrees with the
/// full-document render. The common shapes (top-level list item, blockquote) do
/// dispatch.
#[test]
fn deeply_indented_container_closers_stay_literal() {
    // Content indent 2, closer at raw indent 2: dispatched.
    let dispatched = "- ```mermaid\n  graph LR\n    A[One] --> B[Two]\n  ```\n";
    assert!(
        RichRenderer::plain()
            .render(&markdown::parse(dispatched), 80)
            .plain_text()
            .contains('▶'),
        "a list fence whose closer sits at the content indent must dispatch"
    );

    // Nested list: content indent 4, closer at raw indent 4 (content-relative
    // 0). The parser closes the fence and the body is clean, but the raw
    // indentation is indistinguishable from a body line, so the fence keeps its
    // source.
    let literal = "  - ```mermaid\n    graph LR\n      A[One] --> B[Two]\n    ```\n";
    let document = markdown::parse(literal);
    let code = match document.blocks.as_slice() {
        [Block::List(list)] => match list.items[0].blocks.as_slice() {
            [Block::CodeBlock(code)] => code.clone(),
            other => panic!("expected one code block, got {other:?}"),
        },
        other => panic!("expected one list, got {other:?}"),
    };
    assert_eq!(code.language.as_deref(), Some("mermaid"));
    assert_eq!(
        code.code, "graph LR\n  A[One] --> B[Two]\n",
        "the parser closed the fence: the closer line is not part of the body"
    );

    let renderer = RichRenderer::plain();
    let rendered = renderer.render(&document, 80).plain_text();
    assert!(rendered.contains("A[One] --> B[Two]"), "{rendered}");
    assert!(!rendered.contains('┌'), "no art from an unproven closure:\n{rendered}");

    // Streaming must agree with the document render for the same source: the
    // literal rows are what the reader sees while the fence arrives and after
    // the message completes.
    let mut stream = StreamingMarkdown::new();
    let mut cache = StreamingRenderCache::default();
    for chunk in literal.as_bytes().chunks(5) {
        stream.push_bytes(chunk);
        let rows = cache.render_lines(&stream, &renderer, 80, false).join("\n");
        assert!(!rows.contains('┌'), "art mid-stream:\n{rows}");
    }
    stream.finish();
    let streamed = cache.render_lines(&stream, &renderer, 80, false);
    let direct: Vec<String> = renderer
        .render(&markdown::parse(literal), 80)
        .lines
        .iter()
        .map(|line| line.styled.clone())
        .collect();
    assert_eq!(streamed, direct, "streamed rows diverged");
}
