//! Fixtures shared by more than one view test area: the emulated terminal, the vt100 grid
//! assertions, the shell and panel builders, and the theme and status-loop fixtures. One
//! module rather than one copy per area so the shapes they hand out cannot drift apart.

use super::*;

pub(super) struct EmulatedTerminal {
    pub(super) size: Arc<Mutex<(u16, u16)>>,
    pub(super) bytes: Arc<Mutex<Vec<u8>>>,
    pub(super) synchronized_output: bool,
    pub(super) status_frames: Option<StatusFrameObserver>,
}

pub(super) struct StatusFrameObserver {
    pub(super) state: SharedState,
    // Completion time, selected shimmer phase, and the complete frame's byte end.
    pub(super) frames: mpsc::Sender<(Instant, usize, usize)>,
    pub(super) delay: Arc<std::sync::atomic::AtomicU64>,
}

impl EmulatedTerminal {
    pub(super) fn push(&self, bytes: &[u8]) {
        self.bytes
            .lock()
            .expect("emulated terminal output mutex poisoned")
            .extend_from_slice(bytes);
    }
}

impl sexy_tui_rs::Terminal for EmulatedTerminal {
    fn start_events(
        &mut self,
        _on_input: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>,
        _on_resize: Box<dyn FnMut()>,
    ) {
    }

    fn stop(&mut self) {}

    fn write(&mut self, data: &str) {
        // The production primary-screen terminal uses the normal output
        // post-processing convention where LF returns to column zero.
        // vt100 deliberately models raw bytes, so make that convention
        // explicit in the test backend.
        let mut previous = None;
        for byte in data.bytes() {
            if byte == b'\n' && previous != Some(b'\r') {
                self.push(b"\r");
            }
            self.push(&[byte]);
            previous = Some(byte);
        }
        if data.contains("\x1b[?2026l") {
            if let Some(observer) = &self.status_frames {
                let phase = observer.state.borrow().status_shimmer_frame;
                let end = self.bytes.lock().unwrap().len();
                let _ = observer.frames.send((Instant::now(), phase, end));
                thread::sleep(Duration::from_millis(
                    observer.delay.load(std::sync::atomic::Ordering::Relaxed),
                ));
            }
        }
    }

    fn columns(&self) -> u16 {
        self.size.lock().expect("terminal size mutex poisoned").0
    }

    fn rows(&self) -> u16 {
        self.size.lock().expect("terminal size mutex poisoned").1
    }

    fn move_by(&mut self, lines: i16) {
        match lines.cmp(&0) {
            std::cmp::Ordering::Less => {
                self.push(format!("\x1b[{}A", lines.unsigned_abs()).as_bytes());
            }
            std::cmp::Ordering::Greater => {
                self.push(format!("\x1b[{}B", lines.unsigned_abs()).as_bytes());
            }
            std::cmp::Ordering::Equal => {}
        }
    }

