//! Builders for octet's coding surfaces as TSP nodes.
//!
//! These mirror what `omp` describes to Tern — a transcript of prompt cards,
//! reasoning sections, tool cards, a clocked working row, a todo HUD and a
//! composer — using the same component kinds so the terminal draws them
//! natively. Callers own the data; this module only shapes it.

use serde_json::json;

use crate::wire::{
    Effect, Kind, Node, Op, Props, Space, Span, Text, Tone, REGION_DOCK, REGION_MAIN,
};

/// Semantic span tokens the terminal maps to the active palette.
pub mod tok {
    /// Default text.
    pub const TEXT: &str = "text";
    /// Muted secondary text.
    pub const MUTED: &str = "muted";
    /// Dim tertiary text.
    pub const DIM: &str = "dim";
    /// Accent ink.
    pub const ACCENT: &str = "accent";
    /// Success ink.
    pub const SUCCESS: &str = "success";
    /// Warning ink.
    pub const WARNING: &str = "warning";
    /// Error ink.
    pub const ERROR: &str = "error";
    /// Informational ink.
    pub const INFO: &str = "info";
    /// Inline code.
    pub const CODE: &str = "code";
    /// A path.
    pub const PATH: &str = "path";
    /// A number.
    pub const NUM: &str = "num";
    /// Bold run.
    pub const STRONG: &str = "strong";
}

/// A styled span.
pub fn span(text: impl Into<String>, s: &str) -> Span {
    Span::styled(text, s)
}

/// Props from a JSON object literal.
fn props(value: serde_json::Value) -> Props {
    Props::from_value(value)
}

fn node(id: impl Into<String>, k: Kind, p: Props) -> Node {
    Node::new(id, k, p)
}

fn parent(id: impl Into<String>, k: Kind, p: Props, children: Vec<Node>) -> Node {
    Node::with_children(id, k, p, children)
}

/// A prompt card for one submitted message.
pub fn prompt_card(id: impl Into<String>, markdown: &str, model: &str, time: &str) -> Node {
    let id = id.into();
    let head = Text::Spans(vec![
        Span::styled(model.to_owned(), tok::ACCENT),
        Span::new("  "),
        Span::styled(time.to_owned(), tok::DIM),
    ]);
    let body = node(
        format!("{id}.md"),
        Kind::Md,
        Props::new().set("text", markdown),
    );
    parent(
        id,
        Kind::Card,
        Props::new()
            .role("octet.user")
            .tone(Tone::User)
            .text("head", head)
            .set("frame", "card"),
        vec![body],
    )
}

/// A reasoning section, with the terminal clocking the elapsed time.
pub fn thinking_section(id: impl Into<String>, markdown: &str, took_ms: u64) -> Node {
    let id = id.into();
    let head = Text::Spans(vec![
        Span::styled("Thinking", tok::MUTED),
        Span::new("  "),
        Span::styled(format!("{:.1}s", took_ms as f64 / 1000.0), tok::DIM),
    ]);
    let body = node(
        format!("{id}.md"),
        Kind::Md,
        Props::new().set("text", markdown),
    );
    parent(
        id,
        Kind::Section,
        Props::new()
            .role("octet.thinking")
            .text("head", head)
            .set("took", took_ms)
            .set("collapsible", true),
        vec![body],
    )
}

/// An assistant message section.
pub fn assistant_section(id: impl Into<String>, markdown: &str) -> Node {
    let id = id.into();
    let body = node(
        format!("{id}.md"),
        Kind::Md,
        Props::new().set("text", markdown),
    );
    parent(
        id,
        Kind::Section,
        Props::new()
            .role("octet.assistant")
            .set("collapsible", true),
        vec![body],
    )
}

/// A tool card. `body` is the disclosed content (a diff, code, or output).
#[allow(clippy::too_many_arguments)]
pub fn tool_card(
    id: impl Into<String>,
    name: &str,
    title: &str,
    target: &str,
    target_kind: &str,
    status: &str,
    meta: Vec<Text>,
    body: Vec<Node>,
) -> Node {
    parent(
        id,
        Kind::Tool,
        props(json!({
            "name": name,
            "title": title,
            "target": target,
            "targetKind": target_kind,
            "status": status,
            "frame": "card",
            "collapsible": true,
            "meta": meta,
        })),
        body,
    )
}

