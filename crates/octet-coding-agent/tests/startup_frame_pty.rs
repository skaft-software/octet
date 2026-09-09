#![cfg(unix)]

//! Deterministic PTY/frame regression coverage for primary-screen startup.
//!
//! The real binary is run against a disposable HOME, workspace, session store,
//! and a local custom-provider record. Startup tests submit no prompt. API-wait
//! and plain-prompt tests use only a gated loopback fixture, never credentials
//! or a live model.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::ops::Range;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use sexy_tui_rs::{ColorDepth, Component, Terminal, TerminalCapabilities, TerminalInput, TUI};
use tempfile::TempDir;

const INITIAL_COLUMNS: u16 = 96;
const INITIAL_ROWS: u16 = 18;
const RESIZED_COLUMNS: u16 = 64;
const RESIZED_ROWS: u16 = 12;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
const DRAIN_TIME: Duration = Duration::from_millis(35);
const FRAME_BEGIN: &[u8] = b"\x1b[?2026h";
const FRAME_END: &[u8] = b"\x1b[?2026l";
const STALE_MARKER: &str = "OCTET_PTY_STALE_STARTUP";
const READY_MARKER: &[u8] = b"custom/probe";
const PREEXISTING_STARTUP: &[u8] =
    include_bytes!("fixtures/startup-frame-pty/preexisting-startup.txt");
const INLINE_STARTUP: &str = include_str!("fixtures/startup-frame-pty/inline-startup.txt");
const INLINE_READY: &str = include_str!("fixtures/startup-frame-pty/inline-ready.txt");

fn pty_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MouseMode {
    Auto,
    App,
}

impl MouseMode {
    const fn as_arg(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::App => "app",
        }
    }
}

/// PTY master/slave pair with a nonblocking transcript of bytes seen by the
/// terminal. The slave stays open in the parent so terminal attributes can be
/// checked after the child exits.
struct Pty {
    master: File,
    slave: File,
    original_termios: libc::termios,
    output: Vec<u8>,
}

impl Pty {
    fn open(columns: u16, rows: u16) -> Self {
        let mut master_fd = -1;
        let mut slave_fd = -1;
        let mut dimensions = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let opened = unsafe {
            // macOS declares `winp` mutable while Linux declares it const.
            #[allow(clippy::unnecessary_mut_passed)]
            libc::openpty(
                &mut master_fd,
                &mut slave_fd,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut dimensions,
            )
        };
        assert_eq!(opened, 0, "openpty failed: {}", io::Error::last_os_error());
        set_close_on_exec(master_fd);
        set_close_on_exec(slave_fd);
        set_nonblocking(master_fd);

        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        let original_termios = terminal_attributes(slave.as_raw_fd());
        Self {
            master,
            slave,
            original_termios,
            output: Vec::new(),
        }
    }

    fn seed_startup_rows(&mut self) {
        self.slave
            .write_all(PREEXISTING_STARTUP)
            .expect("write preexisting PTY rows");
        self.slave.flush().expect("flush preexisting PTY rows");
        self.wait_for_bytes(Duration::from_secs(1), b"OCTET_PTY_STALE_STARTUP_B");
    }

    fn write_input(&mut self, input: &[u8]) {
        self.master.write_all(input).expect("write PTY input");
        self.master.flush().expect("flush PTY input");
    }

    fn set_size(&self, columns: u16, rows: u16) {
        let dimensions = libc::winsize {
            ws_row: rows,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let result = unsafe {
            libc::ioctl(
                self.slave.as_raw_fd(),
                libc::TIOCSWINSZ,
                &dimensions as *const libc::winsize,
            )
        };
        assert_eq!(
            result,
            0,
            "TIOCSWINSZ failed: {}",
            io::Error::last_os_error()
        );
    }

    fn duplicate_slave(&self) -> File {
        let duplicated = unsafe { libc::dup(self.slave.as_raw_fd()) };
        assert!(
            duplicated >= 0,
            "dup PTY slave failed: {}",
            io::Error::last_os_error()
        );
        unsafe { File::from_raw_fd(duplicated) }
    }

    fn wait_for_bytes(&mut self, timeout: Duration, needle: &[u8]) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.read_available();
            if contains_bytes(&self.output, needle) {
                return;
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "PTY did not receive {needle:?}; transcript: {}",
            visible_bytes(&self.output)
        );
    }

    fn drain_for(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;
        while Instant::now() < deadline {
            self.read_available();
            thread::sleep(Duration::from_millis(2));
        }
        self.read_available();
    }

