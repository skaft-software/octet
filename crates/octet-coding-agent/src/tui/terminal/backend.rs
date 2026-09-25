//! Render-only terminal backend and opaque image-protocol boundary.
//!
//! This module owns terminal writes, synchronized-frame batching, line-ending
//! normalization, diagnostics logging, and validated image placement. It does
//! not own input or terminal capability policy.
#![allow(missing_docs)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Stdout, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use crossterm::{cursor, event, execute, queue, terminal};
use sexy_tui_rs::{
    ImageAnchor, ImageCapabilities, ImageCapabilityOverrides, ImageId, ImageLimits, ImageProtocol,
    ImageProtocolEncoder, TerminalCapabilities as SexyTerminalCapabilities, TerminalImage,
};

use super::capabilities::{sexy_terminal_capabilities, ColorMode, TerminalCapabilities};
use super::lifecycle;
use super::TerminalSize;

/// Owned, validated image payloads addressable from semantic image anchors.
///
/// This store is intentionally internal to the interactive renderer. It has no
/// path, URL, or byte-exposure API: only the terminal adapter can resolve an
/// already allocated ID into an opaque `TerminalImage` for protocol encoding.
#[derive(Clone, Default)]
pub(crate) struct TerminalImageStore {
    images: Arc<Mutex<HashMap<u32, Arc<TerminalImage>>>>,
}

impl TerminalImageStore {
    /// Retain one image under an ID allocated by `ImageRegistry`.
    pub(crate) fn register(&self, id: ImageId, image: Arc<TerminalImage>) {
        self.images
            .lock()
            .expect("terminal image store mutex poisoned")
            .insert(id.get(), image);
    }

    fn get(&self, id: ImageId) -> Option<Arc<TerminalImage>> {
        self.images
            .lock()
            .expect("terminal image store mutex poisoned")
            .get(&id.get())
            .cloned()
    }

    /// Drop all payload references when a transcript is replaced.
    pub(crate) fn clear(&self) {
        self.images
            .lock()
            .expect("terminal image store mutex poisoned")
            .clear();
    }
}

fn open_tui_write_log(configured: Option<std::ffi::OsString>) -> Option<File> {
    let mut path = PathBuf::from(configured?);
    if path.as_os_str().is_empty() {
        return None;
    }
    if path.is_dir() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        path.push(format!("tui-{timestamp}-{}.log", std::process::id()));
    }
    OpenOptions::new().create(true).append(true).open(path).ok()
}

// sexy-tui wraps every frame in synchronized-output mode. Buffer the many
// per-line Terminal::write calls until this delimiter so one frame reaches the
// terminal in one flush rather than dozens of tiny writes.
const SYNC_OUTPUT_BEGIN: &str = "\x1b[?2026h";
const SYNC_OUTPUT_END: &str = "\x1b[?2026l";

fn normalize_line_endings(data: &str, last_was_cr: &mut bool) -> String {
    let mut normalized = String::with_capacity(data.len().saturating_add(8));
    for character in data.chars() {
        if character == '\n' && !*last_was_cr {
            normalized.push('\r');
        }
        normalized.push(character);
        *last_was_cr = character == '\r';
    }
    normalized
}

/// Render-only terminal adapter used by sexy-tui.
///
/// Input is deliberately driven by the application's async crossterm stream;
/// sexy-tui's blocking `Terminal::start` is never called.
pub struct OctetTerminal<W: Write = Stdout> {
    out: W,
    size: TerminalSize,
    last_was_cr: bool,
    pending: Vec<u8>,
    /// Backend diagnostics mirror terminal control bytes but replace opaque
    /// image protocol payloads with a fixed marker.
    pending_log: Vec<u8>,
    image_store: TerminalImageStore,
    write_log: Option<File>,
    in_synchronized_frame_depth: usize,
}

impl OctetTerminal<Stdout> {
    /// Enter raw mode on the primary screen, returning the shared size cell.
    #[allow(dead_code)] // Used by the separately compiled Gate-0 spike target.
    pub fn enter() -> Result<(Self, TerminalSize)> {
        let size = Arc::new(Mutex::new(terminal::size().unwrap_or((80, 24))));
        let terminal = Self::enter_with_size(size.clone())?;
        Ok((terminal, size))
    }

    /// Enter using a caller-owned shared dimensions cell. This lets the shell
    /// update dimensions after resize while the terminal is boxed in the TUI.
    pub fn enter_with_size(size: TerminalSize) -> Result<Self> {
        Self::enter_with_mouse(size, false)
    }

    /// Enter the primary screen with optional SGR mouse reporting. When capture
    /// is enabled octet owns semantic scrolling and selection; without capture,
    /// native terminal selection and history remain available.
    pub fn enter_with_mouse(size: TerminalSize, capture_mouse: bool) -> Result<Self> {
        Self::enter_with_mouse_and_images(size, capture_mouse, TerminalImageStore::default())
    }

