//! Behavioral goldens for the Mermaid graph renderer (parity card 2c.4).
//!
//! Layout goldens are captured from Pi's grok-mermaid 0.2.3 (see the fixtures
//! directory), except dotted node identifiers, an existing octet extension.
//! Malformed syntax still fails closed rather than publishing partial art.

use sexy_tui_rs::rich_text::mermaid::{render_mermaid, MermaidError};
use sexy_tui_rs::width::display_width;

/// `(source, expected)` for every supported construct: layouts, chains, branches,
/// labels, shapes, directives and wide labels.
const SUPPORTED: &[(&str, &str)] = &[
    // lr_basic
    ("flowchart LR\n  A[Start] --> B[Done]", "┌───────┐    ┌──────┐\n│ Start ├───▶│ Done │\n└───────┘    └──────┘"),
    // td_basic
    ("graph TD\n  A[Start] --> B[Done]", " ┌───────┐\n │ Start │\n └───┬───┘\n     │\n     ▼\n ┌──────┐\n │ Done │\n └──────┘"),
    // tb_alias
    ("flowchart TB\n  A[One] --> B[Two]", " ┌─────┐\n │ One │\n └──┬──┘\n    │\n    ▼\n ┌─────┐\n │ Two │\n └─────┘"),
    // lr_chain
    ("flowchart LR\n  A[Parse] --> B[Layout] --> C[Render]", "┌───────┐    ┌────────┐    ┌────────┐\n│ Parse ├───▶│ Layout ├───▶│ Render │\n└───────┘    └────────┘    └────────┘"),
    // td_branch
    ("flowchart TD\n  A[In] --> B{Valid?}\n  B --> C[Store]\n  B --> D[Reject]", "        ┌────┐\n        │ In │\n        └──┬─┘\n           │\n           ▼\n      ╭────────╮\n      │ Valid? │\n      ╰────┬───╯\n     ┌─────┴─────┐\n     ▼           ▼\n ┌───────┐  ┌────────┐\n │ Store │  │ Reject │\n └───────┘  └────────┘"),
    // lr_branch
    ("flowchart LR\n  A[In] --> B{Valid?}\n  B --> C[Store]\n  B --> D[Reject]", "                        ┌───────┐\n                    ┌──▶│ Store │\n┌────┐    ╭────────╮│   └───────┘\n│ In ├───▶│ Valid? ├┤\n└────┘    ╰────────╯│   ┌────────┐\n                    └──▶│ Reject │\n                        └────────┘"),
    // lr_link_label
    ("flowchart LR\n  A -->|start| B\n  B --> C", "┌───┐ start  ┌───┐        ┌───┐\n│ A ├───────▶│ B ├───────▶│ C │\n└───┘        └───┘        └───┘"),
    // td_link_label
    ("flowchart TD\n  A -->|start| B", " ┌───┐\n │ A │\n └─┬─┘\n   │\n   ▼start\n ┌───┐\n │ B │\n └───┘"),
    // lr_diamond
    ("graph LR\n  A[Alpha] --> B[Beta]\n  A --> C[Gamma]\n  B --> D[Delta]\n  C --> D", "             ┌──────┐\n         ┌──▶│ Beta ├─┐\n┌───────┐│   └──────┘ │   ┌───────┐\n│ Alpha ├┤            ├──▶│ Delta │\n└───────┘│   ┌───────┐│   └───────┘\n         └──▶│ Gamma ├┘\n             └───────┘"),
    // td_decision
    ("flowchart TD\n  A[Start] --> B{Choice}\n  B --> C[Yes]\n  B --> D[No]", "    ┌───────┐\n    │ Start │\n    └───┬───┘\n        │\n        ▼\n   ╭────────╮\n   │ Choice │\n   ╰────┬───╯\n    ┌───┴────┐\n    ▼        ▼\n ┌─────┐  ┌────┐\n │ Yes │  │ No │\n └─────┘  └────┘"),
    // shapes
    ("flowchart LR\n  A(Rounded) --> B{Diamond}", "╭─────────╮    ╭─────────╮\n│ Rounded ├───▶│ Diamond │\n╰─────────╯    ╰─────────╯"),
    // shapes_nested_brackets
    ("flowchart LR\n  A([Stadium]) --> B[[Subroutine]]\n  C((Circle)) --> D{{Hexagon}}", "╭─────────╮    ┌────────────┐\n│ Stadium ├───▶│ Subroutine │\n╰─────────╯    └────────────┘\n\n╭────────╮     ╭─────────╮\n│ Circle ├────▶│ Hexagon │\n╰────────╯     ╰─────────╯"),
    // class_annotation
    ("flowchart LR\n  A[Foo]:::highlight --> B[Bar]", "┌─────┐    ┌─────┐\n│ Foo ├───▶│ Bar │\n└─────┘    └─────┘"),
    // undirected_td
    ("flowchart TD\n  A[One] --- B[Two]", " ┌─────┐\n │ One │\n └──┬──┘\n    │\n    │\n ┌─────┐\n │ Two │\n └─────┘"),
    // undirected_lr
    ("flowchart LR\n  A[One] --- B[Two]", "┌─────┐    ┌─────┐\n│ One ├────│ Two │\n└─────┘    └─────┘"),
    // link_styles
    ("flowchart LR\n  A[One] -.-> B[Two]\n  A ==> C[Three]", "           ┌─────┐\n       ┌╌╌▶│ Two │\n┌─────┐╎   └─────┘\n│ One ├┤\n└─────┘┃   ┌───────┐\n       ┗━━▶│ Three │\n           └───────┘"),
    // directives_and_comments
    ("flowchart TD\n  %% comment\n  classDef big fill:#f00\n  A[Alpha] --> B[Beta]\n  style A fill:#0f0\n  linkStyle 0 stroke:#00f\n  class A big\n  click A \"https://example.com\";", " ┌───────┐\n │ Alpha │\n └───┬───┘\n     │\n     ▼\n ┌──────┐\n │ Beta │\n └──────┘"),
    // semicolons
    ("flowchart LR;\n  A[One] --> B[Two];", "┌─────┐    ┌─────┐\n│ One ├───▶│ Two │\n└─────┘    └─────┘"),
    // quoted_label
    ("flowchart LR\n  A[\"Quoted label\"] --> B[Two words]", "┌──────────────┐    ┌───────────┐\n│ Quoted label ├───▶│ Two words │\n└──────────────┘    └───────────┘"),
    // dotted_ids
    ("flowchart LR\n  a.b_c-1 --> d2", "┌─────────┐    ┌────┐\n│ a.b_c-1 ├───▶│ d2 │\n└─────────┘    └────┘"),
    // unlabelled_nodes
    ("flowchart LR\n  A --> B", "┌───┐    ┌───┐\n│ A ├───▶│ B │\n└───┘    └───┘"),
    // semicolon_separated_statements
    ("flowchart LR; A[One] --> B[Two]; B --> C[Three]", "┌─────┐    ┌─────┐    ┌───────┐\n│ One ├───▶│ Two ├───▶│ Three │\n└─────┘    └─────┘    └───────┘"),
    // trailing_comment
    ("flowchart LR\n  A[One] --> B[Two] %% trailing comment", "┌─────┐    ┌─────┐\n│ One ├───▶│ Two │\n└─────┘    └─────┘"),
    // comment_after_header
    ("flowchart LR %% the pipeline\n  A[One] --> B[Two]", "┌─────┐    ┌─────┐\n│ One ├───▶│ Two │\n└─────┘    └─────┘"),
    // quoted_label_with_delimiters
    ("flowchart LR\n  A[\"a[b]c\"] --> B[Two]", "┌───────┐    ┌─────┐\n│ a[b]c ├───▶│ Two │\n└───────┘    └─────┘"),
    // quoted_link_label
    ("flowchart LR\n  A -->|\"two words\"| B", "┌───┐ two words  ┌───┐\n│ A ├───────────▶│ B │\n└───┘            └───┘"),
    // percent_inside_quoted_label
    ("flowchart LR\n  A[\"100%% done\"] --> B[Two]", "┌────────────┐    ┌─────┐\n│ 100%% done ├───▶│ Two │\n└────────────┘    └─────┘"),
    // semicolon_inside_quoted_label
    ("flowchart LR\n  A[\"a;b\"] --> B[Two]", "┌─────┐    ┌─────┐\n│ a;b ├───▶│ Two │\n└─────┘    └─────┘"),
    // wide_labels
    ("flowchart LR\n  A[界_x] --> B[y界]", "┌──────┐    ┌─────┐\n│ 界_x ├───▶│ y界 │\n└──────┘    └─────┘"),
];