/// A unified diff block.
pub fn diff_block(id: impl Into<String>, path: &str, text: &str) -> Node {
    node(
        id,
        Kind::Diff,
        Props::new()
            .set("path", path)
            .set("text", text)
            .set("mode", "auto"),
    )
}

/// A syntax-highlighted code block.
pub fn code_block(id: impl Into<String>, lang: &str, text: &str) -> Node {
    node(
        id,
        Kind::Code,
        Props::new()
            .set("lang", lang)
            .set("text", text)
            .set("numbers", true),
    )
}

/// Raw terminal output.
pub fn ansi_block(id: impl Into<String>, text: &str) -> Node {
    node(
        id,
        Kind::Ansi,
        Props::new().set("text", text).set("follow", true),
    )
}

/// The clocked working row: spinner, shimmering label, elapsed time, rate.
pub fn working_row(id: impl Into<String>, label: &str, age_ms: u64, rate: Option<f64>) -> Node {
    let mut children = vec![
        node(
            "work.spin",
            Kind::Spinner,
            Props::new().set("style", "dots"),
        ),
        Node::new(
            "work.label",
            Kind::Shimmer,
            Props::new()
                .text(
                    "spans",
                    Text::Spans(vec![span(label.to_owned(), tok::TEXT)]),
                )
                .set("mode", "kitt"),
        ),
        node(
            "work.elapsed",
            Kind::Elapsed,
            Props::new().set("age", age_ms).set("format", "short"),
        ),
    ];
    if let Some(rate) = rate {
        children.push(node(
            "work.rate",
            Kind::Rate,
            Props::new().set("value", rate).set("unit", "tok/s"),
        ));
    }
    parent(
        id,
        Kind::Row,
        Props::new()
            .role("octet.activity")
            .set("gap", Space::Sm)
            .set("align", "center"),
        children,
    )
}

/// One row of turn facts (time, tokens, cost).
pub fn turn_usage(id: impl Into<String>, parts: &[(&str, &str)]) -> Node {
    let spans: Vec<Span> = parts
        .iter()
        .enumerate()
        .flat_map(|(i, (text, token))| {
            let mut out = vec![span(*text, token)];
            if i + 1 < parts.len() {
                out.push(span(" · ", tok::DIM));
            }
            out
        })
        .collect();
    turn_usage_spans(id, spans)
}

/// One row of turn facts from owned spans (for callers that build them dynamically).
///
/// The text child's id derives from the row's id: node ids are unique within a
/// surface and every completed turn contributes one row.
pub fn turn_usage_spans(id: impl Into<String>, spans: Vec<Span>) -> Node {
    let id = id.into();
    let text_id = format!("{id}.text");
    parent(
        id,
        Kind::Row,
        Props::new().role("octet.turn.usage").set("gap", Space::Sm),
        vec![node(
            text_id,
            Kind::Text,
            Props::new()
                .text("spans", Text::Spans(spans))
                .set("measure", "fill"),
        )],
    )
}

/// A todo HUD for the dock region.
pub fn todo_hud(id: impl Into<String>, note: &str, phases: serde_json::Value) -> Node {
    node(
        id,
        Kind::Checklist,
        Props::new()
            .role("octet.todo")
            .set("mode", "hud")
            .set("note", note)
            .set("phases", phases),
    )
}

/// The composer: model chip, effort glyph, context meter, and the editor.
pub fn composer(
    id: impl Into<String>,
    text: &str,
    model: &str,
    effort: &str,
    context: f64,
) -> Node {
    let id = id.into();
    let chips = parent(
        format!("{id}.chips"),
        Kind::Row,
        Props::new()
            .role("octet.composer.chips")
            .set("gap", Space::Sm)
            .set("align", "center"),
        vec![
            node(
                "composer.model",
                Kind::Badge,
                Props::new().set("text", model).tone(Tone::Accent),
            ),
            node(
                "composer.effort",
                Kind::Effort,
                Props::new().set("level", effort),
            ),
            node(
                "composer.context",
                Kind::Meter,
                Props::new()
                    .set("value", context)
                    .set("style", "bar")
                    .set("size", "sm")
                    .set("label", format!("{:.0}%", context * 100.0))
                    .set("total", "200K"),
            ),
        ],
    );
    let editor = node(
        format!("{id}.editor"),
        Kind::Editor,
        Props::new()
            .text("prompt", Text::Spans(vec![Span::styled("❯", tok::ACCENT)]))
            .set("placeholder", "Ask octet anything")
            .set("text", text)
            .set("cursor", text.chars().count()),
    );
    parent(
        id,
        Kind::Col,
        Props::new().role("octet.composer").set("gap", Space::Sm),
        vec![chips, editor],
    )
}

