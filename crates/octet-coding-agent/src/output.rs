#![allow(missing_docs)]

//! Terminal-aware boundaries for human-facing command output.
//!
//! Human-facing output is always control-safe, including when it is piped into
//! another program that later writes to a terminal. TTYs use compact Unicode
//! control pictures; redirected streams use ASCII descriptions.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt::Display;
use std::io::{self, IsTerminal, Write};
use std::sync::Mutex;

use sexy_tui_rs::{sanitize_line, sanitize_text, SanitizeOptions};

// The real terminal boundary owns this route, including lifecycle workers on
// other threads. A diagnostic must never bypass the retained renderer while it
// owns the physical cursor. Redirected stderr does not share that cursor.
static TUI_DIAGNOSTICS: Mutex<Option<TuiDiagnostics>> = Mutex::new(None);
const MAX_TUI_DIAGNOSTIC_BYTES: usize = 64 * 1024;
const MAX_TUI_DIAGNOSTIC_ENTRIES: usize = 128;
const MAX_TUI_DIAGNOSTIC_ENTRY_BYTES: usize = 8 * 1024;

#[derive(Default)]
struct TuiDiagnostics {
    entries: VecDeque<String>,
    bytes: usize,
    omitted: usize,
    deferred: usize,
}

/// Keep lifecycle diagnostics pending through the caller's final hydration,
/// not merely until its worker returns. Errors release them by ordinary Drop.
pub(crate) struct DeferredTuiDiagnostics(bool);

pub(crate) fn defer_tui_diagnostics() -> DeferredTuiDiagnostics {
    let mut route = TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let active = if let Some(queue) = route.as_mut() {
        queue.deferred += 1;
        true
    } else {
        false
    };
    DeferredTuiDiagnostics(active)
}

impl Drop for DeferredTuiDiagnostics {
    fn drop(&mut self) {
        if self.0 {
            if let Some(queue) = TUI_DIAGNOSTICS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_mut()
            {
                queue.deferred -= 1;
            }
        }
    }
}

impl TuiDiagnostics {
    fn push(&mut self, mut message: String) {
        const TRUNCATED: &str = " … [diagnostic truncated]";
        if message.len() > MAX_TUI_DIAGNOSTIC_ENTRY_BYTES {
            let mut end = MAX_TUI_DIAGNOSTIC_ENTRY_BYTES - TRUNCATED.len();
            while !message.is_char_boundary(end) {
                end -= 1;
            }
            message.truncate(end);
            message.push_str(TRUNCATED);
        }
        while self.entries.len() >= MAX_TUI_DIAGNOSTIC_ENTRIES
            || self.bytes + message.len() > MAX_TUI_DIAGNOSTIC_BYTES
        {
            let removed = self
                .entries
                .pop_front()
                .expect("bounded entry fits an empty queue");
            self.bytes -= removed.len();
            self.omitted = self.omitted.saturating_add(1);
        }
        self.bytes += message.len();
        self.entries.push_back(message);
    }

    fn take(&mut self) -> Vec<String> {
        let mut entries = Vec::with_capacity(self.entries.len() + usize::from(self.omitted > 0));
        if self.omitted > 0 {
            entries.push(format!(
                "warning: {} earlier terminal diagnostics omitted",
                self.omitted
            ));
        }
        entries.extend(self.entries.drain(..));
        self.bytes = 0;
        self.omitted = 0;
        entries
    }
}

pub(crate) fn begin_tui_diagnostics() {
    *TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(TuiDiagnostics::default());
}

pub(crate) fn has_tui_diagnostics() -> bool {
    TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|queue| {
            queue.deferred == 0 && (!queue.entries.is_empty() || queue.omitted > 0)
        })
}

/// Producers hold only the queue lock and never acquire shell state.
pub(crate) fn take_tui_diagnostics() -> Vec<String> {
    TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
        .filter(|queue| queue.deferred == 0)
        .map(TuiDiagnostics::take)
        .unwrap_or_default()
}