    fn hide_cursor(&mut self) {
        self.push(b"\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.push(b"\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.push(b"\x1b[0m\x1b[2K");
    }

    fn clear_from_cursor(&mut self) {
        self.push(b"\x1b[0m\x1b[0J");
    }

    fn clear_screen(&mut self) {
        self.push(b"\x1b[0m\x1b[2J");
    }

    fn capabilities(&self) -> sexy_tui_rs::TerminalCapabilities {
        let mut capabilities = sexy_tui_rs::TerminalCapabilities::interactive(
            sexy_tui_rs::ColorDepth::TrueColor,
            true,
        );
        capabilities.synchronized_output = self.synchronized_output;
        capabilities.sync_output = self.synchronized_output;
        capabilities
    }
}

pub(super) fn test_effective_tool_policy() -> octet_agent::EffectiveToolPolicy {
    octet_agent::SandboxConfig::new(".")
        .effective_tool_policy(octet_agent::EffectPolicy::Controlled)
}

pub(super) fn inherited_delegation_provenance() -> octet_agent::DelegationOrchestrationProvenance {
    octet_agent::DelegationOrchestrationProvenance::all(
        octet_agent::DelegationPolicySource::ParentInherited,
    )
}

pub(super) fn emulated_shell(
    theme: OctetTheme,
    width: u16,
    height: u16,
) -> (InteractiveShell, Arc<Mutex<Vec<u8>>>) {
    emulated_shell_with_sync(theme, width, height, false)
}

pub(super) fn emulated_shell_with_sync(
    theme: OctetTheme,
    width: u16,
    height: u16,
    synchronized_output: bool,
) -> (InteractiveShell, Arc<Mutex<Vec<u8>>>) {
    emulated_shell_with_mode(theme, width, height, synchronized_output, false)
}

pub(super) fn emulated_shell_with_mode(
    theme: OctetTheme,
    width: u16,
    height: u16,
    synchronized_output: bool,
    application_viewport: bool,
) -> (InteractiveShell, Arc<Mutex<Vec<u8>>>) {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let size = Arc::new(Mutex::new((width, height)));
    let state = SharedState::new(ShellState {
        theme,
        size: (width, height),
        follow_tail: true,
        application_viewport_requested: application_viewport,
        ..ShellState::default()
    });
    let mut tui = TUI::new(Box::new(EmulatedTerminal {
        size: size.clone(),
        bytes: bytes.clone(),
        synchronized_output,
        status_frames: None,
    }));
    tui.add_child(Box::new(ShellComponent::new(
        state.clone(),
        application_viewport,
    )));
    tui.start();
    (
        InteractiveShell {
            input_dispatch: input_dispatch::InputDispatch::new(
                crate::tui::keymap::keybindings::KeybindingsManager::current_platform(),
            ),
            tui: Some(tui),
            state,
            size,
            render_tx: Arc::new(Mutex::new(None)),
            render_thread: None,
            capture_mouse: application_viewport,
            terminal_ceded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            herdr: crate::herdr::PaneReporter::disabled(),
        },
        bytes,
    )
}

pub(super) fn lazy_history_test_shell() -> InteractiveShell {
    let mut shell = InteractiveShell::test_shell();
    shell.capture_mouse = true;
    shell.state.borrow_mut().application_viewport_requested = true;
    shell
}

pub(super) fn session_with_user_prompts(
    path: &std::path::Path,
    prefix: &str,
    count: usize,
) -> Session {
    use octet_ai::{Message, UserMessage, UserPart};

    let mut session = Session::create(path).unwrap();
    for index in 0..count {
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("{prefix} {index}"))],
            })))
            .unwrap();
    }
    session
}

pub(super) fn emulate_rows(lines: &[String], width: u16) -> vt100::Parser {
    let rows = u16::try_from(lines.len()).unwrap_or(u16::MAX).max(1);
    let mut terminal = vt100::Parser::new(rows, width, 0);
    for (index, line) in lines.iter().enumerate() {
        terminal.process(line.as_bytes());
        if index + 1 < lines.len() {
            terminal.process(b"\r\n");
        }
    }
    terminal
}

