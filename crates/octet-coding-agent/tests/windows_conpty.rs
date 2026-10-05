//! Native Windows pseudoconsole (ConPTY) coverage for the interactive frontend.
//!
//! Windows Terminal hosts every shell through ConPTY. This harness drives the
//! real binary through the same API with no `TERM`, as PowerShell and cmd do:
//! console detection, the rendered frame, keyboard input translated into
//! console input records, a pseudoconsole resize, and exit. It runs against
//! the machine's inbox console host rather than Windows Terminal's bundled
//! one and inspects the reconstructed screen, so it is not a visual flicker
//! measurement; docs/windows.md keeps that manual acceptance checklist.
//!
//! `OCTET_CONPTY_BINARY` points the harness at another `octet.exe`, such as
//! the release build CI packages, instead of the test profile's binary.
//!
//! Windows resolves the profile directory through the known-folder API, not an
//! environment variable, so the child reads the current user's octet
//! configuration. The interactive scenarios need a profile with no configured
//! provider, as on a CI runner: they continue through first-run provider setup
//! without writing provider data. When the account does have a configured
//! custom provider they skip themselves (a configured provider opens the model
//! picker instead of first-run setup). The workspace and session store are
//! disposable.
#![cfg(windows)]

use std::ffi::{c_void, OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, ResizePseudoConsole, COORD, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

const TIMEOUT: Duration = Duration::from_secs(45);
const INITIAL: (u16, u16) = (90, 28);
const RESIZED: (u16, u16) = (120, 32);
const MARKER: &str = "octetconpty";
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const OSC11_QUERY: &[u8] = b"\x1b]11;?";

/// The persisted custom-provider registry, which the child always reads.
///
/// Windows resolves the profile through the known-folder API, so these
/// scenarios cannot isolate it: a configured provider makes startup open the
/// model picker instead of first-run setup.
fn profile_has_configured_provider() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    home.join(".octet")
        .join("credentials")
        .join("custom.json")
        .is_file()
}

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(Some(0)).collect()
}

/// Quote one argument with the rules the Microsoft C runtime uses to split a
/// command line.
fn quote_argument(argument: &OsStr, line: &mut Vec<u16>) {
    let units: Vec<u16> = argument.encode_wide().collect();
    let plain = !units.is_empty()
        && !units.iter().any(|&unit| {
            unit == u16::from(b' ') || unit == u16::from(b'\t') || unit == u16::from(b'"')
        });
    if plain {
        line.extend(units);
        return;
    }
    line.push(u16::from(b'"'));
    let mut backslashes = 0usize;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
            continue;
        }
        let escapes = if unit == u16::from(b'"') {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(std::iter::repeat_n(u16::from(b'\\'), escapes));
        backslashes = 0;
        line.push(unit);
    }
    line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes * 2));
    line.push(u16::from(b'"'));
}

fn command_line(program: &Path, arguments: &[OsString]) -> Vec<u16> {
    let mut line = Vec::new();
    quote_argument(program.as_os_str(), &mut line);
    for argument in arguments {
        line.push(u16::from(b' '));
        quote_argument(argument, &mut line);
    }
    line.push(0);
    line
}

