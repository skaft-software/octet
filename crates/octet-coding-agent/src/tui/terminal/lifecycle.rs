//! Raw-mode, keyboard-enhancement, panic, and terminal restoration lifecycle.
//!
//! The process-global guards are deliberately small and idempotent. Setup marks
//! each mode only after it succeeds; every exit path, including panic and
//! signal cleanup, funnels through the same restoration routine.
#![allow(missing_docs)]

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::{cursor, event, execute, terminal};

static RAW_ACTIVE: AtomicBool = AtomicBool::new(false);
static KEYBOARD_ENHANCEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Keep ordinary text in the terminal's normal text path while asking Kitty
/// protocol terminals to include the layout-resolved alternate character for
/// modified keys. `REPORT_ALL_KEYS_AS_ESCAPE_CODES` intentionally does not
/// belong here: it turns every printable key into a physical/base-key event,
/// and crossterm cannot recover associated IME/dead-key text from that form.
pub(super) fn keyboard_enhancement_flags() -> event::KeyboardEnhancementFlags {
    event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | event::KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
}

pub(super) fn mark_raw_active() {
    RAW_ACTIVE.store(true, Ordering::SeqCst);
}

pub(super) fn mark_keyboard_enhancement_active() {
    KEYBOARD_ENHANCEMENT_ACTIVE.store(true, Ordering::SeqCst);
}

/// Restore the process terminal state. Repeated calls are harmless.
pub fn force_restore() {
    restore_terminal(true);
}

/// Restore the terminal state without adding a line, for a renderer that has
/// already completed its final frame at the next line.
pub(super) fn restore_without_line() {
    restore_terminal(false);
}

fn restore_terminal(advance_line: bool) {
    let raw_active = RAW_ACTIVE.swap(false, Ordering::SeqCst);
    let keyboard_enhancement_active = KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst);
    if !raw_active && !keyboard_enhancement_active {
        return;
    }

    let mut out = std::io::stdout();
    if keyboard_enhancement_active {
        let _ = execute!(out, event::PopKeyboardEnhancementFlags);
    }
    if raw_active {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            out,
            event::DisableBracketedPaste,
            event::DisableMouseCapture,
            cursor::SetCursorStyle::DefaultUserShape,
            cursor::Show
        );
    } else {
        let _ = execute!(out, cursor::Show);
    }
    if advance_line {
        let _ = execute!(out, cursor::MoveToNextLine(1), cursor::MoveToColumn(0));
    }
    let _ = out.flush();
    crate::output::end_tui_diagnostics();
}

/// Install a panic hook which restores the terminal before delegating to the
/// hook that was installed by the caller (or by the standard library).
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        force_restore();
        previous(info);
    }));
}

#[cfg(test)]
#[path = "lifecycle_tests.rs"]
mod tests;