pub(super) fn assert_input_suggestions_replace_status_footer(
    shell: &mut InteractiveShell,
    expected_hint: &str,
) {
    const WIDTH: u16 = 120;
    shell.set_size(WIDTH, 24);
    shell.set_context_estimate(80, 272_000);

    let state = shell.state.borrow();
    let now = Instant::now();
    let standalone_composer =
        crate::tui::composer_surface::render_composer_surface(&state, WIDTH, now);
    assert!(strip_terminal_sequences(
        standalone_composer
            .last()
            .expect("standalone composer should include its status footer")
    )
    .contains("0%/272K"));

    let chrome = shell_chrome(&state, WIDTH, now);
    assert!(!chrome.suggestions.is_empty());
    assert!(chrome
        .suggestions
        .iter()
        .any(|line| { strip_terminal_sequences(line).contains(expected_hint) }));
    assert!(
        chrome
            .composer
            .iter()
            .all(|line| !strip_terminal_sequences(line).contains("0%")),
        "autocomplete must replace the model and token status row"
    );

    let expected_tail = chrome
        .composer
        .iter()
        .chain(&chrome.suggestions)
        .cloned()
        .collect::<Vec<_>>();
    for (mode, rendered) in [
        ("terminal-owned", render_shell_at(&state, WIDTH, now)),
        (
            "application-owned",
            render_shell_viewport_at(&state, WIDTH, now),
        ),
    ] {
        assert!(
            rendered.ends_with(&expected_tail),
            "{mode} suggestions must replace the status row below the composer"
        );
    }
}

/// `vt100` 0.15 ignores ED 3. Recreate its grid at the final saved-line
/// clear so protocol tests can model the destructive reset before replay.
/// This deliberately does not pretend to model modern terminal reflow.
pub(super) fn process_vt100_with_saved_line_clear(
    terminal: &mut vt100::Parser,
    output: &[u8],
    rows: u16,
    columns: u16,
    scrollback_len: usize,
) {
    const CLEAR_SAVED_LINES: &[u8] = b"\x1b[3J";
    if let Some(clear_at) = output
        .windows(CLEAR_SAVED_LINES.len())
        .rposition(|window| window == CLEAR_SAVED_LINES)
    {
        *terminal = vt100::Parser::new(rows, columns, scrollback_len);
        terminal.process(&output[clear_at + CLEAR_SAVED_LINES.len()..]);
    } else {
        terminal.process(output);
    }
}

pub(super) fn find_ascii_cell(screen: &vt100::Screen, needle: &str) -> Option<(u16, u16)> {
    screen
        .rows(0, screen.size().1)
        .enumerate()
        .find_map(|(row, contents)| {
            contents.find(needle).map(|byte| {
                (
                    row as u16,
                    u16::try_from(visible_width(&contents[..byte])).unwrap_or(u16::MAX),
                )
            })
        })
}

pub(super) fn assert_ascii_foreground(
    terminal: &vt100::Parser,
    needle: &str,
    expected: vt100::Color,
) {
    let (row, column) = find_ascii_cell(terminal.screen(), needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in {:?}", terminal.screen().contents()));
    for offset in 0..needle.len() as u16 {
        let cell = terminal
            .screen()
            .cell(row, column + offset)
            .expect("text cell inside terminal bounds");
        assert_eq!(
            cell.fgcolor(),
            expected,
            "foreground mismatch for {needle:?} at ({row}, {})",
            column + offset
        );
    }
}

pub(super) fn assert_ascii_bold(terminal: &vt100::Parser, needle: &str) {
    let (row, column) = find_ascii_cell(terminal.screen(), needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in {:?}", terminal.screen().contents()));
    for offset in 0..needle.len() as u16 {
        assert!(
            terminal
                .screen()
                .cell(row, column + offset)
                .expect("text cell inside terminal bounds")
                .bold(),
            "{needle:?} was not bold at offset {offset}"
        );
    }
}

pub(super) fn assert_ascii_default_rendition(terminal: &vt100::Parser, needle: &str) {
    let (row, column) = find_ascii_cell(terminal.screen(), needle)
        .unwrap_or_else(|| panic!("{needle:?} not found in {:?}", terminal.screen().contents()));
    for offset in 0..needle.len() as u16 {
        let cell = terminal
            .screen()
            .cell(row, column + offset)
            .expect("text cell inside terminal bounds");
        assert_eq!(cell.fgcolor(), vt100::Color::Default);
        assert_eq!(cell.bgcolor(), vt100::Color::Default);
        assert!(!cell.bold(), "{needle:?} retained bold at offset {offset}");
        assert!(
            !cell.italic(),
            "{needle:?} retained italic at offset {offset}"
        );
        assert!(
            !cell.underline(),
            "{needle:?} retained underline at offset {offset}"
        );
        assert!(
            !cell.inverse(),
            "{needle:?} retained inverse at offset {offset}"
        );
    }
}

pub(super) fn role_rgb_color(theme: &OctetTheme, role: &str) -> vt100::Color {
    let (red, green, blue) = theme
        .role_rgb(role)
        .unwrap_or_else(|| panic!("test theme role {role:?} did not resolve to RGB"));
    vt100::Color::Rgb(red, green, blue)
}

/// Build a key-press event for panel input tests.
pub(super) fn panel_key(code: crossterm::event::KeyCode) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        code,
        crossterm::event::KeyModifiers::NONE,
    ))
}

