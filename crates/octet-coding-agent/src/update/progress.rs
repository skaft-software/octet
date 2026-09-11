//! Monochrome, stderr-owned presentation for the explicit updater only.
//! Unknown work uses an activity bar, never a fabricated percentage or ETA.

use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use sexy_tui_rs::{visible_width, wrap_text_with_ansi};
use tokio::io::AsyncReadExt;

const TICK: Duration = Duration::from_millis(100);

fn interactive() -> bool {
    io::stderr().is_terminal()
        && std::env::var("TERM").is_ok_and(|term| !term.is_empty() && term != "dumb")
}

fn unicode() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .is_some_and(|locale| {
            let locale = locale.to_ascii_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        })
}

fn width() -> usize {
    // Query the stream we paint, not stdout (which may be redirected).
    #[cfg(unix)]
    let columns = {
        let mut size = libc::winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: ioctl writes one winsize to a valid, live pointer. It neither
        // changes terminal modes nor transfers ownership of stderr.
        let valid = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
        if valid && size.ws_col > 0 {
            usize::from(size.ws_col)
        } else {
            80
        }
    };
    #[cfg(not(unix))]
    let columns = crossterm::terminal::size()
        .map(|(columns, _)| usize::from(columns))
        .unwrap_or(80);
    columns.saturating_sub(1).max(1)
}

fn banner_lines(
    version: &semver::Version,
    detail: &str,
    width: usize,
    unicode: bool,
) -> Vec<String> {
    let version = format!("v{version}");
    let mark = if unicode { [' ', '█'] } else { [' ', '#'] };
    let mark_rows = |scale: usize| {
        ["01101111", "11111111"].into_iter().flat_map(move |bits| {
            let row = bits
                .chars()
                .map(|bit| mark[usize::from(bit == '1')].to_string().repeat(scale))
                .collect::<String>();
            std::iter::repeat_n(row, scale)
        })
    };
    let mut lines = vec![String::new()];
    if width >= 42 && visible_width(&version).max(visible_width(detail)) <= width - 21 {
        for (row, text) in mark_rows(2).zip(["octet", &version, detail, ""]) {
            lines.push(format!("  {row}   {text}"));
        }
    } else {
        if width >= 10 {
            lines.extend(mark_rows(1).map(|row| format!("  {row}")));
        }
        for text in ["octet", &version, detail] {
            lines.extend(wrap_text_with_ansi(text, width.max(1)));
        }
    }
    lines.push(String::new());
    lines
}

pub(super) fn banner(version: &semver::Version, detail: &str) {
    let mut stderr = io::stderr().lock();
    if interactive() {
        for line in banner_lines(version, detail, width(), unicode()) {
            let _ = writeln!(stderr, "{line}");
        }
    } else {
        let _ = writeln!(stderr, "octet v{version} - {detail}");
    }
}

fn activity_line(label: &str, width: usize, frame: usize) -> String {
    let available = width.saturating_sub(visible_width(label) + 5);
    if available < 5 {
        let spinner = ['|', '/', '-', '\\'][frame % 4];
        return format!("{spinner} {label}").chars().take(width).collect();
    }
    let cells = available.min(20);
    let travel = cells - 3;
    let position = frame % (travel * 2);
    let start = position.min(travel * 2 - position);
    let bar = (0..cells)
        .map(|cell| {
            if (start..start + 3).contains(&cell) {
                '='
            } else {
                ' '
            }
        })
        .collect::<String>();
    format!("  [{bar}] {label}")
}

pub(super) struct Activity {
    label: &'static str,
    interactive: bool,
    drawn: bool,
    frame: usize,
}

impl Activity {
    pub(super) fn new(label: &'static str) -> Self {
        let mut activity = Self {
            label,
            interactive: interactive(),
            drawn: false,
            frame: 0,
        };
        if activity.interactive {
            activity.draw();
        } else {
            let _ = writeln!(io::stderr(), "{label}...");
        }
        activity
    }

