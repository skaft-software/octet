#![allow(missing_docs)]

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::LazyLock;

use super::lifecycle::force_restore;

static SIGNAL_NUMBER: AtomicI32 = AtomicI32::new(0);
static SIGNAL_NOTIFY: LazyLock<tokio::sync::Notify> = LazyLock::new(tokio::sync::Notify::new);
const SIGNAL_WATCHDOG_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(2500);

/// Returns the first Unix termination signal received by the process.
pub async fn wait_for_shutdown_signal() -> i32 {
    loop {
        let notified = SIGNAL_NOTIFY.notified();
        let signal = SIGNAL_NUMBER.load(Ordering::Acquire);
        if signal != 0 {
            return signal;
        }
        notified.await;
    }
}

/// Returns the pending Unix termination signal, if coordinated shutdown began.
pub fn received_shutdown_signal() -> Option<i32> {
    let signal = SIGNAL_NUMBER.load(Ordering::Acquire);
    (signal != 0).then_some(signal)
}

fn conventional_signal_exit_code(signal: i32) -> i32 {
    128i32.saturating_add(signal)
}

fn emergency_signal_exit(signal: i32) -> ! {
    octet_agent::extension_process::force_kill_registered_process_groups();
    force_restore();
    std::process::exit(conventional_signal_exit_code(signal));
}

/// Begin the same coordinated shutdown path used by the Unix signal thread.
///
/// Raw terminal input turns Ctrl-C into a key event instead of a kernel
/// signal. Long-running lifecycle boundaries use this helper so that key is
/// still an immediate, level-triggered cancellation request with the same
/// bounded cleanup watchdog and conventional exit status as SIGINT.
pub fn request_coordinated_shutdown(signal: i32) -> std::io::Result<()> {
    if signal <= 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "shutdown signal must be positive",
        ));
    }
    if SIGNAL_NUMBER
        .compare_exchange(0, signal, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        emergency_signal_exit(SIGNAL_NUMBER.load(Ordering::Acquire));
    }

    octet_agent::extension_process::begin_host_shutdown();
    SIGNAL_NOTIFY.notify_waiters();
    std::thread::Builder::new()
        .name("octet-signal-watchdog".into())
        .spawn(move || {
            std::thread::sleep(SIGNAL_WATCHDOG_TIMEOUT);
            emergency_signal_exit(signal);
        })?;
    Ok(())
}

/// Exit with the shell-conventional `128 + signal` status after cleanup.
pub fn exit_if_signaled() {
    if let Some(signal) = received_shutdown_signal() {
        emergency_signal_exit(signal);
    }
}

/// Installs level-triggered Unix termination handling.
///
/// The signal thread only announces shutdown. Async mode owners then stop
/// input, abort active runs, await process groups, stop extensions, and restore
/// the terminal. A short watchdog force-cleans children and terminal state if
/// an owner stalls; a repeated signal takes that emergency path immediately.
#[cfg(unix)]
pub fn install_signal_restore() -> std::io::Result<()> {
    use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGHUP, SIGINT, SIGQUIT, SIGTERM])?;
    std::thread::Builder::new()
        .name("octet-signal-restore".into())
        .spawn(move || {
            for signal in signals.forever() {
                request_coordinated_shutdown(signal)?;
            }
            Ok::<(), std::io::Error>(())
        })?;
    Ok(())
}

/// Installs Windows console control handling with the same coordinated
/// shutdown as the Unix signals.
///
/// Raw mode turns Ctrl-C into a key event, but Ctrl-Break, closing the console
/// window, logoff, shutdown, and Ctrl-C in plain or print mode still arrive as
/// control events. Without a handler Windows ends the process immediately and
/// leaves the console in raw mode for the parent shell.
#[cfg(windows)]
pub fn install_signal_restore() -> std::io::Result<()> {
    use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;

    // SAFETY: the handler is a 'static function with the documented
    // PHANDLER_ROUTINE signature and stays valid for the process lifetime.
    if unsafe { SetConsoleCtrlHandler(Some(console_control_handler), 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
unsafe extern "system" fn console_control_handler(kind: u32) -> windows_sys::core::BOOL {
    use windows_sys::Win32::System::Console::{
        CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
    };

    let Some(signal) = windows_control_signal(kind) else {
        return 0;
    };
    if request_coordinated_shutdown(signal).is_err() {
        return 0;
    }
    if matches!(
        kind,
        CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT
    ) {
        // Windows ends the process as soon as this handler returns for these
        // events. Give coordinated cleanup its watchdog window first; the
        // watchdog or the mode owner exits the process before this returns.
        std::thread::sleep(SIGNAL_WATCHDOG_TIMEOUT + std::time::Duration::from_millis(250));
    }
    1
}

#[cfg(not(any(unix, windows)))]
pub fn install_signal_restore() -> std::io::Result<()> {
    Ok(())
}

/// Map a Windows console control event to the signal number used for the
/// conventional `128 + signal` exit status. Unknown events keep Windows'
/// default handling.
#[cfg_attr(not(windows), allow(dead_code))]
fn windows_control_signal(kind: u32) -> Option<i32> {
    const SIGHUP: i32 = 1;
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIGBREAK: i32 = 21;
    match kind {
        0 => Some(SIGINT),      // CTRL_C_EVENT
        1 => Some(SIGBREAK),    // CTRL_BREAK_EVENT
        2 => Some(SIGHUP),      // CTRL_CLOSE_EVENT: the console went away
        5 | 6 => Some(SIGTERM), // CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT
        _ => None,
    }
}

#[cfg(test)]
#[path = "signal_tests.rs"]
mod tests;
