//! Retained component tree with line-differential terminal rendering.
use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::images::{ImageAnchor, ImageProtocol};
use crate::scrollback::reset_and_replay;
use crate::terminal::{key_to_string, Terminal, TerminalInput};
use crate::utils::visible_width;

/// Zero-width APC escape sequence used as a cursor position marker.
/// Pi's zero-cell APC cursor marker.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";

/// Whether a rendered row carries a Kitty graphics placement.
pub(crate) fn is_image_line(line: &str) -> bool {
    line.contains("\x1b_G")
        || ImageAnchor::parse_all(line)
            .iter()
            .any(|anchor| anchor.protocol() == ImageProtocol::Kitty)
}

/// Kitty graphics protocol escape that deletes every placed image. Destructive
/// inline replays must emit it before rebuilding rows they may have erased.
pub(crate) fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
const PI_LINE_RESET: &str = "\x1b[0m\x1b]8;;\x07";

#[derive(Clone, Debug)]
struct KittyImageHeader {
    ids: Vec<u32>,
    rows: usize,
}

fn parse_kitty_image_headers(line: &str) -> Vec<KittyImageHeader> {
    let mut headers = Vec::new();
    let mut search_from = 0;
    while let Some(relative_start) = line[search_from..].find(KITTY_SEQUENCE_PREFIX) {
        let sequence_start = search_from.saturating_add(relative_start);
        let params_start = sequence_start.saturating_add(KITTY_SEQUENCE_PREFIX.len());
        let Some(relative_end) = line[params_start..].find(';') else {
            break;
        };
        let params_end = params_start.saturating_add(relative_end);
        let mut header = KittyImageHeader {
            ids: Vec::new(),
            rows: 1,
        };
        for parameter in line[params_start..params_end].split(',') {
            let Some((key, value)) = parameter.split_once('=') else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            if value == 0 {
                continue;
            }
            match key {
                "i" => header.ids.push(value),
                "r" => header.rows = value as usize,
                _ => {}
            }
        }
        headers.push(header);
        search_from = params_end.saturating_add(1);
    }
    headers
}

fn extract_kitty_image_ids(line: &str) -> Vec<u32> {
    let mut ids = parse_kitty_image_headers(line)
        .into_iter()
        .flat_map(|header| header.ids)
        .collect::<Vec<_>>();
    ids.extend(
        ImageAnchor::parse_all(line)
            .into_iter()
            .filter(|anchor| anchor.protocol() == ImageProtocol::Kitty)
            .map(|anchor| anchor.id().get()),
    );
    ids
}

fn extract_kitty_image_rows(line: &str) -> usize {
    parse_kitty_image_headers(line)
        .into_iter()
        .map(|header| header.rows)
        .chain(
            ImageAnchor::parse_all(line)
                .into_iter()
                .filter(|anchor| anchor.protocol() == ImageProtocol::Kitty)
                .map(|anchor| usize::from(anchor.layout().rows())),
        )
        .max()
        .unwrap_or(1)
}

fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LogicalCursorPosition {
    row: usize,
    column: usize,
}

fn is_termux_session() -> bool {
    std::env::var_os("TERMUX_VERSION").is_some()
}

/// Global input listener. Returning `Some` consumes the input event.
pub type InputListener<'a> = Box<dyn FnMut(&str) -> Option<String> + 'a>;

// =============================================================================
// Component Trait
// =============================================================================

/// Width-independent identity for a finalized semantic transcript boundary.
/// The renderer retains this cursor after the corresponding rows enter native
/// scrollback, then asks the component to map it into each new physical layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommitCursor {
    pub generation: u64,
    pub block: u64,
    pub segment: u64,
}

/// A semantic commit cursor and its exclusive visual-row boundary in the
/// current frame layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitPosition {
    pub cursor: CommitCursor,
    pub row: usize,
}

/// Commit handshake for the native-scrollback renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PinnedFrame {
    /// Semantic timeline containing this frame.
    pub generation: u64,
    /// Current-layout position of the cursor supplied to
    /// [`Component::render_update_with_cursor`].
    pub acknowledged: Option<CommitPosition>,
    /// Furthest finalized semantic boundary that may enter scrollback this
    /// frame.
    pub target: Option<CommitPosition>,
    /// Exclusive current-layout row boundary proven immutable. Physical rows
    /// may enter terminal history up to this seam before a coarser semantic
    /// `target` can be acknowledged.
    pub stable_rows: usize,
    /// The visible tail is a temporary, screen-relative surface rather than an
    /// extension of the append-only transcript tape. While this is set, repaint
    /// the surface in place without advancing native history; the retained
    /// transcript ledger is reconciled when the surface closes.
    pub viewport_surface: bool,
}

/// A lazy replacement for the mutable tail of a retained frame. Lines before
/// `stable_prefix` are guaranteed byte-identical to the previous frame, so the
/// TUI can reuse them without cloning or comparing a long committed history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameUpdate {
    pub stable_prefix: usize,
    pub replacement: Vec<String>,
    /// Semantic commit metadata for the native-scrollback renderer. `None`
    /// keeps the generic shell-style differential renderer.
    pub pinned: Option<PinnedFrame>,
    /// Complete application-owned frame to replay on a destructive resize
    /// before the visible `replacement` is repainted. This lets a temporary
    /// screen surface obscure transcript rows without becoming the source of
    /// truth for rebuilt scrollback. The alternate frame must produce the same
    /// number of off-screen rows as the displayed frame.
    pub resize_replay: Option<Vec<String>>,
    /// The component replaced its logical timeline (for example, a resumed
    /// conversation was replaced by a new session). Repaint the visible tail
    /// from the top of the terminal so later fixed-height chrome remains
    /// anchored to the physical bottom row.
    pub reanchor_viewport: bool,
    /// The presentation of committed rows changed. Generic inline frames may
    /// rebuild retained history; pinned frames preserve terminal-owned history
    /// and re-anchor only their live suffix.
    pub rebuild_scrollback: bool,
}

/// Exact row-level facts captured while the old retained frame is still
/// available. A lazy update moves that frame into the next frame, so terminal
/// writing must not try to rediscover these facts afterward.
#[derive(Debug)]
struct FrameChangeHints {
    first_changed: usize,
    fixed_height: Option<FixedHeightChangeHints>,
    affected_tail_has_image: bool,
}

#[derive(Debug)]
struct FixedHeightChangeHints {
    last_changed: Option<usize>,
    changed_rows: Vec<usize>,
    image_rows: Vec<usize>,
}

fn frame_change_hints(
    previous: &[String],
    stable_prefix: usize,
    replacement: &[String],
) -> FrameChangeHints {
    let previous_tail = &previous[stable_prefix..];
    let mut changed_rows = Vec::new();
    let mut image_rows = Vec::new();
    for (offset, (old, new)) in previous_tail.iter().zip(replacement).enumerate() {
        if old == new {
            continue;
        }
        let row = stable_prefix.saturating_add(offset);
        changed_rows.push(row);
        if is_image_line(old) || is_image_line(new) {
            image_rows.push(row);
        }
    }
    let shared_len = previous_tail.len().min(replacement.len());
    let first_changed = changed_rows
        .first()
        .copied()
        .unwrap_or_else(|| stable_prefix.saturating_add(shared_len));
    let changed_offset = first_changed.saturating_sub(stable_prefix);
    let affected_tail_has_image = previous_tail[changed_offset.min(previous_tail.len())..]
        .iter()
        .chain(replacement[changed_offset.min(replacement.len())..].iter())
        .any(|line| is_image_line(line));
    let fixed_height =
        (stable_prefix.saturating_add(replacement.len()) == previous.len()).then(|| {
            FixedHeightChangeHints {
                last_changed: changed_rows.last().copied(),
                changed_rows,
                image_rows,
            }
        });
    FrameChangeHints {
        first_changed,
        fixed_height,
        affected_tail_has_image,
    }
}

/// Component interface — all UI elements must implement this.
pub trait Component {
    /// Render the component to lines for the given viewport width.
    fn render(&self, width: u16) -> Vec<String>;

    /// Optionally render only the mutable frame tail. Implementations must
    /// return `None` after any change that invalidates the stable-prefix
    /// guarantee (for example a width change).
    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        None
    }

    /// Render a lazy update while mapping the native-scrollback commit cursor
    /// retained by the TUI. Components without semantic commit points can keep
    /// implementing [`Component::render_update`].
    fn render_update_with_cursor(
        &self,
        width: u16,
        _cursor: Option<CommitCursor>,
    ) -> Option<FrameUpdate> {
        self.render_update(width)
    }

    /// Handle keyboard input when component has focus.
    fn handle_input(&mut self, _data: &str) {}

    /// Handle a bracketed-paste payload when component has focus.
    ///
    /// The default preserves legacy single-string behavior. Multiline editors
    /// can override this to keep paste atomic instead of replaying it as keys.
    fn handle_paste(&mut self, data: &str) {
        self.handle_input(data);
    }

    /// Pi-compatible constrained layout descriptor.
    ///
    /// A component that returns a node here (`VStack`, `HStack`, `ScrollView`,
    /// or a product wrapper that delegates to one) is sized by
    /// [`crate::layout::render_layout_frame`] instead of being treated as a
    /// leaf. The default keeps every existing component a leaf.
    fn layout_node(&self) -> Option<crate::layout::LayoutNode<'_>> {
        None
    }

    /// Pi-compatible normalized mouse handler.
    ///
    /// Returning a result makes this component the dispatch target for the
    /// event; the default declines, so an event travels outward along the
    /// hit path resolved from the last rendered frame. [`crate::mouse::MouseRegion`]
    /// wraps a component that has no mouse handling of its own.
    fn handle_mouse<'a>(
        &'a self,
        _event: &crate::mouse::TuiMouseEvent,
    ) -> Option<crate::mouse::MouseOutcome<'a>> {
        None
    }

    /// If true, component receives key release events (Kitty protocol).
    fn wants_key_release(&self) -> bool {
        false
    }

    /// Invalidate any cached rendering state.
    fn invalidate(&mut self);
}

// =============================================================================
// TUI — Main Interface
// =============================================================================