    // Keep the cursor anchored at the live row's beginning after painting.
    // Resize can reflow the activity below that anchor; clearing to end-of-screen
    // then removes only our live tail, never preceding completed diagnostics.
    fn draw(&mut self) {
        if self.interactive {
            let mut stderr = io::stderr().lock();
            let _ = write!(
                stderr,
                "\r\x1b[J{}\r",
                activity_line(self.label, width(), self.frame)
            );
            let _ = stderr.flush();
            self.drawn = true;
            self.frame = self.frame.wrapping_add(1);
        }
    }

    fn clear(&mut self) {
        if self.drawn {
            let mut stderr = io::stderr().lock();
            let _ = write!(stderr, "\r\x1b[J");
            let _ = stderr.flush();
            self.drawn = false;
        }
    }

    pub(super) async fn wait<F: Future>(&mut self, future: F) -> F::Output {
        tokio::pin!(future);
        let mut ticks = tokio::time::interval_at(tokio::time::Instant::now() + TICK, TICK);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                result = &mut future => { self.clear(); return result; }
                _ = ticks.tick(), if self.interactive => self.draw(),
            }
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        self.clear();
    }
}

/// Serialize a child tool's output with the activity row, without retaining its
/// log or treating output frequency as a measure of completed work. Native
/// installers own their own progress and do not pass through this function.
pub(super) async fn command(
    command: &mut tokio::process::Command,
    label: &'static str,
) -> io::Result<ExitStatus> {
    let mut activity = Activity::new(label);
    command.kill_on_drop(true);
    if !activity.interactive {
        return command.status().await;
    }
    // We own the one activity row. Do not stack Cargo/npm's terminal meters on
    // it; ordinary diagnostic lines are forwarded live and unchanged.
    command
        .env("CARGO_TERM_PROGRESS_WHEN", "never")
        .env("CARGO_TERM_COLOR", "never")
        .env("npm_config_progress", "false")
        .stderr(Stdio::piped());
    // A redirected stdout belongs to its consumer. Do not proxy it through
    // synchronous terminal writes and stall the UI on that consumer's pipe.
    let stdout_is_terminal = io::stdout().is_terminal();
    if stdout_is_terminal {
        command.stdout(Stdio::piped());
    }
    let mut child = command.spawn()?;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take().expect("piped stderr");
    let (mut out_open, mut err_open) = (stdout.is_some(), true);
    let (mut out_line_start, mut err_line_start) = (true, true);
    let (mut out_buf, mut err_buf) = ([0; 8192], [0; 8192]);
    let mut status = None;
    let mut drain_deadline = None;
    let mut ticks = tokio::time::interval(TICK);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let status = loop {
        tokio::select! {
            result = child.wait(), if status.is_none() => {
                status = Some(result?);
                drain_deadline = Some(tokio::time::Instant::now() + Duration::from_millis(250));
            }
            _ = async { tokio::time::sleep_until(drain_deadline.expect("child exited")).await }, if status.is_some() => {
                // A background descendant may retain an output descriptor. The
                // updater's completion is owned by the direct package manager,
                // not by indefinitely waiting for that unrelated pipe to close.
                break status.expect("child exited");
            }
            read = async { stdout.as_mut().expect("open stdout").read(&mut out_buf).await }, if out_open => {
                let count = read?;
                out_open = count != 0;
                if count > 0 {
                    activity.clear();
                    let mut output = io::stdout().lock();
                    output.write_all(&out_buf[..count])?;
                    output.flush()?;
                    out_line_start = out_buf[count - 1] == b'\n';
                }
            }
            read = stderr.read(&mut err_buf), if err_open => {
                let count = read?;
                err_open = count != 0;
                if count > 0 {
                    activity.clear();
                    let mut output = io::stderr().lock();
                    output.write_all(&err_buf[..count])?;
                    output.flush()?;
                    err_line_start = err_buf[count - 1] == b'\n';
                }
            }
            _ = ticks.tick(), if status.is_none() => {
                if err_line_start && (!stdout_is_terminal || out_line_start) { activity.draw(); }
            }
        }
        if !out_open && !err_open {
            if let Some(status) = status {
                break status;
            }
        }
    };
    activity.clear();
    if !err_line_start || (stdout_is_terminal && !out_line_start) {
        let _ = writeln!(io::stderr());
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_banner_has_the_canonical_eight_column_mark_and_target_version() {
        for unicode in [false, true] {
            let version = semver::Version::new(0, 7, 5);
            let lines = banner_lines(&version, "Updating from v0.7.4", 80, unicode);
            let glyph = if unicode { '█' } else { '#' };
            let top = format!(
                "  {}  {}",
                glyph.to_string().repeat(4),
                glyph.to_string().repeat(8)
            );
            let bottom = glyph.to_string().repeat(16);
            assert!(lines[1].starts_with(&format!("  {top}   octet")));
            assert!(lines[2].starts_with(&format!("  {top}   v0.7.5")));
            assert!(lines[3].starts_with(&format!("  {bottom}")));
            assert!(lines[4].starts_with(&format!("  {bottom}")));
            assert!(!lines.join("\n").contains('\x1b'));
        }
    }

    #[test]
    fn banner_and_activity_fit_narrow_terminals_without_fabricated_percentages() {
        for width in 1..121 {
            for unicode in [false, true] {
                let lines =
                    banner_lines(&semver::Version::new(0, 7, 5), "Installing", width, unicode);
                assert!(lines.iter().all(|line| visible_width(line) <= width));
                assert!(lines.join("").contains("v0.7.5"));
            }
            assert_ne!(
                activity_line("Verifying installed version", width, 0),
                activity_line("Verifying installed version", width, 1)
            );
            for frame in 0..100 {
                let line = activity_line("Verifying installed version", width, frame);
                assert!(visible_width(&line) <= width);
                assert!(!line.contains('%'));
                assert!(!line.contains("100"));
            }
        }
        assert_ne!(
            activity_line("Checking", 80, 0),
            activity_line("Checking", 80, 5)
        );
    }

    #[tokio::test]
    async fn progress_wait_returns_the_real_result_without_delaying_completion() {
        let mut activity = Activity {
            label: "Checking",
            interactive: false,
            drawn: false,
            frame: 0,
        };
        let result = tokio::time::timeout(Duration::from_secs(1), activity.wait(async { 42 }))
            .await
            .unwrap();
        assert_eq!(result, 42);
        assert!(!activity.drawn);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn progress_subprocess_fixture() {
        let Ok(mode) = std::env::var("OCTET_TEST_UPDATE_UI") else {
            return;
        };
        if mode.starts_with("fetch-") {
            let result = super::super::run_installer(&semver::Version::new(0, 7, 5)).await;
            if mode == "fetch-ok" {
                assert!(result.unwrap().success());
            } else {
                let error = result.unwrap_err().to_string();
                if mode == "fetch-failed" {
                    assert!(error.contains("22"));
                    assert!(error.contains("/v0.7.5/install-octet.sh"));
                }
            }
            return;
        }
        banner(&semver::Version::new(0, 7, 5), "Updating from v0.7.4");
        let mut child = tokio::process::Command::new("sh");
        child.arg("-c").arg(if mode == "interrupt" {
            "exec sleep 30"
        } else if mode == "orphan-pipe" {
            "sleep 30 & exit 9"
        } else if mode == "backpressure" {
            "dd if=/dev/zero bs=1048576 count=1 2>/dev/null; exit 9"
        } else if mode == "closed-pipes" {
            "exec 1>&- 2>&-; sleep 0.4; exit 9"
        } else {
            "printf 'probe stdout\n'; printf 'probe stderr\n' >&2; printf partial >&2; sleep 0.25; printf ' line\n' >&2; sleep 0.6; exit 9"
        });
        let status = command(&mut child, "Installing fixture").await.unwrap();
        assert_eq!(status.code(), Some(9));
    }

    #[cfg(unix)]
    #[test]
    fn actual_updater_progress_pty_and_plain_streams() {
        let output = std::process::Command::new("python3")
            .arg("-c")
            .arg(include_str!("progress_pty.py"))
            .arg(std::env::current_exe().unwrap())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