pub(super) fn panel_key_with_modifiers(
    code: crossterm::event::KeyCode,
    modifiers: crossterm::event::KeyModifiers,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new(code, modifiers))
}

pub(super) fn picker_session(
    id: &str,
    title: &str,
    message_count: usize,
    modified_seconds: u64,
) -> SessionMeta {
    SessionMeta {
        id: id.to_owned(),
        path: PathBuf::from(format!("/tmp/{id}.jsonl")),
        title: title.to_owned(),
        name: None,
        tags: Vec::new(),
        pinned: false,
        archived: false,
        trashed_at_ms: None,
        purge_after_ms: None,
        forked_from_session_id: None,
        forked_from_entry_id: None,
        message_count,
        modified: std::time::UNIX_EPOCH + std::time::Duration::from_secs(modified_seconds),
        workspace: Some(PathBuf::from("/work")),
    }
}

pub(super) fn panel_key_kind(
    code: crossterm::event::KeyCode,
    kind: crossterm::event::KeyEventKind,
) -> crossterm::event::Event {
    crossterm::event::Event::Key(crossterm::event::KeyEvent::new_with_kind(
        code,
        crossterm::event::KeyModifiers::NONE,
        kind,
    ))
}

/// Open a select-list panel with no descriptions.
pub(super) fn open_select_panel(shell: &mut InteractiveShell, items: &[&str]) {
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Select model"),
        items: items.iter().map(|item| item.to_string()).collect(),
        descriptions: vec![None; items.len()],
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectModel(vec![]),
    });
}

pub(super) fn panel_state(shell: &InteractiveShell) -> (Vec<String>, usize, String) {
    let state = shell.state.borrow();
    let Some(Panel::SelectList {
        items,
        selected,
        filter,
        ..
    }) = state.panel.as_ref()
    else {
        panic!("panel should be open");
    };
    (items.clone(), *selected, filter.clone())
}

pub(super) fn plain_composer_surface(
    shell: &InteractiveShell,
    width: u16,
    now: Instant,
) -> Vec<String> {
    crate::tui::composer_surface::render_composer_surface(&shell.state.borrow(), width, now)
        .into_iter()
        .map(|line| strip_terminal_sequences(&line))
        .collect()
}

pub(super) fn plain_footer(shell: &InteractiveShell, width: u16, now: Instant) -> String {
    plain_composer_surface(shell, width, now)
        .pop()
        .expect("composer always has a status row at useful widths")
}

pub(super) fn default_composer_rule(width: u16) -> String {
    let theme = crate::tui::theme::test_theme();
    theme.glyph("horizontal").repeat(usize::from(width))
}