/// Main TUI instance managing the render loop.
pub struct TUI<'a> {
    terminal: Box<dyn Terminal + 'a>,
    children: Vec<Box<dyn Component>>,
    previous_frame: Vec<String>,
    /// Terminal dimensions used for `previous_frame`. A resize invalidates all
    /// cursor-relative differential-rendering assumptions.
    previous_size: Option<(u16, u16)>,
    first_render: bool,
    running: bool,
    capabilities: crate::capabilities::TerminalCapabilities,
    input_listeners: Vec<InputListener<'a>>,
    /// Absolute marker position retained independently of the prepared frame.
    /// Lazy replacements can therefore reuse a cursor owned by their stable
    /// prefix and remap it when the viewport moves.
    logical_cursor_position: Option<LogicalCursorPosition>,
    /// Pi-compatible logical row containing the end of rendered content.
    cursor_row: usize,
    /// Pi-compatible logical row containing the physical terminal cursor.
    hardware_cursor_row: usize,
    /// Largest logical frame painted since the most recent full redraw.
    max_lines_rendered: usize,
    /// Logical row represented by the top of the previous terminal viewport.
    previous_viewport_top: usize,
    /// Number of Pi full-frame renders, exposed for parity regressions.
    full_redraw_count: usize,
    /// Pi's optional full-redraw-on-shrink policy.
    clear_on_shrink: bool,
    /// Pi tracks Kitty placements by image ID so redraws delete only affected
    /// images before retransmitting their rows.
    previous_kitty_image_ids: BTreeSet<u32>,
    /// O(1) guard for Pi lazy updates. Frames containing any Kitty command use
    /// the established full-component path so image reservations and duplicate
    /// IDs never cross an uninspected stable-prefix seam.
    previous_frame_has_kitty: bool,
    #[cfg(test)]
    last_pi_lazy_inspected_rows: usize,
    /// Pi positions the hardware cursor for IME but hides it by default because
    /// editor components render their own visual cursor.
    show_hardware_cursor: bool,
    /// Render into the primary screen. The initial paint is limited to the
    /// visible tail; later appended lines can flow into native scrollback.
    /// Off-screen logical rows remain retained in `previous_frame`, so callers
    /// must keep committed lines byte-stable.
    inline_scrollback: bool,
    /// Screen row (0-based) currently showing `previous_frame`'s last line.
    /// A frame shrink cannot scroll the screen back down, so the frame's tail
    /// can sit above the bottom row; every inline repaint derives its cursor
    /// addressing from this anchor rather than assuming a bottom-aligned tail.
    inline_bottom_row: usize,
    /// Current-layout prefix already represented in terminal-owned history.
    /// Immutable physical rows can advance beyond the coarser semantic commit
    /// cursor. A destructive replay can also place provisional rows here, so
    /// the seam is tracked independently to prevent later acknowledgement from
    /// appending them twice.
    inline_history_rows: usize,
    /// Current-layout row corresponding to `inline_commit_cursor`. This is
    /// remapped from semantic identity on every update and is never reused
    /// across a width change.
    inline_committed_rows: usize,
    /// Last semantic boundary physically appended to native scrollback.
    inline_commit_cursor: Option<CommitCursor>,
    /// Semantic timeline owning the current replay/commit ledger. This remains
    /// known after a destructive replay even though its commit cursor is reset,
    /// so an immediate session replacement cannot inherit the old row seam.
    inline_generation: Option<u64>,
    /// First logical row represented by grid row zero in pinned mode.
    inline_window_top: usize,
    /// Temporary screen-relative tails (pickers, completion menus, reports, or
    /// a retreating streamed layout) repaint the grid without advancing the
    /// append-only transcript ledger. The physical rows are retained here for
    /// bounded differential updates until the semantic tape is re-anchored.
    inline_surface_active: bool,
    inline_surface_window: Vec<String>,
    /// Nested renderer helpers share one synchronized-output transaction so
    /// cursor placement becomes visible atomically with the frame.
    synchronized_output_depth: usize,
    /// Opt-in fullscreen session. When this is `Some`, the renderer owns a
    /// fixed alternate-screen viewport and restores the final document to the
    /// main screen on exit (Pi's `TuiAltScreen`).
    alternate_screen_session: Option<crate::alt_screen::AlternateScreen>,
    /// Replay the rendered document onto the main screen when the session ends.
    restore_transcript_on_exit: bool,
}

impl<'a> TUI<'a> {
    pub fn new(terminal: Box<dyn Terminal + 'a>) -> Self {
        let capabilities = terminal.capabilities();
        TUI {
            terminal,
            children: Vec::new(),
            previous_frame: Vec::new(),
            previous_size: None,
            first_render: true,
            running: false,
            capabilities,
            input_listeners: Vec::new(),
            logical_cursor_position: None,
            cursor_row: 0,
            hardware_cursor_row: 0,
            max_lines_rendered: 0,
            previous_viewport_top: 0,
            full_redraw_count: 0,
            clear_on_shrink: std::env::var_os("PI_CLEAR_ON_SHRINK")
                .is_some_and(|value| value == "1"),
            previous_kitty_image_ids: BTreeSet::new(),
            previous_frame_has_kitty: false,
            #[cfg(test)]
            last_pi_lazy_inspected_rows: 0,
            show_hardware_cursor: std::env::var_os("PI_HARDWARE_CURSOR")
                .is_some_and(|value| value == "1"),
            inline_scrollback: false,
            inline_bottom_row: 0,
            inline_history_rows: 0,
            inline_committed_rows: 0,
            inline_commit_cursor: None,
            inline_generation: None,
            inline_window_top: 0,
            inline_surface_active: false,
            inline_surface_window: Vec::new(),
            synchronized_output_depth: 0,
            alternate_screen_session: None,
            restore_transcript_on_exit: true,
        }
    }

    /// Opt into Pi's fullscreen alternate-screen session.
    ///
    /// The session owns a fixed viewport: autowrap is disabled while it is
    /// active, every frame paints absolute viewport rows instead of scrolling
    /// native history, and leaving it restores the final document to the main
    /// screen. Inline scrollback is a primary-screen compatibility path, so it
    /// is disabled with the opt-in.
    /// The opt-in takes effect at [`TUI::start`]; the session owns entering
    /// and leaving the alternate screen for its whole lifetime.
    pub fn set_alternate_screen(&mut self, enabled: bool) {
        self.inline_scrollback = false;
        let synchronized_output = self.capabilities.synchronized_output;
        self.alternate_screen_session = if enabled {
            Some(crate::alt_screen::AlternateScreen::new(
                crate::alt_screen::AltScreenOptions {
                    // Mouse reporting stays with the backend that enabled it.
                    mouse: false,
                    all_motion_mouse: false,
                    synchronized_output,
                },
            ))
        } else {
            None
        };
    }

    /// Whether the final document is replayed onto the main screen when the
    /// alternate-screen session ends. `false` keeps Pi's `preserveScreen`
    /// behaviour instead (the main screen is restored untouched).
    pub fn set_restore_transcript_on_exit(&mut self, restore: bool) {
        self.restore_transcript_on_exit = restore;
    }

    /// Whether the renderer currently owns the alternate screen.
    pub fn alternate_screen_active(&self) -> bool {
        self.alternate_screen_session
            .as_ref()
            .is_some_and(crate::alt_screen::AlternateScreen::is_active)
    }

    /// Opt into inline scrollback rendering (see the field's invariants).
    pub fn set_inline_scrollback(&mut self, enabled: bool) {
        self.inline_scrollback = enabled;
    }

    /// Number of Pi-compatible full redraws performed by this TUI.
    pub fn full_redraws(&self) -> usize {
        self.full_redraw_count
    }

    /// Whether Pi's optional full redraw on frame shrink is enabled.
    pub fn clear_on_shrink(&self) -> bool {
        self.clear_on_shrink
    }

    /// Match Pi's `setClearOnShrink` runtime policy.
    pub fn set_clear_on_shrink(&mut self, enabled: bool) {
        self.clear_on_shrink = enabled;
    }

    /// Whether the hardware cursor is visible after IME positioning.
    pub fn show_hardware_cursor(&self) -> bool {
        self.show_hardware_cursor
    }

    /// Match Pi's hardware-cursor visibility policy.
    pub fn set_show_hardware_cursor(&mut self, enabled: bool) {
        if self.show_hardware_cursor == enabled {
            return;
        }
        self.show_hardware_cursor = enabled;
        if !enabled {
            self.terminal.hide_cursor();
        }
        self.request_render();
    }

    /// Set the terminal window title via OSC 2. Useful with inline
    /// scrollback, where no chrome row stays visible while the user scrolls
    /// history — the title bar is the one surface that always remains.
    pub fn set_window_title(&mut self, title: &str) {
        if self.capabilities.plain || !self.capabilities.interactive {
            return;
        }
        // OSC payloads must never contain control bytes; a stray BEL or ESC
        // would terminate or corrupt the sequence.
        let clean: String = title.chars().filter(|c| !c.is_control()).collect();
        self.terminal.write(&format!("\x1b]2;{clean}\x07"));
    }

    /// Add a component. Input and paste are delivered to the most recently
    /// added child, matching the single active-component usage of the shell.
    pub fn add_child(&mut self, child: Box<dyn Component>) {
        self.children.push(child);
    }

    /// Remove a component by index.
    pub fn remove_child(&mut self, idx: usize) {
        if idx < self.children.len() {
            self.children.remove(idx);
        }
    }

    /// The child that currently receives input and paste events.
    fn active_child_mut(&mut self) -> Option<&mut Box<dyn Component>> {
        self.children.last_mut()
    }

    /// Add an input listener for global key handling.
    pub fn add_input_listener(&mut self, f: InputListener<'a>) {
        self.input_listeners.push(f);
    }

    /// The most recently composed frame, one entry per terminal row.
    ///
    /// This is the frame as last rendered, retained for differential rendering.
    /// It is exposed for diagnostics (the coding agent's `/debug` surface) and
    /// carries no cursor or viewport state.
    pub fn rendered_frame(&self) -> &[String] {
        &self.previous_frame
    }

    /// Request a re-render at the next opportunity.
    pub fn request_render(&mut self) {
        self.request_render_force(false);
    }

    /// Match Pi's forced-render path by invalidating every retained cursor and
    /// viewport assumption before rendering.
    pub fn request_render_force(&mut self, force: bool) {
        if force {
            self.previous_frame.clear();
            self.previous_size = Some((u16::MAX, u16::MAX));
            self.logical_cursor_position = None;
            self.cursor_row = 0;
            self.hardware_cursor_row = 0;
            self.max_lines_rendered = 0;
            self.previous_viewport_top = 0;
            if let Some(session) = self.alternate_screen_session.as_mut() {
                session.invalidate();
            }
        }
        if self.running {
            self.render_frame();
        }
    }

    /// Start the TUI render loop.
    pub fn start(&mut self) {
        self.running = true;
        if self.capabilities.interactive {
            self.terminal.hide_cursor();
        }
        // A terminal that cannot address the cursor stays on the primary
        // screen; the session reports that refusal and paints nothing.
        if let Some(mut session) = self.alternate_screen_session.take() {
            if session.enter(&mut *self.terminal) {
                session.invalidate();
                self.alternate_screen_session = Some(session);
            }
        }

        // Perform first render
        self.render_frame();

        // Input/event loop is handled externally by the caller
        // (matching pi-tui's architecture where the consumer drives the loop)
    }

    /// Stop the TUI render loop.
    pub fn stop(&mut self) {
        if !self.running {
            return;
        }
        self.running = false;
        if let Some(mut session) = self.alternate_screen_session.take() {
            // Pi's fullscreen teardown: leave the alternate screen and restore
            // the complete rendered document to the *main* screen, so the
            // reader's terminal keeps the transcript after the session ends.
            // Autowrap returns to the terminal inside the session's exit.
            let document = std::mem::take(&mut self.previous_frame);
            session.exit(
                &mut *self.terminal,
                &document,
                !self.restore_transcript_on_exit,
            );
            self.terminal.stop();
            return;
        }
        if self.uses_pi_renderer() {
            // Pi clears the editor's inverted fake cursor, moves to the line
            // after the complete logical frame, and only then restores the
            // process terminal.
            if !self.previous_frame.is_empty() {
                self.terminal.write(" ");
                let target_row = self.previous_frame.len();
                let mut buffer = String::new();
                push_vertical_move(
                    &mut buffer,
                    signed_difference(target_row, self.hardware_cursor_row),
                );
                buffer.push_str("\r\n");
                self.terminal.write(&buffer);
                self.hardware_cursor_row = target_row;
            }
            self.terminal.show_cursor();
            self.terminal.stop();
            return;
        }
        // Close any interrupted synchronized frame and all text/hyperlink
        // styling before restoring the backend. Repeated backend cleanup is
        // expected to be idempotent.
        if self.capabilities.synchronized_output {
            self.terminal.write("\x1b[?2026l");
            self.synchronized_output_depth = 0;
        }
        if !self.capabilities.plain {
            // Inline scrollback paints a mutable frame on the primary screen.
            // Its editor marker can leave the hardware cursor in the middle of
            // that frame. Anchor it at the final painted row before handing
            // the terminal back so the caller's normal-mode cleanup can move
            // to a fresh line without letting the shell prompt overwrite the
            // composer while leaving its footer behind.
            if self.inline_scrollback
                && self.capabilities.cursor_addressing
                && !self.previous_frame.is_empty()
            {
                self.terminal.write(&format!(
                    "\x1b[{};1H",
                    self.inline_bottom_row.saturating_add(1)
                ));
            }
            self.terminal.write("\x1b[0m\x1b]8;;\x1b\\");
            self.terminal.show_cursor();
        }
        self.terminal.stop();
    }

