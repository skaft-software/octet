//! Native Tern Surface Protocol rendering for octet's shell.
//!
//! Tern renders a program's UI natively when it describes that UI over TSP; a
//! live inline surface takes over the pane, so octet's ANSI grid is hidden
//! while one is open. This module projects the shell's semantic display model
//! ([`ShellState`] + [`TranscriptBlock`]) into native surfaces on the render
//! thread, next to the existing ANSI paint. Non-Tern terminals are untouched.
//!
//! Enabled automatically inside a Tern pane (`TERM_PROGRAM=tern`).
//! `OCTET_TUI_TERN=0` disables it; `OCTET_TUI_TERN=1` forces it.

use std::collections::BTreeMap;
use std::time::Duration;

use octet_tern::client::TernClient;
use octet_tern::wire::{Node, Op, Palette, Props, Span, SurfaceMode, Text, Tone, REGION_MAIN};

use super::renderer_runtime::SharedState;
use super::{NoticeTone, ShellState, TranscriptBlock};

/// The surface this renderer owns.
const SURFACE: &str = "octet.session";
/// Id of the single subtree replaced whenever the projection changes.
const CONTENT: &str = "content";
/// Minimum spacing between native frames.
const FRAME_INTERVAL: Duration = Duration::from_millis(80);

/// Whether native Tern rendering should run in this process.
pub(crate) fn enabled() -> bool {
    let forced = std::env::var("OCTET_TUI_TERN").ok();
    match forced.as_deref() {
        Some("0") | Some("off") | Some("false") | Some("no") => false,
        Some(_) => true,
        None => std::env::var("TERM_PROGRAM").is_ok_and(|v| v.eq_ignore_ascii_case("tern")),
    }
}

/// [`enabled`] resolved once; the input filter asks on every event.
pub(crate) fn enabled_cached() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(enabled);
    *ENABLED
}

/// Change key: resend only when the semantic state moved.
type Key = (u64, u64, u64, (u16, u16), usize);

/// The native surface: one TSP connection, its palette, and the last sent key.
pub(super) struct TernSurface {
    client: TernClient,
    last: Option<Key>,
    last_sent: Option<std::time::Instant>,
    regions: bool,
    content: bool,
    palette_epoch: Option<u64>,
}

impl TernSurface {
    /// Start a surface if Tern rendering is enabled, else `None`.
    pub(super) fn start() -> Option<Self> {
        if !enabled() {
            return None;
        }
        let client = TernClient::connect("octet", Some(env!("CARGO_PKG_VERSION"))).ok()?;
        let mut surface = TernSurface {
            client,
            last: None,
            last_sent: None,
            regions: false,
            content: false,
            palette_epoch: None,
        };
        let _ = surface
            .client
            .open(SURFACE, SurfaceMode::Inline, "octet", Some("octet.session"));
        Some(surface)
    }

    /// Send the current projection when it changed since the last frame.
    pub(super) fn present(&mut self, state: &SharedState) {
        let shell = state.borrow();
        if shell.startup_pending {
            return;
        }
        let theme_epoch = shell.theme_epoch;
        if self.palette_epoch != Some(theme_epoch) {
            if let Some(palette) = palette(&shell.theme, SURFACE) {
                let _ = self.client.palette(&palette);
            }
            self.palette_epoch = Some(theme_epoch);
        }
        let key: Key = (
            shell.render_revision,
            shell.transcript_epoch,
            shell.editor.revision(),
            shell.size,
            shell.transcript.len(),
        );
        let now = std::time::Instant::now();
        if self.last == Some(key)
            || self
                .last_sent
                .is_some_and(|sent| now.duration_since(sent) < FRAME_INTERVAL)
        {
            return;
        }

        let mut ops = Vec::new();
        if !self.regions {
            ops.extend(TernClient::regions(SURFACE));
            self.regions = true;
        }
        if self.content {
            ops.push(Op::Del {
                id: CONTENT.to_owned(),
            });
            self.content = false;
        }
        ops.push(Op::add(REGION_MAIN, project(&shell)));
        // Acks are consumed by the input filter, so never block on credit here.
        let _ = self.client.frame_ops_now(SURFACE, ops);
        self.last = Some(key);
        self.last_sent = Some(now);
        self.content = true;
    }
}

