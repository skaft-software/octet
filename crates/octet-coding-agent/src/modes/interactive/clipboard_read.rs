//! Native **text** clipboard read (parity row 2c.6). Clipboard image capture is
//! an explicit exclusion, so only text ever leaves the clipboard and nothing in
//! this module writes to it. The write transport (the native helper plus OSC 52
//! in `tui/view.rs`) is separate and mirrors this helper order.
//!
//! Every helper is bounded by a deadline and a byte cap, and every failure fails
//! closed. A helper that blocks — a disconnected display, a wedged Wayland
//! compositor, no `pbpaste` on PATH — must never hold the interactive loop.

use std::process::Stdio;
use std::time::Duration;

/// Deadline for one helper process.
const READ_TIMEOUT: Duration = Duration::from_millis(600);
/// Accepted clipboard text. A larger payload fails closed rather than
/// pasting a silently truncated document.
const MAX_TEXT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    MacOs,
    Linux,
    Windows,
}

fn host_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Helper {
    program: String,
    args: Vec<String>,
}

fn helper(program: &str, args: &[&str]) -> Helper {
    Helper {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    }
}

/// Declared helper order. A platform helper is used when it exists, so an
/// environment that declares no display yields no helper at all and the read
/// fails closed instead of scraping an unrelated transport.
fn helpers(platform: Platform, env: impl Fn(&str) -> Option<String>) -> Vec<Helper> {
    match platform {
        Platform::MacOs => vec![helper("pbpaste", &[])],
        // `clip` writes only; PowerShell is the declared text reader.
        Platform::Windows => vec![helper(
            "powershell",
            &["-NoProfile", "-Command", "Get-Clipboard -Raw"],
        )],
        Platform::Linux => {
            let mut helpers = Vec::new();
            if env("TERMUX_VERSION").is_some() {
                helpers.push(helper("termux-clipboard-get", &[]));
            }
            if env("WAYLAND_DISPLAY").is_some() {
                helpers.push(helper("wl-paste", &["--no-newline", "--type", "text"]));
            }
            if env("DISPLAY").is_some() {
                helpers.push(helper("xclip", &["-selection", "clipboard", "-out"]));
                helpers.push(helper("xsel", &["--clipboard", "--output"]));
            }
            helpers
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    /// The helper exited successfully and produced bytes.
    Text(Vec<u8>),
    /// The helper exited successfully and produced nothing.
    Empty,
    /// The helper is missing, failed, timed out, or exceeded the byte cap.
    Failed,
}

/// Decide one helper's contribution. `Err(())` means "try the next helper";
/// every other outcome settles the read, matching the reference loop, where
/// a successful helper that printed nothing reports an empty clipboard
/// rather than leaking into the next transport.
fn settle(outcome: Outcome) -> Result<Option<String>, ()> {
    match outcome {
        Outcome::Failed => Err(()),
        Outcome::Empty => Ok(None),
        Outcome::Text(bytes) => Ok(decode(&bytes)),
    }
}

/// Invalid UTF-8 is replaced rather than dropped, matching the reference
/// reader's `toString("utf8")`. At this point the payload is already
/// bounded, so only a genuinely empty clipboard yields `None`.
fn decode(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_TEXT_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(bytes).into_owned())
}

fn classify(bytes: Vec<u8>) -> Outcome {
    if bytes.len() > MAX_TEXT_BYTES {
        Outcome::Failed
    } else if bytes.is_empty() {
        Outcome::Empty
    } else {
        Outcome::Text(bytes)
    }
}

async fn run(helper: &Helper) -> Outcome {
    let command = tokio::process::Command::new(&helper.program)
        .args(&helper.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        // A helper that ignores the deadline is killed when its future is
        // dropped, so cancellation cannot leak a blocked child.
        .kill_on_drop(true)
        .spawn();
    let Ok(mut child) = command else {
        return Outcome::Failed;
    };
    let Some(mut stdout) = child.stdout.take() else {
        return Outcome::Failed;
    };
    let operation = async move {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut bounded = (&mut stdout).take(MAX_TEXT_BYTES as u64 + 1);
        bounded.read_to_end(&mut bytes).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((bytes, status))
    };
    match tokio::time::timeout(READ_TIMEOUT, operation).await {
        // A helper that reports failure is transport failure, not an empty
        // clipboard: `settle` then tries the next declared helper.
        Ok(Ok((bytes, status))) if status.success() => classify(bytes),
        _ => Outcome::Failed,
    }
}

pub(super) async fn read_text() -> Option<String> {
    #[cfg(test)]
    if let Some(helper) = TEST_HELPER.with(|slot| slot.borrow_mut().take()) {
        return helper.await;
    }
    #[cfg(test)]
    if let Some(overridden) = test_override() {
        return overridden;
    }
    for helper in helpers(host_platform(), |name| std::env::var(name).ok()) {
        if let Ok(text) = settle(run(&helper).await) {
            return text;
        }
    }
    None
}

#[cfg(test)]
type TestHelper = std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>>;

#[cfg(test)]
thread_local! {
    /// One-shot controllable helper; no developer clipboard is accessed.
    static TEST_HELPER: std::cell::RefCell<Option<TestHelper>> =
        const { std::cell::RefCell::new(None) };
    /// The outer `None` means no override; per-thread state isolates tests.
    static OVERRIDE: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(super) fn set_test_helper(
    helper: impl std::future::Future<Output = Option<String>> + Send + 'static,
) {
    TEST_HELPER.with(|slot| *slot.borrow_mut() = Some(Box::pin(helper)));
}

#[cfg(test)]
fn test_override() -> Option<Option<String>> {
    OVERRIDE.with(|slot| slot.borrow().clone())
}

#[cfg(test)]
pub(super) fn set_test_text(text: Option<String>) {
    OVERRIDE.with(|slot| *slot.borrow_mut() = Some(text));
}

#[cfg(test)]
pub(super) fn clear_test_text() {
    OVERRIDE.with(|slot| *slot.borrow_mut() = None);
}

#[cfg(test)]
mod tests;