    /// Process input data. Should be called by the consumer's event loop.
    pub fn handle_input(&mut self, data: &str) {
        // Run input listeners first
        for listener in &mut self.input_listeners {
            if let Some(_modified) = listener(data) {
                // Listener consumed/modified the input
                return;
            }
        }

        if let Some(child) = self.active_child_mut() {
            child.handle_input(data);
        }

        self.request_render();
    }

    /// Route semantic terminal input without serializing printable keys into
    /// escape strings.  In particular, bracketed paste stays atomic until the
    /// focused component decides how to insert it.
    pub fn handle_terminal_input(&mut self, input: TerminalInput) {
        match input {
            TerminalInput::Text(text) => self.handle_input(&text),
            TerminalInput::Key(key) => {
                if let Some(control) = key_to_string(&key) {
                    self.handle_input(&control);
                }
            }
            TerminalInput::Paste(text) => {
                // Existing listeners receive the exact payload for backwards
                // compatibility. A consumed paste must not reach the editor.
                for listener in &mut self.input_listeners {
                    if listener(&text).is_some() {
                        return;
                    }
                }
                if let Some(child) = self.active_child_mut() {
                    child.handle_paste(&text);
                }
                self.request_render();
            }
        }
    }

    fn uses_pi_renderer(&self) -> bool {
        !self.capabilities.plain
            && !self.inline_scrollback
            && self.capabilities.cursor_addressing
            && self.capabilities.line_clearing
    }

    /// Render the current frame. Interactive non-inline terminals use Pi's
    /// normative differential algorithm; plain output and the explicit legacy
    /// inline extension retain their separate compatibility contracts.
    fn render_frame(&mut self) {
        if self.alternate_screen_session.is_some() {
            self.render_alternate_frame();
        } else if self.uses_pi_renderer() {
            self.render_pi_frame();
        } else {
            self.render_extended_frame();
        }
    }

    /// Paint one fixed-viewport alternate-screen frame.
    ///
    /// The complete document is rendered exactly as the primary-screen Pi
    /// renderer renders it and retained as [`TUI::rendered_frame`], so the
    /// hidden `/debug` frame seam and the exit handoff both see the whole
    /// logical transcript. Only the visible tail window is painted, at absolute
    /// viewport rows; the session diffs it against the previous window, so a
    /// status tick repaints its own row and nothing else.
    fn render_alternate_frame(&mut self) {
        let width_u16 = self.terminal.columns().max(1);
        let height_u16 = self.terminal.rows().max(1);
        let height = usize::from(height_u16);
        let update = (!self.first_render && self.previous_size == Some((width_u16, height_u16)))
            .then(|| self.root_render_update_without_cursor(width_u16))
            .flatten()
            .filter(|update| {
                update.stable_prefix <= self.previous_frame.len()
                    && !update.reanchor_viewport
                    && !update.rebuild_scrollback
                    && update.resize_replay.is_none()
            });
        // Retain the complete document for exit/debug, but prepare only the
        // changed tail. Animation must not clone or normalize settled history.
        let (stable_prefix, mut replacement) = update.map_or_else(
            || (0, self.root_render(width_u16)),
            |update| (update.stable_prefix, update.replacement),
        );
        let logical_cursor_position =
            extract_logical_cursor_position_from(&mut replacement, stable_prefix).or_else(|| {
                self.logical_cursor_position
                    .filter(|cursor| cursor.row < stable_prefix)
            });
        for line in &mut replacement {
            if !is_image_line(line) {
                *line = format!(
                    "{}{}",
                    crate::utils::normalize_terminal_output(line),
                    PI_LINE_RESET
                );
            }
        }
        let mut rendered = std::mem::take(&mut self.previous_frame);
        rendered.truncate(stable_prefix);
        rendered.extend(replacement);
        let window_top = rendered.len().saturating_sub(height);
        let window = rendered[window_top..].to_vec();
        let cursor = logical_cursor_position
            .filter(|cursor| cursor.row >= window_top)
            .map(|cursor| (cursor.row - window_top, cursor.column));
        if let Some(session) = self.alternate_screen_session.as_mut() {
            session.paint(
                &mut *self.terminal,
                &window,
                width_u16,
                height_u16,
                cursor,
                self.show_hardware_cursor,
            );
        }
        self.logical_cursor_position = logical_cursor_position;
        self.cursor_row =
            logical_cursor_position.map_or(rendered.len().saturating_sub(1), |cursor| cursor.row);
        self.hardware_cursor_row = self.cursor_row;
        self.max_lines_rendered = rendered.len();
        self.previous_viewport_top = window_top;
        self.previous_frame = rendered;
        self.previous_size = Some((width_u16, height_u16));
        self.first_render = false;
    }

