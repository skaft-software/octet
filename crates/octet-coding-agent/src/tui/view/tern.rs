//! Native Tern adaptation of octet's semantic shell. This renderer exclusively
//! owns presentation while active: no hidden ANSI grid is painted underneath it.
//! `OCTET_TUI_TERN=0` disables automatic `TERM_PROGRAM=tern` detection.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use octet_tern::client::TernClient;
use octet_tern::frame::Incoming;
use octet_tern::wire::{
    Event, Kind, Node, Op, Props, Reply, Span, SurfaceMode, Text, Tone, REGION_DOCK, REGION_LAYER,
    REGION_MAIN,
};
use serde_json::json;

use super::renderer_model::{RenderModel, RenderOwner};
use super::renderer_runtime::SharedState;
use super::terminal_text::sanitize_for_terminal;
use super::tern_theme::NativeTheme;
use super::{NoticeTone, ShellState, TranscriptBlock};
use crate::tui::theme::ModelLab;

pub(super) const SURFACE: &str = "octet.session";
const FRAME_INTERVAL: Duration = Duration::from_millis(32);
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) fn enabled() -> bool {
    match std::env::var("OCTET_TUI_TERN").ok().as_deref() {
        Some("0" | "off" | "false" | "no") => false,
        Some(_) => true,
        None => std::env::var("TERM_PROGRAM").is_ok_and(|v| v.eq_ignore_ascii_case("tern")),
    }
}

pub(crate) fn enabled_cached() -> bool {
    static ENABLED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(enabled);
    *ENABLED
}

#[derive(Default, Clone)]
struct Projection {
    main: Vec<Node>,
    dock: Vec<Node>,
    layer: Vec<Node>,
    focus: Option<String>,
    panel_receipt: Option<super::renderer_geometry::PanelRenderReceipt>,
}

type Key = (u64, u64, bool, u64);

#[derive(Clone, PartialEq, Eq)]
struct MainKey {
    transcript: (u64, u64),
    theme: (u64, Option<ModelLab>, bool),
    cols: u16,
    verbose: bool,
    startup: (bool, bool),
}

pub(super) struct TernSurface {
    client: TernClient,
    owner: RenderOwner,
    sent: Projection,
    last_key: Option<Key>,
    main_key: Option<MainKey>,
    last_sent: Option<Instant>,
    started: Instant,
    ready: bool,
    opened: bool,
    visible: bool,
    regions: bool,
    theme: Option<NativeTheme>,
    theme_key: Option<(u64, Option<ModelLab>)>,
    collapsed: HashMap<String, bool>,
    verbose: bool,
    last_resync: u64,
    last_editor_revision: u64,
    credit_blocked: Option<Instant>,
    images: super::tern_images::NativeImages,
    receipts:
        std::collections::VecDeque<(u64, Option<super::renderer_geometry::PanelRenderReceipt>)>,
}

impl TernSurface {
    pub(super) fn start() -> io::Result<Self> {
        Ok(Self::with_client(TernClient::connect_shared_input(
            "octet",
            Some(env!("CARGO_PKG_VERSION")),
        )?))
    }

    pub(super) fn with_client(client: TernClient) -> Self {
        Self {
            client,
            owner: RenderOwner::default(),
            sent: Projection::default(),
            last_key: None,
            main_key: None,
            last_sent: None,
            started: Instant::now(),
            ready: false,
            opened: false,
            visible: true,
            regions: false,
            theme: None,
            theme_key: None,
            collapsed: HashMap::new(),
            verbose: false,
            last_resync: 0,
            last_editor_revision: 0,
            credit_blocked: None,
            receipts: Default::default(),
            images: Default::default(),
        }
    }