    fn read_available(&mut self) {
        let mut buffer = [0u8; 8192];
        loop {
            match self.master.read(&mut buffer) {
                Ok(0) => return,
                Ok(read) => self.output.extend_from_slice(&buffer[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
                // PTY masters commonly report EIO after the last slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => return,
                Err(error) => panic!("read PTY: {error}"),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum StartupFixture<'a> {
    Model(&'a str),
    /// A persisted selection, resolved through the real registry with no auth.
    ConfiguredGemma,
    /// Empty inventory and no appearance preference: both onboarding owners run.
    Setup,
}

struct PtyOctet {
    child: Child,
    pty: Pty,
    _root: TempDir,
}

impl PtyOctet {
    fn spawn(binary: &Path, mode: MouseMode) -> Self {
        Self::spawn_configured(binary, mode, None, false)
    }

    fn spawn_configured(binary: &Path, mode: MouseMode, api: Option<&str>, color: bool) -> Self {
        Self::spawn_at(
            binary,
            mode,
            api,
            color,
            (INITIAL_COLUMNS, INITIAL_ROWS),
            (2, false, false),
            StartupFixture::Model("probe"),
        )
    }

    fn spawn_at(
        binary: &Path,
        mode: MouseMode,
        api: Option<&str>,
        color: bool,
        dimensions: (u16, u16),
        start: (u16, bool, bool),
        fixture: StartupFixture<'_>,
    ) -> Self {
        let root = tempfile::tempdir().expect("PTY test tempdir");
        // The CLI resolves the workspace physically. Give HOME the same path
        // identity, including macOS's /var -> /private/var temporary-directory
        // alias, so a workspace beneath HOME is faithfully displayed as ~/….
        let canonical_root = root
            .path()
            .canonicalize()
            .expect("canonical PTY fixture root");
        let home = canonical_root.join("home");
        let workspace = match fixture {
            StartupFixture::Model(_) => canonical_root.join("workspace"),
            _ => home.join("workspace"),
        };
        let sessions = canonical_root.join("sessions");
        create_inert_environment(&home, &workspace, &sessions);
        let credential = home.join(".octet/credentials/custom.json");
        match fixture {
            StartupFixture::Model(model) if api.is_some() || model != "probe" => {
                let base_url = api.unwrap_or("http://127.0.0.1:9/v1/");
                let record = serde_json::json!({
                    "base_url": base_url, "api_key": "", "api_name": model,
                    "headers": [], "auto_discover": false,
                    // The composed-redraw fixture needs genuinely distinct status
                    // values now that successful changes do not append notices.
                    "models": if model == "qwen-3.8-27b" {
                        vec![serde_json::json!({
                            "api_name": model,
                            "reasoning": true, "reasoning_values": ["off", "low", "high"],
                            "reasoning_default": "off",
                        })]
                    } else { vec![] },
                });
                fs::write(&credential, record.to_string()).expect("loopback provider fixture");
            }
            StartupFixture::ConfiguredGemma => {
                let record = serde_json::json!({
                    "version": 1,
                    "providers": {"cerebras": {
                        "label": "Cerebras", "base_url": "http://127.0.0.1:9/v1/",
                        "auth": {"kind": "none"}, "auto_discover": false,
                        "models": [{"api_name": "gemma-4-31b", "display_name": "Gemma 4 31B"}]
                    }}
                });
                fs::write(&credential, record.to_string()).expect("offline Cerebras/Gemma fixture");
                fs::write(
                    home.join(".octet/config.toml"),
                    "model = \"custom/cerebras/gemma-4-31b\"\ntheme = \"dark\"\n",
                )
                .expect("persisted model and appearance fixture");
            }
            StartupFixture::Setup => {
                fs::remove_file(&credential).expect("empty disposable provider inventory");
            }
            StartupFixture::Model(_) => {}
        }

        let mut pty = Pty::open(dimensions.0, dimensions.1);
        // Rows exist before the child is exec'd, exactly as stale shell output
        // does at an interactive startup boundary.
        pty.seed_startup_rows();
        if start.1 {
            // An inherited DECSTBM region survives ED2 and CUP.
            write!(pty.slave, "\x1b[3;{}r", dimensions.1 - 1).unwrap();
        }
        if start.2 {
            pty.slave.write_all(b"\x1b[?6h").unwrap();
        }
        write!(pty.slave, "\x1b[{};7H", start.0 + 1).expect("seed starting cursor");
        pty.slave.flush().expect("flush starting cursor");

        let stdin = duplicate_stdio(pty.slave.as_raw_fd());
        let stdout = duplicate_stdio(pty.slave.as_raw_fd());
        let stderr = duplicate_stdio(pty.slave.as_raw_fd());
        let tty_fd = pty.slave.as_raw_fd();
        let mut command = Command::new(binary);
        command
            .args([
                "--offline",
                "--no-context-files",
                "--no-tools",
                "--color",
                if !color {
                    "never"
                } else if matches!(fixture, StartupFixture::ConfiguredGemma) {
                    "auto"
                } else {
                    "always"
                },
                "--mouse",
                mode.as_arg(),
                "--workspace",
            ])
            .arg(&workspace)
            .arg("--session-dir")
            .arg(&sessions)
            .current_dir(&workspace)
            .env_clear()
            .env("HOME", &home)
            .env("PATH", "/usr/bin:/bin")
            .env("PWD", &workspace)
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .env("LANG", "C.UTF-8")
            .env("OCTET_COLOR_SCHEME", "dark")
            .stdin(stdin)
            .stdout(stdout)
            .stderr(stderr);

        match fixture {
            StartupFixture::Model(model) => {
                command.args(["--model", &format!("custom/{model}")]);
            }
            StartupFixture::ConfiguredGemma | StartupFixture::Setup => {
                // SSH is transport, not evidence of limited terminal colour.
                command
                    .env("TERM_PROGRAM", "ghostty")
                    .env("SSH_CONNECTION", "192.0.2.1 12345 192.0.2.2 22");
                if matches!(fixture, StartupFixture::Setup) {
                    // Reliable background signal avoids an OSC dependency while
                    // leaving first-run appearance onboarding unconfigured.
                    command
                        .env_remove("OCTET_COLOR_SCHEME")
                        .env("COLORFGBG", "15;0");
                }
            }
        }

        // `openpty` alone does not make the slave a controlling terminal. A
        // session/controlling TTY makes the resize path match a real shell.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                if libc::ioctl(tty_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().expect("spawn octet under PTY");
        Self {
            child,
            pty,
            _root: root,
        }
    }

    fn wait_until(&mut self, timeout: Duration, predicate: impl Fn(&[u8]) -> bool) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            self.pty.read_available();
            if predicate(&self.pty.output) {
                return;
            }
            if let Some(status) = self.child.try_wait().expect("poll octet") {
                panic!(
                    "octet exited before PTY condition ({status}); transcript: {}",
                    visible_bytes(&self.pty.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        }
        panic!(
            "PTY condition timed out; transcript: {}",
            visible_bytes(&self.pty.output)
        );
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        self.pty.set_size(columns, rows);
        let signaled = unsafe { libc::kill(self.child.id() as i32, libc::SIGWINCH) };
        assert_eq!(
            signaled,
            0,
            "SIGWINCH failed: {}",
            io::Error::last_os_error()
        );
    }

    fn shutdown(mut self) -> ShutdownCapture {
        let shutdown_start = self.pty.output.len();
        self.pty.write_input(&[4]); // Ctrl-D
        let started = Instant::now();
        let status = loop {
            self.pty.read_available();
            if let Some(status) = self.child.try_wait().expect("poll Ctrl-D shutdown") {
                break status;
            }
            if started.elapsed() >= SHUTDOWN_TIMEOUT {
                unsafe {
                    let _ = libc::kill(self.child.id() as i32, libc::SIGKILL);
                }
                let _ = self.child.wait();
                panic!(
                    "octet did not stop after Ctrl-D within {SHUTDOWN_TIMEOUT:?}; transcript: {}",
                    visible_bytes(&self.pty.output)
                );
            }
            thread::sleep(Duration::from_millis(5));
        };
        self.pty.drain_for(DRAIN_TIME);
        // A controlling-terminal session exit revokes the parent-held slave on
        // macOS (tcgetattr returns ENOTTY), while the PTY master continues to
        // expose the same terminal mode state on both macOS and Linux.
        let restored = terminal_modes_equal(
            &self.pty.original_termios,
            &terminal_attributes(self.pty.master.as_raw_fd()),
        );
        ShutdownCapture {
            output: std::mem::take(&mut self.pty.output),
            shutdown_start,
            status,
            termios_restored: restored,
        }
    }
}

impl Drop for PtyOctet {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            unsafe {
                let _ = libc::kill(self.child.id() as i32, libc::SIGKILL);
            }
            let _ = self.child.wait();
        }
    }
}

struct ShutdownCapture {
    output: Vec<u8>,
    shutdown_start: usize,
    status: ExitStatus,
    termios_restored: bool,
}

#[derive(Clone, Debug)]
struct PrimaryTrace {
    mode: MouseMode,
    startup_clear_screen: bool,
    first_frame_synchronized: bool,
    first_frame_stale_rows: bool,
    ready_frame_stale_rows: bool,
    resize_redraw_synchronized: bool,
    resize_clear_screen: bool,
    resize_clear_saved_lines: bool,
    resize_stale_rows: bool,
    shutdown_cursor_visible: bool,
    shutdown_bracketed_paste_disabled: bool,
    shutdown_termios_restored: bool,
    shutdown_cursor_position_valid: bool,
    shutdown_mouse_restored: bool,
    alternate_screen_used: bool,
    frames_balanced: bool,
    mouse_capture: bool,
    first_screen: String,
    ready_screen: String,
    resize_screen: String,
    shutdown_screen: String,
    output_len: usize,
}

impl PrimaryTrace {
    fn normalized(&self) -> String {
        format!(
            "mode={}\nstartup.clear_screen={}\nstartup.first_full_frame.synchronized={}\nstartup.first_full_frame.stale_rows={}\nstartup.ready_frame.stale_rows={}\nresize.redraw.synchronized={}\nresize.clear_screen={}\nresize.clear_saved_lines={}\nresize.stale_rows={}\nshutdown.cursor_visible={}\nshutdown.bracketed_paste_disabled={}\nshutdown.termios_restored={}\nshutdown.cursor_position_valid={}\nshutdown.mouse_restored={}\nalternate_screen.used={}\nframes.balanced={}\nmouse.capture={}\n",
            self.mode.as_arg(),
            self.startup_clear_screen,
            self.first_frame_synchronized,
            self.first_frame_stale_rows,
            self.ready_frame_stale_rows,
            self.resize_redraw_synchronized,
            self.resize_clear_screen,
            self.resize_clear_saved_lines,
            self.resize_stale_rows,
            self.shutdown_cursor_visible,
            self.shutdown_bracketed_paste_disabled,
            self.shutdown_termios_restored,
            self.shutdown_cursor_position_valid,
            self.shutdown_mouse_restored,
            self.alternate_screen_used,
            self.frames_balanced,
            self.mouse_capture,
        )
    }

    fn debug_report(&self) -> String {
        format!(
            "normalized:\n{}bytes={}\nfirst screen:\n{}\nready screen:\n{}\nresize screen:\n{}\nshutdown screen:\n{}",
            self.normalized(),
            self.output_len,
            self.first_screen,
            self.ready_screen,
            self.resize_screen,
            self.shutdown_screen,
        )
    }
}

fn run_primary(binary: &Path, mode: MouseMode) -> PrimaryTrace {
    let mut octet = PtyOctet::spawn(binary, mode);
    octet.wait_until(STARTUP_TIMEOUT, |output| nth_frame_end(output, 1).is_some());
    let first_end = nth_frame_end(&octet.pty.output, 1).expect("first synchronized frame");
    octet.wait_until(STARTUP_TIMEOUT, |output| {
        synchronized_frame_end_containing(output, READY_MARKER).is_some()
    });
    let ready_end = synchronized_frame_end_containing(&octet.pty.output, READY_MARKER)
        .expect("ready synchronized frame");
    let raw = terminal_attributes(octet.pty.slave.as_raw_fd());
    assert_eq!(
        raw.c_lflag & (libc::ICANON | libc::ECHO),
        0,
        "octet did not enter raw terminal mode"
    );

    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
    parser.process(&octet.pty.output[..first_end]);
    let first_screen = screen_text(&parser, INITIAL_COLUMNS);
    parser.process(&octet.pty.output[first_end..ready_end]);
    let ready_screen = screen_text(&parser, INITIAL_COLUMNS);

    octet.pty.drain_for(DRAIN_TIME);
    let resize_start = octet.pty.output.len();
    octet.resize(RESIZED_COLUMNS, RESIZED_ROWS);
    octet.wait_until(STARTUP_TIMEOUT, |output| {
        output
            .get(resize_start..)
            .and_then(|bytes| synchronized_frame_end_containing(bytes, b"\x1b[2J"))
            .is_some()
    });
    let resize_end = resize_start
        + synchronized_frame_end_containing(&octet.pty.output[resize_start..], b"\x1b[2J")
            .expect("resize redraw frame");
    let (resize_redraw_synchronized, resize_clear_screen, resize_clear_saved_lines) = {
        let resize_frame = &octet.pty.output[resize_start..resize_end];
        parser.process(&octet.pty.output[ready_end..resize_start]);
        parser.set_size(RESIZED_ROWS, RESIZED_COLUMNS);
        parser.process(resize_frame);
        (
            contains_bytes(resize_frame, FRAME_BEGIN) && contains_bytes(resize_frame, FRAME_END),
            contains_bytes(resize_frame, b"\x1b[2J"),
            contains_bytes(resize_frame, b"\x1b[3J"),
        )
    };
    let resize_screen = screen_text(&parser, RESIZED_COLUMNS);

    let shutdown = octet.shutdown();
    assert_eq!(shutdown.status.code(), Some(0), "Ctrl-D exit status");
    parser.process(&shutdown.output[resize_end..shutdown.shutdown_start]);
    parser.process(&shutdown.output[shutdown.shutdown_start..]);
    let shutdown_screen = screen_text(&parser, RESIZED_COLUMNS);
    let restore = &shutdown.output[shutdown.shutdown_start..];
    let cursor = parser.screen().cursor_position();

    PrimaryTrace {
        mode,
        startup_clear_screen: contains_bytes(&shutdown.output[..first_end], b"\x1b[2J"),
        first_frame_synchronized: nth_frame_end(&shutdown.output, 1).is_some(),
        first_frame_stale_rows: first_screen.contains(STALE_MARKER),
        ready_frame_stale_rows: ready_screen.contains(STALE_MARKER),
        resize_redraw_synchronized,
        resize_clear_screen,
        resize_clear_saved_lines,
        resize_stale_rows: resize_screen.contains(STALE_MARKER),
        shutdown_cursor_visible: contains_bytes(restore, b"\x1b[?25h")
            && !parser.screen().hide_cursor(),
        shutdown_bracketed_paste_disabled: contains_bytes(restore, b"\x1b[?2004l")
            && !parser.screen().bracketed_paste(),
        shutdown_termios_restored: shutdown.termios_restored,
        shutdown_cursor_position_valid: cursor.0 < RESIZED_ROWS && cursor.1 < RESIZED_COLUMNS,
        shutdown_mouse_restored: contains_bytes(restore, b"\x1b[?1000l")
            && contains_bytes(restore, b"\x1b[?1006l"),
        alternate_screen_used: uses_alternate_screen(&shutdown.output)
            || parser.screen().alternate_screen(),
        frames_balanced: count_bytes(&shutdown.output, FRAME_BEGIN)
            == count_bytes(&shutdown.output, FRAME_END),
        mouse_capture: contains_bytes(&shutdown.output, b"\x1b[?1000h")
            && contains_bytes(&shutdown.output, b"\x1b[?1006h"),
        first_screen,
        ready_screen,
        resize_screen,
        shutdown_screen,
        output_len: shutdown.output.len(),
    }
}

/// A direct `sexy-tui-rs` PTY backend covers the explicit legacy inline
/// compatibility path. octet's real shell intentionally uses the primary-screen
/// Pi renderer, so this is the narrowest faithful way to keep inline behavior
/// in the same byte-level lane.
struct InlinePtyTerminal {
    writer: File,
    capabilities: TerminalCapabilities,
    columns: u16,
    rows: u16,
}

impl InlinePtyTerminal {
    fn write_bytes(&mut self, bytes: &[u8]) {
        self.writer
            .write_all(bytes)
            .expect("write inline PTY bytes");
        self.writer.flush().expect("flush inline PTY bytes");
    }
}

impl Terminal for InlinePtyTerminal {
    fn start_events(
        &mut self,
        _on_input: Box<dyn FnMut(TerminalInput)>,
        _on_resize: Box<dyn FnMut()>,
    ) {
    }

    fn stop(&mut self) {}

    fn write(&mut self, data: &str) {
        self.write_bytes(data.as_bytes());
    }

    fn columns(&self) -> u16 {
        self.columns
    }

    fn rows(&self) -> u16 {
        self.rows
    }

    fn move_by(&mut self, lines: i16) {
        match lines.cmp(&0) {
            std::cmp::Ordering::Less => {
                self.write_bytes(format!("\x1b[{}A", lines.unsigned_abs()).as_bytes())
            }
            std::cmp::Ordering::Greater => self.write_bytes(format!("\x1b[{lines}B").as_bytes()),
            std::cmp::Ordering::Equal => {}
        }
    }

    fn hide_cursor(&mut self) {
        self.write_bytes(b"\x1b[?25l");
    }

    fn show_cursor(&mut self) {
        self.write_bytes(b"\x1b[?25h");
    }

    fn clear_line(&mut self) {
        self.write_bytes(b"\x1b[0m\x1b[2K");
    }

    fn clear_from_cursor(&mut self) {
        self.write_bytes(b"\x1b[0m\x1b[J");
    }

    fn clear_screen(&mut self) {
        self.write_bytes(b"\x1b[0m\x1b[2J");
    }

    fn capabilities(&self) -> TerminalCapabilities {
        self.capabilities
    }
}

struct MutableFixtureLines {
    lines: Arc<Mutex<Vec<String>>>,
}

impl Component for MutableFixtureLines {
    fn render(&self, _width: u16) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn invalidate(&mut self) {}
}

#[derive(Debug)]
struct InlineTrace {
    startup_synchronized: bool,
    preexisting_rows_preserved: bool,
    shrink_stale_startup_rows: bool,
    shutdown_cursor_visible: bool,
    alternate_screen_used: bool,
    startup_screen: String,
    shrink_screen: String,
}

impl InlineTrace {
    fn normalized(&self) -> String {
        format!(
            "mode=legacy-inline\nstartup.synchronized={}\nstartup.preexisting_rows={}\nshrink.stale_startup_rows={}\nshutdown.cursor_visible={}\nalternate_screen.used={}\n",
            self.startup_synchronized,
            if self.preexisting_rows_preserved {
                "preserved"
            } else {
                "discarded"
            },
            self.shrink_stale_startup_rows,
            self.shutdown_cursor_visible,
            self.alternate_screen_used,
        )
    }

    fn debug_report(&self) -> String {
        format!(
            "normalized:\n{}startup screen:\n{}\nshrink screen:\n{}",
            self.normalized(),
            self.startup_screen,
            self.shrink_screen,
        )
    }
}

fn run_inline() -> InlineTrace {
    let mut pty = Pty::open(INITIAL_COLUMNS, INITIAL_ROWS);
    pty.seed_startup_rows();
    let lines = Arc::new(Mutex::new(fixture_lines(INLINE_STARTUP)));
    let mut capabilities = TerminalCapabilities::interactive(ColorDepth::None, true);
    capabilities.synchronized_output = true;
    capabilities.sync_output = true;
    capabilities.animation = false;
    let terminal = InlinePtyTerminal {
        writer: pty.duplicate_slave(),
        capabilities,
        columns: INITIAL_COLUMNS,
        rows: INITIAL_ROWS,
    };
    let mut tui = TUI::new(Box::new(terminal));
    tui.set_inline_scrollback(true);
    tui.add_child(Box::new(MutableFixtureLines {
        lines: lines.clone(),
    }));
    tui.start();
    pty.drain_for(DRAIN_TIME);
    let startup_end = pty.output.len();

    *lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = fixture_lines(INLINE_READY);
    tui.request_render();
    pty.drain_for(DRAIN_TIME);
    let shrink_end = pty.output.len();
    tui.stop();
    pty.drain_for(DRAIN_TIME);

    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
    parser.process(&pty.output[..startup_end]);
    let startup_screen = screen_text(&parser, INITIAL_COLUMNS);
    parser.process(&pty.output[startup_end..shrink_end]);
    let shrink_screen = screen_text(&parser, INITIAL_COLUMNS);
    parser.process(&pty.output[shrink_end..]);
    let first_clear = find_subsequence(&pty.output[..startup_end], b"\x1b[2J", 0)
        .expect("inline first paint clears its viewport");
    let before_first_clear = &pty.output[..first_clear];

    InlineTrace {
        startup_synchronized: !frame_ranges(&pty.output[..startup_end]).is_empty(),
        // Inline first paint scrolls the old viewport into native history before
        // clearing the mutable viewport. It deliberately must not send ED 3.
        preexisting_rows_preserved: !contains_bytes(before_first_clear, b"\x1b[3J")
            && before_first_clear
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count()
                >= usize::from(INITIAL_ROWS),
        shrink_stale_startup_rows: shrink_screen.contains("OCTET_PTY_INLINE_STARTUP"),
        shutdown_cursor_visible: contains_bytes(&pty.output[shrink_end..], b"\x1b[?25h")
            && !parser.screen().hide_cursor(),
        alternate_screen_used: uses_alternate_screen(&pty.output)
            || parser.screen().alternate_screen(),
        startup_screen,
        shrink_screen,
    }
}

#[test]
fn real_octet_startup_frame_pty_contract() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let current = Path::new(env!("CARGO_BIN_EXE_octet"));
    let current_auto = run_primary(current, MouseMode::Auto);
    let current_app = run_primary(current, MouseMode::App);

    if let Some(baseline) = std::env::var_os("OCTET_STARTUP_FRAME_BASELINE") {
        let baseline = PathBuf::from(baseline);
        let version = baseline_version(&baseline);
        let baseline_auto = run_primary(&baseline, MouseMode::Auto);
        let baseline_app = run_primary(&baseline, MouseMode::App);
        for (mode, current, baseline) in [
            ("auto", &current_auto, &baseline_auto),
            ("app", &current_app, &baseline_app),
        ] {
            assert_baseline_structural_compatibility(mode, current, baseline);
            eprintln!(
                "startup-frame-pty v0.6.7 comparison ({mode}, {version}):\n{}",
                normalized_delta(current, baseline)
            );
        }
    }

    assert_trace_fixture(
        "primary auto",
        &current_auto.normalized(),
        include_str!("fixtures/startup-frame-pty/primary-auto.trace"),
        &current_auto.debug_report(),
    );
    assert_trace_fixture(
        "primary app",
        &current_app.normalized(),
        include_str!("fixtures/startup-frame-pty/primary-app.trace"),
        &current_app.debug_report(),
    );
}

fn assert_unbranded_startup(parser: &vt100::Parser, columns: u16) {
    let text = screen_text(parser, columns);
    assert!(!text.contains("octet v"), "premature branded frame\n{text}");
    assert!(!text.contains('█'), "premature byte mark\n{text}");
    assert!(!text.contains("selecting model"), "{text}");
    assert!(!text.contains("workspace unavailable"), "{text}");
    assert!(!text.contains(STALE_MARKER), "{text}");
}

fn assert_green_gemma_frame(parser: &vt100::Parser) {
    let text = screen_text(parser, INITIAL_COLUMNS);
    assert_single_welcome(parser, INITIAL_COLUMNS, "first-ready Gemma");
    assert!(text.contains("Gemma 4 31B"), "{text}");
    assert!(text.contains("~/workspace"), "{text}");
    let wordmark = status_colors(parser, "octet", INITIAL_COLUMNS).unwrap();
    let vt100::Color::Rgb(red, green, blue) = wordmark[0] else {
        panic!("SSH/Ghostty fixture lost truecolor: {wordmark:?}");
    };
    assert!(
        green > red && green > blue,
        "Gemma accent is green: {wordmark:?}"
    );
    assert!(wordmark.iter().all(|color| *color == wordmark[0]));
    let (rows, columns) = parser.screen().size();
    let mut logo_columns = vec![None; usize::from(columns)];
    let mut rules = 0;
    for row in 0..rows {
        for col in 0..columns {
            let cell = parser.screen().cell(row, col).unwrap();
            match cell.contents().as_str() {
                "─" => {
                    assert_eq!(cell.fgcolor(), wordmark[0], "mixed composer accent\n{text}");
                    rules += 1;
                }
                "█" => {
                    // The approved logo intentionally blends a gradient with
                    // the model accent. Shimmer may lift each column by <=20%,
                    // but all occupied rows in that column must agree.
                    let previous = &mut logo_columns[usize::from(col)];
                    if let Some(color) = previous {
                        assert_eq!(*color, cell.fgcolor(), "mixed logo column {col}\n{text}");
                    }
                    *previous = Some(cell.fgcolor());
                    let gradient: [[u8; 3]; 8] = [
                        [0x4b, 0x8d, 0xff],
                        [0x48, 0xad, 0xf5],
                        [0x45, 0xce, 0xeb],
                        [0x49, 0xdc, 0xd9],
                        [0x4f, 0xe2, 0xc3],
                        [0x5c, 0xe9, 0xaa],
                        [0x74, 0xf4, 0x8a],
                        [0x8d, 0xff, 0x6a],
                    ];
                    let vt100::Color::Rgb(r, g, b) = cell.fgcolor() else {
                        panic!("logo lost truecolor");
                    };
                    let column = usize::from(col - 2) / 3; // 24-cell mark in a 96-column fixture
                    for ((base, accent), actual) in gradient[column]
                        .into_iter()
                        .zip([red, green, blue])
                        .zip([r, g, b])
                    {
                        let blended =
                            (f32::from(base) + (f32::from(accent) - f32::from(base)) * 0.58) as u8;
                        let brightest =
                            (f32::from(blended) + (255.0 - f32::from(blended)) * 0.2) as u8;
                        assert!(
                            (blended.saturating_sub(1)..=brightest.saturating_add(1))
                                .contains(&actual),
                            "logo retained a provisional palette at {row},{col}: {:?}\n{text}",
                            cell.fgcolor()
                        );
                    }
                }
                _ => {}
            }
        }
    }
    assert_eq!(rules, usize::from(INITIAL_COLUMNS) * 2);
}

#[test]
fn real_octet_first_branded_frame_has_resolved_gemma_workspace_and_accent() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [MouseMode::Auto, MouseMode::App] {
        let mut octet = PtyOctet::spawn_at(
            Path::new(env!("CARGO_BIN_EXE_octet")),
            mode,
            None,
            true,
            (INITIAL_COLUMNS, INITIAL_ROWS),
            (7, false, false),
            StartupFixture::ConfiguredGemma,
        );
        octet.wait_until(STARTUP_TIMEOUT, |output| {
            synchronized_frame_end_containing(output, b"Gemma 4 31B").is_some()
        });
        octet.pty.drain_for(Duration::from_millis(150));
        let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
        let mut consumed = 0;
        let mut branded = false;
        for frame in frame_ranges(&octet.pty.output) {
            parser.process(&octet.pty.output[consumed..frame.end]);
            consumed = frame.end;
            if !branded && !parser.screen().contents().contains("octet v") {
                assert_unbranded_startup(&parser, INITIAL_COLUMNS);
            } else {
                branded = true;
                assert_green_gemma_frame(&parser);
            }
        }
        assert!(branded, "no branded frame");
        let capture = octet.shutdown();
        assert!(capture.status.success());
        assert!(capture.termios_restored);
        assert!(!uses_alternate_screen(&capture.output));
    }
}

#[test]
fn real_octet_setup_surfaces_work_before_modeless_startup_readiness() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [MouseMode::Auto, MouseMode::App] {
        let mut octet = PtyOctet::spawn_at(
            Path::new(env!("CARGO_BIN_EXE_octet")),
            mode,
            None,
            true,
            (INITIAL_COLUMNS, INITIAL_ROWS),
            (7, false, false),
            StartupFixture::Setup,
        );
        let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
        let mut consumed = 0;
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "Choose terminal appearance",
            STARTUP_TIMEOUT,
        );
        assert_unbranded_startup(&parser, INITIAL_COLUMNS);
        // Preview/confirm Light; no provider is installed by an appearance choice.
        octet.pty.write_input(b"\x1b[B\r");
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "Set up a provider",
            STARTUP_TIMEOUT,
        );
        assert_unbranded_startup(&parser, INITIAL_COLUMNS);
        for (columns, rows) in [
            (RESIZED_COLUMNS, RESIZED_ROWS),
            (INITIAL_COLUMNS, INITIAL_ROWS),
        ] {
            octet.pty.drain_for(DRAIN_TIME);
            let start = octet.pty.output.len();
            parser.process(&octet.pty.output[consumed..start]);
            octet.resize(columns, rows);
            parser.set_size(rows, columns);
            octet.wait_until(STARTUP_TIMEOUT, |bytes| {
                synchronized_frame_end_containing(&bytes[start..], b"\x1b[2J").is_some()
            });
            let end = start
                + synchronized_frame_end_containing(&octet.pty.output[start..], b"\x1b[2J")
                    .unwrap();
            parser.process(&octet.pty.output[start..end]);
            consumed = end;
            assert_unbranded_startup(&parser, columns);
            assert!(parser.screen().contents().contains("Set up a provider"));
            assert!(!parser.screen().hide_cursor());
        }
        // Open the existing endpoint-input owner, type without submitting, and
        // Ctrl-C out. This never probes a service or writes provider state.
        octet.pty.write_input(b"\x1b[B\r");
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "Endpoint URL:",
            STARTUP_TIMEOUT,
        );
        octet.pty.write_input(b"http://127.0.0.1:9/v1/");
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "http://127.0.0.1:9/v1/",
            STARTUP_TIMEOUT,
        );
        assert_unbranded_startup(&parser, INITIAL_COLUMNS);
        octet.pty.write_input(&[3]);
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "Set up a provider",
            STARTUP_TIMEOUT,
        );
        assert_unbranded_startup(&parser, INITIAL_COLUMNS);
        let readiness_start = consumed;
        octet.pty.write_input(b"\x1b[B\x1b[B\r"); // Continue without a provider.
        octet.wait_until(STARTUP_TIMEOUT, |bytes| {
            synchronized_frame_end_containing(&bytes[readiness_start..], b"setup needed").is_some()
        });
        octet.pty.drain_for(DRAIN_TIME);
        let mut branded = false;
        for frame in frame_ranges(&octet.pty.output[readiness_start..]) {
            let end = readiness_start + frame.end;
            parser.process(&octet.pty.output[consumed..end]);
            consumed = end;
            let text = screen_text(&parser, INITIAL_COLUMNS);
            if !branded && !text.contains("octet v") {
                assert_unbranded_startup(&parser, INITIAL_COLUMNS);
                continue;
            }
            branded = true;
            assert_single_welcome(&parser, INITIAL_COLUMNS, "model-less ready");
            assert!(
                text.contains("no configured model · setup needed"),
                "{text}"
            );
            assert!(text.contains("~/workspace"), "{text}");
            assert!(
                !text.contains("Set up a provider"),
                "stale setup rows\n{text}"
            );
            assert!(!text.contains("Endpoint URL:"), "stale input rows\n{text}");
        }
        assert!(branded, "model-less setup never became ready");
        assert!(
            fs::read_to_string(octet._root.path().join("home/.octet/config.toml"))
                .unwrap()
                .contains("light")
        );
        assert!(!octet
            ._root
            .path()
            .join("home/.octet/credentials/custom.json")
            .exists());
        let capture = octet.shutdown();
        assert!(capture.status.success());
        assert!(capture.termios_restored);
        assert!(!uses_alternate_screen(&capture.output));
        assert_eq!(
            count_bytes(&capture.output, FRAME_BEGIN),
            count_bytes(&capture.output, FRAME_END)
        );
    }
}

