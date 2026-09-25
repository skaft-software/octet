use sexy_tui_rs::rich_text::mermaid::render_mermaid;
include!("fixtures/mermaid/pi-flowcharts.rs");
#[test]
fn flowcharts_match_pi_oracle() {
    let mut failures = Vec::new();
    for (source, expected) in PI_FLOWCHARTS {
        match render_mermaid(source) {
            Ok(art) if art.plain() == *expected => {}
            other => failures.push(format!("{source:?}: {other:?}\nEXPECTED: {expected:?}")),
        }
    }
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn reported_architecture_matches_pi_with_equivalent_edge_spelling() {
    let art = render_mermaid(include_str!("fixtures/mermaid/reported-architecture.mmd")).unwrap();
    assert_eq!(
        art.plain(),
        include_str!("fixtures/mermaid/reported-architecture.pi.txt").trim_end()
    );
}

#[test]
fn reported_graph_needs_a_wide_viewport_not_wrapped_rows() {
    let art = render_mermaid(include_str!("fixtures/mermaid/reported-architecture.mmd")).unwrap();
    assert_eq!((art.width, art.lines.len()), (501, 86));
    assert!(art.width > 80);
    assert_eq!(
        art.width,
        art.lines
            .iter()
            .map(|row| sexy_tui_rs::width::display_width(row))
            .max()
            .unwrap()
    );
}

#[test]
fn inline_punctuation_labels_keep_the_complete_edge() {
    for (inline, pipe) in [
        (
            "A -.PROXY_ENV or ./proxy.env.-> B",
            "A -.->|PROXY_ENV or ./proxy.env| B",
        ),
        ("README-.documents.->Flake", "README -.->|documents| Flake"),
        ("A == build / test ==> B", "A ==>|build / test| B"),
    ] {
        assert_eq!(
            render_mermaid(&format!("graph TB\n{inline}")).unwrap(),
            render_mermaid(&format!("graph TB\n{pipe}")).unwrap()
        );
    }
}

#[test]
fn groups_and_fanout_are_bounded_before_layout() {
    use sexy_tui_rs::rich_text::mermaid::MermaidError;
    let too_deep = format!(
        "graph TB\n{}A\n{}",
        (0..7)
            .map(|i| format!("subgraph g{i}\n"))
            .collect::<String>(),
        "end\n".repeat(7)
    );
    let too_many_groups = format!(
        "graph TB\n{}",
        (0..25)
            .map(|i| format!("subgraph g{i}\na{i}\nend\n"))
            .collect::<String>()
    );
    let too_many_edges = format!("graph TB\n{}", "A --> B\n".repeat(513));
    let fanout = format!(
        "graph TB\n{} --> {}",
        (0..24)
            .map(|i| format!("a{i}"))
            .collect::<Vec<_>>()
            .join(" & "),
        (0..24)
            .map(|i| format!("b{i}"))
            .collect::<Vec<_>>()
            .join(" & ")
    );
    for source in [too_deep, too_many_groups, too_many_edges, fanout] {
        assert!(matches!(
            render_mermaid(&source),
            Err(MermaidError::TooLarge { .. })
        ));
    }
    for source in [
        "graph TB\nsubgraph S\nA",
        "graph TB\nend",
        "graph TB\nsubgraph S\nA\nend\nsubgraph S\nB\nend",
    ] {
        assert!(matches!(
            render_mermaid(source),
            Err(MermaidError::UnsupportedSyntax { .. })
        ));
    }
}

#[test]
fn wide_valid_graphs_are_not_rejected_by_old_subset_limits() {
    let source = format!(
        "graph TB\n{}",
        (0..128)
            .map(|i| format!("a{i}[abcdefghijklmnopqrstuvwx]\n"))
            .collect::<String>()
    );
    let art = render_mermaid(&source).unwrap();
    assert!(art.width > 400);
    assert!(art.width <= 4096);
    assert_eq!(art.lines.len(), 3);
}

#[test]
fn labels_cannot_inject_terminal_controls() {
    for source in [
        "graph TB\nA[\x1b[31mevil]",
        "graph TB\nA[\u{009b}31mevil]",
        "graph TB\nA[\0]",
    ] {
        assert!(render_mermaid(source).is_err());
    }
    let art = render_mermaid("graph TB\nA[\"&#27; &#x9b; &#0; &amp;lt; <b>safe</b>\"]").unwrap();
    assert!(art
        .lines
        .iter()
        .all(|row| row.chars().all(|c| !c.is_control())));
    assert!(art.plain().contains("safe"));
    assert!(!art.plain().contains("<b>"));
}