/// Called only after restoring cooked output. Keep even shutdown-time warnings
/// that the renderer has not consumed; never print them over a raw frame.
pub(crate) fn end_tui_diagnostics() {
    let pending = TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
        .map(|mut queue| queue.take())
        .unwrap_or_default();
    for message in pending {
        write_multiline(io::stderr().lock(), &message, io::stderr().is_terminal());
    }
}

fn route_tui_diagnostic(message: &str, terminal: bool, fallback: impl FnOnce()) {
    if !terminal {
        fallback();
        return;
    }
    let mut route = TUI_DIAGNOSTICS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(queue) = route.as_mut() {
        queue.push(message.to_owned());
    } else {
        // Serialize cooked output with route installation, so a writer cannot
        // check the old route and then emit after raw mode has been enabled.
        fallback();
    }
}

macro_rules! stderr {
    ($($argument:tt)*) => {
        $crate::output::stderr_line(format!($($argument)*))
    };
}
pub(crate) use stderr;

pub(crate) fn stdout_is_terminal() -> bool {
    io::stdout().is_terminal()
}

fn safe_line(value: &str, terminal: bool) -> Cow<'_, str> {
    sanitize_line(value, !terminal)
}

fn safe_table_line(value: &str, terminal: bool) -> Cow<'_, str> {
    sanitize_text(
        value,
        SanitizeOptions {
            controls: if terminal {
                sexy_tui_rs::ControlPictures::Unicode
            } else {
                sexy_tui_rs::ControlPictures::Ascii
            },
            preserve_newlines: false,
            preserve_tabs: true,
        },
    )
}

fn safe_multiline(value: &str, terminal: bool) -> Cow<'_, str> {
    sanitize_text(
        value,
        SanitizeOptions {
            controls: if terminal {
                sexy_tui_rs::ControlPictures::Unicode
            } else {
                sexy_tui_rs::ControlPictures::Ascii
            },
            ..SanitizeOptions::default()
        },
    )
}

/// Sanitize one untrusted table field without consuming trusted separators.
pub(crate) fn table_field(value: &str, terminal: bool) -> Cow<'_, str> {
    safe_line(value, terminal)
}

fn write_line(mut writer: impl Write, value: &str, terminal: bool) {
    let _ = writeln!(writer, "{}", safe_line(value, terminal));
}

fn write_multiline(mut writer: impl Write, value: &str, terminal: bool) {
    let value = safe_multiline(value, terminal);
    let _ = write!(writer, "{value}");
    if !value.ends_with('\n') {
        let _ = writeln!(writer);
    }
}

pub(crate) fn stdout_line(value: impl Display) {
    let terminal = stdout_is_terminal();
    write_line(io::stdout().lock(), &value.to_string(), terminal);
}

pub(crate) fn stdout_table_line(value: impl Display) {
    let terminal = stdout_is_terminal();
    let value = value.to_string();
    let value = safe_table_line(&value, terminal);
    let _ = writeln!(io::stdout().lock(), "{value}");
}

pub(crate) fn stderr_line(value: impl Display) {
    let terminal = io::stderr().is_terminal();
    let value = value.to_string();
    let safe = safe_line(&value, terminal);
    route_tui_diagnostic(&safe, terminal, || {
        write_line(io::stderr().lock(), &value, terminal);
    });
}

pub(crate) fn stdout_multiline(value: impl Display) {
    let terminal = stdout_is_terminal();
    write_multiline(io::stdout().lock(), &value.to_string(), terminal);
}