#[test]
fn real_octet_ctrl_d_before_startup_readiness_restores_terminal() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for mode in [MouseMode::Auto, MouseMode::App] {
        let mut octet = PtyOctet::spawn_at(
            Path::new(env!("CARGO_BIN_EXE_octet")),
            mode,
            None,
            true,
            (INITIAL_COLUMNS, INITIAL_ROWS),
            (7, false, false),
            StartupFixture::Setup,
        );
        octet.wait_until(STARTUP_TIMEOUT, |bytes| {
            synchronized_frame_end_containing(bytes, b"Choose terminal appearance").is_some()
        });
        let capture = octet.shutdown();
        assert!(capture.status.success());
        assert!(capture.termios_restored);
        let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
        parser.process(&capture.output);
        assert_unbranded_startup(&parser, INITIAL_COLUMNS);
        assert!(!parser.screen().hide_cursor());
        assert!(!parser.screen().bracketed_paste());
        let restore = &capture.output[capture.shutdown_start..];
        assert!(contains_bytes(restore, b"\x1b[?1000l"));
        assert!(contains_bytes(restore, b"\x1b[?1006l"));
        assert!(!uses_alternate_screen(&capture.output));
    }
}

#[test]
fn legacy_inline_startup_frame_pty_contract() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let trace = run_inline();
    assert_trace_fixture(
        "legacy inline",
        &trace.normalized(),
        include_str!("fixtures/startup-frame-pty/legacy-inline.trace"),
        &trace.debug_report(),
    );
}

