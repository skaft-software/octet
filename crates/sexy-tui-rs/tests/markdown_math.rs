//! Current Pi (890f92088) delimiter/rendering examples, not the old fence-only contract.
use sexy_tui_rs::rich_text::{markdown, render::RichRenderer, stream::StreamingMarkdown, Block};

#[test]
fn pi_inline_math_corpus() {
    for (source, expected) in [
        (
            r"A map $\mathbb{C}^3 \to \mathbb{C}^3$, $xy$, $x-y$, $-x$, $\frac{1}{2}$, $\rightarrow$, and \(s \to \infty\).",
            "A map ℂ³ → ℂ³, xy, x-y, -x, 1/2, →, and s → ∞.\n",
        ),
        (
            r"**$x^2$** and \[x_1\] and $$y^2$$ here",
            "x² and x₁ and y² here\n",
        ),
        ("- Formula: $F_1 = u^2$", "- Formula: F₁ = u²\n"),
        ("| Value |\n| --- |\n| $\\mathbb{C}^3$ |", "Value\nℂ³\n"),
        ("| Value |\n| --- |\n| $|x|$ |", "Value\n|x|\n"),
        (r"Escaped \$x-y\$.", "Escaped $x-y$.\n"),
        (r"`$x^2$`", "$x^2$\n"),
        ("```text\n$\\mathbb{C}^3$\n```", "$\\mathbb{C}^3$\n"),
        (r"$=$", "=\n"),
    ] {
        assert_eq!(markdown::parse(source).plain_text(), expected, "{source}");
    }
}

#[test]
fn currency_shell_and_unsupported_math_stay_intact() {
    for source in [
        "Costs $5 and $10 or $8k–$12k; use $HOME, and ${PATH}.",
        "Paths: $HOME/$USER and $XDG_CONFIG_HOME/$APP_CONFIG",
        r"Unknown $x_1 + \unknown{y} *z*$ after",
        r"Streaming $\mathbb{C}^3",
        r"Map \(\mathbb{C}^3",
        r"$a \xrightarrow{n} b$",
        r"\[a \xrightarrow{n} b\]",
        "$$\n\\unknown{x_i}\n=\n**source**\n$$",
        "\\[\nx^2",
    ] {
        assert_eq!(
            markdown::parse(source).plain_text().trim_end(),
            source,
            "{source}"
        );
    }
}

#[test]
fn pi_display_math_keeps_layout_and_standalone_equals() {
    for (source, expected) in [
        (
            r"$$\{3x+2y,\; x \in \{0, \pm 1\}\}$$",
            "{3x+2y, x ∈ {0, ± 1}}\n",
        ),
        (
            "\\[\nE \\approx \\frac{0.1\\ \\text{lux}}{100\\ \\text{lm/W}}\n\\]",
            "    0.1 lux\nE ≈ ────────\n    100 lm/W\n",
        ),
        ("$$\nx^2\n=\ny^2\n$$", "x² = y²\n"),
        ("$$\n=\n$$", "=\n"),
        (
            "\\[\nA=\n\\begin{pmatrix}\n\\pi & 0\\\\\n0 & \\frac{1}{\\pi}\n\\end{pmatrix}.\n\\]",
            "A = ⎛ π │ 0   ⎞\n    ⎝ 0 │ 1/π ⎠.\n",
        ),
    ] {
        let document = markdown::parse(source);
        assert_eq!(document.plain_text(), expected, "{source}");
        assert!(matches!(document.blocks.as_slice(), [Block::Plain(_)]));
        assert_eq!(
            RichRenderer::plain()
                .render(&document, 100)
                .plain_text()
                .trim_end(),
            expected.trim_end()
        );
    }
}

