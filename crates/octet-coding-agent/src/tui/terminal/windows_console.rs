//! Native Windows console negotiation for the interactive frontend.
//!
//! Windows Terminal and conhost do not export `TERM`. A console counts as a VT
//! terminal only when standard output is a console handle with virtual-terminal
//! processing enabled; interactive input additionally requires a console input
//! handle. MSYS/Cygwin pty pipes (for example mintty without ConPTY) report as
//! terminals to `IsTerminal`, but deliver no console input records, so they
//! stay on the plain frontend.
//!
//! The interactive frontend also requests xterm-style delayed end-of-line
//! wrapping (`DISABLE_NEWLINE_AUTO_RETURN`). Without it, older console hosts
//! advance the cursor as soon as a row fills the last column, so a following
//! CRLF skips a row and every later differential frame lands one row low. Only
//! that flag is removed again on restore: VT output stays enabled, matching
//! crossterm and the colour decision already made for this process.
//!
//! Interactive input asks for virtual-terminal input
//! (`ENABLE_VIRTUAL_TERMINAL_INPUT`), so key records carry the bytes the
//! terminal sent, including bracketed-paste markers, SGR mouse reports and key
//! sequences, and octet decodes them with the parser Unix uses. Crossterm's
//! record decoder has no paste event: without VT input a paste arrived as one
//! key per character, so each newline submitted the prompt and a dropped path
//! was only typed text. That flag is removed on restore when octet added it.
//!
//! Rust's standard console writer converts at most a few KiB of UTF-8 per
//! `WriteConsoleW` call, splitting one synchronized frame into many console
//! writes that ConPTY can forward, and Windows Terminal can paint, separately.
//! [`write_frame`] hands a complete frame to a single `WriteConsoleW` call.
#![allow(missing_docs)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use super::input::ConsoleInput;

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetNumberOfConsoleInputEvents, GetStdHandle, ReadConsoleInputW, SetConsoleMode,
    WriteConsoleW, CONSOLE_MODE, DISABLE_NEWLINE_AUTO_RETURN, ENABLE_VIRTUAL_TERMINAL_INPUT,
    ENABLE_VIRTUAL_TERMINAL_PROCESSING, FOCUS_EVENT, INPUT_RECORD, KEY_EVENT, LEFT_CTRL_PRESSED,
    RIGHT_CTRL_PRESSED, SHIFT_PRESSED, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    WINDOW_BUFFER_SIZE_EVENT,
};

/// Set only when octet added delayed wrapping, so restore never clears a flag
/// the parent shell had already chosen.
static ADDED_DELAYED_WRAP: AtomicBool = AtomicBool::new(false);
/// Set only when octet added VT input, for the same reason.
static ADDED_VT_INPUT: AtomicBool = AtomicBool::new(false);
/// The interactive frontend reads VT input (added by octet or inherited).
static VT_INPUT_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Console facts consumed by capability detection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct ConsoleProbe {
    /// Standard input is a console handle that can deliver input records.
    pub input_console: bool,
    /// Standard output is a console handle with VT processing enabled.
    pub output_vt: bool,
}

fn standard_handle(kind: STD_HANDLE) -> Option<HANDLE> {
    // SAFETY: GetStdHandle has no preconditions; the returned handle is
    // borrowed from the process and never closed here.
    let handle = unsafe { GetStdHandle(kind) };
    (!handle.is_null() && handle != INVALID_HANDLE_VALUE).then_some(handle)
}

fn console_mode(handle: HANDLE) -> Option<CONSOLE_MODE> {
    let mut mode = 0;
    // SAFETY: `mode` is a valid out pointer. A pipe, file, or MSYS pty handle
    // makes the call fail rather than touching memory.
    (unsafe { GetConsoleMode(handle, &mut mode) } != 0).then_some(mode)
}

fn set_console_mode(handle: HANDLE, mode: CONSOLE_MODE) -> bool {
    // SAFETY: `handle` was returned by GetStdHandle and accepted by
    // GetConsoleMode; SetConsoleMode validates the requested flags.
    unsafe { SetConsoleMode(handle, mode) != 0 }
}

/// Inspect the standard handles, enabling VT output processing on a console
/// output handle that does not have it yet (the same negotiation crossterm
/// performs before its first command).
pub(super) fn probe() -> ConsoleProbe {
    let input_console = standard_handle(STD_INPUT_HANDLE)
        .and_then(console_mode)
        .is_some();
    let output_vt = standard_handle(STD_OUTPUT_HANDLE).is_some_and(|handle| {
        console_mode(handle).is_some_and(|mode| {
            mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING != 0
                || set_console_mode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING)
        })
    });
    ConsoleProbe {
        input_console,
        output_vt,
    }
}

/// Request VT output with delayed end-of-line wrapping for the renderer, and VT
/// input for the decoder.
pub(super) fn enter_interactive() {
    enter_vt_input();
    let Some(handle) = standard_handle(STD_OUTPUT_HANDLE) else {
        return;
    };
    let Some(mode) = console_mode(handle) else {
        return;
    };
    let vt = mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
    if mode & DISABLE_NEWLINE_AUTO_RETURN != 0 {
        let _ = set_console_mode(handle, vt);
        return;
    }
    if set_console_mode(handle, vt | DISABLE_NEWLINE_AUTO_RETURN) {
        ADDED_DELAYED_WRAP.store(true, Ordering::SeqCst);
    } else {
        // Hosts that reject delayed wrapping still accept plain VT output.
        let _ = set_console_mode(handle, vt);
    }
}