/// One HTTP owner, one explicit response gate per request. It sends headers
/// immediately, but no provider events until the test releases the body.
struct HeldChatApi {
    url: String,
    arrived: std::sync::mpsc::Receiver<usize>,
    release: std::sync::mpsc::Sender<()>,
    count: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<Mutex<Vec<serde_json::Value>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl HeldChatApi {
    fn start() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::mpsc;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/v1/", listener.local_addr().unwrap());
        let (arrived_tx, arrived) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let counted = count.clone();
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("loopback accept: {error}"),
                };
                // BSD/macOS accept inherits O_NONBLOCK from the listener;
                // read timeouts only bound blocking sockets.
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                socket
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let header_end = loop {
                    let mut bytes = [0; 1024];
                    let n = socket.read(&mut bytes).unwrap();
                    assert!(n > 0, "request ended before headers");
                    request.extend_from_slice(&bytes[..n]);
                    assert!(request.len() <= 128 * 1024, "bounded loopback request");
                    if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        break end + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                assert!(headers.starts_with("POST /v1/chat/completions HTTP/1.1"));
                assert!(!headers.to_ascii_lowercase().contains("authorization:"));
                let length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse().unwrap())
                    })
                    .unwrap();
                assert!(length <= 128 * 1024);
                while request.len() < header_end + length {
                    let mut bytes = [0; 1024];
                    let n = socket.read(&mut bytes).unwrap();
                    assert!(n > 0);
                    request.extend_from_slice(&bytes[..n]);
                }
                recorded.lock().unwrap().push(
                    serde_json::from_slice(&request[header_end..header_end + length]).unwrap(),
                );
                let body = concat!(
                    "data: {\"id\":\"fixture\",\"model\":\"probe\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"fixture response done\"},\"finish_reason\":null}]}\n\n",
                    "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\n",
                    "data: [DONE]\n\n",
                );
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
                socket.flush().unwrap();
                let index = counted.fetch_add(1, Ordering::SeqCst) + 1;
                if arrived_tx.send(index).is_err() {
                    break;
                }
                loop {
                    if stopped.load(Ordering::SeqCst) {
                        return;
                    }
                    match released.recv_timeout(Duration::from_millis(50)) {
                        Ok(()) => {
                            let _ = socket.write_all(body.as_bytes());
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
        });
        Self {
            url,
            arrived,
            release,
            count,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn wait_for_request(&self, octet: &mut PtyOctet, index: usize) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            // A PTY has a small bounded output buffer. Keep draining it while
            // waiting for HTTP so the fixture itself cannot block the renderer.
            octet.pty.read_available();
            match self.arrived.try_recv() {
                Ok(actual) => {
                    assert_eq!(actual, index);
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(error) => panic!(
                    "HTTP fixture closed: {error}; terminal: {}",
                    visible_bytes(&octet.pty.output)
                ),
            }
            assert!(
                Instant::now() < deadline,
                "HTTP request {index} did not arrive; terminal: {}",
                visible_bytes(&octet.pty.output)
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for HeldChatApi {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = self.release.send(());
        if let Some(worker) = self.worker.take() {
            if let Err(panic) = worker.join() {
                if !thread::panicking() {
                    std::panic::resume_unwind(panic);
                }
            }
        }
    }
}

fn status_colors(parser: &vt100::Parser, label: &str, columns: u16) -> Option<Vec<vt100::Color>> {
    parser
        .screen()
        .rows(0, columns)
        .enumerate()
        .find_map(|(row, text)| {
            let index = text.find(label)?;
            let column = text[..index].chars().count();
            Some(
                (column..column + label.chars().count())
                    .map(|col| {
                        parser
                            .screen()
                            .cell(row as u16, col as u16)
                            .unwrap()
                            .fgcolor()
                    })
                    .collect(),
            )
        })
}

fn await_screen(
    octet: &mut PtyOctet,
    parser: &mut vt100::Parser,
    consumed: &mut usize,
    text: &str,
    budget: Duration,
) {
    let deadline = Instant::now() + budget;
    loop {
        octet.pty.read_available();
        parser.process(&octet.pty.output[*consumed..]);
        *consumed = octet.pty.output.len();
        if parser.screen().contents().contains(text) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "screen did not show {text:?} within {budget:?}: {}",
            parser.screen().contents()
        );
        assert!(
            octet.child.try_wait().unwrap().is_none(),
            "fixture binary exited"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_held_activity_pty(compact: bool, color: bool) {
    const INPUT_BUDGET: Duration = Duration::from_millis(500);
    const SAMPLE: Duration = Duration::from_millis(640);
    let api = HeldChatApi::start();
    let mut octet = PtyOctet::spawn_configured(
        Path::new(env!("CARGO_BIN_EXE_octet")),
        MouseMode::Auto,
        Some(&api.url),
        color,
    );
    let mut parser = vt100::Parser::new(INITIAL_ROWS, INITIAL_COLUMNS, 512);
    let mut consumed = 0;
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "custom/probe",
        STARTUP_TIMEOUT,
    );
    // Settle finite startup animation before measuring steady idle/redraw work.
    octet.pty.drain_for(Duration::from_secs(3));
    parser.process(&octet.pty.output[consumed..]);
    consumed = octet.pty.output.len();
    let idle_start = consumed;
    octet.pty.drain_for(SAMPLE);
    let idle_frames = frame_ranges(&octet.pty.output[idle_start..]).len();
    assert!(
        idle_frames <= 1,
        "idle redraw must remain bounded: {idle_frames}"
    );
    octet.pty.write_input(b"fixture initial prompt\r");
    api.wait_for_request(&mut octet, 1);
    let (label, request_count) = if compact {
        api.release.send(()).unwrap();
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "completed",
            STARTUP_TIMEOUT,
        );
        // A trailing space bypasses the slash-completion Enter owner and
        // submits the actual no-argument command in one terminal keypress.
        octet.pty.write_input(b"/compact \r");
        api.wait_for_request(&mut octet, 2);
        ("Compacting context", 2)
    } else {
        ("Working", 1)
    };
    await_screen(&mut octet, &mut parser, &mut consumed, label, INPUT_BUDGET);
    // Nothing releases this response gate during sampling or user interaction.
    let mut palettes = vec![status_colors(&parser, label, INITIAL_COLUMNS).unwrap()];
    let sample_start = consumed;
    octet.pty.drain_for(SAMPLE);
    let sample_end = octet.pty.output.len();
    let frames = frame_ranges(&octet.pty.output[sample_start..sample_end]);
    for frame in &frames {
        let end = sample_start + frame.end;
        parser.process(&octet.pty.output[consumed..end]);
        consumed = end;
        if let Some(colors) = status_colors(&parser, label, INITIAL_COLUMNS) {
            if !palettes.contains(&colors) {
                palettes.push(colors);
            }
        }
    }
    assert!(
        frames.len() <= 12,
        "bounded 80 ms animation cadence, not busy redraw: {}",
        frames.len()
    );
    if color {
        assert!(palettes.len() >= 3, "held {label} must change ANSI cell styles without provider events: {} palettes / {} frames", palettes.len(), frames.len());
    } else {
        assert_eq!(
            palettes.len(),
            1,
            "no-color status style intentionally static"
        );
        assert!(
            frames.len() <= 2,
            "static profile should write only elapsed-second changes"
        );
    }
    octet.pty.write_input(b"draft remains local");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "draft remains local",
        INPUT_BUDGET,
    );
    let resize_start = octet.pty.output.len();
    parser.set_size(RESIZED_ROWS, RESIZED_COLUMNS);
    octet.resize(RESIZED_COLUMNS, RESIZED_ROWS);
    octet.wait_until(INPUT_BUDGET, |bytes| {
        synchronized_frame_end_containing(&bytes[resize_start..], b"\x1b[2J").is_some()
    });
    parser.process(&octet.pty.output[consumed..]);
    consumed = octet.pty.output.len();
    assert!(parser.screen().contents().contains("draft remains local"));
    let replay = sexy_tui_rs::strip_terminal_sequences(&String::from_utf8_lossy(
        &octet.pty.output[resize_start..],
    ));
    assert_eq!(
        replay.matches("permissions:").count(),
        1,
        "resize replays exactly one welcome card"
    );
    octet.pty.write_input(b"\x1b");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        if compact {
            "compaction cancelled"
        } else {
            "interrupted"
        },
        INPUT_BUDGET,
    );
    assert!(parser.screen().contents().contains("draft remains local"));
    assert!(
        !parser.screen().contents().contains(label),
        "no stale active status after cancellation"
    );
    // After local settlement, retire the cancelled fixture socket and allow
    // the listener to observe any incorrectly duplicated POST before shutdown.
    api.release.send(()).unwrap();
    octet.pty.drain_for(Duration::from_millis(100));
    assert_eq!(
        api.count.load(std::sync::atomic::Ordering::SeqCst),
        request_count,
        "no duplicate provider request"
    );
    let shutdown = octet.shutdown();
    assert!(shutdown.status.success());
    assert!(shutdown.termios_restored);
    parser.process(&shutdown.output[consumed..]);
    assert!(!parser.screen().hide_cursor());
    assert!(!parser.screen().bracketed_paste());
    assert!(!uses_alternate_screen(&shutdown.output));
    assert_eq!(
        count_bytes(&shutdown.output, FRAME_BEGIN),
        count_bytes(&shutdown.output, FRAME_END)
    );
    eprintln!("held-api-pty compact={compact} color={color}: idle_frames={idle_frames}, active_frames={}, palettes={}, input_budget_ms=500, no_duplicate_posts=true, restored=true", frames.len(), palettes.len());
}

#[test]
fn real_octet_held_api_wait_pty_contract() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for compact in [false, true] {
        for color in [true, false] {
            assert_held_activity_pty(compact, color);
        }
    }
}