    /// Rust port of Pi TUI's `doRender()` at revision
    /// `20be4b18d4c57487f8993d2762bace129f0cf7c6`.
    /// Keep this control flow structurally aligned with
    /// `packages/tui/src/tui.ts`; named upstream cases live in
    /// `tests/pi_tui_render.rs`. octet-specific native-scrollback policy belongs
    /// only to the explicit `inline_scrollback` compatibility path below.
    fn render_pi_frame(&mut self) {
        let width_u16 = self.terminal.columns();
        let height_u16 = self.terminal.rows().max(1);
        let width = usize::from(width_u16);
        let height = usize::from(height_u16);
        let previous_width = self.previous_size.map_or(0, |size| size.0);
        let previous_height = self.previous_size.map_or(0, |size| size.1);
        let width_changed = previous_width != 0 && previous_width != width_u16;
        let height_changed = previous_height != 0 && previous_height != height_u16;
        let previous_buffer_length = if previous_height > 0 {
            self.previous_viewport_top
                .saturating_add(usize::from(previous_height))
        } else {
            height
        };
        let mut previous_viewport_top = if height_changed {
            previous_buffer_length.saturating_sub(height)
        } else {
            self.previous_viewport_top
        };
        let mut viewport_top = previous_viewport_top;
        let mut hardware_cursor_row = self.hardware_cursor_row;

        let previous_len = self.previous_frame.len();
        let mut lazy_stable_prefix = None;
        let mut lazy_previous_tail = None;
        #[cfg(test)]
        {
            self.last_pi_lazy_inspected_rows = 0;
        }
        let (new_lines, logical_cursor_position) =
            if !self.first_render && !width_changed && !height_changed {
                let update = self
                    .root_render_update_without_cursor(width_u16)
                    .filter(|update| {
                        update.stable_prefix <= previous_len
                        // The normal Pi renderer owns its own scrollback ledger;
                        // these flags describe the extended/pinned renderer's
                        // physical reanchor contract and are not safe to reuse as
                        // a partial Pi frame.
                        && !update.reanchor_viewport
                        && !update.rebuild_scrollback
                        && update.resize_replay.is_none()
                        // Kitty row reservations and delete-by-ID semantics are
                        // already covered by the full-component Pi path. Until
                        // a seam-aware metadata algorithm has equivalent
                        // coverage, keep image-bearing frames on that path.
                        && !self.previous_frame_has_kitty
                        && !update.replacement.iter().any(|line| is_image_line(line))
                    });
                if let Some(update) = update {
                    let stable_prefix = update.stable_prefix;
                    let mut replacement = update.replacement;
                    let replacement_cursor =
                        extract_logical_cursor_position_from(&mut replacement, stable_prefix);
                    let retained_cursor = self
                        .logical_cursor_position
                        .filter(|cursor| cursor.row < stable_prefix);
                    let logical_cursor_position = replacement_cursor.or(retained_cursor);
                    #[cfg(test)]
                    {
                        self.last_pi_lazy_inspected_rows = replacement.len();
                    }
                    let replacement = replacement
                        .into_iter()
                        .map(|line| {
                            if is_image_line(&line) {
                                line
                            } else {
                                format!(
                                    "{}{}",
                                    crate::utils::normalize_terminal_output(&line),
                                    PI_LINE_RESET
                                )
                            }
                        })
                        .collect::<Vec<_>>();
                    // Move the old frame's strings into two owned vectors rather
                    // than cloning the stable history. The old tail is retained
                    // only for bounded comparisons.
                    let mut previous = std::mem::take(&mut self.previous_frame);
                    let previous_tail = previous.split_off(stable_prefix);
                    previous.extend(replacement);
                    lazy_stable_prefix = Some(stable_prefix);
                    lazy_previous_tail = Some(previous_tail);
                    (previous, logical_cursor_position)
                } else {
                    let mut rendered = self.root_render(width_u16);
                    let logical_cursor_position =
                        extract_logical_cursor_position_from(&mut rendered, 0);
                    for line in &mut rendered {
                        if !is_image_line(line) {
                            *line = format!(
                                "{}{}",
                                crate::utils::normalize_terminal_output(line),
                                PI_LINE_RESET
                            );
                        }
                    }
                    (rendered, logical_cursor_position)
                }
            } else {
                let mut rendered = self.root_render(width_u16);
                let logical_cursor_position =
                    extract_logical_cursor_position_from(&mut rendered, 0);
                for line in &mut rendered {
                    if !is_image_line(line) {
                        *line = format!(
                            "{}{}",
                            crate::utils::normalize_terminal_output(line),
                            PI_LINE_RESET
                        );
                    }
                }
                (rendered, logical_cursor_position)
            };
        self.logical_cursor_position = logical_cursor_position;
        let cursor_position = pi_cursor_position(logical_cursor_position, new_lines.len(), height);
        // Pi's first render writes the complete frame without touching saved
        // lines. Subsequent structural fallbacks clear and replay it.
        if self.first_render && !width_changed && !height_changed {
            self.pi_full_render(new_lines, width_u16, height_u16, false, cursor_position);
            return;
        }
        if width_changed {
            self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
            return;
        }
        if height_changed && !is_termux_session() {
            self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
            return;
        }
        if self.clear_on_shrink && new_lines.len() < self.max_lines_rendered && !self.first_render {
            self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
            return;
        }

        let mut first_changed = None;
        let mut last_changed = None;
        let max_lines = new_lines.len().max(previous_len);
        // A lazy component has already proven that rows before this boundary
        // are unchanged. Avoid walking every retained historical String just to
        // rediscover that fact on every animation frame.
        let stable_prefix = lazy_stable_prefix.unwrap_or(0);
        let compare_start = stable_prefix.min(max_lines);
        #[cfg(test)]
        if lazy_stable_prefix.is_some() {
            self.last_pi_lazy_inspected_rows = self
                .last_pi_lazy_inspected_rows
                .saturating_add(max_lines.saturating_sub(compare_start));
        }
        for index in compare_start..max_lines {
            let old_line = if let Some(previous_tail) = lazy_previous_tail.as_deref() {
                previous_tail
                    .get(index.saturating_sub(stable_prefix))
                    .map_or("", String::as_str)
            } else {
                self.previous_frame.get(index).map_or("", String::as_str)
            };
            let new_line = new_lines.get(index).map_or("", String::as_str);
            if old_line != new_line {
                first_changed.get_or_insert(index);
                last_changed = Some(index);
            }
        }
        let appended_lines = new_lines.len() > previous_len;
        if appended_lines {
            first_changed.get_or_insert(previous_len);
            last_changed = new_lines.len().checked_sub(1);
        }
        if let (Some(first), Some(last)) = (first_changed, last_changed) {
            let (expanded_first, expanded_last) = if lazy_stable_prefix.is_some() {
                (first, last)
            } else if let Some(previous_tail) = lazy_previous_tail.as_deref() {
                self.pi_expand_changed_range_for_kitty_images(
                    first,
                    last,
                    &new_lines,
                    previous_tail,
                    stable_prefix,
                )
            } else {
                self.pi_expand_changed_range_for_kitty_images(
                    first,
                    last,
                    &new_lines,
                    &self.previous_frame,
                    0,
                )
            };
            first_changed = Some(expanded_first);
            last_changed = Some(expanded_last);
        }
        let append_start = appended_lines
            && first_changed == Some(previous_len)
            && first_changed.is_some_and(|index| index > 0);

        if first_changed.is_none() {
            self.pi_position_hardware_cursor(cursor_position, new_lines.len());
            self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
            self.previous_frame = new_lines;
            self.previous_viewport_top = previous_viewport_top;
            self.previous_size = Some((width_u16, height_u16));
            self.first_render = false;
            return;
        }
        let first_changed = first_changed.expect("checked above");
        let last_changed = last_changed.expect("a changed frame has a last row");

        // All changes are deleted rows. Clear those cells without scrolling
        // unless the target moved above the old viewport, where Pi rebuilds.
        if first_changed >= new_lines.len() {
            if previous_len > new_lines.len() {
                let target_row = new_lines.len().saturating_sub(1);
                if target_row < previous_viewport_top {
                    self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
                    return;
                }
                let extra_lines = previous_len.saturating_sub(new_lines.len());
                if extra_lines > height {
                    self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
                    return;
                }

                let mut buffer = String::from("\x1b[?2026h");
                let deleted_images = if lazy_stable_prefix.is_some() {
                    String::new()
                } else {
                    let previous_tail = lazy_previous_tail.as_deref();
                    previous_tail.map_or_else(
                        || {
                            self.pi_delete_changed_kitty_images(
                                first_changed,
                                last_changed,
                                &self.previous_frame,
                                0,
                            )
                        },
                        |tail| {
                            self.pi_delete_changed_kitty_images(
                                first_changed,
                                last_changed,
                                tail,
                                stable_prefix,
                            )
                        },
                    )
                };
                buffer.push_str(&deleted_images);
                push_vertical_move(
                    &mut buffer,
                    pi_line_difference(
                        hardware_cursor_row,
                        previous_viewport_top,
                        target_row,
                        viewport_top,
                    ),
                );
                buffer.push('\r');
                let clear_start_offset = usize::from(!new_lines.is_empty());
                if extra_lines > 0 && clear_start_offset > 0 {
                    push_cursor_down(&mut buffer, clear_start_offset);
                }
                for index in 0..extra_lines {
                    buffer.push_str("\r\x1b[2K");
                    if index + 1 < extra_lines {
                        push_cursor_down(&mut buffer, 1);
                    }
                }
                let move_back = extra_lines
                    .saturating_sub(1)
                    .saturating_add(clear_start_offset);
                if move_back > 0 {
                    push_cursor_up(&mut buffer, move_back);
                }
                buffer.push_str("\x1b[?2026l");
                self.terminal.write(&buffer);
                self.cursor_row = target_row;
                self.hardware_cursor_row = target_row;
            }
            self.pi_position_hardware_cursor(cursor_position, new_lines.len());
            self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
            self.previous_frame = new_lines;
            self.previous_size = Some((width_u16, height_u16));
            self.previous_viewport_top = previous_viewport_top;
            self.first_render = false;
            return;
        }

        if first_changed < previous_viewport_top {
            self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
            return;
        }

        let mut buffer = String::from("\x1b[?2026h");
        let deleted_images = if lazy_stable_prefix.is_some() {
            String::new()
        } else {
            lazy_previous_tail.as_deref().map_or_else(
                || {
                    self.pi_delete_changed_kitty_images(
                        first_changed,
                        last_changed,
                        &self.previous_frame,
                        0,
                    )
                },
                |tail| {
                    self.pi_delete_changed_kitty_images(
                        first_changed,
                        last_changed,
                        tail,
                        stable_prefix,
                    )
                },
            )
        };
        buffer.push_str(&deleted_images);
        let previous_viewport_bottom = previous_viewport_top.saturating_add(height - 1);
        let move_target_row = if append_start {
            first_changed.saturating_sub(1)
        } else {
            first_changed
        };
        if move_target_row > previous_viewport_bottom {
            let current_screen_row = hardware_cursor_row
                .saturating_sub(previous_viewport_top)
                .min(height - 1);
            let move_to_bottom = height.saturating_sub(1).saturating_sub(current_screen_row);
            if move_to_bottom > 0 {
                push_cursor_down(&mut buffer, move_to_bottom);
            }
            let scroll = move_target_row.saturating_sub(previous_viewport_bottom);
            buffer.push_str(&"\r\n".repeat(scroll));
            previous_viewport_top = previous_viewport_top.saturating_add(scroll);
            viewport_top = viewport_top.saturating_add(scroll);
            hardware_cursor_row = move_target_row;
        }

        push_vertical_move(
            &mut buffer,
            pi_line_difference(
                hardware_cursor_row,
                previous_viewport_top,
                move_target_row,
                viewport_top,
            ),
        );
        buffer.push_str(if append_start { "\r\n" } else { "\r" });

        let render_end = last_changed.min(new_lines.len().saturating_sub(1));
        let mut index = first_changed;
        while index <= render_end {
            if index > first_changed {
                buffer.push_str("\r\n");
            }
            let line = &new_lines[index];
            let image = is_image_line(line);
            let image_reserved_rows = if image {
                self.pi_kitty_image_reserved_rows(&new_lines, index, render_end)
            } else {
                1
            };
            if image_reserved_rows > 1 {
                let image_start_screen_row = index.checked_sub(viewport_top);
                if image_start_screen_row
                    .is_none_or(|row| row.saturating_add(image_reserved_rows) > height)
                {
                    self.pi_full_render(new_lines, width_u16, height_u16, true, cursor_position);
                    return;
                }
                buffer.push_str("\x1b[2K");
                for _ in 1..image_reserved_rows {
                    buffer.push_str("\r\n\x1b[2K");
                }
                push_cursor_up(&mut buffer, image_reserved_rows - 1);
                buffer.push_str(line);
                push_cursor_down(&mut buffer, image_reserved_rows - 1);
                index = index.saturating_add(image_reserved_rows);
                continue;
            }

            buffer.push_str("\x1b[2K");
            if !image && visible_width(line) > width {
                self.stop();
                panic!(
                    "rendered line {index} exceeds terminal width ({} > {width}); components must wrap or truncate to the supplied width",
                    visible_width(line)
                );
            }
            buffer.push_str(line);
            index = index.saturating_add(1);
        }

        let mut final_cursor_row = render_end;
        if previous_len > new_lines.len() {
            if render_end < new_lines.len().saturating_sub(1) {
                let move_down = new_lines.len() - 1 - render_end;
                push_cursor_down(&mut buffer, move_down);
                final_cursor_row = new_lines.len() - 1;
            }
            let extra_lines = previous_len.saturating_sub(new_lines.len());
            for _ in new_lines.len()..previous_len {
                buffer.push_str("\r\n\x1b[2K");
            }
            push_cursor_up(&mut buffer, extra_lines);
        }
        buffer.push_str("\x1b[?2026l");
        self.terminal.write(&buffer);

        self.cursor_row = new_lines.len().saturating_sub(1);
        self.hardware_cursor_row = final_cursor_row;
        self.max_lines_rendered = self.max_lines_rendered.max(new_lines.len());
        self.previous_viewport_top =
            previous_viewport_top.max(final_cursor_row.saturating_sub(height.saturating_sub(1)));
        self.pi_position_hardware_cursor(cursor_position, new_lines.len());
        self.pi_record_kitty_state(&new_lines, lazy_stable_prefix.is_some());
        self.previous_frame = new_lines;
        self.previous_size = Some((width_u16, height_u16));
        self.first_render = false;
    }

    fn pi_full_render(
        &mut self,
        new_lines: Vec<String>,
        width: u16,
        height: u16,
        clear: bool,
        cursor_position: Option<(usize, usize)>,
    ) {
        self.full_redraw_count = self.full_redraw_count.saturating_add(1);
        let height_rows = usize::from(height.max(1));
        let mut buffer = String::from("\x1b[?2026h");
        if clear {
            buffer.push_str(&self.pi_delete_kitty_images(&self.previous_kitty_image_ids));
            buffer.push_str("\x1b[2J\x1b[H\x1b[3J");
        }
        let mut index = 0;
        while index < new_lines.len() {
            if index > 0 {
                buffer.push_str("\r\n");
            }
            let line = &new_lines[index];
            let image_reserved_rows = if is_image_line(line) {
                self.pi_kitty_image_reserved_rows(
                    &new_lines,
                    index,
                    new_lines.len().saturating_sub(1),
                )
            } else {
                1
            };
            if image_reserved_rows > 1 && image_reserved_rows <= height_rows {
                buffer.push_str(&"\r\n".repeat(image_reserved_rows - 1));
                push_cursor_up(&mut buffer, image_reserved_rows - 1);
                buffer.push_str(line);
                push_cursor_down(&mut buffer, image_reserved_rows - 1);
                index = index.saturating_add(image_reserved_rows);
                continue;
            }
            buffer.push_str(line);
            index = index.saturating_add(1);
        }
        buffer.push_str("\x1b[?2026l");
        self.terminal.write(&buffer);

        self.cursor_row = new_lines.len().saturating_sub(1);
        self.hardware_cursor_row = self.cursor_row;
        self.max_lines_rendered = if clear {
            new_lines.len()
        } else {
            self.max_lines_rendered.max(new_lines.len())
        };
        let buffer_length = height_rows.max(new_lines.len());
        self.previous_viewport_top = buffer_length.saturating_sub(height_rows);
        self.pi_position_hardware_cursor(cursor_position, new_lines.len());
        self.pi_record_kitty_state(&new_lines, false);
        self.previous_frame = new_lines;
        self.previous_size = Some((width, height));
        self.first_render = false;
    }

    fn pi_position_hardware_cursor(
        &mut self,
        cursor_position: Option<(usize, usize)>,
        total_lines: usize,
    ) {
        let Some((row, column)) = cursor_position.filter(|_| total_lines > 0) else {
            self.terminal.hide_cursor();
            return;
        };
        let target_row = row.min(total_lines.saturating_sub(1));
        let mut buffer = String::new();
        push_vertical_move(
            &mut buffer,
            signed_difference(target_row, self.hardware_cursor_row),
        );
        buffer.push_str(&format!("\x1b[{}G", column.saturating_add(1)));
        self.terminal.write(&buffer);
        self.hardware_cursor_row = target_row;
        if self.show_hardware_cursor {
            self.terminal.show_cursor();
        } else {
            self.terminal.hide_cursor();
        }
    }

    fn pi_collect_kitty_image_ids(lines: &[String]) -> BTreeSet<u32> {
        lines
            .iter()
            .flat_map(|line| extract_kitty_image_ids(line))
            .collect()
    }

    fn pi_record_kitty_state(&mut self, lines: &[String], known_image_free: bool) {
        if known_image_free {
            self.previous_frame_has_kitty = false;
            self.previous_kitty_image_ids.clear();
            return;
        }
        self.previous_frame_has_kitty = lines.iter().any(|line| is_image_line(line));
        self.previous_kitty_image_ids = Self::pi_collect_kitty_image_ids(lines);
    }