impl Drop for TernSurface {
    fn drop(&mut self) {
        // Keep the transcript in scrollback; the ANSI grid returns beneath it.
        let _ = self.client.close(SURFACE, true);
    }
}

/// The changed root: transcript, then the composer.
fn project(shell: &ShellState) -> Node {
    let mut children = Vec::with_capacity(shell.transcript.len() + 1);
    for (index, block) in shell.transcript.iter().enumerate() {
        if let Some(node) = block_node(index, block) {
            children.push(node);
        }
    }
    if let Some(working) = working_row(shell) {
        children.push(working);
    }
    children.push(composer(shell));
    Node::with_children(
        CONTENT,
        octet_tern::wire::Kind::Col,
        Props::new().role("octet.transcript").set("gap", "lg"),
        children,
    )
}

fn id(index: usize, suffix: &str) -> String {
    format!("t{index}.{suffix}")
}

fn block_node(index: usize, block: &TranscriptBlock) -> Option<Node> {
    match block {
        TranscriptBlock::User { text, .. } => Some(octet_tern::scene::prompt_card(
            id(index, "user"),
            text,
            "",
            "",
        )),
        TranscriptBlock::Assistant(block) => Some(octet_tern::scene::assistant_section(
            id(index, "assistant"),
            &block.text,
        )),
        TranscriptBlock::Reasoning(block) => Some(octet_tern::scene::thinking_section(
            id(index, "reasoning"),
            &block.text,
            block
                .reasoning_elapsed
                .map_or(0, |elapsed| elapsed.as_millis() as u64),
        )),
        TranscriptBlock::Tool(panel) => Some(tool_node(index, panel)),
        TranscriptBlock::Shell(shell) => Some(shell_node(index, shell)),
        TranscriptBlock::Notice(text) => Some(text_node(index, text, "muted")),
        TranscriptBlock::NoticeStatus { text, tone, .. } => Some(text_node(
            index,
            text,
            match tone {
                NoticeTone::Success | NoticeTone::ToolSuccess => "success",
                NoticeTone::Error | NoticeTone::ToolError => "error",
                NoticeTone::ToolActive => "accent",
            },
        )),
        TranscriptBlock::Outcome(outcome) => Some(octet_tern::scene::turn_usage_spans(
            id(index, "outcome"),
            outcome_parts(outcome),
        )),
        TranscriptBlock::Compaction(compaction) => Some(Node::with_children(
            id(index, "compaction"),
            octet_tern::wire::Kind::Section,
            Props::new()
                .role("octet.compaction")
                .text("head", Text::Plain(compaction.label.clone())),
            vec![Node::new(
                id(index, "compaction.md"),
                octet_tern::wire::Kind::Md,
                Props::new().set("text", compaction.summary.clone()),
            )],
        )),
        TranscriptBlock::Subagents(subagents) => {
            Some(text_node(index, &subagents.label(), "muted"))
        }
        TranscriptBlock::UpdateAvailable(version) => Some(text_node(
            index,
            &format!("octet {version} is available"),
            "muted",
        )),
    }
}

fn text_node(index: usize, text: &str, token: &str) -> Node {
    Node::new(
        id(index, "text"),
        octet_tern::wire::Kind::Text,
        Props::new()
            .text(
                "spans",
                Text::Spans(vec![Span::styled(text.to_owned(), token)]),
            )
            .set("measure", "prose"),
    )
}

/// Extract a tool's primary argument from its JSON argument string.
fn tool_target(panel: &super::ToolPanel) -> (String, &'static str) {
    let parsed: Option<serde_json::Value> = serde_json::from_str(&panel.args).ok();
    if let Some(object) = parsed.as_ref().and_then(serde_json::Value::as_object) {
        for (key, kind) in [
            ("command", "command"),
            ("cmd", "command"),
            ("path", "path"),
            ("file_path", "path"),
            ("pattern", "pattern"),
            ("query", "query"),
            ("url", "text"),
        ] {
            if let Some(value) = object.get(key).and_then(serde_json::Value::as_str) {
                return (value.to_owned(), kind);
            }
        }
    }
    let trimmed = panel.args.trim();
    let trimmed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    (trimmed.chars().take(160).collect(), "text")
}