    /// Enter with the image store shared by the retained shell and this output
    /// adapter. The store contains only already validated owned payloads.
    pub(crate) fn enter_with_mouse_and_images(
        size: TerminalSize,
        capture_mouse: bool,
        image_store: TerminalImageStore,
    ) -> Result<Self> {
        crate::output::begin_tui_diagnostics();
        if let Err(error) = terminal::enable_raw_mode() {
            crate::output::end_tui_diagnostics();
            return Err(error.into());
        }
        lifecycle::mark_raw_active();

        let result = Self::enter_inner(size, capture_mouse, image_store);
        if result.is_err() {
            lifecycle::force_restore();
        }
        result
    }

    fn enter_inner(
        size: TerminalSize,
        capture_mouse: bool,
        image_store: TerminalImageStore,
    ) -> Result<Self> {
        let mut out = std::io::stdout();
        execute!(
            out,
            event::EnableBracketedPaste,
            cursor::SetCursorStyle::SteadyBlock,
            cursor::Hide,
            // The renderer owns the complete physical viewport. ED2/CUP do
            // not reset inherited scrolling margins or origin mode: cursor-up
            // can clamp at that margin while the logical cursor keeps moving,
            // leaving a stale version/logo strip on each subsequent redraw.
            // Establish physical coordinates without erasing saved lines.
            crossterm::style::Print("\x1b[?6l\x1b[r"),
            // Pi's first primary-screen frame deliberately preserves saved
            // lines and therefore does not erase physical rows left by the
            // shell. Clear only the visible viewport before that frame; ED 3
            // is intentionally omitted so ordinary terminal history survives.
            terminal::Clear(terminal::ClearType::All),
            cursor::MoveTo(0, 0),
        )?;
        if capture_mouse {
            execute!(out, event::EnableMouseCapture)?;
        }
        // Preserve ordinary text as terminal text. Modified keys still use
        // CSI-u, and REPORT_ALTERNATE_KEYS supplies their layout-resolved
        // character (for example `a:A` and `1:!`) to crossterm. This keeps
        // Ctrl+Enter distinct without asking terminals to report every typed
        // character as an ambiguous physical/base key.
        // Unsupported terminals safely ignore this request; Alt+Enter remains
        // the portable multiline fallback.
        if execute!(
            out,
            event::PushKeyboardEnhancementFlags(lifecycle::keyboard_enhancement_flags())
        )
        .is_ok()
        {
            lifecycle::mark_keyboard_enhancement_active();
        }
        let detected_size = terminal::size()
            .unwrap_or_else(|_| *size.lock().expect("terminal size mutex poisoned"));
        *size.lock().expect("terminal size mutex poisoned") = detected_size;
        Ok(Self {
            out,
            size,
            last_was_cr: false,
            pending: Vec::with_capacity(16 * 1024),
            pending_log: Vec::with_capacity(16 * 1024),
            image_store,
            write_log: open_tui_write_log(std::env::var_os("OCTET_TUI_WRITE_LOG")),
            in_synchronized_frame_depth: 0,
        })
    }
}

impl<W: Write> OctetTerminal<W> {
    /// Conservative product policy: only Kitty gets live image placement.
    /// iTerm2 has no targetable delete, so replay/destructive replacement would
    /// leave stale pixels behind; the transcript uses semantic fallback instead.
    pub(crate) fn image_capabilities(&self) -> ImageCapabilities {
        let dimensions = *self.size.lock().expect("terminal size mutex poisoned");
        let detected = ImageCapabilities::detect(
            &sexy_terminal_capabilities(
                TerminalCapabilities::detect(ColorMode::Auto, false),
                dimensions,
            ),
            &ImageCapabilityOverrides::default(),
        );
        match detected.protocol() {
            Some(ImageProtocol::Kitty) => detected,
            _ => ImageCapabilities::forced(None, detected.cell_pixel_size()),
        }
    }

    fn append_backend_bytes(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        self.pending_log.extend_from_slice(bytes);
    }

    fn append_text(&mut self, text: &str) {
        let normalized = normalize_line_endings(text, &mut self.last_was_cr);
        self.append_backend_bytes(normalized.as_bytes());
    }

    fn append_image_anchor(&mut self, anchor: ImageAnchor) {
        let Some(image) = self.image_store.get(anchor.id()) else {
            return;
        };
        let Ok(command) = ImageProtocolEncoder::new(anchor.protocol(), ImageLimits::default())
            .encode_place(anchor.id(), &image, anchor.layout())
        else {
            return;
        };
        // Reserve the complete opaque command and its fixed diagnostic marker
        // before emitting either. An allocation failure therefore cannot leave
        // a partial graphics sequence in a synchronized frame or an image
        // write without its payload-free log replacement.
        const LOG_MARKER: &[u8] = b"[terminal image omitted]";
        if self.pending.try_reserve(command.encoded_len()).is_err()
            || self.pending_log.try_reserve(LOG_MARKER.len()).is_err()
        {
            return;
        }
        if command.write_to(&mut self.pending).is_ok() {
            self.pending_log.extend_from_slice(LOG_MARKER);
        }
    }