    fn observe(&mut self, message: &Incoming) -> io::Result<()> {
        self.client.observe(message);
        match message {
            Incoming::Reply(Reply::Hello(hello)) => {
                if hello.v != octet_tern::wire::TSP_VERSION
                    || ![
                        Kind::Col,
                        Kind::Row,
                        Kind::Text,
                        Kind::Md,
                        Kind::Editor,
                        Kind::Ansi,
                        Kind::Section,
                        Kind::Rule,
                        Kind::Picker,
                        Kind::Overlay,
                        Kind::List,
                        Kind::Item,
                        Kind::Tool,
                        Kind::Diff,
                    ]
                    .into_iter()
                    .all(|kind| self.client.supports(kind))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "Tern does not support octet's native surface vocabulary",
                    ));
                }
                self.ready = true;
                self.last_key = None;
                self.main_key = None;
            }
            Incoming::Event(Event::Error { sf, msg, .. })
                if sf.as_deref().is_none_or(|id| id == SURFACE) =>
            {
                return Err(io::Error::other(format!("Tern surface: {msg}")));
            }
            Incoming::Event(Event::Gone { sf, .. })
                if sf.as_deref().is_none_or(|id| id == SURFACE) =>
            {
                // Reopen only on explicit eviction, never on resize/zoom. A
                // fresh surface is required before its retained tree is replayed.
                if self.opened {
                    self.client.close(SURFACE, false)?;
                }
                self.opened = false;
                self.regions = false;
                self.sent = Projection::default();
                self.main_key = None;
                self.receipts.clear();
                self.images = Default::default();
                self.last_key = None;
                self.theme_key = None;
                self.client.reset_surface(SURFACE);
            }
            Incoming::Event(Event::Visible { sf, visible })
                if sf.as_deref().is_none_or(|id| id == SURFACE) =>
            {
                self.visible = *visible;
                self.last_key = None;
            }
            Incoming::Event(Event::Theme { .. } | Event::Motion { .. } | Event::Resize { .. }) => {
                self.last_key = None
            }
            Incoming::Event(Event::Toggle {
                sf, id, collapsed, ..
            }) if sf == SURFACE => {
                self.collapsed.insert(id.clone(), *collapsed);
                self.last_key = None;
                self.main_key = None;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn present(&mut self, state: &SharedState) -> io::Result<()> {
        let messages = std::mem::take(
            &mut state
                .native()
                .lock()
                .expect("native mailbox poisoned")
                .messages,
        );
        for message in messages {
            self.observe(&message)?;
            if let Incoming::Event(Event::Ack { sf, s }) = message {
                if sf == SURFACE {
                    let mut latest = None;
                    while self
                        .receipts
                        .front()
                        .is_some_and(|(sequence, _)| *sequence <= s)
                    {
                        latest = self.receipts.pop_front().map(|(_, receipt)| receipt);
                    }
                    if let Some(receipt) = latest {
                        let mut shell = state.borrow();
                        shell.painted_panel = receipt.filter(|receipt| {
                            receipt.is_current(&shell) && receipt.selection_is_current(&shell)
                        });
                    }
                }
            }
        }
        if !self.ready {
            if self.started.elapsed() >= HELLO_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Tern did not answer the native hello query",
                ));
            }
            return Ok(());
        }
        if !self.visible {
            self.credit_blocked = None;
            return Ok(());
        }
        if !self.client.has_credit(SURFACE) {
            if self
                .credit_blocked
                .get_or_insert_with(Instant::now)
                .elapsed()
                >= HELLO_TIMEOUT
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Tern stopped acknowledging native frames",
                ));
            }
            return Ok(());
        }
        self.credit_blocked = None;
        let now = Instant::now();
        let (model, main_key, editor_revision) = {
            let mut shell = state.borrow();
            if shell.startup_pending && shell.panel.is_none() && shell.overlay.is_none() {
                return Ok(());
            }
            let elapsed = shell
                .run
                .current()
                .filter(|run| run.is_active())
                .map_or(0, |run| run.elapsed_at(now).as_secs());
            let key = (
                shell.render_revision,
                shell.theme_epoch,
                self.client.dark(),
                elapsed,
            );
            let resync = state
                .native()
                .lock()
                .expect("native mailbox poisoned")
                .editor_resync;
            if self.last_key == Some(key)
                && resync == self.last_resync
                && !crate::output::has_tui_diagnostics()
            {
                return Ok(());
            }
            let editor_revision = shell.editor.revision();
            let editor_changed = self.last_editor_revision != editor_revision;
            if !editor_changed
                && self
                    .last_sent
                    .is_some_and(|sent| now.duration_since(sent) < FRAME_INTERVAL)
            {
                return Ok(());
            }
            if !shell.startup_pending {
                for message in crate::output::take_tui_diagnostics() {
                    shell.push_block(TranscriptBlock::Notice(message));
                }
            }
            let main_key = MainKey {
                transcript: (shell.transcript_epoch, shell.transcript_semantic_revision),
                theme: (shell.theme_epoch, shell.model_lab, self.client.dark()),
                cols: shell.size.0,
                verbose: shell.verbose_tools,
                startup: (
                    shell.startup_pending,
                    shell.startup_card_started_at.is_some(),
                ),
            };
            (RenderModel::capture(&mut shell), main_key, editor_revision)
        };
        // Accepted source chunks, including streaming text, are materialized
        // outside the frontend lock. Shell metadata is not the streamed source.
        self.owner.accept(model);
        let shell = &mut self.owner.state;
        if self.verbose != shell.verbose_tools {
            self.collapsed.clear();
            self.verbose = shell.verbose_tools;
        }
        let theme_key = (shell.theme_epoch, shell.model_lab);
        if self.theme_key != Some(theme_key) {
            let theme = NativeTheme::resolve(&shell.theme, shell.model_lab, SURFACE)
                .map_err(io::Error::other)?;
            if !self.opened {
                self.client
                    .open(SURFACE, SurfaceMode::Inline, "octet", Some("octet.session"))?;
                self.opened = true;
            }
            self.client.palette(&theme.palette)?;
            self.theme = Some(theme);
            self.theme_key = Some(theme_key);
            shell.invalidate_rich_text();
        }
        shell.theme = self
            .theme
            .as_ref()
            .expect("resolved native theme")
            .active(self.client.dark())
            .clone();
        let main_changed = self.main_key.as_ref() != Some(&main_key);
        if main_changed {
            self.images
                .prepare(shell, self.client.supports(Kind::Image))?;
            self.images.upload(&mut self.client)?;
        }
        let mut next = project(
            shell,
            self.client.reduce_motion(),
            &self.images,
            main_changed,
            editor_revision,
        );
        for nodes in [&mut next.main, &mut next.dock, &mut next.layer] {
            apply_disclosure(nodes, &self.collapsed);
        }
        let mut ops = if self.regions {
            Vec::new()
        } else {
            TernClient::regions(SURFACE)
        };
        if main_changed {
            octet_tern::reconcile::children(REGION_MAIN, &self.sent.main, &next.main, &mut ops);
        }
        for (region, previous, children) in [
            (REGION_DOCK, &self.sent.dock, &next.dock),
            (REGION_LAYER, &self.sent.layer, &next.layer),
        ] {
            octet_tern::reconcile::children(region, previous, children, &mut ops);
        }
        let resync = state
            .native()
            .lock()
            .expect("native mailbox poisoned")
            .editor_resync;
        if resync != self.last_resync {
            if let Some(editor) = find_node(&next.dock, "composer.editor") {
                ops.push(Op::Set {
                    id: editor.id.clone(),
                    props: editor.p.clone().expect("editor props"),
                });
            }
        }
        if !self.regions || self.sent.focus != next.focus {
            ops.push(Op::Focus {
                id: next.focus.clone(),
            });
        }
        if !ops.is_empty() {
            let sequence = self.client.frame_ops_now(SURFACE, ops)?;
            self.receipts
                .push_back((sequence, next.panel_receipt.clone()));
            self.last_sent = Some(now);
        }
        // Never acknowledge a skipped/failed write in the retained baseline.
        self.regions = true;
        // Draft/picker/timer edits do not re-render, clone or diff history.
        if !main_changed {
            next.main = std::mem::take(&mut self.sent.main);
        }
        self.sent = next;
        self.main_key = Some(main_key);
        self.last_key = Some((
            self.owner.revision,
            shell.theme_epoch,
            self.client.dark(),
            shell
                .run
                .current()
                .filter(|run| run.is_active())
                .map_or(0, |run| run.elapsed_at(now).as_secs()),
        ));
        self.last_resync = resync;
        self.last_editor_revision = editor_revision;
        Ok(())
    }

    pub(super) fn flush(&mut self, state: &SharedState) -> io::Result<()> {
        self.last_sent = None;
        self.last_key = None;
        self.present(state)
    }

    pub(super) fn close(&mut self, keep: bool) -> io::Result<()> {
        if self.opened {
            self.client.close(SURFACE, keep)?;
            self.opened = false;
        }
        Ok(())
    }

    pub(super) fn dump(&self) -> Vec<String> {
        serde_json::to_string_pretty(&[&self.sent.main, &self.sent.dock, &self.sent.layer])
            .expect("native node JSON")
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

impl Drop for TernSurface {
    fn drop(&mut self) {
        let _ = self.close(false);
    }
}

fn find_node<'a>(nodes: &'a [Node], id: &str) -> Option<&'a Node> {
    nodes.iter().find_map(|node| {
        if node.id == id {
            Some(node)
        } else {
            find_node(node.c.as_deref().unwrap_or_default(), id)
        }
    })
}

