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
static REMOTE_KEYBOARD_ENHANCEMENT_ACTIVE: AtomicBool = AtomicBool::new(false);

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

pub(super) fn mark_remote_keyboard_enhancement_active() {
    REMOTE_KEYBOARD_ENHANCEMENT_ACTIVE.store(true, Ordering::SeqCst);
}

pub(super) fn take_remote_keyboard_enhancement_active() -> bool {
    REMOTE_KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst)
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

/// Discard already-queued input without blocking.
///
/// Called during teardown to drop Kitty keyboard-protocol repeats/releases
/// (e.g. the exiting Ctrl+D as `ESC[100;5u`) and stale device-attribute
/// replies before the parent shell reads them as literal text. Bounded: at
/// most a handful of polls/reads, never waits for new input.
fn drain_pending_input() {
    use std::time::Duration;
    // Enough to cover a held-key auto-repeat burst plus a DA reply; the loop
    // exits early as soon as the queue is empty.
    for _ in 0..32 {
        match event::poll(Duration::from_millis(0)) {
            Ok(true) => {
                // Poll true means a read will not block on the kernel buffer.
                // Discard whatever it is (key repeat/release, DA reply).
                if event::read().is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

fn restore_terminal(advance_line: bool) {
    let raw_active = RAW_ACTIVE.swap(false, Ordering::SeqCst);
    let keyboard_enhancement_active = KEYBOARD_ENHANCEMENT_ACTIVE.swap(false, Ordering::SeqCst);
    let remote_keyboard_enhancement_active = take_remote_keyboard_enhancement_active();
    if !raw_active && !keyboard_enhancement_active && !remote_keyboard_enhancement_active {
        // Even when modes are already clear, pending input (e.g. a Kitty
        // CSI-u repeat/release of the exiting Ctrl+D, tail `00;5u`) may still
        // sit in the kernel buffer and leak into the parent shell. Drain it.
        drain_pending_input();
        return;
    }

    let mut out = std::io::stdout();
    if remote_keyboard_enhancement_active {
        let _ = execute!(out, event::PopKeyboardEnhancementFlags);
    }
    if keyboard_enhancement_active {
        let _ = execute!(out, event::PopKeyboardEnhancementFlags);
        // Pop leaves any already-emitted key repeats/releases queued behind
        // it. Discard them before returning to cooked mode so the shell never
        // echoes a partial `ESC[100;5u` as literal `00;5u`.
        drain_pending_input();
    }
    if raw_active {
        let _ = terminal::disable_raw_mode();
        // A release arriving between Pop and raw-mode exit lands here; drain
        // again now that the terminal is back in cooked mode.
        drain_pending_input();
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
    // The console output mode changes line-feed and wrapping semantics for
    // whatever the parent shell writes next, so it is restored only after the
    // final mode-reset sequences above have reached the console.
    #[cfg(windows)]
    if raw_active {
        super::windows_console::restore();
    }
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