/// `(source, expected error message)` — input outside the supported subset must
/// produce a typed `MermaidError`, never a panic or a partial diagram.
const FAIL_CLOSED: &[(&str, &str)] = &[
    // empty_input
    ("", "dropped, expected a graph or flowchart header"),
    // no_header
    ("A --> B", "dropped, unsupported diagram type: \"A\""),
    // pie
    (
        "pie\n  title Pets",
        "dropped, unsupported diagram type: \"pie\"",
    ),
    // sequence_diagram
    (
        "sequenceDiagram\n  A->>B: hi",
        "dropped, unsupported diagram type: \"sequenceDiagram\"",
    ),
    // unknown_direction
    (
        "flowchart XY\n  A --> B",
        "dropped, unsupported diagram type: \"XY\"",
    ),
    // end_statement
    (
        "flowchart LR\n  A --> B\n  end",
        "dropped, line 3: `end` without a subgraph",
    ),
    // unterminated_quoted_label
    (
        "flowchart LR\n  A[\"oops] --> B",
        "dropped, line 2: unterminated quoted label opened with `[`",
    ),
    // quoted_label_missing_delimiter
    (
        "flowchart LR\n  A[\"oops\" --> B",
        "dropped, line 2: quoted label opened with `[` is not closed by `]`",
    ),
    // unbalanced_label
    (
        "flowchart LR\n  A[Unbalanced --> B",
        "dropped, line 2: unbalanced node label opened with `[`",
    ),
    // trailing_link
    (
        "flowchart LR\n  A -->",
        "dropped, line 2: trailing link without a target node",
    ),
    // unterminated_link_label
    (
        "flowchart LR\n  A -->|oops B",
        "dropped, line 2: unterminated link label",
    ),
    // unknown_node_after_edge
    (
        "flowchart LR\n  A --> :::x",
        "dropped, line 2: expected a node id at \":::x\"",
    ),
];