/// Add a node under a region.
fn add(region: &str, node: Node) -> Op {
    Op::add(region, node)
}

/// A representative octet session for a surface: opening the three fixed
/// regions and filling `main` with a transcript plus `dock` with a todo HUD.
///
/// The demo binary uses this; real callers build their own op stream from the
/// same builders.
pub fn demo_session(surface: &str) -> Vec<Op> {
    let mut ops = crate::client::TernClient::regions(surface);

    let transcript = parent(
        "transcript",
        Kind::Col,
        Props::new().role("octet.transcript").set("gap", Space::Lg),
        vec![
            prompt_card(
                "t1.prompt",
                "Refactor the credit check so underflow can't pass, then open `/tern`.",
                "opus-4.6",
                "19:16",
            ),
            thinking_section(
                "t1.thinking",
                "The guard uses `saturating_sub`, so a negative balance clamps to zero and the \
                 comparison passes.\n\n- Check the call site\n- Prefer a checked subtraction\n- \
                 Keep the error type stable",
                4200,
            ),
            assistant_section(
                "t1.answer",
                "## Fix\n\nReplace the saturating subtraction with a checked one and return the \
                 typed error:\n\n```rust\nlet remaining = balance\n    .checked_sub(amount)\n    \
                 .ok_or(Error::InsufficientFunds)?;\n```\n\nThe retry budget is $t_r = 1.5$s, so a \
                 single retry is enough here.\n\n| field | before | after |\n| --- | --- | --- |\n| \
                 overflow | pass | fail |\n| latency | 1.2s | 1.1s |\n\n> The invariant is \
                 `remaining >= 0` on every path.",
            ),
            tool_card(
                "t2.edit",
                "edit",
                "Edit",
                "crates/credit/src/ledger.rs",
                "path",
                "done",
                vec![Text::from("+9 −4"), Text::from("1 file")],
                vec![diff_block(
                    "t2.edit.diff",
                    "crates/credit/src/ledger.rs",
                    "--- a/crates/credit/src/ledger.rs\n+++ b/crates/credit/src/ledger.rs\n@@ -18,7 +18,9 @@ impl Ledger {\n-    let remaining = self.balance.saturating_sub(amount);\n-    if remaining == 0 && amount > self.balance {\n-        return Err(Error::InsufficientFunds);\n-    }\n+    let remaining = self\n+        .balance\n+        .checked_sub(amount)\n+        .ok_or(Error::InsufficientFunds)?;\n+    debug_assert!(remaining <= self.balance);\n",
                )],
            ),
            tool_card(
                "t3.test",
                "bash",
                "Bash",
                "cargo test -p credit",
                "command",
                "done",
                vec![Text::from("52 passed"), Text::from("2.4s")],
                vec![ansi_block(
                    "t3.test.out",
                    "running 52 tests\n....................................................\ntest result: ok. 52 passed; 0 failed; 0 ignored\n",
                )],
            ),
            turn_usage(
                "t1.usage",
                &[("1.1s", tok::DIM), ("4.2k tokens", tok::MUTED), ("$0.012", tok::NUM)],
            ),
            working_row("t4.work", "Running the integration suite", 2400, Some(38.6)),
        ],
    );

    ops.push(add(REGION_MAIN, transcript));
    ops.push(add(
        REGION_MAIN,
        composer("composer", "open /tern", "opus-4.6", "high", 0.12),
    ));
    ops.push(add(
        REGION_DOCK,
        todo_hud(
            "todos",
            "2/4 done",
            json!([
                {
                    "id": "p1",
                    "title": "Diagnose",
                    "items": [
                        { "id": "i1", "text": "Reproduce the underflow", "status": "done" },
                        { "id": "i2", "text": "Find the guard", "status": "done" }
                    ]
                },
                {
                    "id": "p2",
                    "title": "Fix and verify",
                    "items": [
                        { "id": "i3", "text": "Checked subtraction", "status": "active" },
                        { "id": "i4", "text": "Run the suite", "status": "pending" }
                    ]
                }
            ]),
        ),
    ));
    ops
}