#[test]
fn math_streams_raw_then_renders_and_committed_rows_stay_stable() {
    for source in [
        "before\n\n$$\nx^2\n=\ny^2\n$$\n\nafter\n",
        "before\n\n\\[\n\\frac{1}{2}\n\\]\n\nafter\n",
        "Map $\\mathbb{C}^3$\n\nafter\n",
        "Map \\(\\mathbb{C}^3\\)\n\nafter\n",
        "$$\n\\unknown{x_i}\n=\n**source**\n$$\n\nafter\n",
    ] {
        let expected = markdown::parse(source);
        for chunk_size in [1, 2, 7, 31] {
            let mut stream = StreamingMarkdown::new();
            for chunk in source.as_bytes().chunks(chunk_size) {
                let committed = stream.committed().clone();
                stream.push_bytes(chunk);
                assert!(stream.committed().blocks.starts_with(&committed.blocks));
                assert!(
                    expected.blocks.starts_with(&stream.committed().blocks),
                    "{source:?}: {:?}",
                    stream.committed()
                );
            }
            assert_eq!(stream.finish(), &expected);
        }
    }
    let mut stream = StreamingMarkdown::from_text(r"Map $\mathbb{C}^3");
    assert_eq!(stream.preview().plain_text(), "Map $\\mathbb{C}^3\n");
    stream.push_str("$");
    assert_eq!(stream.preview().plain_text(), "Map ℂ³\n");
    let mut stream = StreamingMarkdown::from_text("\\[\nx^2");
    assert_eq!(stream.preview().plain_text(), "\\[\nx^2\n");
    stream.push_str("\n\\]");
    assert_eq!(stream.preview().plain_text(), "x²\n");
}

#[test]
fn reported_equations_match_current_pi_oracle() {
    // Captured from current upstream latex.ts + its real visibleWidth (Node 26).
    for (body, expected) in [
        (
            r"\text{Active Configuration}=\texttt{flake.nix}+\texttt{flake.lock}+\texttt{nix/*.nix}",
            "Active Configuration = flake.nix+flake.lock+nix/*.nix",
        ),
        (
            r"S_{\text{nix}}=\text{Eval}(\texttt{flake.nix},\texttt{flake.lock},\texttt{nix/*.nix})",
            "Sₙᵢₓ = Eval(flake.nix,flake.lock,nix/*.nix)",
        ),
        (
            r"S_{\text{fallback}}=\text{brew bundle}(\texttt{Brewfile})+\text{stow}(\texttt{zsh},\texttt{git},\texttt{ssh})",
            "S_fallback = brew bundle(Brewfile)+stow(zsh,git,ssh)",
        ),
        (
            r"\text{Shell Environment}=\text{Nix Profile}+\text{Homebrew Prefix}+\text{User Secrets}+\text{Home Manager Init}",
            "Shell Environment = Nix Profile+Homebrew Prefix+User Secrets+Home Manager Init",
        ),
    ] {
        assert_eq!(
            markdown::parse(&format!("\\[\n{body}\n\\]"))
                .plain_text()
                .trim_end(),
            expected
        );
    }
    let unsupported = r"\[ \text{Local HTTP Client} \xrightarrow{\text{localhost}} \text{Local Relay Proxy} \xrightarrow{\text{HTTPS + CF Access}} \text{Remote Relay} \]";
    assert_eq!(
        markdown::parse(unsupported).plain_text().trim_end(),
        unsupported
    );
}

#[test]
fn nested_display_math_and_unsupported_source_are_protected() {
    for (source, expected) in [
        ("> \\[\n> x_1 = y\n> \\]", "> x₁ = y\n"),
        ("- item\n\n  $$\n  x_1=y\n  $$", "- item\n  x₁ = y\n"),
        (
            "> \\[\n> \\unknown{x_i}\n> =\n> \\]",
            "> \\[\n> \\unknown{x_i}\n> =\n> \\]\n",
        ),
    ] {
        let document = markdown::parse(source);
        assert_eq!(document.plain_text(), expected, "{source}: {document:?}");
        let mut stream = StreamingMarkdown::new();
        for byte in source.as_bytes() {
            stream.push_bytes(&[*byte]);
        }
        assert_eq!(stream.finish(), &document);
    }
}