/// The parent environment as a native PowerShell or cmd session would pass
/// it: no POSIX terminal or locale negotiation, plus scenario overrides.
fn environment_block(overrides: &[(&str, &str)]) -> Vec<u16> {
    const REMOVED: &[&str] = &[
        "TERM",
        "COLORTERM",
        "TERM_PROGRAM",
        "WT_SESSION",
        "WT_PROFILE_ID",
        "NO_COLOR",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "OCTET_COLOR_SCHEME",
    ];
    let mut variables: Vec<(OsString, OsString)> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !REMOVED
                .iter()
                .any(|removed| key.eq_ignore_ascii_case(removed))
        })
        .collect();
    variables.extend(
        overrides
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value))),
    );
    variables.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut block = Vec::new();
    for (key, value) in variables {
        block.extend(key.encode_wide());
        block.push(u16::from(b'='));
        block.extend(value.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// Replies a terminal would send to queries forwarded through the console
/// host. Keeping them answered makes the scenario independent of whether the
/// inbox host answers or forwards them.
fn respond_to_queries(output: &[u8], scanned: &mut usize, input: &Mutex<File>) {
    const QUERIES: &[(&[u8], &[u8])] = &[
        (b"\x1b[6n", b"\x1b[1;1R"),
        (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0c0c/0c0c/0c0c\x1b\\"),
        (b"\x1b]11;?\x07", b"\x1b]11;rgb:0c0c/0c0c/0c0c\x07"),
    ];
    let longest = QUERIES
        .iter()
        .map(|(query, _)| query.len())
        .max()
        .unwrap_or(0);
    let mut position = *scanned;
    while position < output.len() {
        for (query, reply) in QUERIES {
            if output[position..].starts_with(query) {
                let mut input = input
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let _ = input.write_all(reply);
                let _ = input.flush();
            }
        }
        position += 1;
    }
    // Re-examine a possibly incomplete query at the end on the next read.
    *scanned = output
        .len()
        .saturating_sub(longest.saturating_sub(1))
        .max(*scanned);
}

struct PseudoConsole {
    console: HPCON,
    input: Arc<Mutex<File>>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<JoinHandle<()>>,
    process: OwnedHandle,
    _thread: OwnedHandle,
    parser: vt100::Parser,
    processed: usize,
}

impl PseudoConsole {
    fn spawn(
        program: &Path,
        arguments: &[OsString],
        directory: &Path,
        environment: &[(&str, &str)],
        size: (u16, u16),
    ) -> Self {
        // SAFETY: every Win32 call below receives valid out pointers and
        // handles created in this function; ownership of each handle is either
        // transferred to an owning Rust type or closed before returning.
        unsafe {
            let mut input_read: HANDLE = std::ptr::null_mut();
            let mut input_write: HANDLE = std::ptr::null_mut();
            let mut output_read: HANDLE = std::ptr::null_mut();
            let mut output_write: HANDLE = std::ptr::null_mut();
            assert_ne!(
                CreatePipe(&mut input_read, &mut input_write, std::ptr::null(), 0),
                0,
                "input pipe: {}",
                std::io::Error::last_os_error()
            );
            assert_ne!(
                CreatePipe(&mut output_read, &mut output_write, std::ptr::null(), 0),
                0,
                "output pipe: {}",
                std::io::Error::last_os_error()
            );
            let mut console: HPCON = 0;
            let result = CreatePseudoConsole(
                COORD {
                    X: size.0 as i16,
                    Y: size.1 as i16,
                },
                input_read,
                output_write,
                0,
                &mut console,
            );
            assert!(result >= 0, "CreatePseudoConsole failed: {result:#x}");
            // The pseudoconsole holds its own duplicates of these ends.
            CloseHandle(input_read);
            CloseHandle(output_write);

            let mut attribute_bytes = 0usize;
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut attribute_bytes);
            // u64 storage keeps the opaque attribute list pointer-aligned.
            let mut attribute_storage = vec![0u64; attribute_bytes.div_ceil(8)];
            let attributes = attribute_storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            assert_ne!(
                InitializeProcThreadAttributeList(attributes, 1, 0, &mut attribute_bytes),
                0,
                "attribute list: {}",
                std::io::Error::last_os_error()
            );
            assert_ne!(
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    console as *const c_void,
                    std::mem::size_of::<HPCON>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ),
                0,
                "pseudoconsole attribute: {}",
                std::io::Error::last_os_error()
            );

            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            // Invalid standard handles make the child use the pseudoconsole
            // instead of inheriting the test harness's redirected streams.
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            startup.lpAttributeList = attributes;

            let mut line = command_line(program, arguments);
            let block = environment_block(environment);
            let directory = wide(directory.as_os_str());
            let mut information: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                block.as_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut information,
            );
            let spawn_error = std::io::Error::last_os_error();
            DeleteProcThreadAttributeList(attributes);
            assert_ne!(created, 0, "CreateProcessW: {spawn_error}");

            let input = Arc::new(Mutex::new(File::from_raw_handle(input_write as RawHandle)));
            let output = Arc::new(Mutex::new(Vec::new()));
            let mut output_file = File::from_raw_handle(output_read as RawHandle);
            let reader_output = output.clone();
            let reader_input = input.clone();
            let reader = thread::Builder::new()
                .name("conpty-output".into())
                .spawn(move || {
                    let mut buffer = [0u8; 16 * 1024];
                    let mut scanned = 0usize;
                    loop {
                        match output_file.read(&mut buffer) {
                            Ok(0) | Err(_) => break,
                            Ok(read) => {
                                let mut output = reader_output
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                output.extend_from_slice(&buffer[..read]);
                                respond_to_queries(&output, &mut scanned, &reader_input);
                            }
                        }
                    }
                })
                .expect("ConPTY reader thread");
            Self {
                console,
                input,
                output,
                reader: Some(reader),
                process: OwnedHandle::from_raw_handle(information.hProcess as RawHandle),
                _thread: OwnedHandle::from_raw_handle(information.hThread as RawHandle),
                parser: vt100::Parser::new(size.1, size.0, 0),
                processed: 0,
            }
        }
    }

    fn send(&self, bytes: &[u8]) {
        let mut input = self
            .input
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        input.write_all(bytes).expect("write ConPTY input");
        input.flush().expect("flush ConPTY input");
    }

    /// Type like a user: one key at a time, so each byte becomes its own
    /// console input record pair.
    fn type_text(&self, text: &str) {
        for character in text.chars() {
            let mut encoded = [0u8; 4];
            self.send(character.encode_utf8(&mut encoded).as_bytes());
            thread::sleep(Duration::from_millis(15));
        }
    }

    fn raw(&self) -> Vec<u8> {
        self.output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// The tail of the raw host output, escaped for a failure message.
    fn raw_tail(&self) -> String {
        let raw = self.raw();
        let start = raw.len().saturating_sub(3000);
        format!("{:?}", String::from_utf8_lossy(&raw[start..]))
    }

    fn screen(&mut self) -> String {
        let output = self.raw();
        self.parser.process(&output[self.processed..]);
        self.processed = output.len();
        // ConPTY can emit full-width rows as soft wraps. `contents()` joins
        // those into logical lines, hiding correctly redrawn physical rules.
        // This harness asserts screen geometry, not copy/selection text.
        let screen = self.parser.screen();
        screen
            .rows(0, screen.size().1)
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn wait_for(&mut self, description: &str, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let screen = self.screen();
            if predicate(&screen) {
                return screen;
            }
            if let Some(code) = self.exit_code() {
                // Let the reader collect the child's last output first.
                thread::sleep(Duration::from_millis(500));
                let screen = self.screen();
                panic!(
                    "octet exited with {code:#x} while waiting for {description}:\n{screen}\nraw: {}",
                    self.raw_tail()
                );
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}; screen:\n{screen}\nraw: {}",
                self.raw_tail()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn resize(&mut self, size: (u16, u16)) {
        let _ = self.screen();
        // SAFETY: `self.console` is a live pseudoconsole owned by this value.
        let result = unsafe {
            ResizePseudoConsole(
                self.console,
                COORD {
                    X: size.0 as i16,
                    Y: size.1 as i16,
                },
            )
        };
        assert!(result >= 0, "ResizePseudoConsole failed: {result:#x}");
        self.parser.set_size(size.1, size.0);
    }

    fn exit_code(&self) -> Option<u32> {
        let handle = self.process.as_raw_handle() as HANDLE;
        // SAFETY: the process handle is owned by `self` and remains open.
        match unsafe { WaitForSingleObject(handle, 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                // SAFETY: `code` is a valid out pointer for a signalled process.
                unsafe { GetExitCodeProcess(handle, &mut code) };
                Some(code)
            }
            WAIT_TIMEOUT => None,
            other => panic!("WaitForSingleObject returned {other:#x}"),
        }
    }

    fn wait_for_exit(&mut self) -> u32 {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Some(code) = self.exit_code() {
                return code;
            }
            assert!(
                Instant::now() < deadline,
                "octet did not exit; screen:\n{}",
                self.screen()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for PseudoConsole {
    fn drop(&mut self) {
        if self.exit_code().is_none() {
            // SAFETY: the process handle is owned by `self`.
            unsafe { TerminateProcess(self.process.as_raw_handle() as HANDLE, 1) };
        }
        // SAFETY: the pseudoconsole is closed exactly once. The reader keeps
        // draining output until the host closes its end, so this cannot block
        // on a full pipe.
        unsafe { ClosePseudoConsole(self.console) };
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

struct Scenario {
    _root: tempfile::TempDir,
    workspace: std::path::PathBuf,
    sessions: std::path::PathBuf,
}

impl Scenario {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scenario directory");
        let canonical = root.path().canonicalize().expect("canonical scenario root");
        let workspace = canonical.join("workspace");
        let sessions = canonical.join("sessions");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&sessions).expect("session store");
        Self {
            _root: root,
            workspace,
            sessions,
        }
    }

    fn arguments(&self) -> Vec<OsString> {
        let mut arguments: Vec<OsString> = [
            "--offline",
            "--no-context-files",
            "--no-tools",
            "--workspace",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        arguments.push(self.workspace.clone().into_os_string());
        arguments.push("--session-dir".into());
        arguments.push(self.sessions.clone().into_os_string());
        arguments
    }
}

fn octet_binary() -> std::path::PathBuf {
    std::env::var_os("OCTET_CONPTY_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_octet").into())
}

fn full_width_rule(screen: &str, width: usize, glyph: char) -> bool {
    screen.lines().any(|line| {
        let line = line.trim_end();
        line.chars().count() == width && line.chars().all(|character| character == glyph)
    })
}

/// Start, type, resize, and exit through ConPTY, asserting the frontend
/// profile selected from the native console rather than from TERM.
fn interactive_round_trip(environment: &[(&str, &str)], rule: char) {
    let _serial = serial();
    if profile_has_configured_provider() {
        eprintln!(
            "skipping: this account has a configured custom provider; the ConPTY \
             interactive scenarios require a profile with none"
        );
        return;
    }
    let scenario = Scenario::new();
    let binary = octet_binary();
    let mut console = PseudoConsole::spawn(
        &binary,
        &scenario.arguments(),
        &scenario.workspace,
        environment,
        INITIAL,
    );

    // The composer accepts typing while startup model discovery runs, and
    // discovery then opens first-run provider setup over it, so input typed
    // before setup appears would race it. Wait for setup (after first-run
    // appearance setup on a fresh profile) and continue without a provider,
    // which writes no provider data, before typing into the composer.
    let first_run = console.wait_for(
        "first-run setup (this scenario needs a profile with no configured provider)",
        |screen| {
            screen.contains("Choose terminal appearance") || screen.contains("Set up a provider")
        },
    );
    if !first_run.contains("Set up a provider") {
        console.send(b"\r");
        console.wait_for("first-run provider setup", |screen| {
            screen.contains("Set up a provider")
        });
    }
    console.type_text("Continue without");
    console.send(b"\r");
    let started = console.wait_for("the composer after provider setup", |screen| {
        !screen.contains("Set up a provider")
            && ['-', '─']
                .into_iter()
                .any(|glyph| full_width_rule(screen, INITIAL.0.into(), glyph))
    });
    assert!(
        full_width_rule(&started, INITIAL.0.into(), rule),
        "expected full-width {rule:?} rules for this console profile:\n{started}"
    );

    console.type_text(MARKER);
    let typed = console.wait_for("typed input", |screen| screen.contains(MARKER));
    for residue in ["]11;", "rgb:", "0c0c"] {
        assert!(
            !typed.contains(residue),
            "a terminal reply leaked into input as {residue:?}:\n{typed}"
        );
    }

    console.resize(RESIZED);
    console.wait_for("a re-render at the resized width", |screen| {
        full_width_rule(screen, RESIZED.0.into(), rule)
    });

    console.send(&vec![0x7f; MARKER.len()]);
    console.wait_for("erased input", |screen| !screen.contains(MARKER));
    console.send(b"\x04");
    let code = console.wait_for_exit();
    let raw = console.raw();
    // Native Windows never sends the OSC 11 background query: a console host
    // delivers the reply as key records, and Windows Terminal's reply reached
    // the composer as typed text. A host that answers the query itself never
    // forwards it, so this only catches a forwarded query.
    assert!(
        !raw.windows(OSC11_QUERY.len())
            .any(|window| window == OSC11_QUERY),
        "octet sent an OSC 11 background query on native Windows"
    );
    let frames = raw
        .windows(SYNC_BEGIN.len())
        .filter(|window| *window == SYNC_BEGIN)
        .count();
    // Baseline data for the console host, not an assertion: an inbox host
    // may re-render instead of forwarding synchronized-output markers.
    eprintln!(
        "ConPTY baseline: {} output bytes, {frames} synchronized-output markers forwarded",
        raw.len()
    );
    assert_eq!(code, 0, "octet exit status; screen:\n{}", console.screen());
    // The host renders the child's final mode reset after the process exits.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let screen = console.screen();
        if !console.parser.screen().hide_cursor() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the cursor must be visible again after exit:\n{screen}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn profile_guard_ignores_environment_home_overrides() {
    let _serial = serial();
    let home = dirs::home_dir().expect("native Windows profile");
    let expected = home.join(".octet/credentials/custom.json").is_file();
    let fake_home = tempfile::tempdir().expect("disposable environment profile");
    // Make the environment profile give the opposite answer on both fresh
    // accounts and developer accounts, without changing native credentials.
    if !expected {
        let credentials = fake_home.path().join(".octet/credentials");
        std::fs::create_dir_all(&credentials).expect("fixture credentials directory");
        std::fs::write(credentials.join("custom.json"), b"{}").expect("fixture registry");
    }
    let saved: Vec<_> = ["USERPROFILE", "HOME"]
        .into_iter()
        .map(|key| (key, std::env::var_os(key)))
        .collect();
    for (key, _) in &saved {
        std::env::set_var(key, fake_home.path());
    }
    let actual = profile_has_configured_provider();
    for (key, value) in saved {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    assert_eq!(
        actual, expected,
        "guard must use the frontend's native profile"
    );
}

#[test]
fn windows_terminal_console_gets_the_rich_interactive_frontend() {
    interactive_round_trip(
        &[("WT_SESSION", "00000000-0000-0000-0000-000000000000")],
        '─',
    );
}

#[test]
fn plain_conhost_console_gets_the_ascii_interactive_frontend() {
    interactive_round_trip(&[], '-');
}

#[test]
fn redirected_streams_stay_plain_without_terminal_controls() {
    let _serial = serial();
    let scenario = Scenario::new();
    let binary = octet_binary();

    let version = Command::new(&binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .expect("octet --version");
    assert!(version.status.success());
    let text = String::from_utf8(version.stdout).expect("UTF-8 version");
    assert_eq!(text.trim(), format!("octet {}", env!("CARGO_PKG_VERSION")));

    // Redirected stdin selects print mode. With no prompt it must fail fast
    // and write no terminal control sequences to either stream.
    let redirected = Command::new(&binary)
        .args(scenario.arguments())
        .current_dir(&scenario.workspace)
        .env_remove("TERM")
        .env_remove("WT_SESSION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("octet with redirected streams");
    assert!(!redirected.status.success());
    let stderr = String::from_utf8_lossy(&redirected.stderr);
    assert!(stderr.contains("requires a prompt"), "stderr: {stderr}");
    for stream in [&redirected.stdout, &redirected.stderr] {
        assert!(
            !stream.contains(&0x1b),
            "redirected output contains terminal controls: {:?}",
            String::from_utf8_lossy(stream)
        );
    }
}