fn tool_node(index: usize, panel: &super::ToolPanel) -> Node {
    let (target, target_kind) = tool_target(panel);
    let status = if !panel.finished {
        "running"
    } else if panel.is_error {
        "error"
    } else {
        "done"
    };
    let mut meta = Vec::new();
    if let Some(duration) = panel.duration {
        meta.push(Text::Plain(format_duration(duration)));
    }
    if panel.finished && panel.is_error {
        if let Some(reason) = &panel.failure_reason {
            meta.push(Text::Plain(reason.clone()));
        }
    }

    let mut body = Vec::new();
    if let Some(diff) = super::tool_render::tool_diff(panel) {
        body.push(octet_tern::scene::diff_block(
            id(index, "diff"),
            &target,
            &diff,
        ));
    } else if !panel.output.trim().is_empty() {
        let shown: String = panel
            .output
            .lines()
            .take(200)
            .collect::<Vec<_>>()
            .join("\n");
        body.push(octet_tern::scene::ansi_block(id(index, "out"), &shown));
    }

    octet_tern::scene::tool_card(
        id(index, "tool"),
        &panel.name,
        &tool_title(&panel.name),
        &target,
        target_kind,
        status,
        meta,
        body,
    )
}

fn shell_node(index: usize, shell: &super::ShellOutput) -> Node {
    let status = if shell.running {
        "running"
    } else if shell.exit_code == 0 {
        "done"
    } else {
        "error"
    };
    let mut meta = Vec::new();
    if !shell.running && shell.exit_code != 0 {
        meta.push(Text::Plain(format!("exit {}", shell.exit_code)));
    }
    let body = if shell.output.trim().is_empty() {
        Vec::new()
    } else {
        vec![octet_tern::scene::ansi_block(
            id(index, "out"),
            &shell.output,
        )]
    };
    octet_tern::scene::tool_card(
        id(index, "shell"),
        "bash",
        "Bash",
        &shell.command,
        "command",
        status,
        meta,
        body,
    )
}