/// A shimmering label node, for callers that only need one.
pub fn shimmer(id: impl Into<String>, text: &str) -> Node {
    Node::new(
        id,
        Kind::Shimmer,
        Props::new()
            .text("spans", Text::Spans(vec![span(text.to_owned(), tok::TEXT)]))
            .set("mode", "kitt"),
    )
}

/// A clocked elapsed readout.
pub fn elapsed(id: impl Into<String>, age_ms: u64) -> Node {
    node(
        id,
        Kind::Elapsed,
        Props::new().set("age", age_ms).set("format", "short"),
    )
}

/// Emit a `shimmer` effect span, used to keep a label alive before the terminal
/// clocks it itself.
pub fn shimmer_span(text: impl Into<String>) -> Span {
    Span::styled(text, tok::TEXT).effect(Effect::Shimmer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_session_opens_regions_and_fills_main_and_dock() {
        let ops = demo_session("octet.session");
        // Three region adds plus main content plus composer plus dock.
        assert!(ops.len() >= 6);
        let json = serde_json::to_value(&ops).unwrap();
        let array = json.as_array().unwrap();
        // First op is the main region under the surface.
        assert_eq!(array[0][0], "add");
        assert_eq!(array[0][1], REGION_MAIN);
        assert_eq!(array[0][2], "octet.session");
        // Some op adds into the dock.
        assert!(array
            .iter()
            .any(|op| op[0] == "add" && op[2] == REGION_DOCK));
    }

    #[test]
    fn tool_card_shapes_kind_tool_with_a_diff_child() {
        let card = tool_card(
            "t",
            "edit",
            "Edit",
            "src/lib.rs",
            "path",
            "done",
            vec![],
            vec![diff_block("t.diff", "src/lib.rs", "@@ -1 +1 @@\n-a\n+b\n")],
        );
        let value = serde_json::to_value(&card).unwrap();
        assert_eq!(value["k"], "tool");
        assert_eq!(value["p"]["name"], "edit");
        assert_eq!(value["c"][0]["k"], "diff");
    }

    #[test]
    fn composer_has_a_prompt_and_context_meter() {
        let node = composer("c", "hi", "opus", "high", 0.5);
        let value = serde_json::to_value(&node).unwrap();
        assert_eq!(value["k"], "col");
        let children = value["c"].as_array().unwrap();
        assert!(children.iter().any(|c| c["k"] == "editor"));
        assert!(children[0]["c"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["k"] == "meter"));
    }

    #[test]
    fn working_row_clocks_elapsed_and_rate() {
        let node = working_row("w", "Working", 1200, Some(12.0));
        let value = serde_json::to_value(&node).unwrap();
        let kinds: Vec<&str> = value["c"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["k"].as_str().unwrap())
            .collect();
        assert!(kinds.contains(&"spinner"));
        assert!(kinds.contains(&"shimmer"));
        assert!(kinds.contains(&"elapsed"));
        assert!(kinds.contains(&"rate"));
    }

    fn collect_ids<'a>(node: &'a Node, ids: &mut Vec<&'a str>) {
        ids.push(&node.id);
        for child in node.c.iter().flatten() {
            collect_ids(child, ids);
        }
    }

    #[test]
    fn turn_usage_rows_of_different_turns_never_share_node_ids() {
        // Tern rejects a frame that repeats a node id within one surface
        // ("duplicate id usage.text"), which dropped octet back to ANSI after
        // the second completed turn. Each row must derive its children's ids
        // from its own id.
        let spans = || vec![span("1.9s", tok::DIM)];
        let rows = [
            turn_usage("t1.outcome", &[("1.9s", tok::DIM), ("11K tok", tok::DIM)]),
            turn_usage("t2.outcome", &[("3.1s", tok::DIM)]),
            turn_usage_spans("t3.outcome", spans()),
            turn_usage_spans("t4.outcome", spans()),
        ];
        let mut ids = Vec::new();
        for row in &rows {
            collect_ids(row, &mut ids);
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "duplicate node ids: {ids:?}");
    }
}
