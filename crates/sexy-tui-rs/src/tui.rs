//! Retained component tree with line-differential terminal rendering.
//!
//! A [`TUI`] owns a component tree and a terminal, and after every frame it
//! knows both the rows it painted last time and the semantic commit cursor the
//! component handed it. That retained state is what makes the expensive cases
//! cheap: a long settled transcript is never re-cloned, an appended tail never
//! repaints the history above it, and a resize can be answered without replaying
//! the whole conversation.
//!
//! This file is the module seam. It owns the public surface — the [`Component`]
//! trait implementors write, the frame-handshake types they return, the [`TUI`]
//! type and its public methods, and the choice of which renderer runs — and
//! nothing else. Everything that actually addresses a terminal lives in a
//! sibling, split by the contract it implements rather than by size:
//!
//! * `kitty` — which image placements the terminal holds, and the deletion
//!   escapes needed before affected rows are repainted. Consulted by all three
//!   renderers.
//! * `frame` — composing the child tree into a frame, normalizing each row to
//!   the viewport width, and reducing two frames to the row-level
//!   `FrameChangeHints` a renderer needs while the previous frame is still
//!   alive.
//! * `cursor` — the pure arithmetic between a component's zero-width APC
//!   cursor marker and the row/column the terminal should park at.
//! * `pi_render` — the normative differential algorithm ported from Pi TUI's
//!   `doRender()`, plus the alternate-screen variant that paints the same
//!   document through a fixed viewport.
//! * `extended` — the legacy path for terminals that cannot address the
//!   cursor, where no in-place repaint is possible.
//! * `inline` — the opt-in inline-scrollback path, which has to reconcile an
//!   application-owned transcript tape with a terminal-owned history buffer.
//! * `repaint` — the two non-differential primitives every renderer falls
//!   back to, and the reference-counted synchronized-output transaction.
//!
//! The three renderer siblings are separate because each is a distinct contract
//! with a distinct failure mode, not three implementations of one: the
//! normative path may always repaint the smallest changed set, the extended path
//! cannot repaint a row at all, and the inline path can lose history if its
//! commit ledger is wrong. Keeping them apart is what lets design §11 be
//! checked line by line against `packages/tui/src/tui.ts`.

use std::collections::BTreeSet;

use crate::terminal::{key_to_string, Terminal, TerminalInput};

mod cursor;
mod extended;
mod frame;
mod inline;
mod kitty;
mod pi_render;
mod repaint;

use self::cursor::{push_vertical_move, signed_difference, LogicalCursorPosition};
pub(crate) use self::kitty::delete_all_kitty_images;

/// Zero-width APC escape sequence used as a cursor position marker.
/// Pi's zero-cell APC cursor marker.
pub const CURSOR_MARKER: &str = "\x1b_pi:c\x07";
pub type InputListener<'a> = Box<dyn FnMut(&str) -> Option<String> + 'a>;
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
    /// Keep already-emitted native history on structural changes. The live grid
    /// is repaired in place; canonical historical edits remain in the retained
    /// document rather than erasing the terminal's emitted snapshots.
    preserve_scrollback: bool,
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
            preserve_scrollback: false,
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

    /// Opt the primary-screen Pi renderer into preserving terminal-owned saved
    /// lines on resize, historical edits, and viewport transitions. Structural
    /// repair paints at most one live grid without ED 2/3 or document replay.
    /// Saved lines retain their emitted presentation; [`TUI::rendered_frame`]
    /// remains canonical. Legacy inline and alternate-screen paths are unchanged.
    /// Disabled by default to retain Pi's destructive-replay compatibility.
    pub fn set_preserve_scrollback(&mut self, preserve: bool) {
        self.preserve_scrollback = preserve;
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

    /// First addressable logical row of the last primary-screen Pi paint.
    ///
    /// Unlike `rendered_frame().len() - rows`, this retains the physical seam
    /// after a differential shrink and resets it after a complete replay.
    /// Other renderer modes and an unpainted TUI return `None`.
    pub fn rendered_viewport_top(&self) -> Option<usize> {
        (!self.first_render && self.uses_pi_renderer() && self.alternate_screen_session.is_none())
            .then_some(self.previous_viewport_top)
    }

    /// Request a re-render at the next opportunity.
    pub fn request_render(&mut self) {
        self.request_render_force(false);
    }

    /// Match Pi's forced-render path by invalidating every retained cursor and
    /// viewport assumption before rendering.
    pub fn request_render_force(&mut self, force: bool) {
        if force {
            if !self.preserve_scrollback || !self.uses_pi_renderer() {
                self.previous_frame.clear();
                self.previous_viewport_top = 0;
            }
            // Preserving Pi repair addresses the grid absolutely, but still
            // needs its old visible image rows to retire their placements.
            self.previous_size = Some((u16::MAX, u16::MAX));
            self.logical_cursor_position = None;
            self.cursor_row = 0;
            self.hardware_cursor_row = 0;
            self.max_lines_rendered = 0;
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
}

impl Drop for TUI<'_> {
    fn drop(&mut self) {
        if self.running {
            self.stop();
        }
    }
}

/// Terminal presentation, frame diffing, and pinned-row scrollback.
///
/// The assertions live in `tests/`, one file per cohesive area, so this source
/// file reads as pure implementation.
#[cfg(test)]
mod tests;