    fn pi_delete_kitty_images(&self, ids: &BTreeSet<u32>) -> String {
        ids.iter()
            .map(|image_id| delete_kitty_image(*image_id))
            .collect()
    }

    fn pi_kitty_image_reserved_rows(
        &self,
        lines: &[String],
        index: usize,
        max_index: usize,
    ) -> usize {
        let rows = lines
            .get(index)
            .map_or(1, |line| extract_kitty_image_rows(line));
        if rows <= 1 {
            return 1;
        }
        let max_rows = rows
            .min(max_index.saturating_sub(index).saturating_add(1))
            .min(lines.len().saturating_sub(index));
        let mut reserved_rows = 1;
        while reserved_rows < max_rows {
            let line = lines
                .get(index.saturating_add(reserved_rows))
                .map_or("", String::as_str);
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows = reserved_rows.saturating_add(1);
        }
        reserved_rows
    }

    fn pi_expand_changed_range_for_kitty_images(
        &self,
        first_changed: usize,
        last_changed: usize,
        new_lines: &[String],
        previous_lines: &[String],
        previous_offset: usize,
    ) -> (usize, usize) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        for (index, line) in previous_lines.iter().enumerate() {
            if extract_kitty_image_ids(line).is_empty() {
                continue;
            }
            let block_end = previous_offset
                .saturating_add(index)
                .saturating_add(self.pi_kitty_image_reserved_rows(
                    previous_lines,
                    index,
                    previous_lines.len().saturating_sub(1),
                ))
                .saturating_sub(1);
            let absolute_index = previous_offset.saturating_add(index);
            if absolute_index >= first_changed
                || (absolute_index <= last_changed && block_end >= first_changed)
            {
                expanded_first = expanded_first.min(absolute_index);
                expanded_last = expanded_last.max(block_end);
            }
        }
        for index in 0..new_lines.len() {
            if extract_kitty_image_ids(&new_lines[index]).is_empty() {
                continue;
            }
            let block_end = index
                .saturating_add(self.pi_kitty_image_reserved_rows(
                    new_lines,
                    index,
                    new_lines.len().saturating_sub(1),
                ))
                .saturating_sub(1);
            if index >= first_changed || (index <= last_changed && block_end >= first_changed) {
                expanded_first = expanded_first.min(index);
                expanded_last = expanded_last.max(block_end);
            }
        }
        (expanded_first, expanded_last)
    }

    fn pi_delete_changed_kitty_images(
        &self,
        first_changed: usize,
        last_changed: usize,
        previous_lines: &[String],
        previous_offset: usize,
    ) -> String {
        if last_changed < first_changed || previous_lines.is_empty() {
            return String::new();
        }
        let first = first_changed
            .max(previous_offset)
            .saturating_sub(previous_offset);
        let last = last_changed
            .saturating_sub(previous_offset)
            .min(previous_lines.len().saturating_sub(1));
        if first > last {
            return String::new();
        }
        let mut ids = BTreeSet::new();
        for line in &previous_lines[first..=last] {
            ids.extend(extract_kitty_image_ids(line));
        }
        self.pi_delete_kitty_images(&ids)
    }

    fn render_extended_frame(&mut self) {
        let width = self.terminal.columns();
        let height = self.terminal.rows();
        let size_changed = self
            .previous_size
            .is_some_and(|size| size != (width, height));
        let width_changed = self
            .previous_size
            .is_some_and(|(previous_width, _)| previous_width != width);

        let reset_scrollback_on_resize = size_changed
            && self.inline_scrollback
            && self.capabilities.cursor_addressing
            && self.capabilities.line_clearing;
        let render_commit_cursor = if reset_scrollback_on_resize {
            // The old cursor described history that the resize reset replaces.
            // Ask the component for a fresh boundary in the new layout.
            None
        } else {
            self.inline_commit_cursor
        };
        let lazy_update = (self.capabilities.plain
            || (self.inline_scrollback
                && self.capabilities.cursor_addressing
                && self.capabilities.line_clearing))
            .then(|| self.root_render_update(width, render_commit_cursor))
            .flatten();
        let previous_len = self.previous_frame.len();
        // A prefix beyond the retained frame cannot be validated. Fall back to
        // the component's full renderer rather than pairing its replacement
        // with the wrong historic rows. Width reflow likewise requires a full
        // replacement, but retaining a zero-prefix update preserves pinned
        // viewport metadata across both width and height changes.
        let mut lazy_update = lazy_update.filter(|update| {
            update.stable_prefix <= previous_len && (!width_changed || update.stable_prefix == 0)
        });
        let resize_replay = lazy_update
            .as_mut()
            .and_then(|update| update.resize_replay.take())
            .map(|mut lines| {
                let _ = extract_cursor_position(&mut lines, width, height);
                let mut lines = lines
                    .into_iter()
                    .map(|line| self.prepare_line(line, width))
                    .collect::<Vec<_>>();
                if !self.capabilities.plain {
                    for line in &mut lines {
                        line.push_str("\x1b[0m\x1b]8;;\x1b\\");
                    }
                }
                lines
            });
        let reanchor_viewport = lazy_update
            .as_ref()
            .is_some_and(|update| update.reanchor_viewport);
        let rebuild_scrollback = lazy_update
            .as_ref()
            .is_some_and(|update| update.rebuild_scrollback);
        // Kitty placements only need a full-frame presence check when a
        // destructive inline replay may erase them. Ordinary differential
        // frames inspect just the rows they repaint below.
        let previous_frame_has_image = (reset_scrollback_on_resize || rebuild_scrollback)
            && self.previous_frame.iter().any(|line| is_image_line(line));
        let pinned = lazy_update.as_ref().and_then(|update| update.pinned);
        // Lazy frame assembly reuses `previous_frame` with `mem::take` below.
        // Preserve only the old physical viewport needed by pinned diffing;
        // cloning the complete retained transcript would defeat lazy updates.
        let pinned_previous_window = pinned.map_or_else(Vec::new, |_| {
            let rows = usize::from(height.max(1));
            (0..rows)
                .map(|screen_row| {
                    let logical_row = self.inline_window_top.saturating_add(screen_row);
                    if logical_row < self.inline_history_rows {
                        String::new()
                    } else {
                        self.previous_frame
                            .get(logical_row)
                            .cloned()
                            .unwrap_or_default()
                    }
                })
                .collect()
        });
        let mut first_changed_hint = None;
        let mut lazy_change_hints = None;
        let cursor;

        let new_lines: Vec<String> = if let Some(update) = lazy_update {
            let stable_prefix = update.stable_prefix.min(previous_len);
            let mut replacement = update.replacement;
            let total_len = stable_prefix.saturating_add(replacement.len());
            cursor = extract_cursor_position_from(
                &mut replacement,
                stable_prefix,
                total_len,
                width,
                height,
            );
            let mut replacement = replacement
                .into_iter()
                .map(|line| self.prepare_line(line, width))
                .collect::<Vec<_>>();
            if !self.capabilities.plain {
                for line in &mut replacement {
                    line.push_str("\x1b[0m\x1b]8;;\x1b\\");
                }
            }
            let hints = frame_change_hints(&self.previous_frame, stable_prefix, &replacement);
            first_changed_hint = Some(hints.first_changed);
            lazy_change_hints = Some(hints);

            // Reuse the committed prefix in place. No historic String is
            // cloned and no committed row is compared on an active-run tick.
            let mut reused = std::mem::take(&mut self.previous_frame);
            reused.truncate(stable_prefix);
            reused.extend(replacement);
            reused
        } else {
            let mut rendered = self.root_render(width);
            // Extract the typed cursor marker before clipping or sanitizing the
            // line. It is a trusted library control token, never accepted from
            // semantic text.
            cursor = extract_cursor_position(&mut rendered, width, height);

            // Prepare the children in order. Plain/log mode is escape-free and
            // does not right-pad every row with terminal-width spaces. Inline
            // scrollback also skips padding: every repaint erases before
            // writing, and padded rows would put trailing spaces into native
            // text selection.
            rendered = rendered
                .into_iter()
                .map(|line| self.prepare_line(line, width))
                .collect();

            // Apply per-line resets only in terminal-control mode. Plain/log
            // backends receive escape-free chronological output.
            if !self.capabilities.plain {
                rendered = rendered
                    .into_iter()
                    .map(|line| format!("{}\x1b[0m\x1b]8;;\x1b\\", line))
                    .collect();
            }
            rendered
        };

        // Cursor movement caused by frame writes must not become visible before
        // the final hardware-cursor address. This is especially noticeable as
        // a transient hollow cursor in the terminal's bottom-right cell.
        self.begin_synchronized_output();

        // A terminal reflows the old grid and saved lines before delivering
        // its resize event. Rebuilding octet-owned history below avoids trying to
        // repair terminal-dependent physical rows after that reflow.
        if self.capabilities.plain {
            self.write_plain_changes(&new_lines, first_changed_hint, previous_len);
            self.first_render = false;
        } else if self.inline_scrollback
            && self.capabilities.cursor_addressing
            && self.capabilities.line_clearing
        {
            self.write_inline_changes(
                &new_lines,
                height,
                size_changed,
                reanchor_viewport,
                rebuild_scrollback,
                pinned,
                &pinned_previous_window,
                first_changed_hint,
                previous_len,
                previous_frame_has_image,
                lazy_change_hints.as_ref(),
                resize_replay.as_deref(),
            );
            self.first_render = false;
        } else if self.first_render {
            if self.capabilities.cursor_addressing {
                self.terminal.write("\x1b[H");
            }
            self.write_all_lines(&new_lines);
            self.first_render = false;
        } else if previous_len == 0 {
            self.write_all_lines(&new_lines);
        } else if size_changed {
            self.redraw_all_from_home(&new_lines);
        } else {
            // Strategy 3: update only the changed tail. This handles pure
            // append, replacement, shrink, and empty frames.
            let first_changed = first_changed_hint.unwrap_or_else(|| {
                self.previous_frame
                    .iter()
                    .zip(&new_lines)
                    .position(|(prev, new)| prev != new)
                    .unwrap_or(previous_len.min(new_lines.len()))
            });

            let old_viewport_start = previous_len.saturating_sub(usize::from(height));
            let new_viewport_start = new_lines.len().saturating_sub(usize::from(height));
            let viewport_shifted = old_viewport_start != new_viewport_start;
            if !self.capabilities.cursor_addressing || !self.capabilities.line_clearing {
                // A styled but non-addressable backend behaves like an append-only
                // log: never emit cursor/erase controls it did not advertise.
                self.write_all_lines(&new_lines);
            } else if (first_changed == 0 && previous_len != new_lines.len())
                || viewport_shifted
                || first_changed < new_viewport_start
            {
                self.redraw_all_from_home(&new_lines);
            } else if first_changed < previous_len || first_changed < new_lines.len() {
                self.begin_synchronized_output();
                let screen_row = first_changed.saturating_sub(new_viewport_start);
                self.terminal
                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                self.terminal.clear_from_cursor();
                let changed = &new_lines[first_changed..];
                for (index, line) in changed.iter().enumerate() {
                    self.terminal.write(line);
                    // A newline after the terminal's bottom row scrolls the
                    // alternate screen and invalidates every absolute row in
                    // the retained frame. Cursor-addressed updates do not need
                    // a trailing newline after their final row.
                    if index + 1 < changed.len() {
                        self.terminal.write("\n");
                    }
                }
                if new_lines.len() < previous_len {
                    self.terminal.clear_from_cursor();
                }
                self.end_synchronized_output();
            }
        }

        if let Some((row, column)) = cursor.filter(|_| self.capabilities.cursor_addressing) {
            let row = if self.inline_scrollback && !self.capabilities.plain {
                // Re-anchor from the bottom-aligned viewport model to the
                // frame's true on-screen bottom row (a shrink can leave the
                // tail above the screen's last row).
                let viewport_start = new_lines.len().saturating_sub(usize::from(height));
                let logical = usize::from(row) + viewport_start;
                let from_end = new_lines.len().saturating_sub(1).saturating_sub(logical);
                self.inline_bottom_row.saturating_sub(from_end) as u16
            } else {
                row
            };
            self.terminal.write(&format!(
                "\x1b[{};{}H",
                row.saturating_add(1),
                column.saturating_add(1)
            ));
            self.terminal.show_cursor();
        } else if self.capabilities.cursor_addressing {
            self.terminal.hide_cursor();
        }
        self.end_synchronized_output();
        self.previous_frame = new_lines;
        self.previous_size = Some((width, height));
    }

    /// Differential update against the primary screen. Logical rows above the
    /// visible region are never repainted; rows appended after first paint can
    /// enter native scrollback when a bottom-row newline scrolls naturally.
    /// `inline_bottom_row` anchors all cursor addressing because a frame shrink
    /// leaves the tail above the bottom row (the screen cannot scroll back down).
    #[allow(clippy::too_many_arguments)]
    fn write_inline_changes(
        &mut self,
        new_lines: &[String],
        height: u16,
        size_changed: bool,
        reanchor_viewport: bool,
        rebuild_scrollback: bool,
        pinned: Option<PinnedFrame>,
        pinned_previous_window: &[String],
        first_changed_hint: Option<usize>,
        previous_len: usize,
        previous_frame_has_image: bool,
        frame_change_hints: Option<&FrameChangeHints>,
        resize_replay: Option<&[String]>,
    ) {
        let rows = usize::from(height.max(1));
        if size_changed && !self.first_render {
            let displayed_window_top = new_lines.len().saturating_sub(rows);
            let retained_generation = self
                .inline_generation
                .or_else(|| self.inline_commit_cursor.map(|cursor| cursor.generation));
            let generation_continues = pinned.is_none_or(|frame| {
                retained_generation.is_none_or(|generation| generation == frame.generation)
            });
            let preserve_native_history = pinned.filter(|frame| {
                generation_continues
                    && !frame.viewport_surface
                    && frame.stable_rows >= displayed_window_top
                    && resize_replay.is_none()
                    && !rebuild_scrollback
                    && !previous_frame_has_image
                    && !new_lines.iter().any(|line| is_image_line(line))
            });
            if let Some(pinned) = preserve_native_history {
                // The terminal has already reflowed its grid and saved lines.
                // Treat that reflowed prefix as the new physical history seam
                // and repaint only one complete visible grid. Replaying the
                // application tape here duplicates history in multiplexers and
                // makes resize cost proportional to the whole conversation.
                self.inline_history_rows = displayed_window_top;
                self.inline_committed_rows = 0;
                self.inline_commit_cursor = None;
                self.inline_generation = Some(pinned.generation);
                self.inline_window_top = displayed_window_top;
                self.inline_surface_active = false;
                self.inline_surface_window.clear();
                self.write_inline_pinned(
                    new_lines,
                    rows,
                    pinned,
                    true,
                    false,
                    pinned_previous_window,
                );
                return;
            }

            let reanchor_replacement_timeline = pinned.filter(|frame| {
                !generation_continues
                    && !frame.viewport_surface
                    && resize_replay.is_none()
                    && !previous_frame_has_image
                    && !new_lines.iter().any(|line| is_image_line(line))
            });
            if let Some(pinned) = reanchor_replacement_timeline {
                // The terminal may reflow its old saved lines, but they belong
                // to another semantic tape. Preserve that native history while
                // starting the replacement generation at row zero and repainting
                // its live grid; never claim the old off-screen prefix as new
                // history and never clear scrollback merely because resize and
                // timeline replacement arrived in the same frame.
                self.write_inline_pinned(
                    new_lines,
                    rows,
                    pinned,
                    true,
                    rebuild_scrollback,
                    pinned_previous_window,
                );
                return;
            }

            // A temporary surface or Kitty placement cannot be reconstructed
            // safely from terminal reflow alone. Keep the destructive replay
            // fallback for those bounded exceptional paths.
            // Modern terminals reflow both the grid and saved lines before the
            // application observes a resize. Physical-row repair cannot be
            // made terminal-independent, so discard that presentation and
            // replay the complete application-owned tape. A temporary screen
            // surface may provide the unobscured tape, then repaint its visible
            // frame without scrolling any additional rows.
            let displayed_window_top = new_lines.len().saturating_sub(rows);
            let replay = resize_replay.filter(|replay| {
                replay.len().saturating_sub(rows) == displayed_window_top
                    && !replay
                        .iter()
                        .chain(new_lines)
                        .any(|line| is_image_line(line))
            });
            let pinned_surface = pinned.is_some_and(|frame| frame.viewport_surface);
            self.reset_inline_scrollback(
                replay.unwrap_or(new_lines),
                rows,
                previous_frame_has_image,
            );
            self.inline_generation = pinned.map(|frame| frame.generation);
            if replay.is_some() {
                self.terminal.write("\x1b[H");
                let visible = &new_lines[displayed_window_top..];
                for (index, line) in visible.iter().enumerate() {
                    self.terminal.clear_line();
                    self.terminal.write(line);
                    if index + 1 < visible.len() {
                        self.terminal.write("\n");
                    }
                }
                for row in visible.len()..rows {
                    self.terminal
                        .write(&format!("\x1b[{};1H", row.saturating_add(1)));
                    self.terminal.clear_line();
                }
                self.inline_history_rows = displayed_window_top;
                self.inline_window_top = displayed_window_top;
                self.inline_bottom_row = visible.len().saturating_sub(1);
            }
            if pinned_surface {
                let visible = &new_lines[displayed_window_top..];
                self.inline_surface_window = visible.to_vec();
                self.inline_surface_window.resize(rows, String::new());
                self.inline_surface_active = true;
                self.inline_bottom_row = visible.len().saturating_sub(1);
            }
            return;
        }
        if rebuild_scrollback && !self.first_render && pinned.is_none() {
            // Generic inline frames have no semantic commit boundary, so a
            // disclosure rebuild must replace their complete presentation.
            self.reset_inline_scrollback(new_lines, rows, previous_frame_has_image);
            return;
        }
        if let Some(pinned) = pinned {
            self.write_inline_pinned(
                new_lines,
                rows,
                pinned,
                reanchor_viewport || rebuild_scrollback,
                rebuild_scrollback,
                pinned_previous_window,
            );
            return;
        }
        if self.first_render {
            // Push the caller's existing screen content into scrollback
            // instead of erasing it, then paint the visible tail from home.
            // The complete logical frame remains retained for differential
            // updates, but restoring a large session must not synchronously
            // stream megabytes of off-screen history through the PTY before
            // the composer becomes usable.
            self.terminal.write(&"\n".repeat(rows));
            self.terminal.write("\x1b[H");
            self.terminal.clear_screen();
            self.terminal.write("\x1b[H");
            let visible = &new_lines[new_lines.len().saturating_sub(rows)..];
            self.write_all_lines(visible);
            self.inline_bottom_row = visible.len().saturating_sub(1);
            return;
        }

        let prev_len = previous_len;
        // Frame lines currently on screen span [visible_start, prev_len).
        let visible_start = prev_len.saturating_sub(self.inline_bottom_row + 1);
        let removed_history = !reanchor_viewport && new_lines.len() <= visible_start;
        if removed_history {
            // A generic frame has no semantic cursor with which to prove that
            // rows already in native history still belong to the new frame. A
            // shrink that removes that prefix therefore needs destructive
            // reconciliation rather than a tail repaint.
            let has_image = previous_frame_has_image
                || self.previous_frame.iter().any(|line| is_image_line(line))
                || new_lines.iter().any(|line| is_image_line(line));
            self.reset_inline_scrollback(new_lines, rows, has_image);
            return;
        }
        if reanchor_viewport || prev_len == 0 {
            // Reflow or an explicit logical-timeline replacement invalidates
            // every row assumption. Repaint the visible tail from home;
            // replacement timelines intentionally leave the old session's
            // native history reachable.
            self.begin_synchronized_output();
            let erased_has_image = frame_change_hints.map_or_else(
                || {
                    self.previous_frame[visible_start..]
                        .iter()
                        .any(|line| is_image_line(line))
                },
                |hints| hints.affected_tail_has_image,
            );
            if erased_has_image {
                // Erasing text cells does not remove Kitty placements. The
                // complete new visible tail is painted below, so a global
                // delete cannot strand any unchanged on-screen image.
                self.terminal.write(&delete_all_kitty_images());
            }
            self.terminal.write("\x1b[H");
            let start = new_lines.len().saturating_sub(rows);
            let visible = &new_lines[start..];
            // ED 2 is not history-neutral in multiplexers such as tmux: cells
            // erased from the grid are retained as native scrollback. Erase
            // each physical row instead so a transient overlay, resize, or
            // timeline reanchor cannot commit mutable chrome.
            for (index, line) in visible.iter().enumerate() {
                self.terminal.clear_line();
                self.terminal.write(line);
                if index + 1 < visible.len() {
                    self.terminal.write("\n");
                }
            }
            for row in visible.len()..rows {
                self.terminal
                    .write(&format!("\x1b[{};1H", row.saturating_add(1)));
                self.terminal.clear_line();
            }
            self.end_synchronized_output();
            self.inline_bottom_row = visible.len().saturating_sub(1);
            return;
        }

        let first_changed = first_changed_hint.unwrap_or_else(|| {
            self.previous_frame
                .iter()
                .zip(new_lines)
                .position(|(prev, new)| prev != new)
                .unwrap_or(prev_len.min(new_lines.len()))
        });
        if first_changed >= prev_len && new_lines.len() == prev_len {
            return;
        }

        if first_changed < visible_start {
            // Rows already owned by native scrollback cannot be edited. Do not
            // clear and replay the retained timeline here: multiplexers may
            // preserve the old history and append that replay, while terminals
            // without synchronized paint expose it as a full-screen flash.
            // Align the old and new visible tails instead and repaint only the
            // physical rows whose final cells differ. Off-screen history keeps
            // the version that was committed when it originally scrolled out.
            self.repaint_inline_visible_rows(new_lines, rows);
            return;
        }

        let mut delete_images_before_repaint = false;

        // A fixed-height frame can change in the middle when an application
        // replaces elastic viewport padding with a newly arrived event. Repaint
        // only the changed rows in that case: clearing the entire tail would
        // needlessly erase and redraw pinned composer/footer rows and visibly
        // flickers on terminals without synchronized-output support.
        if new_lines.len() == prev_len {
            let fixed_height_hint =
                frame_change_hints.and_then(|hints| hints.fixed_height.as_ref());
            let last_changed = fixed_height_hint.map_or_else(
                || {
                    self.previous_frame
                        .iter()
                        .zip(new_lines)
                        .rposition(|(previous, next)| previous != next)
                },
                |hints| hints.last_changed,
            );
            if let Some(last_changed) = last_changed {
                let repaint_from = first_changed.max(visible_start);
                if repaint_from > last_changed {
                    return;
                }
                let changed_has_image = fixed_height_hint.map_or_else(
                    || {
                        (repaint_from..=last_changed).any(|index| {
                            self.previous_frame[index] != new_lines[index]
                                && (is_image_line(&self.previous_frame[index])
                                    || is_image_line(&new_lines[index]))
                        })
                    },
                    |hints| {
                        hints
                            .image_rows
                            .iter()
                            .any(|row| *row >= repaint_from && *row <= last_changed)
                    },
                );
                if !changed_has_image {
                    self.begin_synchronized_output();
                    if let Some(hints) = fixed_height_hint {
                        for &index in hints
                            .changed_rows
                            .iter()
                            .filter(|row| **row >= repaint_from && **row <= last_changed)
                        {
                            let from_end = prev_len.saturating_sub(1).saturating_sub(index);
                            let screen_row = self.inline_bottom_row.saturating_sub(from_end);
                            self.terminal
                                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                            self.terminal.clear_line();
                            self.terminal.write(&new_lines[index]);
                        }
                    } else {
                        let mut index = repaint_from;
                        while index <= last_changed {
                            if self.previous_frame[index] != new_lines[index] {
                                let from_end = prev_len.saturating_sub(1).saturating_sub(index);
                                let screen_row = self.inline_bottom_row.saturating_sub(from_end);
                                self.terminal
                                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                                self.terminal.clear_line();
                                self.terminal.write(&new_lines[index]);
                            }
                            index = index.saturating_add(1);
                        }
                    }
                    self.end_synchronized_output();
                    return;
                }
                // Text erase controls do not remove Kitty graphics
                // placements. Delete them before the generic tail redraw and
                // repaint the complete visible viewport so unchanged images
                // removed by the global delete are restored as well.
                delete_images_before_repaint = true;
            }
        } else {
            // Length changes clear and rewrite the affected tail. Kitty image
            // placements survive those text controls, and retransmitting an
            // affected image without first deleting it can also leave stacked
            // placements. Repaint the complete visible viewport after a
            // global delete so unchanged visible images are restored too.
            let affected_from = first_changed
                .min(prev_len.saturating_sub(1))
                .max(visible_start);
            delete_images_before_repaint = frame_change_hints.map_or_else(
                || {
                    let affected_old_has_image = self.previous_frame[affected_from..]
                        .iter()
                        .any(|line| is_image_line(line));
                    let affected_new_has_image = new_lines[affected_from.min(new_lines.len())..]
                        .iter()
                        .any(|line| is_image_line(line));
                    affected_old_has_image || affected_new_has_image
                },
                |hints| hints.affected_tail_has_image,
            );
        }

        // Start at or before the last existing line so appends write a
        // newline from the current tail (scrolling as needed) rather than
        // addressing a row past the screen. A change above the visible
        // region cannot be painted (those rows are scrollback); clamp and
        // accept the stale history.
        let repaint_from = if delete_images_before_repaint {
            visible_start
        } else {
            first_changed
                .min(prev_len.saturating_sub(1))
                .max(visible_start)
        };
        let screen_row = self.inline_bottom_row - (prev_len - 1 - repaint_from);
        self.begin_synchronized_output();
        if delete_images_before_repaint {
            self.terminal.write(&delete_all_kitty_images());
        }
        self.terminal
            .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
        self.terminal.clear_from_cursor();
        let changed = &new_lines[repaint_from.min(new_lines.len())..];
        for (index, line) in changed.iter().enumerate() {
            self.terminal.write(line);
            if index + 1 < changed.len() {
                self.terminal.write("\n");
            }
        }
        self.end_synchronized_output();
        self.inline_bottom_row = if changed.is_empty() {
            screen_row.saturating_sub(1)
        } else {
            (screen_row + changed.len() - 1).min(rows - 1)
        };
    }

    /// Append-only native-scrollback renderer for a frame with semantic commit
    /// points. The acknowledged cursor is remapped into the current width, so
    /// no physical row coordinate survives terminal reflow. The mutable grid
    /// always starts at or after the independently tracked physical-history
    /// seam.
    fn write_inline_pinned(
        &mut self,
        new_lines: &[String],
        rows: usize,
        pinned: PinnedFrame,
        mut reanchor: bool,
        atomic_presentation_rebuild: bool,
        previous_window: &[String],
    ) {
        let desired_window_top = new_lines.len().saturating_sub(rows);
        let same_generation = self
            .inline_generation
            .or_else(|| self.inline_commit_cursor.map(|cursor| cursor.generation))
            .is_none_or(|generation| generation == pinned.generation);
        if !same_generation {
            // A replacement timeline starts a new append-only tape after the
            // old terminal-owned history. Its row zero is unrelated to the old
            // cursor, but the old history itself remains untouched.
            self.inline_commit_cursor = None;
            self.inline_history_rows = 0;
            self.inline_committed_rows = 0;
            self.inline_surface_active = false;
            self.inline_surface_window.clear();
            reanchor = true;
        }

        let acknowledged = self.inline_commit_cursor.and_then(|cursor| {
            pinned
                .acknowledged
                .filter(|position| position.cursor == cursor)
        });
        let cursor_unmapped = self.inline_commit_cursor.is_some() && acknowledged.is_none();
        debug_assert!(
            !cursor_unmapped,
            "component did not map the retained semantic commit cursor"
        );

        // The semantic cursor can lag a large finalized block while immutable
        // physical rows from that block move into history one at a time.
        let prior_history_rows = self
            .inline_history_rows
            .max(acknowledged.map_or(0, |position| position.row.min(new_lines.len())));

        // Temporary chrome and reports are physical-screen surfaces, not new
        // transcript tape. An ordinary streaming frame can also contract after
        // Markdown reparses. In either case, advancing or repainting before the
        // monotonic history seam would either commit chrome, duplicate history,
        // or punch blank rows into the live grid. Keep the append ledger frozen
        // and repaint the complete visible tail in place. Explicit presentation
        // rebuilds still honor their atomic semantic commit boundary below.
        if pinned.viewport_surface
            || (!atomic_presentation_rebuild && desired_window_top < prior_history_rows)
        {
            self.write_inline_viewport_surface(new_lines, rows, previous_window);
            self.inline_generation = Some(pinned.generation);
            return;
        }
        if self.inline_surface_active {
            reanchor = true;
        }

        let mut commit_row = acknowledged
            .map(|position| position.row.min(new_lines.len()))
            .unwrap_or_else(|| {
                if cursor_unmapped {
                    reanchor = true;
                    self.inline_committed_rows.min(new_lines.len())
                } else {
                    0
                }
            });
        let mut commit_cursor = self.inline_commit_cursor;
        let target = if cursor_unmapped { None } else { pinned.target }.filter(|target| {
            target.cursor.generation == pinned.generation
                && target.row >= commit_row
                && target.row <= desired_window_top
                && commit_cursor.is_none_or(|cursor| target.cursor > cursor)
        });

        // Stable rows may cross the seam incrementally. A semantic target is
        // also safe to stage once its complete boundary is above the live
        // viewport, even when rows inside that block are disclosure-sensitive.
        let append_limit = target.map_or(pinned.stable_rows, |target| {
            pinned.stable_rows.max(target.row)
        });
        let stable_rows = if cursor_unmapped {
            prior_history_rows
        } else {
            append_limit
                .max(acknowledged.map_or(0, |position| position.row))
                .min(desired_window_top)
                .max(prior_history_rows)
        };
        let append_start = prior_history_rows.min(new_lines.len());
        let append_end = stable_rows.min(new_lines.len());
        let appended = &new_lines[append_start..append_end];
        let history_rows = prior_history_rows.max(append_end);

        // Advance semantic identity only after its complete boundary is known
        // to be in physical history. A resize replay may already have put that
        // boundary there without an append in this frame.
        if let Some(target) = target.filter(|target| target.row <= history_rows) {
            commit_row = target.row;
            commit_cursor = Some(target.cursor);
        }

        // Ordinary streaming retreats use the temporary-surface path above.
        // An explicit semantic presentation rebuild may still contract behind
        // its atomic history boundary; those terminal-owned rows stay blank in
        // the live grid rather than being duplicated.
        let window_top = desired_window_top;
        let window_line = |screen_row: usize| {
            let logical_row = window_top.saturating_add(screen_row);
            (logical_row >= history_rows)
                .then(|| new_lines.get(logical_row))
                .flatten()
                .map(String::as_str)
                .unwrap_or("")
        };

        // When the old grid begins exactly at the physical history seam, its
        // stable top rows can enter scrollback with bottom-row newlines. This
        // is the terminal's native append operation: it preserves a reader's
        // scrollback anchor and avoids repainting the whole live grid.
        let can_scroll_naturally = !appended.is_empty()
            && !self.first_render
            && !reanchor
            && self.inline_window_top == prior_history_rows
            && appended.len() <= rows
            && previous_window.get(..appended.len()) == Some(appended);

        self.begin_synchronized_output();
        if self.first_render {
            // Preserve whatever preceded the application, then establish a
            // clean grid without erasing terminal-owned history.
            self.terminal.write(&"\n".repeat(rows));
        }

        if self.first_render || reanchor || (!appended.is_empty() && !can_scroll_naturally) {
            self.terminal.write("\x1b[H");
            if self.first_render {
                self.terminal.clear_screen();
                self.terminal.write("\x1b[H");
            }

            // A reanchor or a previously compressed mutable window may not
            // contain the rows now becoming stable. Stage that semantic chunk
            // above one complete grid so only the staged rows scroll out.
            let paint_len = appended.len().saturating_add(rows);
            for index in 0..paint_len {
                let line = if index < appended.len() {
                    appended[index].as_str()
                } else {
                    window_line(index - appended.len())
                };
                self.terminal.clear_line();
                self.terminal.write(line);
                if index + 1 < paint_len {
                    self.terminal.write("\r\n");
                }
            }
        } else {
            let shifted_rows = if can_scroll_naturally {
                // Address the live grid's bottom row before emitting newlines;
                // cursor placement from the prior differential frame is not a
                // reliable scroll origin.
                self.terminal
                    .write(&format!("\x1b[{rows};1H{}", "\r\n".repeat(appended.len())));
                appended.len()
            } else {
                0
            };

            // Compare against the grid after any natural scroll. Pure appends
            // now repaint only newly exposed bottom rows instead of replaying
            // every physical row at a shifted logical index.
            for screen_row in 0..rows {
                let previous = previous_window
                    .get(screen_row.saturating_add(shifted_rows))
                    .map(String::as_str)
                    .unwrap_or("");
                let next = window_line(screen_row);
                if previous == next {
                    continue;
                }
                self.terminal
                    .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
                self.terminal.clear_line();
                self.terminal.write(next);
            }
        }
        self.end_synchronized_output();

        self.inline_history_rows = history_rows;
        self.inline_committed_rows = commit_row;
        self.inline_commit_cursor = commit_cursor;
        self.inline_generation = Some(pinned.generation);
        self.inline_window_top = window_top;
        self.inline_surface_active = false;
        self.inline_surface_window.clear();
        self.inline_bottom_row = new_lines
            .len()
            .saturating_sub(window_top)
            .saturating_sub(1)
            .min(rows.saturating_sub(1));
    }

    fn write_inline_viewport_surface(
        &mut self,
        new_lines: &[String],
        rows: usize,
        previous_window: &[String],
    ) {
        let visible = &new_lines[new_lines.len().saturating_sub(rows)..];
        let visible_len = visible.len();
        let mut next_window = visible.to_vec();
        next_window.resize(rows, String::new());

        let previous = if self.inline_surface_active {
            self.inline_surface_window.clone()
        } else {
            previous_window.to_vec()
        };
        let delete_images = previous
            .iter()
            .zip(&next_window)
            .any(|(old, new)| old != new && (is_image_line(old) || is_image_line(new)))
            || previous
                .get(next_window.len()..)
                .is_some_and(|tail| tail.iter().any(|line| is_image_line(line)));
        let repaint_all = self.first_render || !self.inline_surface_active || delete_images;

        self.begin_synchronized_output();
        if self.first_render {
            // Preserve content that preceded the application, then establish a
            // clean primary-screen grid without committing any surface rows.
            self.terminal.write(&"\n".repeat(rows));
            self.terminal.write("\x1b[H");
            self.terminal.clear_screen();
            self.terminal.write("\x1b[H");
        }
        if delete_images {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (screen_row, next) in next_window.iter().enumerate() {
            if !repaint_all && previous.get(screen_row) == Some(next) {
                continue;
            }
            self.terminal
                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
            self.terminal.clear_line();
            self.terminal.write(next);
        }
        self.end_synchronized_output();

        self.inline_surface_active = true;
        self.inline_surface_window = next_window;
        self.inline_bottom_row = visible_len.saturating_sub(1).min(rows.saturating_sub(1));
    }

    fn repaint_inline_visible_rows(&mut self, new_lines: &[String], rows: usize) {
        let visible_rows = (self.inline_bottom_row + 1)
            .min(rows)
            .min(self.previous_frame.len())
            .min(new_lines.len());
        if visible_rows == 0 {
            return;
        }
        let previous_start = self.previous_frame.len() - visible_rows;
        let next_start = new_lines.len() - visible_rows;
        let previous = &self.previous_frame[previous_start..];
        let next = &new_lines[next_start..];
        let delete_images = previous
            .iter()
            .zip(next)
            .any(|(old, new)| old != new && (is_image_line(old) || is_image_line(new)));
        let changed = previous
            .iter()
            .zip(next)
            .enumerate()
            .filter(|(_, (old, new))| delete_images || old != new)
            .map(|(screen_row, (_, new))| (screen_row, new.clone()))
            .collect::<Vec<_>>();
        if changed.is_empty() {
            return;
        }

        self.begin_synchronized_output();
        if delete_images {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (screen_row, new) in changed {
            self.terminal
                .write(&format!("\x1b[{};1H", screen_row.saturating_add(1)));
            self.terminal.clear_line();
            self.terminal.write(&new);
        }
        self.end_synchronized_output();
    }

    /// Destructively replace terminal-owned history after a resize or an
    /// explicit generic presentation rebuild.
    fn reset_inline_scrollback(
        &mut self,
        new_lines: &[String],
        rows: usize,
        previous_frame_has_image: bool,
    ) {
        let window_top = new_lines.len().saturating_sub(rows);
        let delete_images =
            previous_frame_has_image || new_lines.iter().any(|line| is_image_line(line));

        self.begin_synchronized_output();
        reset_and_replay(
            self.terminal.as_mut(),
            delete_images,
            new_lines.iter().map(String::as_str),
        );
        self.end_synchronized_output();

        // The old semantic cursor referred to the discarded presentation.
        // The next frame negotiates a fresh cursor while `inline_history_rows`
        // prevents that acknowledgement from duplicating replayed rows.
        self.inline_history_rows = window_top;
        self.inline_committed_rows = 0;
        self.inline_commit_cursor = None;
        self.inline_generation = None;
        self.inline_window_top = window_top;
        self.inline_surface_active = false;
        self.inline_surface_window.clear();
        self.inline_bottom_row = new_lines
            .len()
            .saturating_sub(window_top)
            .saturating_sub(1)
            .min(rows.saturating_sub(1));
    }

    fn redraw_all_from_home(&mut self, lines: &[String]) {
        // `Clear(All)` does not universally home the cursor. Do both before
        // repainting so resize and line-count redraws cannot append a frame.
        if self.capabilities.cursor_addressing {
            self.terminal.write("\x1b[H");
        }
        self.terminal.clear_screen();
        if self.capabilities.cursor_addressing {
            self.terminal.write("\x1b[H");
        }
        self.write_all_lines(lines);
    }

    fn write_all_lines(&mut self, lines: &[String]) {
        self.begin_synchronized_output();
        if !self.first_render && self.previous_frame.iter().any(|line| is_image_line(line)) {
            self.terminal.write(&delete_all_kitty_images());
        }
        for (index, line) in lines.iter().enumerate() {
            self.terminal.write(line);
            // Keep append-only/non-addressable terminals line-delimited. An
            // addressable retained frame deliberately leaves its cursor on the
            // last row so a full-height frame cannot scroll by one line.
            if index + 1 < lines.len() || !self.capabilities.cursor_addressing {
                self.terminal.write("\n");
            }
        }
        self.end_synchronized_output();
    }

    fn write_plain_changes(
        &mut self,
        lines: &[String],
        first_changed_hint: Option<usize>,
        previous_len: usize,
    ) {
        let first_changed = if self.first_render {
            0
        } else {
            first_changed_hint.unwrap_or_else(|| {
                self.previous_frame
                    .iter()
                    .zip(lines)
                    .position(|(previous, next)| previous != next)
                    .unwrap_or(previous_len.min(lines.len()))
            })
        };
        for line in &lines[first_changed..] {
            self.terminal.write(line);
            self.terminal.write("\n");
        }
    }

    fn root_render_update_without_cursor(&self, width: u16) -> Option<FrameUpdate> {
        // Pi does not consume a semantic commit handshake. None passed to the
        // cursor-aware API means bootstrap, not opt-out of that metadata work.
        (self.children.len() == 1)
            .then(|| self.children[0].render_update(width))
            .flatten()
    }

    fn root_render_update(&self, width: u16, cursor: Option<CommitCursor>) -> Option<FrameUpdate> {
        // Lazy updates require exactly one child: a multi-child frame has no
        // single stable prefix to reuse.
        (self.children.len() == 1)
            .then(|| self.children[0].render_update_with_cursor(width, cursor))
            .flatten()
    }

    fn root_render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    fn prepare_line(&self, line: String, width: u16) -> String {
        if self.capabilities.plain {
            ensure_plain_line(&line, width)
        } else if self.inline_scrollback {
            clip_line_width(&line, width)
        } else {
            ensure_line_width(&line, width)
        }
    }

    fn begin_synchronized_output(&mut self) {
        if !self.capabilities.synchronized_output {
            return;
        }
        if self.synchronized_output_depth == 0 {
            self.terminal.write("\x1b[?2026h");
        }
        self.synchronized_output_depth = self.synchronized_output_depth.saturating_add(1);
    }

    fn end_synchronized_output(&mut self) {
        if !self.capabilities.synchronized_output || self.synchronized_output_depth == 0 {
            return;
        }
        self.synchronized_output_depth -= 1;
        if self.synchronized_output_depth == 0 {
            self.terminal.write("\x1b[?2026l");
        }
    }
}

fn extract_logical_cursor_position_from(
    lines: &mut [String],
    row_offset: usize,
) -> Option<LogicalCursorPosition> {
    let mut cursor = None;
    for (local_row, line) in lines.iter_mut().enumerate() {
        while let Some(marker) = line.find(CURSOR_MARKER) {
            cursor = Some(LogicalCursorPosition {
                row: row_offset.saturating_add(local_row),
                column: visible_width(&line[..marker]),
            });
            line.replace_range(marker..marker + CURSOR_MARKER.len(), "");
        }
    }
    cursor
}

fn pi_cursor_position(
    cursor: Option<LogicalCursorPosition>,
    total_lines: usize,
    height: usize,
) -> Option<(usize, usize)> {
    let viewport_top = total_lines.saturating_sub(height);
    cursor
        .filter(|cursor| cursor.row >= viewport_top && cursor.row < total_lines)
        .map(|cursor| (cursor.row, cursor.column))
}

fn extended_cursor_position(
    cursor: Option<LogicalCursorPosition>,
    total_lines: usize,
    width: u16,
    height: u16,
) -> Option<(u16, u16)> {
    let viewport_start = total_lines.saturating_sub(usize::from(height));
    let max_column = usize::from(width.saturating_sub(1));
    cursor
        .filter(|cursor| cursor.row >= viewport_start && cursor.row < total_lines)
        .map(|cursor| {
            (
                (cursor.row - viewport_start) as u16,
                cursor.column.min(max_column) as u16,
            )
        })
}

fn signed_difference(left: usize, right: usize) -> i64 {
    if left >= right {
        i64::try_from(left - right).unwrap_or(i64::MAX)
    } else {
        -i64::try_from(right - left).unwrap_or(i64::MAX)
    }
}

fn pi_line_difference(
    hardware_cursor_row: usize,
    previous_viewport_top: usize,
    target_row: usize,
    viewport_top: usize,
) -> i64 {
    let current_screen_row = signed_difference(hardware_cursor_row, previous_viewport_top);
    let target_screen_row = signed_difference(target_row, viewport_top);
    target_screen_row.saturating_sub(current_screen_row)
}

fn push_cursor_up(buffer: &mut String, rows: usize) {
    if rows > 0 {
        buffer.push_str(&format!("\x1b[{rows}A"));
    }
}

fn push_cursor_down(buffer: &mut String, rows: usize) {
    if rows > 0 {
        buffer.push_str(&format!("\x1b[{rows}B"));
    }
}

fn push_vertical_move(buffer: &mut String, rows: i64) {
    match rows.cmp(&0) {
        Ordering::Greater => push_cursor_down(buffer, rows as usize),
        Ordering::Less => push_cursor_up(buffer, rows.unsigned_abs() as usize),
        Ordering::Equal => {}
    }
}

impl Drop for TUI<'_> {
    fn drop(&mut self) {
        if self.running {
            self.stop();
        }
    }
}