fn enter_vt_input() {
    let Some(handle) = standard_handle(STD_INPUT_HANDLE) else {
        return;
    };
    let Some(mode) = console_mode(handle) else {
        return;
    };
    if mode & ENABLE_VIRTUAL_TERMINAL_INPUT != 0 {
        VT_INPUT_ACTIVE.store(true, Ordering::SeqCst);
    } else if set_console_mode(handle, mode | ENABLE_VIRTUAL_TERMINAL_INPUT) {
        ADDED_VT_INPUT.store(true, Ordering::SeqCst);
        VT_INPUT_ACTIVE.store(true, Ordering::SeqCst);
    }
    // A host that rejects VT input keeps crossterm's record decoder.
}

/// Remove the flags added by [`enter_interactive`], after the final frame and
/// mode-reset sequences have been written.
pub(super) fn restore() {
    VT_INPUT_ACTIVE.store(false, Ordering::SeqCst);
    if ADDED_VT_INPUT.swap(false, Ordering::SeqCst) {
        if let Some(handle) = standard_handle(STD_INPUT_HANDLE) {
            if let Some(mode) = console_mode(handle) {
                let _ = set_console_mode(handle, mode & !ENABLE_VIRTUAL_TERMINAL_INPUT);
            }
        }
    }
    if !ADDED_DELAYED_WRAP.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(handle) = standard_handle(STD_OUTPUT_HANDLE) {
        if let Some(mode) = console_mode(handle) {
            let _ = set_console_mode(handle, mode & !DISABLE_NEWLINE_AUTO_RETURN);
        }
    }
}

/// Whether the interactive frontend decodes VT input itself.
pub(crate) fn vt_input_active() -> bool {
    VT_INPUT_ACTIVE.load(Ordering::SeqCst)
}

/// Append the records already queued on the console input, without waiting.
/// Returns whether any record was read.
pub(crate) fn read_input(output: &mut Vec<ConsoleInput>) -> io::Result<bool> {
    let Some(handle) = standard_handle(STD_INPUT_HANDLE) else {
        return Ok(false);
    };
    let mut pending = 0u32;
    // SAFETY: `pending` is a valid out pointer for a console input handle.
    if unsafe { GetNumberOfConsoleInputEvents(handle, &mut pending) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if pending == 0 {
        return Ok(false);
    }
    let mut records = [INPUT_RECORD::default(); 128];
    let requested = pending.min(records.len() as u32);
    let mut read = 0u32;
    // SAFETY: the buffer holds `requested` records and `read` is a valid out
    // pointer. Records are already queued, so the call does not block.
    if unsafe { ReadConsoleInputW(handle, records.as_mut_ptr(), requested, &mut read) } == 0 {
        return Err(io::Error::last_os_error());
    }
    for record in &records[..read as usize] {
        match u32::from(record.EventType) {
            KEY_EVENT => {
                // SAFETY: EventType selects the KeyEvent union member.
                let key = unsafe { record.Event.KeyEvent };
                // SAFETY: both union members are plain integers.
                let unit = unsafe { key.uChar.UnicodeChar };
                if key.bKeyDown != 0 && unit != 0 {
                    let shift = key.dwControlKeyState & SHIFT_PRESSED != 0;
                    let control =
                        key.dwControlKeyState & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0;
                    let input = if unit == u16::from(b'\r') && (shift || control) {
                        ConsoleInput::ModifiedEnter { shift, control }
                    } else {
                        ConsoleInput::Unit(unit)
                    };
                    for _ in 0..key.wRepeatCount.max(1) {
                        output.push(input);
                    }
                }
            }
            WINDOW_BUFFER_SIZE_EVENT => output.push(ConsoleInput::Resize),
            FOCUS_EVENT => {
                // SAFETY: EventType selects the FocusEvent union member.
                let focus = unsafe { record.Event.FocusEvent };
                output.push(ConsoleInput::Focus(focus.bSetFocus != 0));
            }
            // VT input reports mouse as SGR sequences; menu records are noise.
            _ => {}
        }
    }
    Ok(read > 0)
}

/// Write one complete frame with as few `WriteConsoleW` calls as the console
/// accepts (normally one). Returns `None` when standard output is not a
/// console or the frame is not UTF-8, so the caller keeps the ordinary writer.
pub(super) fn write_frame(frame: &[u8]) -> Option<io::Result<()>> {
    let handle = standard_handle(STD_OUTPUT_HANDLE)?;
    console_mode(handle)?;
    let units = super::backend::frame_utf16(frame)?;
    Some(write_units(handle, &units))
}

fn write_units(handle: HANDLE, mut units: &[u16]) -> io::Result<()> {
    while !units.is_empty() {
        let requested = u32::try_from(units.len()).unwrap_or(u32::MAX);
        let mut written = 0u32;
        // SAFETY: the pointer and length describe the live `units` slice, and
        // `written` is a valid out pointer. The reserved argument must be null.
        let ok = unsafe {
            WriteConsoleW(
                handle,
                units.as_ptr(),
                requested,
                &mut written,
                std::ptr::null(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        units = &units[(written as usize).min(units.len())..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirected_output_keeps_the_ordinary_writer() {
        // `cargo test` captures or pipes stdout on CI. The frame writer must
        // decline rather than claim a console write it cannot make.
        if standard_handle(STD_OUTPUT_HANDLE)
            .and_then(console_mode)
            .is_none()
        {
            assert!(write_frame(b"frame").is_none());
            assert!(!probe().output_vt);
        }
    }
}
