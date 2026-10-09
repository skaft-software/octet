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
use super::{AssistantBlock, NoticeTone, ShellState, TranscriptBlock};
use crate::tui::theme::ModelLab;

pub(super) const SURFACE: &str = "octet.session";
const FRAME_INTERVAL: Duration = Duration::from_millis(32);
const HELLO_TIMEOUT: Duration = Duration::from_secs(2);

/// The resolved `--tern` / `OCTET_TERN` policy, published once at startup.
///
/// `enabled()` and `enabled_cached()` are reachable from the renderer thread
/// and from the shared input filter, neither of which carries the resolved
/// `Config`; one atomic written before the frontend starts keeps both on the
/// same decision instead of re-reading the environment per event.
static POLICY: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(AUTO);

const AUTO: u8 = 0;
const ON: u8 = 1;
const OFF: u8 = 2;

/// Publish the resolved configuration before the interactive frontend starts.
///
/// A later call overrides an earlier one, which is what the in-process reload
/// path needs; `Auto` restores terminal detection.
pub(crate) fn set_policy(mode: crate::config::TernMode) {
    let value = match mode {
        crate::config::TernMode::Auto => AUTO,
        crate::config::TernMode::On => ON,
        crate::config::TernMode::Off => OFF,
    };
    POLICY.store(value, std::sync::atomic::Ordering::Release);
}

fn policy() -> crate::config::TernMode {
    match POLICY.load(std::sync::atomic::Ordering::Acquire) {
        ON => crate::config::TernMode::On,
        OFF => crate::config::TernMode::Off,
        _ => crate::config::TernMode::Auto,
    }
}

/// The one decision, as a pure function of the policy and what the terminal
/// says. Tests exercise this directly so no test ever has to mutate the
/// process-wide policy while other renderer tests are negotiating.
fn decide(mode: crate::config::TernMode, term_program: Option<&str>, tests: bool) -> bool {
    if !mode.permitted() {
        return false;
    }
    if mode.forced() {
        return true;
    }
    // Unit tests drive the native surface explicitly; a developer running
    // `cargo test` inside a Tern pane must not flip every renderer test
    // onto the protocol path.
    !tests && term_program.is_some_and(|value| value.eq_ignore_ascii_case("tern"))
}

pub(crate) fn enabled() -> bool {
    decide(
        policy(),
        std::env::var("TERM_PROGRAM").ok().as_deref(),
        cfg!(test),
    )
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
    remote_header: Option<crate::extensions::remote_ui::Projection>,
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
    /// Last OS-focus return consumed from the native mailbox. A change forces
    /// a frame and re-asserts native keyboard focus even when Tern sent no
    /// TSP visibility event while another app was in front.
    last_focus_resync: u64,
    /// The pane was hidden and came back: re-assert native keyboard focus even
    /// if the focused control did not change.
    force_focus: bool,
    credit_blocked: Option<Instant>,
    images: super::tern_images::NativeImages,
    brand: super::tern_welcome::Brand,
    receipts:
        std::collections::VecDeque<(u64, Option<super::renderer_geometry::PanelRenderReceipt>)>,
}