#[test]
fn supported_graphs_render_the_expected_box_drawing() {
    for (source, expected) in SUPPORTED {
        let art = render_mermaid(source).unwrap_or_else(|error| panic!("{source:?}: {error}"));
        assert_eq!(art.plain(), *expected, "source = {source:?}");
    }
}

#[test]
fn unsupported_input_fails_closed_with_a_typed_error() {
    for (source, expected) in FAIL_CLOSED {
        let error = render_mermaid(source).expect_err(source);
        assert_eq!(error.to_string(), *expected, "source = {source:?}");
    }
}

#[test]
fn every_rendered_row_fits_the_reported_width() {
    for (source, _) in SUPPORTED {
        let art = render_mermaid(source).expect(source);
        let widest = art
            .lines
            .iter()
            .map(|line| display_width(line))
            .max()
            .unwrap_or(0);
        assert_eq!(art.width, widest, "source = {source:?}");
        for line in &art.lines {
            assert!(
                display_width(line) <= art.width,
                "source = {source:?} line = {line:?}"
            );
        }
    }
}

#[test]
fn wide_labels_keep_box_borders_aligned() {
    // A CJK glyph occupies two terminal columns, so the box must be two cells
    // wider than the label is "long"; the second cell of the glyph must not be
    // emitted as a space.
    let art = render_mermaid("flowchart LR\n  A[界_x] --> B[y界]").expect("wide labels");
    assert_eq!(
        art.plain(),
        "┌──────┐    ┌─────┐\n│ 界_x ├───▶│ y界 │\n└──────┘    └─────┘"
    );
    for line in art.lines.iter().filter(|line| line.starts_with('│')) {
        assert_eq!(display_width(line), 8 + 4 + 7, "line = {line:?}");
    }
}

#[test]
fn undirected_links_have_no_arrow_head() {
    let art = render_mermaid("flowchart LR\n  A[One] --- B[Two]").expect("open link");
    assert!(art.plain().contains("├────│"), "observed:\n{}", art.plain());
    assert!(!art.plain().contains('▶'), "observed:\n{}", art.plain());

    let down = render_mermaid("flowchart TD\n  A[One] --- B[Two]").expect("open link");
    assert!(!down.plain().contains('▼'), "observed:\n{}", down.plain());
}