/// Open a grouped `/subagents` panel: `live` running workers followed by one
/// worker in each terminal group.
pub(super) fn open_grouped_subagent_panel(
    shell: &mut InteractiveShell,
    live: usize,
    finished: usize,
) {
    let mut items: Vec<String> = Vec::new();
    let mut groups: Vec<super::SubagentGroup> = Vec::new();
    let mut running = Vec::new();
    for index in 0..live {
        running.push(index);
        items.push(format!("live-{index}"));
    }
    groups.push(super::SubagentGroup {
        label: "Running".into(),
        indices: running,
        collapsible: false,
    });
    for label in ["Done", "Failed", "Stopped"] {
        let start = items.len();
        for index in 0..finished {
            items.push(format!("{}-{index}", label.to_lowercase()));
        }
        let indices: Vec<usize> = (start..items.len()).collect();
        groups.push(super::SubagentGroup {
            label: label.into(),
            indices,
            collapsible: true,
        });
    }
    let node_ids = items.iter().map(|item| format!("node-{item}")).collect();
    let descriptions = items
        .iter()
        .map(|item| Some(format!("{item} · 42s · explore/inherited · 4 calls")))
        .collect();
    shell.open_panel(Panel::SelectList {
        surface: OrdinarySurfaceMetadata::new("Subagents · Enter views transcript"),
        items,
        descriptions,
        selected: 0,
        filter: String::new(),
        action: PanelAction::SelectSubagent(super::SubagentPanel {
            node_ids,
            groups,
            collapsed: true,
            revealed_node: None,
            state_filter: None,
        }),
    });
}

// These probes deliberately use the real ShellComponent and Pi renderer, not
// render_shell_update alone: a correct final frame can hide repeated ED 3
// history resets during generation. Keep geometry and chrome fixed throughout.
pub(super) struct MarkdownStreamReplay {
    pub(super) shell: InteractiveShell,
    pub(super) bytes: Arc<Mutex<Vec<u8>>>,
    pub(super) terminal: vt100::Parser,
    pub(super) run_id: RunId,
}

impl MarkdownStreamReplay {
    pub(super) const WIDTH: u16 = 80;
    pub(super) const HEIGHT: u16 = 24;
    pub(super) const SCROLLBACK: usize = 2048;

    pub(super) fn new() -> Self {
        let (mut shell, bytes) = emulated_shell_with_mode(
            crate::tui::theme::test_theme(),
            Self::WIDTH,
            Self::HEIGHT,
            true,
            false,
        );
        shell.tui.as_mut().unwrap().set_clear_on_shrink(false);
        for index in 0..30 {
            shell.notice(format!("MARKDOWN-HISTORY-{index:02}"));
        }
        let run_id = shell.begin_run("openai");
        let mut replay = Self {
            shell,
            bytes,
            terminal: vt100::Parser::new(Self::HEIGHT, Self::WIDTH, Self::SCROLLBACK),
            run_id,
        };
        replay.render();
        replay
    }

    pub(super) fn render(&mut self) -> String {
        self.shell.render();
        let output = std::mem::take(&mut *self.bytes.lock().expect("emulated terminal bytes"));
        process_vt100_with_saved_line_clear(
            &mut self.terminal,
            &output,
            Self::HEIGHT,
            Self::WIDTH,
            Self::SCROLLBACK,
        );
        String::from_utf8(output).unwrap()
    }

    pub(super) fn delta(&mut self, text: &str) -> String {
        self.shell.on_run_event(
            self.run_id,
            &AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text: text.into(),
            },
        );
        self.render()
    }

    pub(super) fn full_redraws(&self) -> usize {
        self.shell.tui.as_ref().unwrap().full_redraws()
    }

    pub(super) fn assert_append_frame(&self, output: &str, baseline: usize, context: &str) {
        assert!(
            !output.contains("\x1b[3J") && !output.contains("MARKDOWN-HISTORY-"),
            "{context}: streaming rewrote native history"
        );
        assert_eq!(self.full_redraws(), baseline, "{context}: full replay");
    }

    // Only call after the last frame: vt100 does not model terminal reflow.
    pub(super) fn history(&mut self) -> String {
        self.terminal.set_size(Self::SCROLLBACK as u16, Self::WIDTH);
        self.terminal.set_scrollback(usize::MAX);
        let physical = self.terminal.screen().contents();
        for index in 0..30 {
            let sentinel = format!("MARKDOWN-HISTORY-{index:02}");
            assert_eq!(physical.matches(&sentinel).count(), 1, "{sentinel}");
        }
        physical
    }
}