fn tool_title(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Tool".to_owned(),
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() >= 1 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn outcome_parts(outcome: &super::OutcomeBlock) -> Vec<Span> {
    use crate::presentation::RunOutcome;
    let (duration, verdict, token) = match &outcome.outcome {
        RunOutcome::Completed { elapsed, summary }
        | RunOutcome::CompletedWithWarnings {
            elapsed, summary, ..
        } => (
            Some(*elapsed),
            format!("{} tools", summary.tool_calls),
            "muted",
        ),
        RunOutcome::Failed { elapsed, .. } => (Some(*elapsed), "failed".to_owned(), "error"),
        RunOutcome::Interrupted { elapsed } => {
            (Some(*elapsed), "interrupted".to_owned(), "warning")
        }
        RunOutcome::NeedsInput { .. } => (None, "needs input".to_owned(), "warning"),
    };
    let mut spans = Vec::with_capacity(3);
    if let Some(duration) = duration {
        spans.push(Span::styled(format_duration(duration), "dim"));
        spans.push(Span::new(" · "));
    }
    spans.push(Span::styled(verdict, token));
    spans
}

/// Whether any block is still running, and its label.
fn working_row(shell: &ShellState) -> Option<Node> {
    let running = shell
        .transcript
        .iter()
        .rev()
        .find_map(|block| match block {
            TranscriptBlock::Tool(panel) if !panel.finished => Some(panel.name.clone()),
            TranscriptBlock::Shell(shell) if shell.running => Some(shell.command.clone()),
            TranscriptBlock::Assistant(block) if !block.finished => Some("Thinking".to_owned()),
            _ => None,
        })?;
    Some(octet_tern::scene::working_row(
        "work",
        &format!("Working · {running}"),
        0,
        None,
    ))
}

/// The composer: model chip, context meter, and the draft (display only).
fn composer(shell: &ShellState) -> Node {
    let mut chips = vec![Node::new(
        "composer.model",
        octet_tern::wire::Kind::Badge,
        Props::new()
            .set("text", shell.model_display.clone())
            .tone(Tone::Accent),
    )];
    if let Some((used, total)) = shell.run_context_estimate {
        if total > 0 {
            let value = used as f64 / total as f64;
            chips.push(Node::new(
                "composer.context",
                octet_tern::wire::Kind::Meter,
                Props::new()
                    .set("value", value)
                    .set("style", "bar")
                    .set("size", "sm")
                    .set("label", format!("{:.0}%", value * 100.0))
                    .set("total", format!("{}K", total / 1000)),
            ));
        }
    }
    let chip_row = Node::with_children(
        "composer.chips",
        octet_tern::wire::Kind::Row,
        Props::new().set("gap", "sm").set("align", "center"),
        chips,
    );

    let text = shell.editor.text();
    let draft = text.trim_end_matches('\n');
    let draft = if draft.is_empty() {
        "Ask octet anything"
    } else {
        draft
    };
    let token = if text.is_empty() { "dim" } else { "text" };
    let editor = Node::new(
        "composer.editor",
        octet_tern::wire::Kind::Text,
        Props::new()
            .text(
                "spans",
                Text::Spans(vec![
                    Span::styled("❯ ", "accent"),
                    Span::styled(draft.to_owned(), token),
                ]),
            )
            .set("measure", "prose"),
    );

    Node::with_children(
        CONTENT.to_owned() + ".composer",
        octet_tern::wire::Kind::Col,
        Props::new().role("octet.composer").set("gap", "sm"),
        vec![chip_row, editor],
    )
}

/// Build the TSP theme palette from octet's resolved semantic roles.
fn palette(theme: &crate::tui::theme::OctetTheme, surface: &str) -> Option<Palette> {
    use sexy_tui_rs::Color;

    fn hex(color: Color) -> Option<String> {
        match color {
            Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
            _ => None,
        }
    }

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    // Foreground roles.
    const ROLES: &[(&str, &str)] = &[
        ("text", "text"),
        ("muted", "muted"),
        ("dim", "dim"),
        ("accent", "accent"),
        ("success", "success"),
        ("warning", "warning"),
        ("error", "error"),
        ("border", "border"),
        ("md_heading", "mdHeading"),
        ("md_link", "mdLink"),
        ("inline_code", "mdCode"),
        ("md_code", "mdCode"),
        ("md_code_block", "mdCodeBlock"),
        ("md_quote", "mdQuote"),
        ("md_quote_border", "mdQuoteBorder"),
        ("md_hr", "mdHr"),
        ("md_list_bullet", "mdListBullet"),
        ("diff_add", "toolDiffAdded"),
        ("diff_remove", "toolDiffRemoved"),
        ("diff_context", "toolDiffContext"),
        ("syntax_comment", "syntaxComment"),
        ("syntax_keyword", "syntaxKeyword"),
        ("syntax_function", "syntaxFunction"),
        ("syntax_variable", "syntaxVariable"),
        ("syntax_string", "syntaxString"),
        ("syntax_number", "syntaxNumber"),
        ("syntax_type", "syntaxType"),
        ("syntax_operator", "syntaxOperator"),
        ("syntax_punctuation", "syntaxPunctuation"),
    ];
    for (role, token) in ROLES {
        if let Some(value) = hex(theme.semantic_style(role).foreground) {
            out.insert((*token).to_owned(), value);
        }
    }
    // Surface fills.
    const SURFACES: &[(&str, &str)] = &[
        ("surface.user", "userMessageBg"),
        ("surface.tool", "toolPendingBg"),
        ("surface.shell", "statusLineBg"),
        ("surface.assistant", "customMessageBg"),
    ];
    for (role, token) in SURFACES {
        if let Some(value) = hex(theme.semantic_style(role).background) {
            out.insert((*token).to_owned(), value);
        }
    }
    // Derived reasoning/status tokens.
    let accent = out.get("accent").cloned();
    let dim = out.get("dim").cloned();
    let error = out.get("error").cloned();
    if let Some(value) = accent.clone() {
        out.entry("thinkingLow".to_owned())
            .or_insert_with(|| value.clone());
        out.entry("thinkingMedium".to_owned())
            .or_insert_with(|| value.clone());
        out.entry("thinkingHigh".to_owned()).or_insert(value);
    }
    if let Some(value) = dim {
        out.entry("thinkingOff".to_owned()).or_insert(value);
    }
    if let Some(value) = error {
        out.entry("thinkingXhigh".to_owned()).or_insert(value);
    }

    if out.is_empty() {
        return None;
    }
    let mut dark = serde_json::Map::new();
    for (key, value) in &out {
        dark.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    Some(Palette {
        sf: surface.to_owned(),
        dark: Some(dark),
        light: None,
        name: Some(octet_tern::wire::VariantNames {
            dark: theme.metadata().name.clone().into(),
            light: None,
        }),
    })
}