fn create_inert_environment(home: &Path, workspace: &Path, sessions: &Path) {
    fs::create_dir_all(home.join(".octet/credentials")).expect("credential directory");
    fs::create_dir_all(workspace).expect("workspace directory");
    fs::create_dir_all(sessions).expect("session directory");
    let credential = home.join(".octet/credentials/custom.json");
    fs::write(
        &credential,
        r#"{"base_url":"http://127.0.0.1:9/v1/","api_key":"","api_name":"probe","headers":[],"models":[],"auto_discover":false}"#,
    )
    .expect("inert custom-provider fixture");
    let mut permissions = fs::metadata(&credential)
        .expect("credential fixture metadata")
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(&credential, permissions).expect("credential fixture permissions");
}

fn fixture_lines(fixture: &str) -> Vec<String> {
    fixture.lines().map(str::to_owned).collect()
}

fn baseline_version(binary: &Path) -> String {
    assert!(
        binary.is_file(),
        "baseline is not a file: {}",
        binary.display()
    );
    let output = Command::new(binary)
        .arg("--version")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .expect("run explicit baseline --version");
    assert!(
        output.status.success(),
        "baseline --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let version = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        version.contains("0.6.7"),
        "OCTET_STARTUP_FRAME_BASELINE must be a v0.6.7 binary, got {version:?}"
    );
    version
}