pub(super) fn rendered_phase(phase: RunPhase) -> String {
    let mut shell = InteractiveShell::test_shell();
    shell.set_identity("relay", "gpt-5.6", "high");
    let now = Instant::now();
    {
        let mut state = shell.state.borrow_mut();
        let id = state.run.begin_at("relay", now).unwrap();
        state.run.set_phase_at(id, phase, now);
    }
    let rendered =
        render_shell_at(&shell.state.borrow(), 80, now + Duration::from_millis(600)).join("\n");
    rendered
}

pub(super) fn subagent_transcript_test_view(native: bool) -> SubagentActivityView {
    let children = [
        ("Read changelog", "running", 0, Some(165_000)),
        ("Audit docs", "completed", 1, Some(0)),
        ("Inspect tests", "failed", 16, None),
    ]
    .into_iter()
    .enumerate()
    .map(
        |(index, (task, state, calls, cost))| octet_agent::DelegationTelemetryChild {
            child_id: format!("agent-{index}"),
            task_name: task.into(),
            profile: Some("explore".into()),
            model: "test-model".into(),
            state: state.into(),
            phase: state.into(),
            current_tool: (state == "running").then(|| "read".into()),
            tool_use_count: calls,
            input_tokens: 5_500_000,
            cache_read_tokens: 80_000,
            cache_write_tokens: 20_000,
            estimated_output_tokens: None,
            output_tokens: 3_900,
            reasoning_tokens: 1_000,
            total_tokens: 5_603_900,
            cost: None,
            cost_microdollars: cost,
            elapsed_ms: 500,
            failure_class: (state == "failed").then(|| "provider_failure".into()),
            failure_reason: (state == "failed")
                .then(|| "provider request failed: \x1b[31mupstream unavailable\x1b[0m".into()),
            effective_tool_policy: test_effective_tool_policy(),
            orchestration_provenance: inherited_delegation_provenance(),
            session: Some("agent-session:opaque".into()),
        },
    )
    .collect::<Vec<_>>();
    if native {
        SubagentActivityView {
            telemetry: children,
            ..SubagentActivityView::default()
        }
    } else {
        SubagentActivityView {
            activities: children
                .into_iter()
                .map(|child| octet_agent::ExtensionPresentationActivity {
                    id: child.child_id,
                    kind: "subagent".into(),
                    state: match child.state.as_str() {
                        "running" => octet_agent::ExtensionPresentationState::Running,
                        "completed" => octet_agent::ExtensionPresentationState::Succeeded,
                        _ => octet_agent::ExtensionPresentationState::Failed,
                    },
                    summary: child.task_name,
                    provenance: None,
                    started_at_ms: None,
                    completed_at_ms: None,
                    metrics: Some(octet_agent::ExtensionPresentationMetrics {
                        tool_calls: child.tool_use_count,
                        input_tokens: child.input_tokens,
                        cache_read_tokens: child.cache_read_tokens,
                        cache_write_tokens: child.cache_write_tokens,
                        output_tokens: child.output_tokens,
                        reasoning_tokens: child.reasoning_tokens,
                        cost_microdollars: child.cost_microdollars,
                    }),
                    references: Vec::new(),
                })
                .collect(),
            ..SubagentActivityView::default()
        }
    }
}

