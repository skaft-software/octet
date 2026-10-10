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
//! Rust's standard console writer converts at most a few KiB of UTF-8 per
//! `WriteConsoleW` call, splitting one synchronized frame into many console
//! writes that ConPTY can forward, and Windows Terminal can paint, separately.
//! [`write_frame`] hands a complete frame to a single `WriteConsoleW` call.
#![allow(missing_docs)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Console::{
    GetConsoleMode, GetStdHandle, SetConsoleMode, WriteConsoleW, CONSOLE_MODE,
    DISABLE_NEWLINE_AUTO_RETURN, ENABLE_VIRTUAL_TERMINAL_PROCESSING, STD_HANDLE, STD_INPUT_HANDLE,
    STD_OUTPUT_HANDLE,
};

/// Set only when octet added delayed wrapping, so restore never clears a flag
/// the parent shell had already chosen.
static ADDED_DELAYED_WRAP: AtomicBool = AtomicBool::new(false);

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

/// Request VT output with delayed end-of-line wrapping for the renderer.
pub(super) fn enter_interactive() {
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

/// Remove the delayed-wrap flag added by [`enter_interactive`], after the
/// final frame and mode-reset sequences have been written.
pub(super) fn restore() {
    if !ADDED_DELAYED_WRAP.swap(false, Ordering::SeqCst) {
        return;
    }
    if let Some(handle) = standard_handle(STD_OUTPUT_HANDLE) {
        if let Some(mode) = console_mode(handle) {
            let _ = set_console_mode(handle, mode & !DISABLE_NEWLINE_AUTO_RETURN);
        }
    }
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