fn assert_baseline_structural_compatibility(
    mode: &str,
    current: &PrimaryTrace,
    baseline: &PrimaryTrace,
) {
    let differences = [
        (
            "first synchronized frame",
            current.first_frame_synchronized,
            baseline.first_frame_synchronized,
        ),
        (
            "resize synchronized redraw",
            current.resize_redraw_synchronized,
            baseline.resize_redraw_synchronized,
        ),
        (
            "resize screen clear",
            current.resize_clear_screen,
            baseline.resize_clear_screen,
        ),
        (
            "resize saved-line clear",
            current.resize_clear_saved_lines,
            baseline.resize_clear_saved_lines,
        ),
        (
            "shutdown cursor restoration",
            current.shutdown_cursor_visible,
            baseline.shutdown_cursor_visible,
        ),
        (
            "shutdown bracketed-paste restoration",
            current.shutdown_bracketed_paste_disabled,
            baseline.shutdown_bracketed_paste_disabled,
        ),
        (
            "shutdown terminal mode restoration",
            current.shutdown_termios_restored,
            baseline.shutdown_termios_restored,
        ),
        (
            "alternate-screen policy",
            current.alternate_screen_used,
            baseline.alternate_screen_used,
        ),
        (
            "mouse capture",
            current.mouse_capture,
            baseline.mouse_capture,
        ),
    ]
    .into_iter()
    .filter_map(|(name, current, baseline)| (current != baseline).then_some(name))
    .collect::<Vec<_>>();
    assert!(
        differences.is_empty(),
        "current changed v0.6.7 structural behavior in {mode}: {differences:?}\ncurrent:\n{}\nbaseline:\n{}",
        current.normalized(),
        baseline.normalized(),
    );
}

fn normalized_delta(current: &PrimaryTrace, baseline: &PrimaryTrace) -> String {
    let changes = current
        .normalized()
        .lines()
        .zip(baseline.normalized().lines())
        .filter(|(current, baseline)| current != baseline)
        .map(|(current, baseline)| format!("- {baseline}\n+ {current}"))
        .collect::<Vec<_>>();
    if changes.is_empty() {
        "no normalized delta".to_owned()
    } else {
        changes.join("\n")
    }
}

fn assert_trace_fixture(name: &str, actual: &str, expected: &str, diagnostics: &str) {
    assert_eq!(
        actual.trim(),
        expected.trim(),
        "{name} PTY/frame contract changed\n{diagnostics}"
    );
}

fn frame_ranges(bytes: &[u8]) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while let Some(frame_start) = find_subsequence(bytes, FRAME_BEGIN, start) {
        let frame_body = frame_start + FRAME_BEGIN.len();
        let Some(frame_end_start) = find_subsequence(bytes, FRAME_END, frame_body) else {
            break;
        };
        let frame_end = frame_end_start + FRAME_END.len();
        ranges.push(frame_start..frame_end);
        start = frame_end;
    }
    ranges
}

fn nth_frame_end(bytes: &[u8], n: usize) -> Option<usize> {
    frame_ranges(bytes)
        .get(n.saturating_sub(1))
        .map(|range| range.end)
}

fn synchronized_frame_end_containing(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    frame_ranges(bytes)
        .into_iter()
        .find(|range| contains_bytes(&bytes[range.clone()], needle))
        .map(|range| range.end)
}

fn find_subsequence(bytes: &[u8], needle: &[u8], offset: usize) -> Option<usize> {
    (!needle.is_empty()).then_some(())?;
    bytes
        .get(offset..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|position| offset + position)
}

fn contains_bytes(bytes: &[u8], needle: &[u8]) -> bool {
    find_subsequence(bytes, needle, 0).is_some()
}