#[test]
fn pending_math_does_not_commit_blank_lines_or_parse_internal_fences() {
    for source in [
        "before\n\n$$\n\nx\n\n= y\n$$\n\nafter",
        "\\[\n\\unknown{x}\n\n```rust\nx\n```\n\\]\n\nafter",
    ] {
        let expected = markdown::parse(source);
        let mut stream = StreamingMarkdown::new();
        for byte in source.as_bytes() {
            stream.push_bytes(&[*byte]);
            assert!(
                expected.blocks.starts_with(&stream.committed().blocks),
                "{:?}",
                stream.committed()
            );
        }
        assert_eq!(stream.finish(), &expected);
    }
}

#[test]
fn math_does_not_rewrite_urls_or_reference_definitions() {
    for (source, expected) in [
        (
            "[$x^2$](https://example.test/$y$)",
            "x² (https://example.test/$y$)\n",
        ),
        ("https://example.test/$x$", "https://example.test/$x$\n"),
        ("<https://example.test/$x$>", "https://example.test/$x$\n"),
        (
            "[link][ref]\n\n[ref]: https://example.test/$x$",
            "link (https://example.test/$x$)\n",
        ),
    ] {
        assert_eq!(markdown::parse(source).plain_text(), expected);
    }
}

#[test]
fn oversized_math_preserves_source_and_never_commits_a_partial_expression() {
    let source = format!("\\[\n{}\n\\]\n\nafter", "x_i = y\n\n".repeat(8000));
    let expected = markdown::parse(&source);
    assert!(expected.plain_text().starts_with("\\[\nx_i = y"));
    let mut stream = StreamingMarkdown::new();
    for chunk in source.as_bytes().chunks(511) {
        stream.push_bytes(chunk);
        assert!(expected.blocks.starts_with(&stream.committed().blocks));
    }
    assert_eq!(stream.finish(), &expected);
}

#[test]
fn display_math_does_not_discard_surrounding_container_prose() {
    for source in [
        "> intro\n> \\[\n> x_1=y\n> \\]\n> after",
        "> intro\n> $$\n> x_1=y\n> $$\n> after",
    ] {
        assert_eq!(
            markdown::parse(source).plain_text(),
            "> intro\n> x₁ = y\n> after\n"
        );
    }
}

#[test]
fn nested_math_cannot_consume_source_outside_its_container() {
    for (source, expected) in [
        (
            "> \\[\n> x^2\n\noutside\n\n\\]",
            "> \\[\n> x^2\noutside\n]\n",
        ),
        ("> $$\n> x^2\n\noutside\n\n$$", "> $$\n> x^2\noutside\n$$\n"),
        (
            "- \\[\n  x^2\n\n- outside\n\n\\]",
            "- \n  \\[\nx^2\n- outside\n]\n",
        ),
        ("pending \\(x_1\n\n# Outside", "pending \\(x_1\nOutside\n"),
        ("pending $x_1\n\n# Outside", "pending $x_1\nOutside\n"),
    ] {
        let document = markdown::parse(source);
        assert_eq!(document.plain_text(), expected, "{source}: {document:?}");
        let mut stream = StreamingMarkdown::new();
        for byte in source.as_bytes() {
            stream.push_bytes(&[*byte]);
        }
        assert_eq!(stream.finish(), &document);
    }
}

#[test]
fn decoded_entities_escapes_and_unicode_do_not_confuse_math_offsets() {
    for (source, expected) in [
        ("&amp; $x^2$", "& x²\n"),
        (r"\* $x^2$", "* x²\n"),
        ("界 🦀 e\u{301} $x^2$", "界 🦀 e\u{301} x²\n"),
        ("a &lt; $x^2$ &amp; $y_1$ &gt;", "a < x² & y₁ >\n"),
        (r"\$ &amp; $x^2$", "$ & x²\n"),
    ] {
        assert_eq!(markdown::parse(source).plain_text(), expected, "{source}");
    }
}
