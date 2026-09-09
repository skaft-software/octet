//! Retained renderer scheduling, shared state, and frame bookkeeping.

use std::cell::RefCell;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sexy_tui_rs::{CommitCursor, Component, FrameUpdate, TUI};

use super::native_scrollback::{
    render_shell, render_shell_update_with_cursor, render_shell_update_without_cursor,
    synchronize_shell_frame,
};
use super::shell_chrome::render_startup_surface;
use super::viewport::{render_shell_viewport_at, render_shell_viewport_update};
use super::welcome_card::welcome_animating;
use super::ShellState;
use crate::tui::terminal::{OctetTerminal, TerminalSize};

/// Welcome-card motion is short-lived and limited to roughly 60 fps.
const RENDER_INTERVAL: Duration = Duration::from_millis(16);
/// Transcript activity shares one restrained one-second cycle: streaming
/// response and active tool or shell dots breathe between foreground and muted
/// tones without changing size.
const EVENT_DOT_TOGGLE_INTERVAL: Duration = Duration::from_millis(500);
/// The optional braille spinner and model-adaptive status shimmer share one
/// bounded renderer-thread cadence.
const STATUS_ANIMATION_INTERVAL: Duration = Duration::from_millis(80);
/// Root-run elapsed status changes only at whole-second boundaries.
const STATUS_TIMER_INTERVAL: Duration = Duration::from_secs(1);
/// Resize events are normally delivered by crossterm, but polling while idle
/// also catches terminal-manager resizes that do not emit an event.
const RESIZE_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Thread-safe handle to the mutable shell model. The TUI renderer owns a
/// clone of this handle and performs all expensive layout work away from the
/// async agent/input loop.
#[derive(Clone)]
pub(super) struct SharedState(Arc<Mutex<ShellState>>);

impl SharedState {
    pub(super) fn new(state: ShellState) -> Self {
        Self(Arc::new(Mutex::new(state)))
    }

    pub(super) fn borrow(&self) -> MutexGuard<'_, ShellState> {
        self.0.lock().expect("shell state mutex poisoned")
    }

    pub(super) fn borrow_mut(&self) -> MutexGuard<'_, ShellState> {
        self.0.lock().expect("shell state mutex poisoned")
    }
}

pub(super) enum RenderCommand {
    Render,
    Stop,
}

pub(super) fn event_dot_animating(state: &ShellState) -> bool {
    let capabilities = state.theme.capabilities();
    capabilities.animation && capabilities.interactive && state.has_active_event_dot()
}

pub(super) fn thinking_spinner_animating(state: &ShellState) -> bool {
    let capabilities = state.theme.capabilities();
    capabilities.animation && capabilities.interactive && state.has_active_thinking_spinner()
}

pub(super) fn status_shimmer_animating(state: &ShellState) -> bool {
    let capabilities = state.theme.capabilities();
    capabilities.animation && capabilities.interactive && state.has_active_status_shimmer()
}

pub(super) fn status_timer_active(state: &ShellState) -> bool {
    state.theme.capabilities().interactive && state.has_active_status_timer()
}

fn render_wake_requires_frame(
    semantic_command: bool,
    resized: bool,
    welcome: bool,
    animation_due: bool,
) -> bool {
    semantic_command || resized || welcome || animation_due
}

fn frame_coalesce_delay(last_render: Option<Instant>, now: Instant) -> Duration {
    last_render
        .map(|last| (last + RENDER_INTERVAL).saturating_duration_since(now))
        .unwrap_or_default()
}

fn render_poll_interval(welcome: bool, status_animation: bool) -> Duration {
    if welcome {
        RENDER_INTERVAL
    } else if status_animation {
        STATUS_ANIMATION_INTERVAL
    } else {
        RESIZE_POLL_INTERVAL
    }
}

/// Reconcile the renderer's shared dimensions with the terminal itself. This
/// is a fallback for environments where the resize signal is delayed or
/// swallowed; the normal input path still updates the same cells immediately.
fn synchronize_terminal_size(state: &SharedState, size: &TerminalSize) -> bool {
    let Ok(dimensions) = crossterm::terminal::size() else {
        return false;
    };
    reconcile_terminal_size(state, size, dimensions)
}