fn count_bytes(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn uses_alternate_screen(bytes: &[u8]) -> bool {
    [
        b"\x1b[?47h".as_slice(),
        b"\x1b[?47l",
        b"\x1b[?1047h",
        b"\x1b[?1047l",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
    ]
    .iter()
    .any(|sequence| contains_bytes(bytes, sequence))
}

fn screen_text(parser: &vt100::Parser, columns: u16) -> String {
    parser
        .screen()
        .rows(0, columns)
        .map(|row| row.trim_end().to_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

fn duplicate_stdio(fd: RawFd) -> Stdio {
    let duplicated = unsafe { libc::dup(fd) };
    assert!(
        duplicated >= 0,
        "dup PTY slave failed: {}",
        io::Error::last_os_error()
    );
    let file = unsafe { File::from_raw_fd(duplicated) };
    Stdio::from(file)
}

fn set_close_on_exec(fd: RawFd) {
    let result = unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
    assert_eq!(
        result,
        0,
        "fcntl(FD_CLOEXEC) failed: {}",
        io::Error::last_os_error()
    );
}

fn set_nonblocking(fd: RawFd) {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(
        flags >= 0,
        "fcntl(F_GETFL) failed: {}",
        io::Error::last_os_error()
    );
    let result = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(
        result,
        0,
        "fcntl(O_NONBLOCK) failed: {}",
        io::Error::last_os_error()
    );
}

fn terminal_attributes(fd: RawFd) -> libc::termios {
    let mut attributes = MaybeUninit::<libc::termios>::uninit();
    let result = unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) };
    assert_eq!(
        result,
        0,
        "tcgetattr failed: {}",
        io::Error::last_os_error()
    );
    unsafe { attributes.assume_init() }
}

fn terminal_modes_equal(before: &libc::termios, after: &libc::termios) -> bool {
    before.c_iflag == after.c_iflag
        && before.c_oflag == after.c_oflag
        && before.c_cflag == after.c_cflag
        && before.c_lflag == after.c_lflag
        && before.c_cc == after.c_cc
}

fn visible_bytes(bytes: &[u8]) -> String {
    const MAX_BYTES: usize = 4_096;
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_BYTES)]);
    let escaped = text.escape_default().to_string();
    if bytes.len() > MAX_BYTES {
        format!("{escaped}… ({} bytes total)", bytes.len())
    } else {
        escaped
    }
}

/// Check composed cells, not occurrences in the ANSI transcript: legitimate
/// differential frames repeat the current brand, but must never accumulate it.
fn assert_single_welcome(parser: &vt100::Parser, columns: u16, label: &str) {
    let text = screen_text(parser, columns);
    let version = format!("octet v{}", env!("CARGO_PKG_VERSION"));
    assert!(
        !text.contains("selecting model"),
        "{label}: provisional model\n{text}"
    );
    assert!(
        !text.contains("workspace unavailable"),
        "{label}: unresolved workspace\n{text}"
    );
    assert!(
        !text.contains(STALE_MARKER),
        "{label}: stale startup rows\n{text}"
    );
    assert_eq!(
        text.matches(&version).count(),
        1,
        "{label}: duplicate/missing version\n{text}"
    );
    let version_row = text
        .lines()
        .position(|line| line.contains(&version))
        .unwrap();
    let logo_box = (usize::from(columns) / 3).clamp(14, 24);
    let scale = (logo_box / 8).min(3);
    let top = version_row + (6 - 2 * scale) / 2;
    let left = 2 + (logo_box - 8 * scale) / 2;
    let (rows, _) = parser.screen().size();
    for row in 0..usize::from(rows) {
        for col in 0..usize::from(columns) {
            let in_grid =
                row >= top && row < top + 2 * scale && col >= left && col < left + 8 * scale;
            let expected =
                in_grid && (row >= top + scale || b"01101111"[(col - left) / scale] == b'1');
            let actual = parser
                .screen()
                .cell(row as u16, col as u16)
                .unwrap()
                .contents()
                == "█";
            assert_eq!(
                actual, expected,
                "{label}: stale/missing logo cell at ({row},{col})\n{text}"
            );
        }
    }
}

fn check_welcome_frames(
    octet: &PtyOctet,
    parser: &mut vt100::Parser,
    consumed: &mut usize,
    columns: u16,
    label: &str,
) {
    if let Some(directory) = std::env::var_os("OCTET_STARTUP_REDRAW_TRACE_DIR") {
        fs::write(
            PathBuf::from(directory).join(format!("{label}.ansi")),
            &octet.pty.output,
        )
        .unwrap();
    }
    let start = *consumed;
    let version = format!("octet v{}", env!("CARGO_PKG_VERSION"));
    let mut branded = parser.screen().contents().contains(&version);
    for frame in frame_ranges(&octet.pty.output[start..]) {
        let end = start + frame.end;
        parser.process(&octet.pty.output[*consumed..end]);
        *consumed = end;
        if !branded && !parser.screen().contents().contains(&version) {
            // Startup may need an input owner or a lifecycle wait first. Only
            // the first branded frame claims a resolved welcome-card contract.
            assert_unbranded_startup(parser, columns);
            continue;
        }
        branded = true;
        assert_single_welcome(parser, columns, label);
    }
    assert!(branded, "{label}: no ready branded frame");
    // Retain the entire tail, including cursor controls and any incomplete
    // synchronized frame. A later read completes it; never assert a partial
    // screen that synchronized output has not presented to the user.
}