// Renderer/control fixtures represent workers seen in this turn, not a stale
// session roster. Observe at most eight live workers at once before publishing
// their mixed or settled states.
pub(super) fn publish_current_turn_roster(
    shell: &mut InteractiveShell,
    snapshot: octet_agent::DelegationTelemetrySnapshot,
) {
    let mut observed = Vec::new();
    for wave in snapshot.children.chunks(8) {
        let mut live = snapshot.clone();
        live.children = observed.clone();
        live.children.extend(wave.iter().cloned().map(|mut worker| {
            worker.state = "running".into();
            worker.failure_reason = None;
            worker
        }));
        live.failure_reason = None;
        shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated { snapshot: live });
        observed.extend(wave.iter().cloned().map(|mut worker| {
            worker.state = "completed".into();
            worker
        }));
    }
    shell.on_agent_event(&octet_agent::AgentEvent::DelegationUpdated { snapshot });
}

pub(super) fn theme_with_layout(layout: &str) -> OctetTheme {
    crate::tui::theme::test_theme_from_source(&format!("[layout]\n{layout}"))
}

pub(super) const SURFACE_TEST_THEME: &str = r##"
        [metadata]
        name = "Surface fixture"
        adaptive = false

        [roles."surface.user"]
        foreground = "default"
        background = "#112233"
        [roles."surface.user.border"]
        foreground = "#6688aa"
        [roles."surface.user.label"]
        foreground = "#99ccff"
        bold = true

        [roles."surface.assistant"]
        foreground = "default"
        background = "#221133"
        [roles."surface.assistant.border"]
        foreground = "#9966bb"
        [roles."surface.assistant.label"]
        foreground = "#ddbbff"
        bold = true

        [surfaces.user]
        chrome = "card"
        heading = "tab"
        label = "INPUT"
        padding = 1
        width = "full"
        narrow_chrome = "rail"
        narrow_heading = "none"
        narrow_padding = 0

        [surfaces.assistant]
        chrome = "card"
        heading = "overline"
        label = "RESPONSE"
        padding = 1
        width = "full"
        narrow_chrome = "plain"
        narrow_heading = "none"
        narrow_padding = 0

        [glyphs]
        top_left = "╭"
        top_right = "╮"
        bottom_left = "╰"
        bottom_right = "╯"
        horizontal = "─"
        vertical = "│"
        rail = "┃"
        prompt = "›"

        [glyphs_ascii]
        top_left = "+"
        top_right = "+"
        bottom_left = "+"
        bottom_right = "+"
        horizontal = "-"
        vertical = "|"
        rail = "|"
        prompt = ">"

        [layout]
        density = "compact"
        transcript_inset = 1
        narrow_breakpoint = 60
    "##;

pub(super) fn populate_theme_fixture(shell: &mut InteractiveShell) {
    shell.set_identity("local", "qwen3.6-27b", "high");
    let mut state = shell.state.borrow_mut();
    state.push_block(TranscriptBlock::User {
        text: "Review `src/lib.rs` and keep the public API stable.".into(),
        model_lab: Some(ModelLab::Alibaba),
        prompt_color: Some("#ff7018".into()),
        persisted: true,
    });
    state.push_block(TranscriptBlock::Assistant(Box::new(
            AssistantBlock::finalized(
                "# Patch plan\n\nKeep the change **small** and verify it.\n\n```rust\nfn answer() -> u8 { 42 }\n```"
                    .into(),
            ),
        )));
    state.push_block(TranscriptBlock::Reasoning(Box::new(
        AssistantBlock::finalized_reasoning(
            "Checking ownership, invariants, and the narrow fallback.".into(),
        ),
    )));
    let args = serde_json::json!({"path": "src/lib.rs"});
    state.push_block(TranscriptBlock::Tool(Box::new(ToolPanel::new(
        ToolCallId("fixture-edit".into()),
        "edit".into(),
        args.to_string(),
        summarize_tool("edit", &args),
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new".into(),
        true,
        false,
        None,
        Some(ModelLab::Alibaba),
    ))));
    state.push_block(TranscriptBlock::Shell(Box::new(ShellOutput {
        id: "fixture-shell".into(),
        command: "cargo test -p octet-coding-agent".into(),
        output: "test result: ok. 386 passed".into(),
        exit_code: 0,
        running: false,
    })));
    state.push_block(TranscriptBlock::Notice(
        "Extension reloaded with one status contribution.".into(),
    ));
    state.push_block(TranscriptBlock::Outcome(OutcomeBlock::new(
        RunOutcome::Completed {
            elapsed: Duration::from_millis(13700),
            summary: crate::presentation::RunSummary {
                files_changed: 1,
                tool_calls: 2,
                warnings: 0,
            },
        },
        None,
    )));
    state.editor.set_text("draft a local patch");
}