pub(crate) fn stderr_multiline(value: impl Display) {
    let terminal = io::stderr().is_terminal();
    let value = value.to_string();
    let safe = safe_multiline(&value, terminal);
    route_tui_diagnostic(&safe, terminal, || {
        write_multiline(io::stderr().lock(), &value, terminal);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_diagnostics_wait_through_delayed_hydration() {
        const CHILD: &str = "OCTET_TEST_DEFERRED_DIAGNOSTICS";
        if std::env::var_os(CHILD).is_none() {
            // Isolate the real process-global output route from concurrent
            // terminal-restoration and test-shell tests in the lib harness.
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "output::tests::lifecycle_diagnostics_wait_through_delayed_hydration",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        begin_tui_diagnostics();
        let hydration = defer_tui_diagnostics();
        {
            let worker = defer_tui_diagnostics();
            std::thread::spawn(|| {
                route_tui_diagnostic("warning: retained after rebuild", true, || {
                    panic!("raw fallback")
                })
            })
            .join()
            .unwrap();
            drop(worker);
        }
        // The worker is finished, but the caller has not hydrated its new
        // transcript. Any number of renderer polls must leave the warning queued.
        for _ in 0..3 {
            assert!(!has_tui_diagnostics());
            assert!(take_tui_diagnostics().is_empty());
        }
        drop(hydration);
        assert!(has_tui_diagnostics());
        assert_eq!(take_tui_diagnostics(), ["warning: retained after rebuild"]);
        let failed = || -> Result<(), ()> {
            let _hydration = defer_tui_diagnostics();
            route_tui_diagnostic("warning: failed rebuild", true, || panic!("raw fallback"));
            Err(())
        };
        assert!(failed().is_err());
        assert_eq!(take_tui_diagnostics(), ["warning: failed rebuild"]);
        end_tui_diagnostics();
    }

    #[test]
    fn terminal_diagnostic_queue_bounds_count_bytes_and_unicode_entries() {
        let mut queue = TuiDiagnostics::default();
        for index in 0..(MAX_TUI_DIAGNOSTIC_ENTRIES + 3) {
            queue.push(format!("warning {index}"));
        }
        assert_eq!(queue.entries.len(), MAX_TUI_DIAGNOSTIC_ENTRIES);
        assert_eq!(queue.omitted, 3);
        let messages = queue.take();
        assert_eq!(
            messages[0],
            "warning: 3 earlier terminal diagnostics omitted"
        );
        assert_eq!(messages[1], "warning 3");
        assert!(queue.take().is_empty());
        assert_eq!(queue.bytes, 0);
        for _ in 0..16 {
            queue.push("🙂".repeat(MAX_TUI_DIAGNOSTIC_ENTRY_BYTES));
        }
        assert!(queue.bytes <= MAX_TUI_DIAGNOSTIC_BYTES);
        assert!(queue.omitted > 0);
        for entry in &queue.entries {
            assert!(entry.len() <= MAX_TUI_DIAGNOSTIC_ENTRY_BYTES);
            assert!(entry.ends_with("[diagnostic truncated]"));
        }
    }

    #[test]
    fn redirected_diagnostics_keep_chronological_plain_bytes() {
        let mut output = Vec::new();
        route_tui_diagnostic("warning: first", false, || {
            write_line(&mut output, "warning: first", false);
        });
        write_multiline(&mut output, "second\nthird", false);
        assert_eq!(output, b"warning: first\nsecond\nthird\n");
    }

    #[test]
    fn terminal_lines_neutralize_controls_and_line_injection() {
        let rendered = safe_line("name\x1b]52;c;YXR0YWNr\x07\nforged\tcolumn", true);
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\t'));
        assert!(rendered.contains('␛'));
        assert!(rendered.contains('␇'));
    }

    #[test]
    fn redirected_lines_use_ascii_control_descriptions() {
        let rendered = safe_line("name\x1b]52;c;YXR0YWNr\x07\nsecond\tcolumn", false);
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
        assert!(!rendered.contains('\n'));
        assert!(!rendered.contains('\t'));
        assert!(rendered.contains("^["));
        assert!(rendered.contains("<BEL>"));
    }

    #[test]
    fn terminal_table_fields_cannot_add_rows_or_columns() {
        let field = table_field("name\nforged\tcolumn\x1b", true);
        assert!(!field.contains('\n'));
        assert!(!field.contains('\t'));
        let row = format!("id\t{field}\ttag");
        assert_eq!(safe_table_line(&row, true).matches('\t').count(), 2);
        assert!(!row.contains('\x1b'));
    }

    #[test]
    fn terminal_multiline_preserves_layout_but_not_commands() {
        let rendered = safe_multiline("first\nsecond\tvalue\x1b]0;owned\x07", true);
        assert!(rendered.contains("first\nsecond\tvalue"));
        assert!(!rendered.contains('\x1b'));
        assert!(!rendered.contains('\x07'));
    }
}