#[test]
fn real_octet_repeated_startup_redraw_composed_screen() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Optional, explicit immutable binary for reproducing a pre-fix artifact.
    let binary = std::env::var_os("OCTET_STARTUP_REDRAW_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_octet")));
    for (columns, rows, starting_row) in [
        (80, 24, 0),
        (190, 18, 7),
        (190, 19, 17),
        (190, 20, 5),
        (46, 18, 4),
    ] {
        for mode in [MouseMode::Auto, MouseMode::App] {
            for (margins, origin) in [(false, false), (true, false), (true, true)] {
                let label = format!(
                    "{columns}x{rows}-start{starting_row}-{mode:?}-margins{margins}-origin{origin}"
                );
                let mut octet = PtyOctet::spawn_at(
                    &binary,
                    mode,
                    None,
                    true,
                    (columns, rows),
                    (starting_row, margins, origin),
                    StartupFixture::Model("qwen-3.8-27b"),
                );
                octet.wait_until(STARTUP_TIMEOUT, |bytes| {
                    synchronized_frame_end_containing(bytes, b"Qwen 3.8 27B").is_some()
                });
                octet.pty.drain_for(Duration::from_millis(250));
                let mut parser = vt100::Parser::new(rows, columns, 512);
                let mut consumed = 0;
                check_welcome_frames(
                    &octet,
                    &mut parser,
                    &mut consumed,
                    columns,
                    &format!("{label}-startup"),
                );
                // Shift+Tab updates a local setting and restarts model-colour
                // animation, without a redundant transcript success notice.
                // Wait for the actual new chrome: persistence precedes the
                // lifecycle rebuild and is not an input-readiness barrier.
                for (step, level) in ["low", "high"].into_iter().enumerate() {
                    let redraw_start = octet.pty.output.len();
                    octet.pty.write_input(b"\x1b[Z");
                    // Reconfiguration is asynchronous. Await the changed
                    // complete composed frame, checking every intervening
                    // frame rather than sleeping past potential logo artifacts.
                    let deadline = Instant::now() + STARTUP_TIMEOUT;
                    loop {
                        octet.pty.read_available();
                        check_welcome_frames(
                            &octet,
                            &mut parser,
                            &mut consumed,
                            columns,
                            &format!("{label}-setting{step}"),
                        );
                        let screen = parser.screen().contents();
                        assert!(
                            !screen.contains("thinking changed to"),
                            "{label}-setting{step}: redundant thinking notice\n{screen}"
                        );
                        if consumed > redraw_start
                            && screen.contains(&format!("Qwen 3.8 27B / {level}"))
                        {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "{label}-setting{step}: thinking status/redraw timed out\n{screen}"
                        );
                        assert!(
                            octet.child.try_wait().unwrap().is_none(),
                            "{label}-setting{step}: fixture binary exited"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                }
                octet.pty.drain_for(Duration::from_millis(2300));
                check_welcome_frames(
                    &octet,
                    &mut parser,
                    &mut consumed,
                    columns,
                    &format!("{label}-settled"),
                );
                let resize_start = octet.pty.output.len();
                octet.resize(80, 24);
                parser.set_size(24, 80);
                if (columns, rows) != (80, 24) {
                    octet.wait_until(STARTUP_TIMEOUT, |bytes| {
                        synchronized_frame_end_containing(&bytes[resize_start..], b"\x1b[2J")
                            .is_some()
                    });
                    // Old-width frames may already be queued when the PTY
                    // resizes. Replay them, but apply the new geometry contract
                    // from the first complete clearing redraw, not to those
                    // in-flight frames. Every subsequent frame is still checked.
                    let resize_end = resize_start
                        + synchronized_frame_end_containing(
                            &octet.pty.output[resize_start..],
                            b"\x1b[2J",
                        )
                        .unwrap();
                    parser.process(&octet.pty.output[consumed..resize_end]);
                    consumed = resize_end;
                    assert_single_welcome(&parser, 80, &format!("{label}-resize-first"));
                }
                octet.pty.drain_for(Duration::from_millis(100));
                check_welcome_frames(
                    &octet,
                    &mut parser,
                    &mut consumed,
                    80,
                    &format!("{label}-resize"),
                );
                let capture = octet.shutdown();
                assert!(capture.status.success());
                assert!(capture.termios_restored);
                assert!(!uses_alternate_screen(&capture.output));
                assert_eq!(
                    count_bytes(&capture.output, FRAME_BEGIN),
                    count_bytes(&capture.output, FRAME_END)
                );
                eprintln!("composed redraw PASS {label}");
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum PlainInput<'a> {
    Interactive,
    Positional(&'a str),
    Template(&'a str),
    Piped(&'a str),
}

fn spawn_plain(
    api: &str,
    input: PlainInput<'_>,
    redirected_stdout: bool,
) -> (PtyOctet, Option<PathBuf>) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    let sessions = root.path().join("sessions");
    create_inert_environment(&home, &workspace, &sessions);
    fs::write(
        home.join(".octet/credentials/custom.json"),
        serde_json::json!({
            "base_url": api, "api_key": "", "api_name": "probe",
            "headers": [], "models": [], "auto_discover": false,
        })
        .to_string(),
    )
    .unwrap();
    let mut pty = Pty::open(INITIAL_COLUMNS, INITIAL_ROWS);
    // Plain mode leaves line editing and echo to the terminal. Set that
    // contract explicitly rather than inheriting the test runner's modes.
    pty.original_termios.c_lflag |= libc::ICANON | libc::ECHO;
    pty.original_termios.c_iflag |= libc::ICRNL;
    pty.original_termios.c_oflag |= libc::OPOST | libc::ONLCR;
    pty.original_termios.c_cc[libc::VEOF] = 4;
    assert_eq!(
        unsafe { libc::tcsetattr(pty.slave.as_raw_fd(), libc::TCSANOW, &pty.original_termios) },
        0
    );
    let log = redirected_stdout.then(|| root.path().join("stdout.log"));
    let stdout = match &log {
        Some(path) => Stdio::from(File::create(path).unwrap()),
        None => duplicate_stdio(pty.slave.as_raw_fd()),
    };
    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .args([
            "--plain",
            "--offline",
            "--no-context-files",
            "--no-tools",
            "--color",
            "never",
            "--model",
            "custom/probe",
            "--system-prompt",
            "PTY fixture",
        ])
        .arg("--workspace")
        .arg(&workspace)
        .arg("--session-dir")
        .arg(&sessions)
        .current_dir(&workspace)
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("PWD", &workspace)
        .env("TERM", "xterm-256color")
        .env("LANG", "C.UTF-8")
        .stdin(duplicate_stdio(pty.slave.as_raw_fd()))
        .stdout(stdout)
        .stderr(duplicate_stdio(pty.slave.as_raw_fd()));
    match input {
        PlainInput::Interactive => {}
        PlainInput::Positional(prompt) => {
            command.arg(prompt);
        }
        PlainInput::Template(prompt) => {
            fs::create_dir_all(home.join(".octet/prompts")).unwrap();
            fs::write(home.join(".octet/prompts/plain-fixture.md"), prompt).unwrap();
            command.args(["--prompt", "plain-fixture"]);
        }
        PlainInput::Piped(_) => {
            command.stdin(Stdio::piped());
        }
    }
    let tty_fd = pty.slave.as_raw_fd();
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1
                || libc::ioctl(tty_fd, libc::TIOCSCTTY as libc::c_ulong, 0) == -1
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("spawn plain octet under PTY");
    if let PlainInput::Piped(prompt) = input {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(prompt.as_bytes())
            .unwrap();
    }
    (
        PtyOctet {
            child,
            pty,
            _root: root,
        },
        log,
    )
}

fn plain_output(octet: &mut PtyOctet, log: Option<&Path>) -> Vec<u8> {
    octet.pty.read_available();
    match log {
        Some(path) => fs::read(path).unwrap(),
        None => octet.pty.output.clone(),
    }
}

fn wait_for_plain_ready(octet: &mut PtyOctet, log: Option<&Path>, completed: usize) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        let output = plain_output(octet, log);
        if count_bytes(&output, b"[completed]") == completed && output.ends_with(b"\n> ") {
            return;
        }
        assert!(
            octet.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "plain mode not ready after {completed} runs; stdout: {}; PTY: {}",
            visible_bytes(&output),
            visible_bytes(&octet.pty.output),
        );
        thread::sleep(Duration::from_millis(5));
    }
}

fn assert_plain_user_messages(api: &HeldChatApi, index: usize, expected: &[&str]) {
    let requests = api.requests.lock().unwrap();
    let users = requests[index]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|message| message["role"] == "user")
        .map(|message| {
            message["content"]
                .as_str()
                .expect("text-only fixture prompt")
        })
        .collect::<Vec<_>>();
    // History reappears in the second request; each input must be appended
    // once, not counted as a duplicate merely because history is replayed.
    assert_eq!(users, expected);
}

#[test]
fn real_octet_plain_tty_prompts_once() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for redirected in [false, true] {
        let api = HeldChatApi::start();
        let (mut octet, log) = spawn_plain(&api.url, PlainInput::Interactive, redirected);
        let prompts = ["plain first unique input", "plain second unique input"];
        wait_for_plain_ready(&mut octet, log.as_deref(), 0);
        for (index, prompt) in prompts.iter().copied().enumerate() {
            let start = plain_output(&mut octet, log.as_deref()).len();
            octet.pty.write_input(format!("{prompt}\n").as_bytes());
            api.wait_for_request(&mut octet, index + 1);
            assert_plain_user_messages(&api, index, &prompts[..=index]);
            api.release.send(()).unwrap();
            wait_for_plain_ready(&mut octet, log.as_deref(), index + 1);
            let output = plain_output(&mut octet, log.as_deref());
            let run = String::from_utf8(output[start..].to_vec()).unwrap();
            assert_eq!(
                run.matches(prompt).count(),
                1,
                "redirected={redirected}: {run}"
            );
            assert_eq!(run.matches("fixture response done").count(), 1, "{run}");
            let prompt_at = run.find(prompt).unwrap();
            let working_at = run.find("[working]").unwrap();
            let response_at = run.find("fixture response done").unwrap();
            let completed_at = run.find("[completed]").unwrap();
            assert!(
                prompt_at < working_at && working_at < response_at && response_at < completed_at,
                "{run}"
            );
            assert!(run.ends_with("\n> "), "ready prompt after each run: {run}");
        }
        // EOF is supplied only after the second ready prompt. A duplicate
        // submission would either prevent readiness or appear in the count.
        let output = plain_output(&mut octet, log.as_deref());
        let capture = octet.shutdown();
        assert!(capture.status.success());
        assert!(capture.termios_restored);
        assert!(
            !capture.output.contains(&0x1b),
            "plain PTY must be cursor-free"
        );
        for prompt in prompts {
            assert_eq!(count_bytes(&output, prompt.as_bytes()), 1);
        }
        assert!(!output.contains(&0x1b), "plain output must be cursor-free");
        assert_eq!(api.count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[test]
fn real_octet_plain_explicit_prompts_once() {
    let _guard = pty_test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let prompt = "plain explicit unique input";
    for input in [
        PlainInput::Positional(prompt),
        PlainInput::Template(prompt),
        PlainInput::Piped(prompt),
    ] {
        for redirected in [false, true] {
            let api = HeldChatApi::start();
            let (mut octet, log) = spawn_plain(&api.url, input, redirected);
            api.wait_for_request(&mut octet, 1);
            assert_plain_user_messages(&api, 0, &[prompt]);
            api.release.send(()).unwrap();
            let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
            loop {
                octet.pty.read_available();
                if let Some(status) = octet.child.try_wait().unwrap() {
                    assert!(status.success(), "{input:?}: {status}");
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "{input:?} did not exit; {}",
                    visible_bytes(&octet.pty.output)
                );
                thread::sleep(Duration::from_millis(5));
            }
            let output = plain_output(&mut octet, log.as_deref());
            let text = String::from_utf8(output).unwrap();
            assert_eq!(
                text.matches(prompt).count(),
                1,
                "{input:?}, redirected={redirected}: {text}"
            );
            assert!(
                text.contains(&format!("> {prompt}\n"))
                    || text.contains(&format!("> {prompt}\r\n")),
                "{text}"
            );
            assert_eq!(text.matches("fixture response done").count(), 1, "{text}");
            assert_eq!(text.matches("[completed]").count(), 1, "{text}");
            assert!(
                !text.ends_with("> "),
                "one-shot must not wait for another prompt"
            );
            assert!(!text.contains('\x1b'), "plain output must be cursor-free");
            assert_eq!(api.count.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }
}