fn apply_disclosure(nodes: &mut [Node], collapsed: &HashMap<String, bool>) {
    for node in nodes {
        if let Some(value) = collapsed.get(&node.id) {
            node.p = Some(node.p.take().unwrap_or_default().set("collapsed", value));
        }
        if let Some(children) = &mut node.c {
            apply_disclosure(children, collapsed);
        }
    }
}

fn project(
    shell: &ShellState,
    reduce_motion: bool,
    images: &super::tern_images::NativeImages,
    main_changed: bool,
    editor_revision: u64,
) -> Projection {
    let mut out = Projection::default();
    if main_changed && !shell.startup_pending {
        if shell.startup_card_started_at.is_some() {
            out.main.push(welcome(shell));
        }
        for (index, block) in shell.transcript.iter().enumerate() {
            // Commit IDs survive prepends, deletion, resume and streaming.
            let identity = shell.transcript_commit_ids[index];
            if let Some(node) = block_node(identity, block, shell, images) {
                out.main.push(node);
            }
        }
    }
    let chrome = super::shell_chrome::shell_chrome(shell, shell.size.0, Instant::now());
    for (id, rows) in [
        ("extension.header", &chrome.header),
        ("extension.above", &chrome.extension_above),
        ("pending", &chrome.pending),
        ("subagents", &chrome.subagents),
    ] {
        if !rows.is_empty() {
            out.dock.push(ansi_rows(id, rows));
        }
    }
    if let Some(working) = working_row(shell, reduce_motion) {
        out.dock.push(working);
    }
    out.dock.push(composer(shell));
    for (id, rows) in [
        ("extension.below", &chrome.extension_below),
        ("error", &chrome.error),
    ] {
        if !rows.is_empty() {
            out.dock.push(ansi_rows(id, rows));
        }
    }
    out.panel_receipt = super::renderer_geometry::PanelRenderReceipt::capture(shell, &chrome.panel);
    if let Some(picker) = super::tern_picker::node(shell) {
        out.focus = Some(picker.id.clone());
        out.layer.push(picker);
    } else if let Some(overlay) = report(shell) {
        out.layer.push(overlay);
    } else if !chrome.panel.is_empty()
        || shell.overlay.is_some()
        || shell.tool_input_prompt.is_some()
    {
        // Internally styled documents and approval labels remain native ANSI
        // content, not a second ANSI TUI or a pane-wide migration Rows node.
        // Approvals retain their bounded full-label consent receipt, accepted
        // only after Tern acknowledges this exact frame.
        let mut rows =
            super::viewport::overlay_lines(shell, shell.size.0, usize::from(shell.size.1));
        rows.extend(chrome.panel);
        if shell.tool_input_prompt.is_some() {
            rows.extend(chrome.composer);
        }
        out.layer.push(Node::with_children(
            format!("modal.{}", shell.panel_epoch),
            Kind::Overlay,
            Props::new()
                .role("octet.modal")
                .set("modal", true)
                .set("size", "lg")
                .set("anchor", "center"),
            vec![ansi_rows("modal.body", &rows)],
        ));
    } else {
        out.focus = Some("composer.editor".into());
        if let Some(completion) =
            super::tern_completion::Completion::capture_revision(shell, editor_revision)
        {
            out.layer.push(completion.node(shell));
        }
    }
    out
}