pub(super) fn reconcile_terminal_size(
    state: &SharedState,
    size: &TerminalSize,
    dimensions: (u16, u16),
) -> bool {
    let changed = {
        let mut current = size.lock().expect("terminal size mutex poisoned");
        if *current == dimensions {
            false
        } else {
            *current = dimensions;
            true
        }
    };
    if !changed {
        return false;
    }

    let mut shell = state.borrow_mut();
    shell.size = dimensions;
    // Deferred history remains lazy; only the materialized tail participates
    // in this resize reflow. Semantic navigation can hydrate older blocks
    // later without delaying the resize or replaying them through the PTY.
    // Do not ask for transcript geometry here: that would synchronously reflow
    // the complete history and then invalidate it, paying the resize cost
    // twice. Viewport readers clamp the retained scroll offset after the render
    // thread performs the single required layout pass.
    shell.invalidate_transcript_layout();
    true
}

pub(super) fn render_loop(
    terminal: OctetTerminal,
    state: SharedState,
    size: TerminalSize,
    rx: Receiver<RenderCommand>,
    application_viewport: bool,
    clear_on_start: bool,
) {
    let mut tui = TUI::new(Box::new(terminal));
    // Removing bounded live activity must not clear saved lines merely because
    // the frame contracted. Offscreen semantic mutations still use Pi's replay.
    tui.set_clear_on_shrink(false);
    // octet's composer uses the terminal cursor itself; unlike Pi's editor, it
    // does not paint a separate inverted cursor cell around CURSOR_MARKER.
    // Restore visibility after panels, resize replays, and renderer resumes.
    tui.set_show_hardware_cursor(true);
    tui.add_child(Box::new(ShellComponent::new(
        state.clone(),
        application_viewport,
    )));
    if clear_on_start {
        // A resumed renderer has no copy of Pi's physical cursor/viewport
        // state. Force one authoritative clear-and-replay instead of treating
        // the retained transcript as a fresh append and duplicating it.
        tui.request_render_force(true);
    }
    tui.start();

    let mut last_render: Option<Instant> = None;
    let mut last_event_dot_toggle = Instant::now();
    let mut last_status_animation = Instant::now();
    let mut last_status_timer_update = Instant::now();
    loop {
        // The welcome card uses the terminal-frame wake; active status text
        // sleeps until its own 80 ms frame. Model, tool, and input events still
        // preempt either timer immediately.
        let (welcome, event_dot, thinking_spinner, status_shimmer, status_timer) = {
            let shell = state.borrow();
            let welcome = welcome_animating(&shell, Instant::now());
            let event_dot = event_dot_animating(&shell);
            let thinking_spinner = thinking_spinner_animating(&shell);
            let status_shimmer = status_shimmer_animating(&shell);
            let status_timer = status_timer_active(&shell);
            (
                welcome,
                event_dot,
                thinking_spinner,
                status_shimmer,
                status_timer,
            )
        };
        if !event_dot {
            last_event_dot_toggle = Instant::now();
        }
        let status_animation = thinking_spinner || status_shimmer;
        if !status_animation {
            last_status_animation = Instant::now();
        }
        if !status_timer {
            last_status_timer_update = Instant::now();
        }
        let poll = render_poll_interval(welcome, status_animation);
        let command = match rx.recv_timeout(poll) {
            Ok(command) => Some(command),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if matches!(command, Some(RenderCommand::Stop)) {
            break;
        }

        let resized = if command.is_none() {
            synchronize_terminal_size(&state, &size)
        } else {
            false
        };
        let advance_event_dot =
            event_dot && last_event_dot_toggle.elapsed() >= EVENT_DOT_TOGGLE_INTERVAL;
        let advance_status_animation =
            status_animation && last_status_animation.elapsed() >= STATUS_ANIMATION_INTERVAL;
        let advance_status_timer =
            status_timer && last_status_timer_update.elapsed() >= STATUS_TIMER_INTERVAL;
        let semantic_command = matches!(command, Some(RenderCommand::Render));
        if !render_wake_requires_frame(
            semantic_command,
            resized,
            welcome,
            advance_event_dot || advance_status_animation || advance_status_timer,
        ) {
            continue;
        }

        // Coalesce everything already queued into the latest semantic state.
        let mut stop = false;
        while let Ok(next) = rx.try_recv() {
            if matches!(next, RenderCommand::Stop) {
                stop = true;
                break;
            }
        }
        if stop {
            break;
        }

        // Bound high-throughput model streams to one frame per terminal refresh
        // without bringing back the old uninterruptible animation sleep. The
        // receiver remains live during the short deadline: more semantic work
        // is folded into the pending frame, and Stop takes effect immediately.
        if let Some(last) = last_render {
            let delay = frame_coalesce_delay(Some(last), Instant::now());
            let deadline = Instant::now() + delay;
            while !delay.is_zero() && Instant::now() < deadline {
                match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(RenderCommand::Render) => {}
                    Ok(RenderCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        stop = true;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                }
            }
        }
        if stop {
            break;
        }

        if welcome || advance_event_dot || advance_status_animation || advance_status_timer {
            let mut shell = state.borrow_mut();
            if welcome {
                // The animated card is a bounded cache prefix. Repaint that
                // prefix without reflowing the complete transcript on every
                // 16 ms tick.
                shell.invalidate_transcript();
            }
            if advance_event_dot {
                shell.advance_event_dot_animation();
                last_event_dot_toggle = Instant::now();
            }
            if advance_status_animation {
                if thinking_spinner {
                    shell.advance_thinking_spinner();
                }
                if status_shimmer {
                    shell.advance_status_shimmer();
                }
                last_status_animation = Instant::now();
            }
            if advance_status_timer {
                shell.advance_status_timer();
                last_status_timer_update = Instant::now();
            }
        }
        tui.request_render();
        last_render = Some(Instant::now());
    }

    tui.stop();
}

#[cfg(test)]
mod scheduler_tests {
    use std::time::{Duration, Instant};

    use super::{frame_coalesce_delay, render_poll_interval, render_wake_requires_frame};

    #[test]
    fn active_state_without_a_concrete_wake_never_requests_a_frame() {
        assert!(!render_wake_requires_frame(false, false, false, false));
    }

    #[test]
    fn semantic_work_and_due_visual_transitions_each_request_a_frame() {
        assert!(render_wake_requires_frame(true, false, false, false));
        assert!(render_wake_requires_frame(false, true, false, false));
        assert!(render_wake_requires_frame(false, false, true, false));
        assert!(render_wake_requires_frame(false, false, false, true));
    }

    #[test]
    fn status_animation_sleeps_until_its_next_frame() {
        assert_eq!(render_poll_interval(true, true), Duration::from_millis(16));
        assert_eq!(render_poll_interval(false, true), Duration::from_millis(80));
        assert_eq!(
            render_poll_interval(false, false),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn semantic_bursts_coalesce_for_at_most_one_terminal_frame() {
        let last = Instant::now();
        assert_eq!(
            frame_coalesce_delay(Some(last), last + Duration::from_millis(1)),
            Duration::from_millis(15)
        );
        assert_eq!(
            frame_coalesce_delay(Some(last), last + Duration::from_millis(16)),
            Duration::ZERO
        );
        assert_eq!(frame_coalesce_delay(None, last), Duration::ZERO);
    }
}

#[derive(Default)]
pub(super) struct ShellFrameState {
    pub(super) initialized: bool,
    pub(super) width: u16,
    pub(super) height: u16,
    pub(super) theme_epoch: u64,
    pub(super) transcript_epoch: u64,
    pub(super) transcript_generation: u64,
    pub(super) transcript_len: usize,
    pub(super) verbose_tools: bool,
    pub(super) overlay_active: bool,
    /// Whether the retained frame currently represents the bounded semantic
    /// viewport rather than the native append-only transcript tail.
    pub(super) application_viewport: bool,
    /// Rows of the native transcript frame retained above the screen-sized
    /// overlay surface. This bounds lazy diffs when mutable chrome changes the
    /// overlay's seam with terminal-owned history.
    pub(super) overlay_prefix_len: usize,
    /// The native pending-tool preview diverges from canonical cached rows at
    /// this seam; even a status-only update must replace that bounded suffix.
    pub(super) pending_tool_start: Option<usize>,
}

/// The retained root component. It reads the shell state at render time, while
/// `InteractiveShell` mutates that same state in response to events.
pub(super) struct ShellComponent {
    state: SharedState,
    frame: RefCell<ShellFrameState>,
    /// Mouse capture starts in bounded semantic mode. Keyboard PageUp can
    /// request the same renderer later without changing terminal mouse policy.
    mouse_application_viewport: bool,
}

impl ShellComponent {
    pub(super) fn new(state: SharedState, application_viewport: bool) -> Self {
        Self {
            state,
            frame: RefCell::new(ShellFrameState::default()),
            mouse_application_viewport: application_viewport,
        }
    }

    fn uses_application_viewport(&self, state: &ShellState) -> bool {
        self.mouse_application_viewport || state.application_viewport_requested
    }
}

impl Component for ShellComponent {
    fn render(&self, width: u16) -> Vec<String> {
        let state = self.state.borrow();
        if state.startup_pending {
            // TUI::start paints immediately. Keep the renderer/input lifecycle
            // live for onboarding, but do not cache or publish a provisional
            // transcript/model frame. The ready frame starts from row zero.
            return render_startup_surface(&state, width);
        }
        if self.uses_application_viewport(&state) {
            state.native_animation_viewport_top.set(None);
            let lines = render_shell_viewport_at(&state, width, Instant::now());
            let mut frame = self.frame.borrow_mut();
            frame.initialized = true;
            frame.width = width;
            frame.height = state.size.1;
            frame.theme_epoch = state.theme_epoch;
            frame.transcript_epoch = state.transcript_epoch;
            frame.verbose_tools = state.verbose_tools;
            frame.application_viewport = true;
            lines
        } else {
            let lines = render_shell(&state, width);
            synchronize_shell_frame(&state, width, &mut self.frame.borrow_mut());
            lines
        }
    }

    fn render_update(&self, width: u16) -> Option<FrameUpdate> {
        let state = self.state.borrow();
        Some(if self.uses_application_viewport(&state) {
            state.native_animation_viewport_top.set(None);
            render_shell_viewport_update(
                &state,
                width,
                Instant::now(),
                &mut self.frame.borrow_mut(),
            )
        } else {
            // Pi owns its physical scrollback ledger. Its text-only lazy path
            // consumes row replacements, not the extended renderer's semantic
            // commit handshake; do not classify settled history for it.
            render_shell_update_without_cursor(
                &state,
                width,
                Instant::now(),
                &mut self.frame.borrow_mut(),
            )
        })
    }

    fn render_update_with_cursor(
        &self,
        width: u16,
        cursor: Option<CommitCursor>,
    ) -> Option<FrameUpdate> {
        let state = self.state.borrow();
        if state.startup_pending {
            // Setup is a bounded transient surface, never a committed prefix.
            // Leave ShellFrameState uninitialized until the first ready frame;
            // the retained TUI diff removes all previous setup rows on release.
            return Some(FrameUpdate {
                stable_prefix: 0,
                replacement: render_startup_surface(&state, width),
                pinned: None,
                resize_replay: None,
                reanchor_viewport: false,
                rebuild_scrollback: false,
            });
        }
        Some(if self.uses_application_viewport(&state) {
            state.native_animation_viewport_top.set(None);
            render_shell_viewport_update(
                &state,
                width,
                Instant::now(),
                &mut self.frame.borrow_mut(),
            )
        } else {
            render_shell_update_with_cursor(
                &state,
                width,
                Instant::now(),
                &mut self.frame.borrow_mut(),
                cursor,
            )
        })
    }

    fn invalidate(&mut self) {
        *self.frame.get_mut() = ShellFrameState::default();
    }
}

#[cfg(test)]
mod commit_metadata_tests {
    use super::super::transcript_commit::take_commit_metadata_visits;
    use super::super::{
        CompactionBlock, InteractiveShell, OutputChannel, ShellOverlay, TranscriptBlock,
    };
    use super::*;

    fn history_shell(blocks: usize) -> InteractiveShell {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        {
            let mut state = shell.state.borrow_mut();
            for index in 0..blocks {
                state.push_block(TranscriptBlock::Notice(format!("settled history {index}")));
            }
        }
        shell
    }

    fn advance_live_shell(shell: &mut InteractiveShell, tick: usize) {
        let mut state = shell.state.borrow_mut();
        match tick % 3 {
            0 => state.append_text_block(OutputChannel::Text, "more words "),
            1 => state.advance_status_shimmer(),
            _ => state.editor.set_text(format!("draft {tick}")),
        }
    }

    #[test]
    fn shell_component_lazy_updates_skip_history_metadata() {
        let mut shell = history_shell(4096);
        shell.begin_run("openai");
        let component = ShellComponent::new(shell.state.clone(), false);
        let initial = component.render(80);
        assert!(initial.len() > 4096);
        take_commit_metadata_visits();
        for tick in 0..12 {
            advance_live_shell(&mut shell, tick);
            let update = component.render_update(80).unwrap();
            assert!(update.pinned.is_none());
            assert!(update.stable_prefix >= 4096);
            assert!(update.replacement.len() <= 32);
            assert_eq!(take_commit_metadata_visits(), 0, "tick {tick}");
        }
    }

    struct InteractiveTerminal;

    impl sexy_tui_rs::Terminal for InteractiveTerminal {
        fn start_events(
            &mut self,
            _on_input: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>,
            _on_resize: Box<dyn FnMut()>,
        ) {
        }
        fn stop(&mut self) {}
        fn write(&mut self, _data: &str) {}
        fn columns(&self) -> u16 {
            80
        }
        fn rows(&self) -> u16 {
            24
        }
        fn move_by(&mut self, _lines: i16) {}
        fn hide_cursor(&mut self) {}
        fn show_cursor(&mut self) {}
        fn clear_line(&mut self) {}
        fn clear_from_cursor(&mut self) {}
        fn clear_screen(&mut self) {}
        fn capabilities(&self) -> sexy_tui_rs::TerminalCapabilities {
            sexy_tui_rs::TerminalCapabilities::interactive(sexy_tui_rs::ColorDepth::TrueColor, true)
        }
    }

    #[test]
    fn shell_pi_lazy_updates_skip_history_metadata() {
        // Exercise the real TUI -> retained ShellComponent boundary, not just a
        // generic LazyTail fixture or a direct call to the suffix builder.
        let mut shell = history_shell(4096);
        shell.begin_run("openai");
        let mut tui = TUI::new(Box::new(InteractiveTerminal));
        tui.add_child(Box::new(ShellComponent::new(shell.state.clone(), false)));
        tui.start();
        shell.tui = Some(tui);
        take_commit_metadata_visits();
        for tick in 0..12 {
            advance_live_shell(&mut shell, tick);
            shell.render();
            assert_eq!(take_commit_metadata_visits(), 0, "Pi tick {tick}");
        }
    }

    #[test]
    fn shell_component_cursor_handshake_keeps_bootstrap_and_ack() {
        let shell = history_shell(4096);
        let component = ShellComponent::new(shell.state.clone(), false);
        component.render(80);
        take_commit_metadata_visits();
        let initial = component.render_update_with_cursor(80, None).unwrap();
        let pinned = initial
            .pinned
            .expect("None cursor still requests a handshake");
        assert!(pinned.acknowledged.is_none());
        assert!(pinned.stable_rows > 4000);
        let target = pinned.target.expect("settled prefix has a commit target");
        assert!(take_commit_metadata_visits() >= 4096);

        let update = component
            .render_update_with_cursor(80, Some(target.cursor))
            .unwrap();
        let pinned = update.pinned.unwrap();
        assert_eq!(pinned.acknowledged, Some(target));
        assert_eq!(pinned.target, Some(target));
        assert!(take_commit_metadata_visits() < 64);
    }

    fn check_unpinned_update(
        component: &ShellComponent,
        retained: &mut Vec<String>,
    ) -> FrameUpdate {
        let width = component.state.borrow().size.0;
        take_commit_metadata_visits();
        let update = component.render_update(width).unwrap();
        assert!(update.pinned.is_none());
        assert_eq!(take_commit_metadata_visits(), 0);
        retained.truncate(update.stable_prefix);
        retained.extend(update.replacement.iter().cloned());
        assert_eq!(*retained, render_shell(&component.state.borrow(), width));
        update
    }

    #[test]
    fn shell_component_unpinned_invalidations_match_full_frame() {
        let shell = history_shell(256);
        let component = ShellComponent::new(shell.state.clone(), false);
        let mut retained = component.render(80);
        let old_start = shell.state.borrow().transcript_cache.borrow().block_starts[3];
        {
            let mut state = shell.state.borrow_mut();
            state.transcript[3] = TranscriptBlock::Notice("offscreen replacement".into());
            state.touch_block(3);
        }
        let changed = check_unpinned_update(&component, &mut retained);
        assert_eq!(changed.stable_prefix, old_start);

        {
            let mut state = shell.state.borrow_mut();
            state.push_block(TranscriptBlock::Compaction(Box::new(CompactionBlock {
                label: "Context compacted".into(),
                summary: "retained disclosure detail\n\n".repeat(40),
                expanded: false,
            })));
        }
        check_unpinned_update(&component, &mut retained);
        for verbose in [true, false] {
            let mut state = shell.state.borrow_mut();
            state.verbose_tools = verbose;
            state.invalidate_disclosure();
            drop(state);
            let changed = check_unpinned_update(&component, &mut retained);
            assert_eq!(changed.stable_prefix, 0);
            assert!(changed.rebuild_scrollback);
        }
        {
            let mut state = shell.state.borrow_mut();
            state.theme_epoch += 1;
            state.invalidate_rich_text();
        }
        assert!(check_unpinned_update(&component, &mut retained).reanchor_viewport);

        shell.state.borrow_mut().overlay = Some(ShellOverlay::Text("temporary overlay".into()));
        assert!(check_unpinned_update(&component, &mut retained).reanchor_viewport);
        {
            let mut state = shell.state.borrow_mut();
            state.size = (64, 16);
            state.invalidate_transcript_layout();
        }
        let resized = check_unpinned_update(&component, &mut retained);
        assert!(resized.reanchor_viewport);
        assert!(resized.resize_replay.is_some());
        shell.state.borrow_mut().overlay = None;
        assert!(check_unpinned_update(&component, &mut retained).reanchor_viewport);
    }

    #[test]
    fn shell_component_unpinned_hydration_replaces_generation() {
        use octet_agent::{EntryValue, Session};
        use octet_ai::{Message, UserMessage, UserPart};

        let mut shell = history_shell(256);
        let component = ShellComponent::new(shell.state.clone(), false);
        let mut retained = component.render(80);
        let old_cursor = component
            .render_update_with_cursor(80, None)
            .unwrap()
            .pinned
            .unwrap()
            .target
            .unwrap()
            .cursor;
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("replacement.jsonl")).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("hydrated replacement prompt".into())],
            })))
            .unwrap();
        shell.hydrate(&session).unwrap();
        let update = check_unpinned_update(&component, &mut retained);
        assert!(update.reanchor_viewport);
        assert_eq!(update.stable_prefix, 0);
        assert!(!retained.iter().any(|line| line.contains("settled history")));
        shell.select_all_transcript();
        assert_eq!(
            shell.copy_selected_plain_text().as_deref(),
            Some("hydrated replacement prompt")
        );
        let pinned = component
            .render_update_with_cursor(80, Some(old_cursor))
            .unwrap()
            .pinned
            .unwrap();
        assert_ne!(pinned.generation, old_cursor.generation);
        assert!(pinned.acknowledged.is_none());
    }

    #[test]
    fn shell_component_semantic_viewports_do_not_request_metadata() {
        let mut shell = history_shell(4096);
        let component = ShellComponent::new(shell.state.clone(), false);
        component.render(80);
        shell.scroll_lines(-12);
        take_commit_metadata_visits();
        let update = component.render_update(80).unwrap();
        assert!(update.reanchor_viewport);
        assert!(update.pinned.is_none());
        assert!(update.replacement.len() <= 24);
        assert_eq!(take_commit_metadata_visits(), 0);

        let mouse = ShellComponent::new(shell.state.clone(), true);
        assert!(mouse.render(80).len() <= 24);
        assert!(mouse
            .render_update_with_cursor(80, None)
            .unwrap()
            .pinned
            .is_none());
        assert_eq!(take_commit_metadata_visits(), 0);
    }
}