/// Ensure a line is exactly `width` columns wide without splitting a grapheme
/// or an ANSI sequence.
fn ensure_line_width(line: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let visible = visible_width(line);
    match visible.cmp(&width) {
        Ordering::Less => format!("{}{}", line, " ".repeat(width - visible)),
        Ordering::Greater => crate::utils::truncate_to_width(line, width, Some("")),
        Ordering::Equal => line.to_owned(),
    }
}

/// Clip a line to `width` columns without right-padding it. Used by inline
/// scrollback mode, where trailing pad spaces would pollute native selection.
fn clip_line_width(line: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    if visible_width(line) > width {
        crate::utils::truncate_to_width(line, width, Some(""))
    } else {
        line.to_owned()
    }
}

fn ensure_plain_line(line: &str, width: u16) -> String {
    let mut safe = String::new();
    for (index, part) in line.split(CURSOR_MARKER).enumerate() {
        if index > 0 {
            safe.push_str(CURSOR_MARKER);
        }
        safe.push_str(&crate::sanitize::sanitize_line(part, true));
    }
    crate::utils::truncate_to_width(
        &safe,
        usize::from(width),
        Some(crate::GlyphSet::ASCII.ellipsis),
    )
}

fn extract_cursor_position(lines: &mut [String], width: u16, height: u16) -> Option<(u16, u16)> {
    let total_lines = lines.len();
    let cursor = extract_logical_cursor_position_from(lines, 0);
    extended_cursor_position(cursor, total_lines, width, height)
}

fn extract_cursor_position_from(
    lines: &mut [String],
    row_offset: usize,
    total_lines: usize,
    width: u16,
    height: u16,
) -> Option<(u16, u16)> {
    let cursor = extract_logical_cursor_position_from(lines, row_offset);
    extended_cursor_position(cursor, total_lines, width, height)
}

/// Terminal presentation, frame diffing, and pinned-row scrollback.
///
/// The assertions live in `tests/`, one file per cohesive area, so this source
/// file reads as pure implementation.
#[cfg(test)]
mod tests;