impl TernSurface {
    pub(super) fn start() -> io::Result<Self> {
        Ok(Self::with_client(TernClient::connect_shared_input(
            "octet",
            Some(env!("CARGO_PKG_VERSION")),
            &["edit"],
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
            last_focus_resync: 0,
            force_focus: false,
            credit_blocked: None,
            receipts: Default::default(),
            images: Default::default(),
            brand: Default::default(),
        }
    }

    /// Track pane visibility. Hiding suspends presentation only; becoming
    /// visible again forces a frame and re-asserts native keyboard focus,
    /// because the terminal may have moved it while another tab was in front.
    fn set_visible(&mut self, visible: bool) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        self.last_key = None;
        if visible {
            self.force_focus = true;
            self.credit_blocked = None;
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
                        Kind::Code,
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
                        Kind::Card,
                        Kind::Icon,
                        Kind::Kbd,
                        Kind::Effort,
                        Kind::Meter,
                        Kind::Kv,
                        Kind::Badge,
                        Kind::Status,
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
                // A suspended pane may reject background frames; that is not a
                // protocol failure, and returning to the tab must not find the
                // renderer already fallen back to ANSI.
                if self.visible {
                    return Err(io::Error::other(format!("Tern surface: {msg}")));
                }
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
                self.brand = Default::default();
                self.last_key = None;
                self.theme_key = None;
                self.client.reset_surface(SURFACE);
            }
            Incoming::Event(Event::Visible { sf, visible })
                if sf.as_deref().is_none_or(|id| id == SURFACE) =>
            {
                self.set_visible(*visible);
            }
            Incoming::Event(Event::Theme { .. } | Event::Motion { .. }) => self.last_key = None,
            Incoming::Event(Event::Resize { sf, visible, .. })
                if sf.as_deref().is_none_or(|id| id == SURFACE) =>
            {
                if let Some(visible) = visible {
                    self.set_visible(*visible);
                }
                // Pane layout can replace Tern's input target without a Visible
                // transition or an OS FocusGained event. Reassert this surface's
                // current owner; an unchanged retained tree is not input recovery.
                self.force_focus = true;
                self.last_key = None;
            }
            Incoming::Event(Event::Toggle {
                sf, id, collapsed, ..
            }) if sf == SURFACE => {
                let node = find_node(&self.sent.main, id)
                    .or_else(|| find_node(&self.sent.dock, id))
                    .or_else(|| find_node(&self.sent.layer, id));
                if !node.is_some_and(|node| {
                    node.p
                        .as_ref()
                        .and_then(|props| props.as_map().get("collapsible"))
                        == Some(&json!(true))
                }) {
                    return Ok(());
                }
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
            if let Incoming::Event(Event::Toggle {
                sf,
                id: node_id,
                collapsed,
                ..
            }) = &message
            {
                if sf == SURFACE && self.collapsed.get(node_id) == Some(collapsed) {
                    if let Some(identity) = node_id
                        .strip_prefix('t')
                        .and_then(|id| id.split('.').next())
                        .and_then(|id| id.parse::<u64>().ok())
                    {
                        let mut shell = state.borrow();
                        if let Some(index) = shell
                            .transcript_commit_ids
                            .iter()
                            .position(|id| *id == identity)
                        {
                            shell
                                .extension_transcript
                                .expanded
                                .insert(identity, !collapsed);
                            shell.touch_block(index);
                        }
                    }
                }
            }
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
        {
            let mut mailbox = state.native().lock().expect("native mailbox poisoned");
            mailbox.scroll_supported = self.ready && self.client.supports_feature("scroll");
            if !mailbox.scroll_supported {
                mailbox.scroll.clear();
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
        // Hidden panes may still have outstanding credit. Keep accepting
        // source and acknowledgements, but do not prepare or send presentation
        // until visibility returns and reasserts the native focus.
        if !self.visible {
            return Ok(());
        }
        if !self.client.has_credit(SURFACE) {
            // A hidden pane stops acknowledging frames while the terminal keeps
            // it suspended: that is not a failure, and returning to the tab must
            // not find the renderer already fallen back. Only a *visible* surface
            // that has stopped being acknowledged is a real timeout.
            let blocked = self.credit_blocked.get_or_insert_with(Instant::now);
            if self.visible && blocked.elapsed() >= HELLO_TIMEOUT {
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
            let focus_resync = state
                .native()
                .lock()
                .expect("native mailbox poisoned")
                .focus_resync;
            if self.last_key == Some(key)
                && resync == self.last_resync
                && focus_resync == self.last_focus_resync
                && state
                    .native()
                    .lock()
                    .expect("native mailbox poisoned")
                    .scroll
                    .is_empty()
                && !crate::output::has_tui_diagnostics()
            {
                return Ok(());
            }
            let editor_revision = shell.editor.revision();
            let editor_changed = self.last_editor_revision != editor_revision;
            let focus_changed = focus_resync != self.last_focus_resync;
            if focus_changed {
                // OS focus returned without a TSP `Visible` event (screen
                // recording, space switch): same recovery as becoming visible
                // again — force a frame, re-assert native keyboard focus, and
                // forgive credit starvation while the tab was in front of
                // another app.
                self.force_focus = true;
                self.credit_blocked = None;
            }
            if !editor_changed
                && !focus_changed
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
                remote_header: shell
                    .extension_ui
                    .remote
                    .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Header)
                    .map(|_| shell.extension_ui.remote.clone()),
            };
            (RenderModel::capture(&mut shell), main_key, editor_revision)
        };
        // Accepted source chunks, including streaming text, are materialized
        // outside the frontend lock. Shell metadata is not the streamed source.
        self.owner.accept(model);
        self.owner.state.native_keys = state
            .native()
            .lock()
            .expect("native mailbox poisoned")
            .bindings
            .clone();
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
            self.images.prepare(
                shell,
                self.client.supports(Kind::Image),
                shell.verbose_tools,
            )?;
            self.images.upload(&mut self.client)?;
            self.brand.prepare(shell, &mut self.client)?;
        }
        let mut next = project(
            shell,
            &self.client,
            &self.images,
            &self.brand,
            main_changed,
            editor_revision,
            &self.collapsed,
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
        let focus_resync_now = state
            .native()
            .lock()
            .expect("native mailbox poisoned")
            .focus_resync;
        if focus_resync_now != self.last_focus_resync {
            // Focus may arrive while materializing outside the frontend lock.
            // Never consume that newer counter with a draft refresh alone.
            self.force_focus = true;
            self.credit_blocked = None;
        }
        // A focus return also refreshes the composer draft: the terminal may
        // hold a stale revision after input was impossible, and stale drafts
        // reject gestures on length mismatch.
        if resync != self.last_resync || focus_resync_now != self.last_focus_resync {
            if let Some(editor_id) = super::tern_prompt::focus(shell) {
                if let Some(editor) = find_node(&next.layer, &editor_id) {
                    ops.push(Op::Set {
                        id: editor.id.clone(),
                        props: editor.p.clone().expect("editor props"),
                    });
                }
            }
            if let Some(editor) = find_node(&next.dock, "composer.editor") {
                ops.push(Op::Set {
                    id: editor.id.clone(),
                    props: editor.p.clone().expect("editor props"),
                });
            }
        }
        if !self.regions || self.force_focus || self.sent.focus != next.focus {
            ops.push(Op::Focus {
                id: next.focus.clone(),
            });
            self.force_focus = false;
        }
        let scroll = state
            .native()
            .lock()
            .expect("native mailbox poisoned")
            .scroll
            .iter()
            .copied()
            .collect::<Vec<_>>();
        ops.extend(scroll.iter().map(|by| Op::Scroll {
            id: REGION_MAIN.into(),
            by: *by,
        }));
        if !ops.is_empty() {
            let sequence = self.client.frame_ops_now(SURFACE, ops)?;
            state
                .native()
                .lock()
                .expect("native mailbox poisoned")
                .scroll
                .drain(..scroll.len());
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
        self.last_focus_resync = focus_resync_now;
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
            if node
                .p
                .as_ref()
                .and_then(|props| props.as_map().get("collapsible"))
                == Some(&json!(true))
            {
                node.p = Some(node.p.take().unwrap_or_default().set("collapsed", value));
            }
        }
        if let Some(children) = &mut node.c {
            apply_disclosure(children, collapsed);
        }
    }
}

fn project(
    shell: &ShellState,
    client: &TernClient,
    images: &super::tern_images::NativeImages,
    brand: &super::tern_welcome::Brand,
    main_changed: bool,
    editor_revision: u64,
    collapsed: &HashMap<String, bool>,
) -> Projection {
    let mut out = Projection::default();
    if main_changed && !shell.startup_pending {
        if shell
            .extension_ui
            .remote
            .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Header)
            .is_some()
        {
            out.main.push(ansi_rows(
                "remote.header",
                &super::remote_ui::render_welcome_card(
                    shell,
                    shell.size.0,
                    usize::from(shell.size.1),
                    Instant::now(),
                ),
            ));
        } else if shell.startup_card_started_at.is_some() {
            out.main.push(super::tern_welcome::node(shell, brand));
        }
        for (index, block) in shell.transcript.iter().enumerate() {
            // Commit IDs survive prepends, deletion, resume and streaming.
            let identity = shell.transcript_commit_ids[index];
            if shell.extension_markdown(index).is_none() {
                if let Some(rendered) =
                    shell.extension_rendered(index, shell.transcript_content_width(shell.size.0))
                {
                    if !rendered.lines.is_empty() {
                        let rows = ansi_rows(&id(identity, "extension"), &rendered.lines);
                        if let (TranscriptBlock::Tool(panel), Some(expanded)) =
                            (block, shell.extension_tool_disclosure(index))
                        {
                            // Keep Tern's disclosure control independent of the
                            // rows: collapsed Pi renderers still draw their own
                            // bounded summary instead of having it hidden by Tern.
                            let mut header = tool_node(
                                identity,
                                panel,
                                shell.verbose_tools,
                                images,
                                shell.workspace.as_deref(),
                                collapsed,
                            );
                            header.c = Some(Vec::new());
                            header.p = Some(
                                header
                                    .p
                                    .take()
                                    .unwrap_or_default()
                                    .set("collapsible", true)
                                    .set("collapsed", !expanded),
                            );
                            out.main.push(Node::with_children(
                                id(identity, "extension.group"),
                                Kind::Col,
                                Props::new(),
                                vec![header, rows],
                            ));
                        } else {
                            out.main.push(rows);
                        }
                    }
                    continue;
                }
            }
            if let Some(node) = block_node(
                identity,
                block,
                shell,
                images,
                collapsed,
                shell.extension_markdown(index),
            ) {
                out.main.push(node);
            }
        }
    }
    let chrome = super::shell_chrome::shell_chrome(shell, shell.size.0, Instant::now());
    for (id, rows) in [
        ("extension.header", &chrome.header),
        ("extension.above", &chrome.extension_above),
    ] {
        if !rows.is_empty() {
            out.dock.push(ansi_rows(id, rows));
        }
    }
    // Run activity stays above queued input, including while a tool owns the
    // turn. Neither adding a follow-up nor changing the editor owner settles it.
    if let Some(working) = working_row(shell, client.reduce_motion()) {
        out.dock.push(working);
    }
    if let Some(pending) = super::tern_pending::node(shell) {
        out.dock.push(pending);
    }
    let remote_editor = shell
        .extension_ui
        .remote
        .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Editor)
        .is_some()
        && shell.panel.is_none()
        && shell.tool_input_prompt.is_none();
    let remote_fullscreen = shell.extension_ui.remote_fullscreen_overlay
        && shell.panel.is_none()
        && shell.tool_input_prompt.is_none();
    if !remote_fullscreen {
        if remote_editor {
            out.dock.push(ansi_rows("remote.editor", &chrome.composer));
        } else {
            out.dock.push(composer(shell));
        }
    }
    for (id, rows) in [
        ("extension.below", &chrome.extension_below),
        ("error", &chrome.error),
    ] {
        if !rows.is_empty() {
            out.dock.push(ansi_rows(id, rows));
        }
    }
    // Override Tern's default reader-column cap with the full available width.
    // A relative bound follows pane resize without a character/zoom-dependent cap.
    for node in out.main.iter_mut().chain(&mut out.dock) {
        node.p = Some(
            node.p
                .take()
                .unwrap_or_default()
                .set("max", json!({"w":1.0})),
        );
    }
    // The neutral dock column is Tern's native shared-column layout hook.
    // Keep live chrome and the integrated composer aligned with the transcript.
    out.dock = vec![Node::with_children(
        "dock.content",
        Kind::Col,
        Props::new().set("gap", "sm"),
        std::mem::take(&mut out.dock),
    )];
    out.panel_receipt = super::renderer_geometry::PanelRenderReceipt::capture(shell, &chrome.panel);
    if let Some(prompt) = super::tern_prompt::node(shell) {
        out.focus = super::tern_prompt::focus(shell);
        out.layer.push(prompt);
    } else if let Some(picker) = super::tern_picker::node(shell) {
        out.focus = super::tern_picker::focus(shell);
        out.layer.push(picker);
    } else if let Some(overlay) = report(shell, client.supports(Kind::Table)) {
        out.layer.push(overlay);
    } else if !chrome.panel.is_empty() || shell.overlay.is_some() {
        // Internally styled documents and approval labels remain native ANSI
        // content, not a second ANSI TUI or a pane-wide migration Rows node.
        // Approvals retain their bounded full-label consent receipt, accepted
        // only after Tern acknowledges this exact frame.
        let mut rows =
            super::viewport::overlay_lines(shell, shell.size.0, usize::from(shell.size.1));
        rows.extend(chrome.panel);
        out.layer.push(Node::with_children(
            if shell.panel.is_some() {
                format!("modal.{}", shell.panel_epoch)
            } else {
                format!("report.{}", shell.overlay_epoch)
            },
            Kind::Overlay,
            Props::new()
                .role("octet.modal")
                .set("modal", true)
                .set("size", "lg")
                .set("anchor", "center"),
            vec![ansi_rows("modal.body", &rows)],
        ));
    } else {
        out.focus = editor_focused(shell).then(|| "composer.editor".into());
        if editor_focused(shell) && !shell.startup_pending {
            if let Some(completion) =
                super::tern_completion::Completion::capture_revision(shell, editor_revision)
            {
                out.layer.push(completion.node(shell));
            }
        }
    }
    out
}

fn ansi_rows(id: &str, lines: &[String]) -> Node {
    Node::new(id, Kind::Ansi, Props::new().set("text", lines.join("\n")))
}

fn id(identity: u64, suffix: &str) -> String {
    format!("t{identity}.{suffix}")
}

/// RAIL replies are unboxed reader prose. The retained Markdown identity is
/// independent of model changes, disclosure, cursor motion and streaming.
fn assistant_node(identity: u64, block: &AssistantBlock, text: &str) -> Node {
    Node::with_children(
        id(identity, "assistant"),
        Kind::Col,
        Props::new().role("omp.assistant"),
        vec![Node::new(
            id(identity, "assistant.md"),
            Kind::Md,
            Props::new()
                .set("text", tighten_markdown(text))
                .set("stream", !block.finished),
        )],
    )
}

/// Collapse excess vertical whitespace in native Markdown projections.
///
/// Tern typesets Markdown with its own paragraph rhythm, so stacked blank
/// lines from streamed joins render as large air gaps. Capping runs at a
/// single blank line and trimming edges keeps the content identical while
/// removing the doubled spacing. Code-fence contents are preserved verbatim:
/// only blank-line runs outside fences are collapsed.
fn tighten_markdown(text: &str) -> String {
    let sanitized = sanitize_for_terminal(text);
    let mut out = String::with_capacity(sanitized.len());
    let mut blanks: usize = 0;
    let mut fence: Option<(u8, usize)> = None;
    let mut indented_code = false;
    for line in sanitized.split('\n') {
        let trimmed = line.trim();
        let start = line.trim_start_matches(' ');
        let marker = start.as_bytes().first().copied();
        let run = marker.map_or(0, |marker| {
            start.bytes().take_while(|byte| *byte == marker).count()
        });
        let fence_line =
            line.len() - start.len() <= 3 && matches!(marker, Some(b'`' | b'~')) && run >= 3;
        if fence.is_some() || fence_line {
            if let Some((open, length)) = fence {
                if fence_line
                    && marker == Some(open)
                    && run >= length
                    && start[run..].trim().is_empty()
                {
                    fence = None;
                }
            } else {
                fence = marker.map(|marker| (marker, run));
            }
            blanks = 0;
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            continue;
        }
        if !trimmed.is_empty() {
            indented_code = line.starts_with("    ") || line.starts_with('\t');
        }
        if indented_code {
            blanks = 0;
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(line);
            continue;
        }
        if trimmed.is_empty() {
            blanks += 1;
            if blanks > 1 {
                continue;
            }
            out.push('\n');
            continue;
        }
        blanks = 0;
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
    }
    let out = out.trim_start_matches('\n');
    if fence.is_some() {
        out.to_owned()
    } else {
        out.trim_end_matches('\n').to_owned()
    }
}

fn block_node(
    identity: u64,
    block: &TranscriptBlock,
    shell: &ShellState,
    images: &super::tern_images::NativeImages,
    collapsed: &HashMap<String, bool>,
    transformed_markdown: Option<&str>,
) -> Option<Node> {
    match block {
        TranscriptBlock::User { text, .. } => {
            // Tone::User retains the model-adaptive prompt wash. Octet's own
            // role avoids omp.user's private right-aligned bubble layout: the
            // native card shares the transcript's left edge and available width.
            Some(Node::with_children(
                id(identity, "user"),
                Kind::Card,
                Props::new().role("octet.user").tone(Tone::User),
                vec![Node::new(
                    id(identity, "user.body"),
                    Kind::Md,
                    Props::new().set(
                        "text",
                        tighten_markdown(transformed_markdown.unwrap_or(text)),
                    ),
                )],
            ))
        }
        TranscriptBlock::Assistant(block) if block.text.is_empty() => None,
        TranscriptBlock::Assistant(block) => Some(Node::with_children(
            id(identity, "assistant.group"),
            Kind::Col,
            Props::new().role("omp.assistant"),
            vec![assistant_node(
                identity,
                block,
                transformed_markdown.unwrap_or(&block.text),
            )],
        )),
        TranscriptBlock::Reasoning(block) if block.text.trim().is_empty() => None,
        TranscriptBlock::Reasoning(block) => Some(Node::with_children(
            id(identity, "reasoning.group"),
            Kind::Col,
            Props::new().role("omp.assistant"),
            vec![Node::with_children(
                id(identity, "reasoning"),
                Kind::Section,
                Props::new()
                    .role("omp.thinking")
                    .text(
                        "head",
                        vec![Span::styled(
                            if let Some(label) = shell
                                .extension_ui
                                .hidden_thinking_label
                                .as_ref()
                                .filter(|_| !shell.verbose_tools)
                            {
                                label.clone()
                            } else if block.finished {
                                block.reasoning_elapsed.map_or_else(
                                    || "Thoughts".into(),
                                    |elapsed| {
                                        format!(
                                            "Thought for {:.1}s",
                                            elapsed.as_secs_f64().max(0.1)
                                        )
                                    },
                                )
                            } else {
                                "Thinking".into()
                            },
                            "muted",
                        )],
                    )
                    .set("collapsible", true)
                    .set(
                        "collapsed",
                        !shell.verbose_tools && !block.reasoning_expanded,
                    ),
                vec![Node::new(
                    id(identity, "reasoning.md"),
                    Kind::Md,
                    Props::new()
                        .set(
                            "text",
                            tighten_markdown(
                                &super::assistant_block::reasoning_markdown_projection(
                                    transformed_markdown.unwrap_or(&block.text),
                                ),
                            ),
                        )
                        .set("stream", !block.finished),
                )],
            )],
        )),
        TranscriptBlock::Tool(panel)
            if panel.grouped_child
                && !shell.verbose_tools
                && !panel.is_error
                && !matches!(panel.name.as_str(), "bash" | "exec") =>
        {
            None
        }
        TranscriptBlock::Tool(panel) => Some(tool_node(
            identity,
            panel,
            shell.verbose_tools,
            images,
            shell.workspace.as_deref(),
            collapsed,
        )),
        TranscriptBlock::Shell(output) => Some(shell_node(identity, output, shell.verbose_tools)),
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
        TranscriptBlock::Outcome(outcome) => Some(Node::with_children(
            id(identity, "outcome"),
            Kind::Row,
            Props::new()
                .role("omp.turn.usage")
                .set("gap", "xs")
                .set("align", "center"),
            vec![
                Node::new(
                    id(identity, "outcome.glyph"),
                    Kind::Text,
                    Props::new().text(
                        "spans",
                        vec![Span::styled(
                            shell.theme.glyph(match &outcome.outcome {
                                crate::presentation::RunOutcome::Completed { .. } => "success",
                                crate::presentation::RunOutcome::Failed { .. } => "error",
                                _ => "warning",
                            }),
                            "muted",
                        )],
                    ),
                ),
                Node::new(
                    id(identity, "outcome.text"),
                    Kind::Text,
                    Props::new().text("spans", outcome_parts(outcome)),
                ),
            ],
        )),
        TranscriptBlock::Compaction(compaction) => Some(Node::with_children(
            id(identity, "compaction"),
            Kind::Section,
            Props::new()
                .role("octet.compaction")
                .text(
                    "head",
                    Text::Plain(sanitize_for_terminal(&compaction.label)),
                )
                .set("collapsible", true)
                .set("collapsed", !compaction.expanded && !shell.verbose_tools),
            vec![Node::new(
                id(identity, "compaction.md"),
                Kind::Md,
                Props::new().set("text", tighten_markdown(&compaction.summary)),
            )],
        )),
        TranscriptBlock::Subagents(subagents) => Some(super::tern_agents::transcript(
            identity,
            subagents,
            shell.verbose_tools,
        )),
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
    if shell
        .extension_ui
        .working
        .as_ref()
        .is_some_and(|working| working.visible == Some(false))
    {
        return None;
    }
    let now = Instant::now();
    let activity = shell
        .active_reasoning
        .and_then(|index| match shell.transcript.get(index) {
            Some(TranscriptBlock::Reasoning(block)) if !block.finished => Some(block),
            _ => None,
        });
    let retry = activity.and_then(|block| block.retry_activity.as_ref());
    let label = if let Some(retry) = retry {
        retry.label_at(now)
    } else {
        match run.phase() {
            crate::presentation::RunPhase::Preparing { summary } => summary.clone(),
            crate::presentation::RunPhase::AwaitingProvider { .. } => "Working".into(),
            crate::presentation::RunPhase::ProviderLifecycle {
                provider,
                state,
                detail,
            } => {
                let mut label = format!(
                    "{} · {}",
                    crate::presentation::provider_status_name(provider),
                    state.as_str()
                );
                if let Some(detail) = detail {
                    label.push_str(&format!(" · {detail}"));
                }
                label
            }
            crate::presentation::RunPhase::Thinking => {
                if activity.is_some_and(|block| !block.text.trim().is_empty()) {
                    "Thinking"
                } else {
                    "Working"
                }
                .into()
            }
            crate::presentation::RunPhase::StreamingResponse => "Working".into(),
            crate::presentation::RunPhase::PreparingToolCall => "Preparing tool".into(),
            crate::presentation::RunPhase::RunningTool { .. } => "Working".into(),
            crate::presentation::RunPhase::AwaitingApproval { prompt } => prompt.clone(),
            crate::presentation::RunPhase::Finished(_) => return None,
        }
    };
    let label = shell
        .extension_ui
        .working
        .as_ref()
        .and_then(|working| working.message.as_deref())
        .unwrap_or(&label);
    let age = run.elapsed_at(now).as_millis() as u64;
    let mut node = octet_tern::scene::working_row("work", &sanitize_for_terminal(label), age, None);
    node.p = Some(node.p.unwrap_or_default().role("omp.working"));
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
    if retry.is_some() {
        node.c
            .as_mut()
            .expect("working row")
            .retain(|child| child.id != "work.elapsed");
    }
    if let Some(working) = &shell.extension_ui.working {
        if let Some(frames) = &working.frames {
            let interval = working.interval_ms.filter(|value| *value > 0).unwrap_or(80);
            let frame = if frames.is_empty() {
                ""
            } else {
                &frames[((age / interval) % frames.len() as u64) as usize]
            };
            node.c.as_mut().expect("working row")[0] =
                octet_tern::scene::ansi_block("work.spin", frame);
        }
    }
    Some(node)
}

/// Pointer focus may only reassert the input owner already selected by the
/// host. It cannot enter an underlying composer, secret field or consent body.
pub(super) fn focus_target(shell: &ShellState) -> Option<String> {
    if shell.tool_input_prompt.is_some() {
        return super::tern_prompt::focus(shell);
    }
    if let Some(panel) = &shell.panel {
        let owns_native_focus = match panel {
            super::Panel::SessionPicker { picker } => !picker.confirming_delete,
            super::Panel::ReadOnlyDocument { .. } => true,
            _ => super::tern_picker::interactive(panel),
        };
        return owns_native_focus
            .then(|| super::tern_picker::focus(shell))
            .flatten();
    }
    editor_focused(shell).then(|| "composer.editor".into())
}

pub(super) fn editor_focused(shell: &ShellState) -> bool {
    super::normal_editor_focused(shell)
        && shell
            .extension_ui
            .remote
            .mount(octet_agent::extension_remote_ui::ExtensionRemoteUiPlacement::Editor)
            .is_none()
        && !shell.extension_ui.remote_fullscreen_overlay
}

fn composer(shell: &ShellState) -> Node {
    let focused = editor_focused(shell);
    let running = shell.run.is_active();
    let level = if shell.reasoning.is_empty() {
        "off"
    } else {
        &shell.reasoning
    };
    let mut children = Vec::new();
    if !shell.startup_pending {
        children.push(Node::new("composer.rule", Kind::Rule, Props::new()));
    }
    if let Some((used, total)) = shell
        .context_estimate
        .filter(|(_, total)| !shell.startup_pending && *total > 0)
    {
        children.push(Node::new(
            "composer.context",
            Kind::Meter,
            Props::new()
                .role("octet.composer.context")
                .set("style", "bar")
                .set("value", (used as f64 / total as f64).min(1.0))
                .set(
                    "label",
                    format!("{:.0}%", used as f64 / total as f64 * 100.0),
                )
                .set("total", crate::presentation::compact_context_limit(total))
                .set("thresholds", json!({"warn":0.6,"bad":0.85}))
                .set("title", format!("Context: {used} of {total} tokens")),
        ));
    }
    children.push(Node::with_children(
        "composer.line",
        Kind::Row,
        Props::new()
            .role("omp.composer.line")
            .set("align", "start")
            .set("gap", "sm"),
        vec![Node::new(
            "composer.editor",
            Kind::Editor,
            Props::new()
                .set("text", shell.editor.text())
                .set(
                    "cursor",
                    shell.editor.text()[..shell.editor.cursor()]
                        .encode_utf16()
                        .count(),
                )
                .set("readonly", !focused)
                .set("maxLines", 12)
                .set(
                    "placeholder",
                    if shell.startup_pending {
                        ""
                    } else {
                        "Ask octet — / commands · @ files · ! shell"
                    },
                ),
        )],
    ));
    if shell.startup_pending {
        // Register and focus the genuine draft immediately. Only its resolved
        // chrome waits for the same ready frame as the welcome and transcript.
        return Node::with_children("composer", Kind::Col, Props::new(), children);
    }
    let mut controls = vec![
        Node::with_children(
            "composer.model",
            Kind::Row,
            Props::new()
                .role("omp.composer.model")
                .set("gap", "xs")
                .set("align", "center")
                .set("actions", json!({"click":"model"}))
                .set("title", "Choose model"),
            vec![
                Node::new(
                    "composer.model.icon",
                    Kind::Icon,
                    Props::new().set("name", "model"),
                ),
                Node::new(
                    "composer.model.name",
                    Kind::Text,
                    Props::new()
                        .set("wrap", "none")
                        .set("truncate", "end")
                        .text(
                            "spans",
                            vec![Span::styled(
                                sanitize_for_terminal(if shell.model_display.is_empty() {
                                    &shell.model
                                } else {
                                    crate::presentation::model::footer_model_name(
                                        &shell.model_display,
                                        &shell.model,
                                    )
                                }),
                                "accent",
                            )],
                        ),
                ),
                Node::new(
                    "composer.model.chev",
                    Kind::Icon,
                    Props::new().set("name", "chev"),
                ),
            ],
        ),
        Node::with_children(
            "composer.effort",
            Kind::Row,
            Props::new()
                .role("omp.composer.effort")
                .set("gap", "xs")
                .set("align", "center")
                .set("actions", json!({"click":"effort"}))
                .set("title", "Cycle reasoning effort"),
            vec![
                Node::new(
                    "composer.effort.glyph",
                    Kind::Effort,
                    Props::new().set("level", level),
                ),
                Node::new(
                    "composer.effort.label",
                    Kind::Text,
                    Props::new().set("text", level).set("wrap", "none"),
                ),
            ],
        ),
        Node::new(
            "composer.extras",
            Kind::Status,
            Props::new()
                .role("omp.composer.extras")
                .set("transparent", true)
                .set("grow", 1),
        ),
    ];
    if let Some(cost) = shell.displayed_session_cost_microdollars() {
        controls.push(Node::new(
            "composer.usage",
            Kind::Text,
            Props::new()
                .role("omp.composer.usage")
                .set("wrap", "none")
                .set(
                    "text",
                    format!("${}.{:06}", cost / 1_000_000, cost % 1_000_000),
                )
                .set("title", "Session cost"),
        ));
    }
    let submit_binding = if running {
        "app.interrupt"
    } else {
        "tui.input.submit"
    };
    let submit_key = super::tern_controls::key(shell, submit_binding);
    let mut submit = Props::new()
        .role(if running {
            "omp.composer.stop"
        } else {
            "omp.composer.send"
        })
        .set("title", if running { "Stop" } else { "Send" });
    if let Some(key) = submit_key {
        submit = submit.set(
            "actions",
            json!({"click":if running { "stop" } else { "send" }}),
        );
        if !running {
            submit = submit.set("keys", [key]);
        }
    }
    if running || submit_key.is_none() {
        submit = submit.set(
            "text",
            if submit_key.is_none() {
                "Unavailable"
            } else {
                "Stop"
            },
        );
    }
    controls.push(Node::new(
        if running {
            "composer.stop"
        } else {
            "composer.send"
        },
        if running || submit_key.is_none() {
            Kind::Text
        } else {
            Kind::Kbd
        },
        submit,
    ));
    for (control, binding) in controls
        .iter_mut()
        .take(2)
        .zip(["app.model.select", "app.thinking.cycle"])
    {
        if super::tern_controls::key(shell, binding).is_none() {
            let mut props = control.p.take().expect("control props").as_map().clone();
            props.remove("actions");
            props.insert("title".into(), json!("Unavailable: keybinding is unbound"));
            control.p = Some(Props::from_value(serde_json::Value::Object(props)));
        }
    }
    children.push(Node::with_children(
        "composer.bar",
        Kind::Row,
        Props::new()
            .role("omp.composer.bar")
            .set("gap", "sm")
            .set("align", "center")
            .set("wrap", true),
        controls,
    ));
    Node::with_children(
        "composer",
        Kind::Col,
        Props::new().set("gap", "md"),
        children,
    )
}

fn report(shell: &ShellState, tables: bool) -> Option<Node> {
    let (title, purpose, body) = match shell.overlay.as_ref()? {
        super::ShellOverlay::Text(text) => (
            "octet".to_owned(),
            String::new(),
            Node::new(
                "report.body",
                if shell.extension_ui.remote_fullscreen_overlay {
                    Kind::Ansi
                } else {
                    Kind::Text
                },
                Props::new()
                    .set(
                        "text",
                        if shell.extension_ui.remote_fullscreen_overlay {
                            text.to_string()
                        } else {
                            sanitize_for_terminal(text)
                        },
                    )
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
                // Tern typesets Markdown natively (headings, tables, code),
                // so it gets the source rather than the flattened text.
                super::ReportBody::Markdown(_, source) => Node::new(
                    "report.body",
                    Kind::Md,
                    Props::new().set("text", tighten_markdown(source)),
                ),
                super::ReportBody::Context(context) => context.native_node(tables),
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
        format!("report.{}", shell.overlay_epoch),
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
    // Unknown arguments are not a display contract and may contain credentials.
    // Known targets above remain exact; never dump a raw argument envelope.
    (String::new(), "text")
}

fn tool_node(
    index: u64,
    panel: &super::ToolPanel,
    verbose: bool,
    images: &super::tern_images::NativeImages,
    workspace: Option<&std::path::Path>,
    collapsed_overrides: &HashMap<String, bool>,
) -> Node {
    let (target, target_kind) = tool_target(panel);
    let href = (target_kind == "path")
        .then(|| {
            let path = std::path::Path::new(&target);
            let absolute = if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace?.join(path)
            };
            url::Url::from_file_path(absolute)
                .ok()
                .map(|url| url.to_string())
        })
        .flatten();
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
    if panel.finished
        && panel.is_error
        && (verbose || !matches!(panel.name.as_str(), "bash" | "exec"))
    {
        if let Some(reason) = &panel.failure_reason {
            meta.push(Text::Plain(sanitize_for_terminal(reason)));
        }
    }

    // Gate command output before constructing the native tree. Keep its card
    // expanded independently: Tern ellipsizes command targets on collapsed cards.
    // Ctrl+O owns output disclosure; captured output stays in the semantic model.
    let command_output = matches!(panel.name.as_str(), "bash" | "exec");
    let script_output = panel.name == "codemode";
    let collapsed = if command_output {
        !verbose
    } else {
        collapsed_overrides
            .get(&id(index, "tool"))
            .copied()
            .unwrap_or(!verbose && panel.finished)
    };
    let mut body = Vec::new();
    let compact_read = panel.name == "read"
        && panel.finished
        && !panel.is_error
        && !verbose
        && target_kind == "path";
    // Bash stays a single text leaf even when its output resembles a diff.
    // Changing kinds mid-stream would replace the displayed body.
    let diff = (!command_output && !script_output)
        .then(|| super::tool_render::tool_diff(panel))
        .flatten();
    if let Some(diff) = diff {
        // Diffs stay mounted while collapsed: Tern hides them, and the user
        // can disclose without waiting for a re-projected child.
        body.push(octet_tern::scene::diff_block(
            id(index, "diff"),
            &target,
            &diff,
        ));
    } else if !compact_read
        && !panel.output.trim().is_empty()
        && (script_output || !collapsed || (panel.is_error && !command_output))
    {
        // Collapsed successful tools project summary-only (header/target):
        // mounting the full output in the same frame that flips `collapsed`
        // paints one expanded frame before the terminal hides it. Bash failures
        // retain their reason in metadata; full output requires disclosure.
        if script_output {
            body.push(Node::new(
                id(index, "out"),
                Kind::Code,
                Props::new()
                    .set("text", super::codemode_render::output_text(panel, verbose))
                    .set("lang", "text")
                    .set("wrap", true)
                    .set("numbers", false),
            ));
        } else {
            let shown = sanitize_for_terminal(&panel.output);
            body.push(Node::new(
                id(index, "out"),
                Kind::Text,
                Props::new()
                    .text("spans", vec![Span::styled(shown, "toolOutput")])
                    .set("wrap", "word"),
            ));
        }
    }

    for (image_index, image) in panel
        .images
        .iter()
        .enumerate()
        .filter(|_| !command_output || !collapsed)
    {
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
    if let Some(decoration) = panel
        .progress_decoration
        .as_ref()
        .filter(|_| !panel.finished && (!command_output || verbose))
    {
        body.push(Node::new(
            id(index, "progress"),
            Kind::Text,
            Props::new()
                .text(
                    "spans",
                    vec![Span::styled(
                        sanitize_for_terminal(&match decoration.detail() {
                            Some(detail) => format!("{} · {detail}", decoration.label()),
                            None => decoration.label().to_owned(),
                        }),
                        "muted",
                    )],
                )
                .set("wrap", "word"),
        ));
    }
    if script_output {
        let source = super::codemode_render::source(panel).unwrap_or_default();
        return command_node(
            index,
            "tool",
            ("codemode", "Codemode"),
            (&source, "javascript"),
            status,
            meta,
            body,
        );
    }
    if command_output {
        return command_node(
            index,
            "tool",
            (&panel.name, &tool_title(&panel.display.label)),
            (&target, "bash"),
            status,
            meta,
            body,
        );
    }
    let mut node = octet_tern::scene::tool_card(
        id(index, "tool"),
        &panel.name,
        &tool_title(&panel.display.label),
        &target,
        target_kind,
        status,
        meta,
        body,
    );
    node.p = Some(
        node.p
            .unwrap_or_default()
            .role(format!("omp.tool.{}", panel.name))
            .set("href", &href)
            .set("frame", "inline")
            .set("collapsible", !compact_read && !command_output)
            .set("collapsed", !compact_read && !command_output && collapsed),
    );
    node
}

fn shell_node(index: u64, shell: &super::ShellOutput, verbose: bool) -> Node {
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
    // Local `!` commands also keep their full input visible, while output is
    // absent from the native tree until Ctrl+O requests it.
    let collapsed = !verbose;
    let body = if shell.output.trim().is_empty() || collapsed {
        Vec::new()
    } else {
        vec![octet_tern::scene::ansi_block(
            id(index, "out"),
            &sanitize_for_terminal(&shell.output),
        )]
    };
    command_node(
        index,
        "shell",
        ("bash", "Bash"),
        (&sanitize_for_terminal(&shell.command), "bash"),
        status,
        meta,
        body,
    )
}

/// A source rail cannot collapse or ellipsize its input. The caller owns
/// output admission: shell disclosure or a bounded Codemode output preview.
fn command_node(
    index: u64,
    suffix: &str,
    tool: (&str, &str),
    source: (&str, &str),
    status: &str,
    meta: Vec<Text>,
    mut body: Vec<Node>,
) -> Node {
    body.insert(
        0,
        Node::new(
            id(index, "command"),
            Kind::Code,
            Props::new()
                .set("text", source.0)
                .set("lang", source.1)
                .set("wrap", true)
                .set("numbers", false),
        ),
    );
    let mut node = octet_tern::scene::tool_card(
        id(index, suffix),
        tool.0,
        tool.1,
        "",
        "text",
        status,
        meta,
        body,
    );
    node.p = Some(
        node.p
            .unwrap_or_default()
            .role(format!("omp.tool.{}", tool.0))
            .set("frame", "inline")
            .set("collapsible", false)
            .set("collapsed", false),
    );
    node
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
            format!("completed · {} tools", summary.tool_calls),
            "muted",
        ),
        RunOutcome::Failed { elapsed, .. } => (Some(*elapsed), "failed".to_owned(), "error"),
        RunOutcome::Interrupted { elapsed } => {
            (Some(*elapsed), "interrupted".to_owned(), "warning")
        }
        RunOutcome::NeedsInput { .. } => (None, "needs input".to_owned(), "warning"),
    };
    let mut spans = Vec::with_capacity(7);
    if let Some(duration) = duration {
        spans.push(Span::styled(format_duration(duration), "dim"));
        spans.push(Span::new(" · "));
    }
    spans.push(Span::styled(verdict, token));
    if let Some(metrics) = outcome.inference.as_deref() {
        let rate = metrics
            .server
            .as_ref()
            .and_then(|server| server.tokens_per_second())
            .or_else(|| {
                metrics
                    .decode_estimate
                    .as_ref()
                    .map(|estimate| estimate.tokens_per_second)
            });
        if let Some(rate) = rate {
            spans.push(Span::styled(format!(" · {rate:.1} tok/s"), "dim"));
        }
    }
    match &outcome.outcome {
        RunOutcome::CompletedWithWarnings { warnings, .. } => {
            spans.push(Span::styled(format!(" · {warnings} warnings"), "warning"))
        }
        RunOutcome::Failed { reason, .. } => spans.push(Span::styled(
            format!(" · {}", sanitize_for_terminal(reason)),
            "error",
        )),
        RunOutcome::NeedsInput { prompt } => spans.push(Span::styled(
            format!(" · {}", sanitize_for_terminal(prompt)),
            "warning",
        )),
        _ => {}
    }
    spans
}

#[cfg(test)]
#[path = "tern_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tern_inference_tests.rs"]
mod inference_tests;

#[cfg(all(test, unix))]
#[path = "pi_renderer_test_support.rs"]
pub(crate) mod pi_renderer_test_support;