fn ansi_rows(id: &str, lines: &[String]) -> Node {
    Node::new(id, Kind::Ansi, Props::new().set("text", lines.join("\n")))
}

fn welcome(shell: &ShellState) -> Node {
    let logo = if shell.theme.unicode() {
        " ██ ████\n████████"
    } else {
        " ## ####\n########"
    };
    Node::with_children(
        "welcome",
        Kind::Row,
        Props::new()
            .role("octet.welcome")
            .set("gap", "lg")
            .set("align", "center")
            .set("wrap", true),
        vec![
            Node::new(
                "welcome.byte",
                Kind::Text,
                Props::new()
                    .text("spans", vec![Span::styled(logo, "accent")])
                    .set("wrap", "none")
                    .set("aria", "octet byte mark: 01101111"),
            ),
            Node::with_children(
                "welcome.copy",
                Kind::Col,
                Props::new().set("gap", "sm"),
                vec![
                    Node::new(
                        "welcome.name",
                        Kind::Text,
                        Props::new().text(
                            "spans",
                            vec![
                                Span::styled("octet", "strong"),
                                Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), "dim"),
                            ],
                        ),
                    ),
                    Node::new(
                        "welcome.model",
                        Kind::Text,
                        Props::new().text(
                            "spans",
                            vec![Span::styled(
                                sanitize_for_terminal(&shell.model_display),
                                "accent",
                            )],
                        ),
                    ),
                    Node::new(
                        "welcome.hint",
                        Kind::Text,
                        Props::new().text(
                            "spans",
                            vec![Span::styled(
                                "/ commands · ! shell · ctrl+o details",
                                "muted",
                            )],
                        ),
                    ),
                ],
            ),
        ],
    )
}

