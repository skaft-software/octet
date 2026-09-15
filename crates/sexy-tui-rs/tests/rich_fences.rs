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
use sexy_tui_rs::{Block, RichRenderer};

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
