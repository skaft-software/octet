//! Shared teardown for PTY tests whose child owns the PTY as its controlling
//! terminal.

use std::fs::File;
use std::io::Read;
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant};

/// Kill `child` and reap it while draining `master` (which must be
/// non-blocking).
///
/// On macOS an exiting session leader waits for its controlling terminal's
/// output queue to drain, even after SIGKILL. A blocking `wait()` while the
/// test still holds an unread master therefore deadlocks, which hides the
/// assertion that triggered the teardown until the whole suite times out.
///
/// Never panics, so it is safe from `Drop` during unwinding. Returns whether
/// the child was reaped before `timeout`.
pub fn kill_and_reap(child: &mut Child, master: &mut File, timeout: Duration) -> bool {
    let _ = child.kill();
    let deadline = Instant::now() + timeout;
    let mut buffer = [0u8; 8192];
    loop {
        for _ in 0..8 {
            match master.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
        if child.try_wait().ok().flatten().is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            eprintln!(
                "killed PTY child {} did not exit within {timeout:?}",
                child.id()
            );
            return false;
        }
        thread::sleep(Duration::from_millis(2));
    }
}