fn id(identity: u64, suffix: &str) -> String {
    format!("t{identity}.{suffix}")
}

fn block_node(
    identity: u64,
    block: &TranscriptBlock,
    shell: &ShellState,
    images: &super::tern_images::NativeImages,
) -> Option<Node> {
    match block {
        TranscriptBlock::User {
            text,
            model_lab,
            prompt_color,
            ..
        } => {
            // TSP v1 has no per-card RGB style. Use its native ANSI component
            // for the scoped, historical wash rather than recolouring every
            // prior prompt with the currently selected model's global palette.
            let rich = shell.theme.rich_renderer();
            let lines = super::render_user_prompt(
                text,
                model_lab,
                prompt_color.as_deref(),
                &rich,
                &shell.theme,
                shell.size.0.saturating_sub(6).max(1),
            );
            let mut cushioned = vec![" ".to_owned()];
            cushioned.extend(lines.into_iter().map(|line| format!(" {line} ")));
            cushioned.push(" ".to_owned());
            let body = cushioned
                .into_iter()
                .map(|line| {
                    if shell.theme.prompt_wash() {
                        shell
                            .theme
                            .prompt_provenance_card(prompt_color.as_deref(), &line)
                    } else {
                        line
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            Some(Node::with_children(
                id(identity, "user"),
                Kind::Section,
                Props::new().role("octet.user"),
                vec![Node::new(
                    id(identity, "user.body"),
                    Kind::Ansi,
                    Props::new()
                        .set("text", body)
                        .set("cols", shell.size.0.saturating_sub(6).max(1)),
                )],
            ))
        }
        TranscriptBlock::Assistant(block) => Some(Node::new(
            id(identity, "assistant"),
            Kind::Md,
            Props::new()
                .role("octet.assistant")
                .set("text", sanitize_for_terminal(&block.text))
                .set("stream", !block.finished),
        )),
        TranscriptBlock::Reasoning(block) => Some(Node::with_children(
            id(identity, "reasoning"),
            Kind::Section,
            Props::new()
                .role("octet.thinking")
                .text("head", vec![Span::styled("Thinking", "thinkingText")])
                .set(
                    "took",
                    block
                        .reasoning_elapsed
                        .map_or(0, |elapsed| elapsed.as_millis() as u64),
                )
                .set("collapsible", true)
                .set("collapsed", !shell.verbose_tools),
            vec![Node::new(
                id(identity, "reasoning.md"),
                Kind::Md,
                Props::new()
                    .set("text", sanitize_for_terminal(&block.text))
                    .set("stream", !block.finished),
            )],
        )),
        TranscriptBlock::Tool(panel) => {
            Some(tool_node(identity, panel, shell.verbose_tools, images))
        }
        TranscriptBlock::Shell(output) => Some(shell_node(identity, output)),
        TranscriptBlock::Notice(text) => Some(text_node(identity, text, "muted")),
        TranscriptBlock::NoticeStatus { text, tone, .. } => Some(text_node(
            identity,
            text,
            match tone {
                NoticeTone::Success | NoticeTone::ToolSuccess => "success",
                NoticeTone::Error | NoticeTone::ToolError => "error",
                NoticeTone::ToolActive => "accent",
            },
        )),
        TranscriptBlock::Outcome(outcome) => Some(octet_tern::scene::turn_usage_spans(
            id(identity, "outcome"),
            outcome_parts(outcome),
        )),
        TranscriptBlock::Compaction(compaction) => Some(Node::with_children(
            id(identity, "compaction"),
            Kind::Section,
            Props::new().role("octet.compaction").text(
                "head",
                Text::Plain(sanitize_for_terminal(&compaction.label)),
            ),
            vec![Node::new(
                id(identity, "compaction.md"),
                Kind::Md,
                Props::new().set("text", sanitize_for_terminal(&compaction.summary)),
            )],
        )),
        TranscriptBlock::Subagents(subagents) => {
            Some(text_node(identity, &subagents.label(), "muted"))
        }
        TranscriptBlock::UpdateAvailable(version) => Some(text_node(
            identity,
            &format!("octet {version} is available"),
            "muted",
        )),
    }
}

fn text_node(identity: u64, text: &str, token: &str) -> Node {
    Node::new(
        id(identity, "text"),
        Kind::Text,
        Props::new()
            .text(
                "spans",
                vec![Span::styled(sanitize_for_terminal(text), token)],
            )
            .set("measure", "prose"),
    )
}

fn working_row(shell: &ShellState, reduce_motion: bool) -> Option<Node> {
    let run = shell.run.current().filter(|run| run.is_active())?;
    let label = match run.phase() {
        crate::presentation::RunPhase::Preparing { summary } => summary.as_str(),
        crate::presentation::RunPhase::AwaitingProvider { .. } => "Waiting for provider",
        crate::presentation::RunPhase::ProviderLifecycle { .. } => "Preparing model",
        crate::presentation::RunPhase::Thinking => "Thinking",
        crate::presentation::RunPhase::StreamingResponse => "Responding",
        crate::presentation::RunPhase::PreparingToolCall => "Preparing tool",
        crate::presentation::RunPhase::RunningTool { summary } => summary.as_str(),
        crate::presentation::RunPhase::AwaitingApproval { prompt } => prompt.as_str(),
        crate::presentation::RunPhase::Finished(_) => return None,
    };
    let age = run.elapsed_at(Instant::now()).as_millis() as u64;
    let mut node = octet_tern::scene::working_row("work", &sanitize_for_terminal(label), age, None);
    node.p = Some(node.p.unwrap_or_default().role("octet.activity"));
    if reduce_motion || !shell.theme.capabilities().animation {
        node.c.as_mut().expect("working row")[0] = Node::new(
            "work.spin",
            Kind::Text,
            Props::new().text("spans", vec![Span::styled("·", "accent")]),
        );
        node.c.as_mut().expect("working row")[1] = Node::new(
            "work.label",
            Kind::Text,
            Props::new().text(
                "spans",
                vec![Span::styled(sanitize_for_terminal(label), "muted")],
            ),
        );
    }
    Some(node)
}

fn composer(shell: &ShellState) -> Node {
    let focused = super::normal_editor_focused(shell) && !shell.startup_pending;
    let running = shell.run.is_active();
    // Flat semantic primitives: no omp.editor role, liquid glass, branded
    // gradient, rounded badges, inset highlight, or terminal-injected omp mark.
    let mut controls = vec![
        Node::new(
            "composer.model",
            Kind::Text,
            Props::new()
                .role("octet.composer.model")
                .text(
                    "spans",
                    vec![Span::styled(
                        sanitize_for_terminal(if shell.model_display.is_empty() {
                            &shell.model
                        } else {
                            &shell.model_display
                        }),
                        "accent mono strong",
                    )],
                )
                .set("wrap", "none")
                .set("truncate", "end")
                .set("actions", json!({"click":"model"}))
                .set("title", "Choose model"),
        ),
        Node::new(
            "composer.effort",
            Kind::Text,
            Props::new()
                .role("octet.composer.effort")
                .text(
                    "spans",
                    vec![Span::styled(
                        format!(
                            "effort {}",
                            if shell.reasoning.is_empty() {
                                "off"
                            } else {
                                &shell.reasoning
                            }
                        ),
                        "muted mono",
                    )],
                )
                .set("wrap", "none")
                .set("actions", json!({"click":"effort"}))
                .set("title", "Cycle reasoning effort"),
        ),
        Node::leaf("composer.gap", Kind::Row),
    ];
    controls[2].p = Some(Props::new().set("grow", 1));
    if let Some((used, total)) = shell.context_estimate.filter(|(_, total)| *total > 0) {
        controls.push(Node::new(
            "composer.context",
            Kind::Text,
            Props::new()
                .role("octet.composer.context")
                .text(
                    "spans",
                    vec![Span::styled(
                        format!(
                            "context {:.0}% / {}K",
                            used as f64 / total as f64 * 100.0,
                            total / 1000
                        ),
                        "dim mono",
                    )],
                )
                .set("wrap", "none")
                .set("title", format!("Context: {used} of {total} tokens")),
        ));
    }
    controls.push(Node::new(
        if running {
            "composer.stop"
        } else {
            "composer.send"
        },
        Kind::Text,
        Props::new()
            .role("octet.composer.action")
            .text(
                "spans",
                vec![Span::styled(
                    if running { "Stop" } else { "Send" },
                    if running {
                        "error strong mono"
                    } else {
                        "accent strong mono"
                    },
                )],
            )
            .set("wrap", "none")
            .set(
                "actions",
                json!({"click": if running { "stop" } else { "send" }}),
            ),
    ));
    Node::with_children(
        "composer",
        Kind::Col,
        Props::new().role("octet.composer").set("gap", "sm"),
        vec![
            Node::new("composer.rule", Kind::Rule, Props::new().tone(Tone::Accent)),
            Node::new(
                "composer.editor",
                Kind::Editor,
                Props::new()
                    .role("octet.composer.input")
                    .set("text", shell.editor.text())
                    .set(
                        "cursor",
                        shell.editor.text()[..shell.editor.cursor()]
                            .encode_utf16()
                            .count(),
                    )
                    .set("readonly", !focused)
                    .set("maxLines", 12)
                    .set("placeholder", "Ask octet anything")
                    .text(
                        "prompt",
                        vec![Span::styled(shell.theme.glyph("prompt"), "accent")],
                    ),
            ),
            Node::with_children(
                "composer.bar",
                Kind::Row,
                Props::new()
                    .role("octet.composer.bar")
                    .set("gap", "md")
                    .set("align", "center")
                    .set("wrap", true),
                controls,
            ),
        ],
    )
}

fn report(shell: &ShellState) -> Option<Node> {
    let (title, purpose, body) = match shell.overlay.as_ref()? {
        super::ShellOverlay::Text(text) => (
            "octet".to_owned(),
            String::new(),
            Node::new(
                "report.body",
                Kind::Text,
                Props::new()
                    .set("text", sanitize_for_terminal(text))
                    .set("wrap", "word"),
            ),
        ),
        super::ShellOverlay::Report(report) => {
            let body = match &report.body {
                super::ReportBody::Text { text, styled } => Node::new(
                    "report.body",
                    if *styled { Kind::Ansi } else { Kind::Text },
                    Props::new()
                        .set(
                            "text",
                            if *styled {
                                text.to_string()
                            } else {
                                sanitize_for_terminal(text)
                            },
                        )
                        .set("wrap", "word"),
                ),
                super::ReportBody::Markdown(document) => Node::new(
                    "report.body",
                    Kind::Text,
                    Props::new()
                        .set("text", sanitize_for_terminal(&document.plain_text()))
                        .set("wrap", "word"),
                ),
                super::ReportBody::Context(_) => ansi_rows(
                    "report.body",
                    &super::viewport::overlay_lines(shell, shell.size.0, usize::from(shell.size.1)),
                ),
            };
            (
                sanitize_for_terminal(&report.surface.title),
                report
                    .surface
                    .purpose
                    .as_deref()
                    .map(sanitize_for_terminal)
                    .unwrap_or_default(),
                body,
            )
        }
    };
    Some(Node::with_children(
        format!("report.{}", shell.panel_epoch),
        Kind::Overlay,
        Props::new()
            .role("octet.report")
            .set("modal", true)
            .set("size", "lg")
            .set("anchor", "center")
            .text("head", title),
        vec![
            Node::new(
                "report.purpose",
                Kind::Text,
                Props::new().text("spans", vec![Span::styled(purpose, "muted")]),
            ),
            body,
        ],
    ))
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
                return (sanitize_for_terminal(value), kind);
            }
        }
    }
    let trimmed = panel.args.trim();
    let trimmed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    (trimmed.chars().take(160).collect(), "text")
}