#[test]
fn directed_links_point_at_their_target() {
    let right = render_mermaid("flowchart LR\n  A[One] --> B[Two]").expect("arrow");
    assert!(right.plain().contains('▶'), "observed:\n{}", right.plain());
    let down = render_mermaid("flowchart TD\n  A[One] --> B[Two]").expect("arrow");
    assert!(down.plain().contains('▼'), "observed:\n{}", down.plain());
}

#[test]
fn link_labels_sit_on_the_connector() {
    let art = render_mermaid("flowchart LR\n  A -->|start| B\n  B --> C").expect("labels");
    assert!(art.plain().contains("start"), "observed:\n{}", art.plain());
    let down = render_mermaid("flowchart TD\n  A -->|start| B").expect("labels");
    assert!(
        down.plain().contains("start"),
        "observed:\n{}",
        down.plain()
    );
}

#[test]
fn layouts_place_the_same_graph_differently() {
    let source = "flowchart LR\n  A[Start] --> B[Done]";
    let left_right = render_mermaid(source).expect("lr");
    let top_down = render_mermaid(&source.replace(" LR", " TD")).expect("td");
    assert_eq!(left_right.lines.len(), 3);
    assert_eq!(top_down.lines.len(), 8);
    assert!(left_right.width > top_down.width);
    // Deterministic: the same source renders identically twice.
    assert_eq!(left_right, render_mermaid(source).expect("lr again"));
}

#[test]
fn size_limits_fail_closed() {
    let many_nodes = format!(
        "flowchart LR\n{}",
        (0..130)
            .map(|i| format!("  n{i} --> n{}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(matches!(
        render_mermaid(&many_nodes),
        Err(MermaidError::TooLarge { .. })
    ));

    let wide_label = format!("flowchart LR\n  A[{}] --> B", "x".repeat(1025));
    assert!(matches!(
        render_mermaid(&wide_label),
        Err(MermaidError::TooLarge { .. })
    ));

    let huge = format!(
        "flowchart LR\n{}",
        (0..3000)
            .map(|i| format!("  a{i} --> a{}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(matches!(
        render_mermaid(&huge),
        Err(MermaidError::TooLarge { .. })
    ));
}

#[test]
fn error_variants_are_typed() {
    assert!(matches!(
        render_mermaid(""),
        Err(MermaidError::UnsupportedDiagram { .. }) | Err(MermaidError::MissingHeader)
    ));
    assert!(matches!(
        render_mermaid("pie\n  title Pets"),
        Err(MermaidError::UnsupportedDiagram { .. })
    ));
    assert!(render_mermaid("graph BT\n  A --> B").is_ok());
    assert!(render_mermaid("flowchart LR\n  A -- text --> B").is_ok());
    assert!(render_mermaid("flowchart LR\n  A --> B --> C --> A").is_ok());
    assert!(render_mermaid("flowchart LR\n  A --> C\n  A --> B\n  B --> C").is_ok());
}

/// Deterministic token soup: no input may panic, exceed the documented limits,
/// or take unbounded time. 1500 short inputs built from Mermaid fragments
/// (valid and invalid) exercise the parser and both layouts.
#[test]
fn random_token_soup_never_panics_and_stays_within_limits() {
    const TOKENS: &[&str] = &[
        "graph",
        "flowchart",
        "TD",
        "TB",
        "LR",
        "BT",
        "RL",
        "-->",
        "---",
        "-.->",
        "==>",
        "->",
        "|",
        "|x|",
        "[",
        "]",
        "(",
        ")",
        "{",
        "}",
        "\"",
        "%%",
        ";",
        "&",
        "subgraph",
        "end",
        "direction",
        ":::c",
        "classDef",
        "A",
        "B",
        "node_1",
        "界",
        "\n",
        " ",
        "pie",
        "\t",
    ];
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (state >> 33) as usize
    };
    for _ in 0..1500 {
        let token_count = 1 + next() % 16;
        let mut source = String::new();
        for _ in 0..token_count {
            source.push_str(TOKENS[next() % TOKENS.len()]);
        }
        if let Ok(art) = render_mermaid(&source) {
            assert!(art.width <= 4096, "width {} for {source:?}", art.width);
            assert!(
                art.lines.len() <= 2048,
                "rows {} for {source:?}",
                art.lines.len()
            );
        }
    }
}