pub(super) fn ansi_background_is_open_at_end(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut background_open = false;
    while index + 2 < bytes.len() {
        if bytes[index] != 0x1b || bytes[index + 1] != b'[' {
            index += 1;
            continue;
        }
        let Some(relative_end) = bytes[index + 2..].iter().position(|byte| *byte == b'm') else {
            break;
        };
        let end = index + 2 + relative_end;
        let parameters = std::str::from_utf8(&bytes[index + 2..end]).unwrap_or("");
        if parameters.is_empty() {
            background_open = false;
        } else {
            let parameters = parameters
                .split(';')
                .filter_map(|value| value.parse::<u16>().ok())
                .collect::<Vec<_>>();
            let mut parameter = 0;
            while parameter < parameters.len() {
                match parameters[parameter] {
                    0 | 49 => background_open = false,
                    40..=47 | 100..=107 => background_open = true,
                    38 | 48 if parameters.get(parameter + 1) == Some(&2) => {
                        if parameters[parameter] == 48 {
                            background_open = true;
                        }
                        parameter = parameter.saturating_add(4);
                    }
                    38 | 48 if parameters.get(parameter + 1) == Some(&5) => {
                        if parameters[parameter] == 48 {
                            background_open = true;
                        }
                        parameter = parameter.saturating_add(2);
                    }
                    _ => {}
                }
                parameter = parameter.saturating_add(1);
            }
        }
        index = end + 1;
    }
    background_open
}

pub(super) fn retry_event(attempt: usize) -> AgentEvent {
    AgentEvent::ProviderRetry {
        attempt,
        max_attempts: 3,
        delay: Duration::from_secs(5),
        error: "diagnostic-only cause".into(),
    }
}

// This fixture runs the production scheduler and Shell -> Pi -> ANSI renderer
// on its owning thread; it never enters raw mode or opens a provider/session.
pub(super) struct StatusRenderLoop {
    pub(super) shell: InteractiveShell,
    pub(super) frames: mpsc::Receiver<(Instant, usize, usize)>,
    pub(super) delay: Arc<std::sync::atomic::AtomicU64>,
    pub(super) bytes: Arc<Mutex<Vec<u8>>>,
}

pub(super) fn status_render_loop_shell(theme: OctetTheme) -> StatusRenderLoop {
    let mut shell = InteractiveShell::test_shell_with_theme(theme);
    shell.set_size(80, 24);
    shell
        .state
        .borrow_mut()
        .open_activity_status(Some("Working"), false);
    shell.tui.take().unwrap().stop();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let delay = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let (frames, frame_rx) = mpsc::channel();
    let terminal = EmulatedTerminal {
        size: shell.size.clone(),
        bytes: bytes.clone(),
        synchronized_output: true,
        status_frames: Some(StatusFrameObserver {
            state: shell.state.clone(),
            frames,
            delay: delay.clone(),
        }),
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let state = shell.state.clone();
    let size = shell.size.clone();
    shell.render_thread = Some(thread::spawn(move || {
        renderer_runtime::render_loop_with_terminal(
            terminal,
            state,
            size,
            rx,
            renderer_runtime::RenderLoopOptions::default(),
            |_, _| false,
        );
    }));
    *shell.render_tx.lock().unwrap() = Some(tx);
    StatusRenderLoop {
        shell,
        frames: frame_rx,
        delay,
        bytes,
    }
}