fn tool_node(
    index: u64,
    panel: &super::ToolPanel,
    verbose: bool,
    images: &super::tern_images::NativeImages,
) -> Node {
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
            meta.push(Text::Plain(sanitize_for_terminal(reason)));
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
        let shown = sanitize_for_terminal(&panel.output);
        body.push(Node::new(
            id(index, "out"),
            Kind::Text,
            Props::new()
                .text("spans", vec![Span::styled(shown, "toolOutput")])
                .set("wrap", "word"),
        ));
    }

    for (image_index, image) in panel.images.iter().enumerate() {
        let image_id = format!("t{index}.image{image_index}");
        body.push(images.node(&image_id).unwrap_or_else(|| {
            Node::new(
                image_id,
                Kind::Text,
                Props::new().text(
                    "spans",
                    vec![Span::styled(
                        image.fallback_text(!panel.image_rendering.enabled),
                        "muted",
                    )],
                ),
            )
        }));
    }
    let mut node = octet_tern::scene::tool_card(
        id(index, "tool"),
        &panel.name,
        &tool_title(&panel.name),
        &target,
        target_kind,
        status,
        meta,
        body,
    );
    node.p = Some(
        node.p
            .unwrap_or_default()
            .role("octet.tool")
            .set("collapsed", !verbose && panel.finished),
    );
    node
}

fn shell_node(index: u64, shell: &super::ShellOutput) -> Node {
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
            &sanitize_for_terminal(&shell.output),
        )]
    };
    octet_tern::scene::tool_card(
        id(index, "shell"),
        "bash",
        "Bash",
        &sanitize_for_terminal(&shell.command),
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

#[cfg(test)]
#[path = "tern_tests.rs"]
mod tests;