    /// Replace only complete internal DCS anchors. Unknown or malformed DCS
    /// controls are suppressed rather than forwarded; ordinary text before
    /// and after them remains in the normal terminal text path.
    fn append_render_data(&mut self, data: &str) {
        let mut cursor = 0;
        while let Some(relative_start) = data[cursor..].find("\x1bP") {
            let start = cursor.saturating_add(relative_start);
            self.append_text(&data[cursor..start]);
            let body_start = start.saturating_add(2);
            let Some(relative_end) = data[body_start..].find("\x1b\\") else {
                // Never forward an unterminated DCS control from a semantic row.
                return;
            };
            let end = body_start
                .saturating_add(relative_end)
                .saturating_add("\x1b\\".len());
            if let Some(anchor) = ImageAnchor::parse(&data[start..end]) {
                self.append_image_anchor(anchor);
            }
            cursor = end;
        }
        self.append_text(&data[cursor..]);
    }

    fn flush_pending(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let _ = self.out.write_all(&self.pending);
        let log_failed = self.write_log.as_mut().is_some_and(|log| {
            log.write_all(&self.pending_log)
                .and_then(|()| log.flush())
                .is_err()
        });
        if log_failed {
            self.write_log = None;
        }
        self.pending.clear();
        self.pending_log.clear();
        let _ = self.out.flush();
    }

    fn flush_if_outside_frame(&mut self) {
        if self.in_synchronized_frame_depth == 0 {
            self.flush_pending();
        }
    }

    /// Clear operations erase using the terminal's current rendition. Reset it
    /// first: a stale background attribute must not turn an erased
    /// differential-render tail into a colored band.
    fn reset_rendition_before_clear(&mut self) {
        self.append_backend_bytes(b"\x1b[0m");
    }
}

impl<W: Write> Drop for OctetTerminal<W> {
    fn drop(&mut self) {
        self.flush_pending();
        lifecycle::force_restore();
    }
}

impl<W: Write> sexy_tui_rs::Terminal for OctetTerminal<W> {
    fn start_events(
        &mut self,
        _on_input: Box<dyn FnMut(sexy_tui_rs::TerminalInput)>,
        _on_resize: Box<dyn FnMut()>,
    ) {
        unreachable!("OctetTerminal::start is never called; input is driven by the select! loop");
    }

    fn stop(&mut self) {
        self.flush_pending();
        // Pi's TUI already moved to the line after the complete frame. Restore
        // terminal modes without adding a second blank line.
        lifecycle::restore_without_line();
    }

    fn write(&mut self, data: &str) {
        // Pi's core renderer emits CRLF explicitly. Plain output and the
        // opt-in legacy inline extension can still write bare LF; raw mode
        // disables output post-processing, so normalize those at the backend
        // while preserving Pi's existing CRLF sequences.
        self.append_render_data(data);

        let begin_count = data.matches(SYNC_OUTPUT_BEGIN).count();
        let end_count = data.matches(SYNC_OUTPUT_END).count();
        self.in_synchronized_frame_depth = self
            .in_synchronized_frame_depth
            .saturating_add(begin_count)
            .saturating_sub(end_count);

        if self.in_synchronized_frame_depth == 0 {
            self.flush_pending();
        }
    }

    fn columns(&self) -> u16 {
        self.size.lock().expect("terminal size mutex poisoned").0
    }

    fn rows(&self) -> u16 {
        self.size.lock().expect("terminal size mutex poisoned").1
    }

    fn move_by(&mut self, lines: i16) {
        let mut bytes = Vec::new();
        let result = match lines.cmp(&0) {
            std::cmp::Ordering::Greater => queue!(bytes, cursor::MoveDown(lines as u16)),
            std::cmp::Ordering::Less => queue!(bytes, cursor::MoveUp((-lines) as u16)),
            std::cmp::Ordering::Equal => Ok(()),
        };
        if result.is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn hide_cursor(&mut self) {
        let mut bytes = Vec::new();
        if queue!(bytes, cursor::Hide).is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn show_cursor(&mut self) {
        let mut bytes = Vec::new();
        if queue!(bytes, cursor::Show).is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn clear_line(&mut self) {
        self.reset_rendition_before_clear();
        let mut bytes = Vec::new();
        if queue!(bytes, terminal::Clear(terminal::ClearType::CurrentLine)).is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn clear_from_cursor(&mut self) {
        self.reset_rendition_before_clear();
        let mut bytes = Vec::new();
        if queue!(bytes, terminal::Clear(terminal::ClearType::FromCursorDown)).is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn clear_screen(&mut self) {
        self.reset_rendition_before_clear();
        let mut bytes = Vec::new();
        if queue!(bytes, terminal::Clear(terminal::ClearType::All)).is_ok() {
            self.append_backend_bytes(&bytes);
        }
        self.flush_if_outside_frame();
    }

    fn capabilities(&self) -> SexyTerminalCapabilities {
        let dimensions = *self.size.lock().expect("terminal size mutex poisoned");
        sexy_terminal_capabilities(
            TerminalCapabilities::detect(ColorMode::Auto, false),
            dimensions,
        )
    }
}

#[cfg(test)]
#[path = "backend_tests.rs"]
mod tests;
