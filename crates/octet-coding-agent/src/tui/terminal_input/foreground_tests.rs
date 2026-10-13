//! Readiness is a wake hint, not another byte owner. Socket tests stay off stdin;
//! the production PTY contract runs in a subprocess with disposable tty stdin.

use super::*;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use tokio_test::task::{spawn, Spawn};

struct SyntheticEvents {
    events: ForegroundEvents,
    reader: UnixStream,
    source_polls: usize,
    reads: usize,
    discard_next: bool,
    hold_input: bool,
    queued: VecDeque<io::Result<Event>>,
}

impl SyntheticEvents {
    fn pair() -> (Self, UnixStream) {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        writer.set_nonblocking(true).unwrap();
        let notifier = AsyncFd::with_interest(
            OwnedFd::from(reader.try_clone().unwrap()),
            Interest::READABLE,
        )
        .unwrap();
        (
            Self {
                events: ForegroundEvents {
                    wake: None,
                    tty: std::sync::OnceLock::from(Some(notifier)),
                    ..Default::default()
                },
                reader,
                source_polls: 0,
                reads: 0,
                discard_next: false,
                hold_input: false,
                queued: VecDeque::new(),
            },
            writer,
        )
    }
}

impl Stream for SyntheticEvents {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let Self {
            events,
            reader,
            source_polls,
            reads,
            discard_next,
            hold_input,
            queued,
        } = self.get_mut();
        *source_polls += 1;
        events.poll_next_with(cx, || {
            *reads += 1;
            if let Some(event) = queued.pop_front() {
                return event.map(Some);
            }
            if *hold_input {
                return Ok(None);
            }
            let mut byte = [0];
            match reader.read(&mut byte) {
                Ok(0) => Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(_) if std::mem::take(discard_next) => Ok(None),
                Ok(_) => Ok(Some(key(byte[0]))),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(error),
            }
        })
    }
}

fn key(byte: u8) -> Event {
    Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char(char::from(byte)),
        KeyModifiers::NONE,
    ))
}

fn event(result: Poll<Option<io::Result<Event>>>) -> Event {
    match result {
        Poll::Ready(Some(Ok(event))) => event,
        other => panic!("expected an event, got {other:?}"),
    }
}

// Keep this test task runnable while the reactor observes local I/O. The
// paused clock cannot auto-advance to the fallback timer and fake an I/O wake.
// The bound is a watchdog in scheduler turns, not a latency assertion.
async fn awakened<T>(task: &Spawn<T>) {
    let now = tokio::time::Instant::now();
    for _ in 0..1024 {
        if task.is_woken() {
            assert_eq!(tokio::time::Instant::now(), now);
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("readiness did not wake the parked input task");
}

#[test]
fn construction_does_not_open_a_tty_or_require_a_runtime() {
    let events = ForegroundEvents::default();
    assert!(events.tty.get().is_none());
    assert!(events.wake.is_none());
    let input = TerminalInput::new();
    assert!(input.source.tty.get().is_none());
    // Construction also remains inert inside a runtime with no I/O driver.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    assert!(ForegroundEvents::default().tty.get().is_none());
}

#[tokio::test(start_paused = true)]
async fn readiness_preempts_the_pending_ten_ms_sleep() {
    let (source, mut writer) = SyntheticEvents::pair();
    let mut task = spawn(source);
    assert!(task.poll_next().is_pending());
    let deadline = task.events.wake.as_ref().unwrap().deadline();
    assert_eq!(
        deadline - tokio::time::Instant::now(),
        Duration::from_millis(10)
    );

    writer.write_all(b"x").unwrap();
    awakened(&task).await;
    assert!(tokio::time::Instant::now() < deadline);
    assert_eq!(event(task.poll_next()), key(b'x'));
    assert!(task.events.wake.is_none());
    assert_eq!(task.reads, 2);
}

#[tokio::test(start_paused = true)]
async fn stale_readiness_is_cleared_without_spin_and_rearmed() {
    let (source, mut writer) = SyntheticEvents::pair();
    let mut task = spawn(source);
    assert!(task.poll_next().is_pending());
    writer.write_all(b"x").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'x'));
    // Tokio still has the old edge, but the foreground reader drained it.
    assert!(task.poll_next().is_pending());
    let reads = task.reads;
    for _ in 0..32 {
        assert!(task.poll_next().is_pending());
        assert!(!task.is_woken(), "stale readiness self-woke the input loop");
    }
    assert_eq!(
        task.reads, reads,
        "pending sleep was bypassed without bytes"
    );

    writer.write_all(b"y").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'y'));
}

#[tokio::test(start_paused = true)]
async fn a_partial_read_rearms_readiness_without_a_public_event() {
    let (mut source, mut writer) = SyntheticEvents::pair();
    source.discard_next = true;
    let mut task = spawn(source);
    assert!(task.poll_next().is_pending());
    writer.write_all(b"_").unwrap();
    awakened(&task).await;
    assert!(task.poll_next().is_pending());
    assert_eq!(task.reads, 2);
    assert!(!task.is_woken());

    writer.write_all(b"x").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'x'));
    assert_eq!(task.reads, 3);
}

#[tokio::test(start_paused = true)]
async fn a_readiness_hint_without_reader_progress_does_not_self_spin() {
    let (mut source, mut writer) = SyntheticEvents::pair();
    source.hold_input = true;
    let mut task = spawn(source);
    assert!(task.poll_next().is_pending());
    writer.write_all(b"xy").unwrap();
    awakened(&task).await;
    assert!(task.poll_next().is_pending());
    assert!(
        !task.is_woken(),
        "unproductive readiness must use the fallback"
    );
    let reads = task.reads;
    for _ in 0..8 {
        tokio::task::yield_now().await;
        assert!(!task.is_woken());
    }
    assert_eq!(task.reads, reads);
    // The wake hint neither consumed bytes nor cleared genuine readiness.
    task.hold_input = false;
    assert_eq!(event(task.poll_next()), key(b'x'));
    assert_eq!(event(task.poll_next()), key(b'y'));
}

#[tokio::test(start_paused = true)]
async fn cancellation_and_cede_leave_bytes_with_the_granted_owner() {
    let (source, mut writer) = SyntheticEvents::pair();
    let mut holder = source.reader.try_clone().unwrap();
    let ceded = Arc::new(AtomicBool::new(false));
    let mut input = TerminalInput::from_stream(source).with_cede_flag(ceded.clone());
    // Like select!: arm next(), then cancel it before transferring ownership.
    {
        let mut next = spawn(input.next());
        assert!(next.poll().is_pending());
    }
    let source_polls = input.source.source_polls;
    ceded.store(true, Ordering::SeqCst);
    let mut task = spawn(input);
    writer.write_all(b"C").unwrap();
    // Let the armed reactor observe the ceded byte, without a host source poll.
    for _ in 0..8 {
        tokio::task::yield_now().await;
        assert!(task.poll_next().is_pending());
    }
    assert_eq!(task.source.source_polls, source_polls);
    let mut byte = [0];
    holder.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"C", "the notifier consumed the holder's byte");

    ceded.store(false, Ordering::SeqCst);
    assert!(task.poll_next().is_pending());
    writer.write_all(b"R").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'R'));
}

#[tokio::test(start_paused = true)]
async fn fallback_timer_keeps_internal_events_and_errors() {
    for notifier_available in [true, false] {
        let (mut source, _writer) = SyntheticEvents::pair();
        if !notifier_available {
            source.events.tty.get_mut().unwrap().take();
        }
        let mut task = spawn(source);
        assert!(task.poll_next().is_pending());
        task.queued.push_back(Ok(Event::Resize(80, 24)));
        assert!(task.poll_next().is_pending());
        tokio::time::advance(Duration::from_millis(10)).await;
        assert_eq!(event(task.poll_next()), Event::Resize(80, 24));

        assert!(task.poll_next().is_pending());
        task.queued
            .push_back(Err(io::Error::other("reader failed")));
        tokio::time::advance(Duration::from_millis(10)).await;
        match task.poll_next() {
            Poll::Ready(Some(Err(error))) => assert_eq!(error.to_string(), "reader failed"),
            other => panic!("reader error was changed: {other:?}"),
        }
    }
}

#[tokio::test(start_paused = true)]
async fn hangup_disables_the_optional_notifier_not_the_source() {
    let (source, writer) = SyntheticEvents::pair();
    let mut task = spawn(source);
    assert!(task.poll_next().is_pending());
    drop(writer);
    awakened(&task).await;
    task.queued.push_back(Ok(Event::Resize(90, 30)));
    assert!(task.poll_next().is_pending());
    assert!(task.events.tty.get().unwrap().is_none());
    assert!(!task.is_woken());
    tokio::time::advance(Duration::from_millis(10)).await;
    assert_eq!(event(task.poll_next()), Event::Resize(90, 30));
}

#[tokio::test(start_paused = true)]
async fn drop_closes_the_notifier_after_cancelling_an_armed_wait() {
    let (mut source, mut writer) = SyntheticEvents::pair();
    {
        let mut next = spawn(source.next());
        assert!(next.poll().is_pending());
    }
    drop(source);
    // No integer-fd reuse race: EOF on the peer proves that both the synthetic
    // foreground reader and its notifier descriptor were closed.
    assert_eq!(writer.read(&mut [0]).unwrap(), 0);
}

fn termios(fd: std::os::fd::RawFd) -> libc::termios {
    let mut attributes = std::mem::MaybeUninit::uninit();
    // SAFETY: tcgetattr writes one termios and fd belongs to this test.
    assert_eq!(unsafe { libc::tcgetattr(fd, attributes.as_mut_ptr()) }, 0);
    unsafe { attributes.assume_init() }
}

fn assert_same_termios(before: &libc::termios, after: &libc::termios) {
    assert_eq!(before.c_iflag, after.c_iflag);
    assert_eq!(before.c_oflag, after.c_oflag);
    assert_eq!(before.c_cflag, after.c_cflag);
    assert_eq!(before.c_lflag, after.c_lflag);
    assert_eq!(before.c_cc, after.c_cc);
    // SAFETY: both references are initialized termios values.
    unsafe {
        assert_eq!(libc::cfgetispeed(before), libc::cfgetispeed(after));
        assert_eq!(libc::cfgetospeed(before), libc::cfgetospeed(after));
    }
}

const PTY_MASTER_ENV: &str = "OCTET_TEST_FOREGROUND_PTY_MASTER";

// Run the actual ForegroundEvents + crossterm reader in an isolated process.
// Parent/test-runner stdin and crossterm's singleton must never be modified.
#[test]
fn real_pty_foreground_notifier_contract() {
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    if let Ok(master) = std::env::var(PTY_MASTER_ENV) {
        // SAFETY: only the parent below passes this owned, inherited master.
        let master = unsafe { File::from_raw_fd(master.parse().unwrap()) };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .start_paused(true)
            .build()
            .unwrap();
        runtime.block_on(real_pty_child_contract(master));
        return;
    }

    struct Reap(Option<Child>);
    impl Drop for Reap {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    // Also cover tty stdin without a controlling terminal: crossterm still
    // chooses stdin, so an unconditional /dev/tty watcher is not equivalent.
    for controlling in [true, false] {
        let (mut master_fd, mut slave_fd) = (-1, -1);
        // SAFETY: openpty writes two fds; null optional arguments use defaults.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master_fd,
                    &mut slave_fd,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: successful openpty returned two newly owned descriptors.
        let master = unsafe { File::from_raw_fd(master_fd) };
        let slave = unsafe { File::from_raw_fd(slave_fd) };
        // Establish the ordinary raw/restore reference for tty stdin without
        // a controlling session. On macOS a pristine PTY acquires EXTPROC after
        // even a plain cfmakeraw/tcsetattr roundtrip, without any notifier; a
        // controlling session's exit clears it again. Leave that fixture
        // pristine, and compare exact attributes in both cases (no flag masks).
        if !controlling {
            let cooked = termios(slave.as_raw_fd());
            let mut raw = cooked;
            // SAFETY: raw is initialized and slave_fd is this disposable PTY.
            unsafe {
                libc::cfmakeraw(&mut raw);
                assert_eq!(libc::tcsetattr(slave_fd, libc::TCSANOW, &raw), 0);
                assert_eq!(libc::tcsetattr(slave_fd, libc::TCSANOW, &cooked), 0);
            }
        }
        let original = termios(master.as_raw_fd());
        // Protect parent-owned fds against unrelated parallel test execs.
        for fd in [master_fd, slave_fd] {
            // SAFETY: these are owned PTY descriptors, never process stdin.
            assert_eq!(
                unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) },
                0
            );
        }
        let test = format!(
            "{}::real_pty_foreground_notifier_contract",
            module_path!().split_once("::").unwrap().1
        );
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", &test, "--nocapture"])
            .env(PTY_MASTER_ENV, master_fd.to_string())
            .stdin(Stdio::from(slave))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // SAFETY: pre_exec performs only async-signal-safe syscalls. stdin is
        // the disposable slave; inherit the master for deterministic injection.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1
                    || (controlling
                        && libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as libc::c_ulong, 0)
                            == -1)
                    || libc::fcntl(master_fd, libc::F_SETFD, 0) == -1
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = Reap(Some(command.spawn().unwrap()));
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "PTY input contract timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = child.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "controlling={controlling}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        // Prevent a zero-tests subprocess from masquerading as coverage.
        assert!(String::from_utf8_lossy(&output.stdout).contains("running 1 test"));
        assert_same_termios(&original, &termios(master_fd));
    }
}

async fn real_pty_child_contract(mut master: std::fs::File) {
    use std::os::fd::AsRawFd;

    struct RestoreRaw;
    impl Drop for RestoreRaw {
        fn drop(&mut self) {
            crossterm::terminal::disable_raw_mode().unwrap();
        }
    }
    // SAFETY: subprocess stdin is the disposable slave installed by the parent.
    let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
    assert_ne!(flags, -1);
    crossterm::terminal::enable_raw_mode().unwrap();
    let _restore = RestoreRaw;
    let raw = termios(libc::STDIN_FILENO);
    let ceded = Arc::new(AtomicBool::new(false));
    let mut input = TerminalInput::new().with_cede_flag(ceded.clone());
    // Cancel the actual next() future after arming the production notifier.
    {
        let mut next = spawn(input.next());
        assert!(next.poll().is_pending());
    }
    let mut task = spawn(input);
    assert!(task.poll_next().is_pending());
    let tty = task
        .source
        .tty
        .get()
        .unwrap()
        .as_ref()
        .expect("PTY notifier registered");
    // SAFETY: tty is owned by the task; F_GETFL only inspects flags.
    let notifier_flags = unsafe { libc::fcntl(tty.get_ref().as_raw_fd(), libc::F_GETFL) };
    assert_ne!(notifier_flags, -1);
    assert_eq!(
        notifier_flags, flags,
        "notifier changed stdin's status flags"
    );
    // SAFETY: F_GETFD inspects the notifier's descriptor-local flags only.
    assert_ne!(
        unsafe { libc::fcntl(tty.get_ref().as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
        0
    );
    let deadline = task.source.wake.as_ref().unwrap().deadline();
    assert_eq!(
        deadline - tokio::time::Instant::now(),
        Duration::from_millis(10)
    );
    master.write_all(b"x").unwrap();
    awakened(&task).await;
    assert!(tokio::time::Instant::now() < deadline);
    assert_eq!(event(task.poll_next()), key(b'x'));
    // The actual decoder drained x; stale readiness must not self-spin.
    assert!(task.poll_next().is_pending());
    for _ in 0..32 {
        assert!(task.poll_next().is_pending());
        assert!(!task.is_woken());
    }
    master.write_all(b"y").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'y'));

    let mut input = task.into_inner();
    {
        let mut next = spawn(input.next());
        assert!(next.poll().is_pending());
    }
    let sleep = input.source.wake.as_ref().unwrap().deadline();
    ceded.store(true, Ordering::SeqCst);
    let mut task = spawn(input);
    let mut holder = std::fs::File::from(ForegroundEvents::open_tty_notifier().unwrap());
    master.write_all(b"C").unwrap();
    for _ in 0..32 {
        tokio::task::yield_now().await;
        assert!(task.poll_next().is_pending());
        assert_eq!(task.source.wake.as_ref().unwrap().deadline(), sleep);
    }
    let mut ready = libc::pollfd {
        fd: holder.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one initialized pollfd for an owned descriptor. A real-time
    // watchdog only: Tokio's paused clock still cannot fire the 10 ms timer.
    assert_eq!(unsafe { libc::poll(&mut ready, 1, 1000) }, 1);
    let mut byte = [0];
    holder.read_exact(&mut byte).unwrap();
    assert_eq!(&byte, b"C", "host consumed the granted owner's byte");
    ceded.store(false, Ordering::SeqCst);
    assert!(task.poll_next().is_pending());
    master.write_all(b"r").unwrap();
    awakened(&task).await;
    assert_eq!(event(task.poll_next()), key(b'r'));
    assert!(
        task.poll_next().is_pending(),
        "duplicated or retained ceded input"
    );
    let mut input = task.into_inner();
    mouse_tests::real_pty_mouse_decoder_contract(&mut master, &mut input).await;
    drop(input);
    // Notifier lifetime, cancellation, cede and rearm preserve reader flags
    // and raw-mode termios. RestoreRaw then restores the parent's attributes.
    assert_eq!(
        unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) },
        flags
    );
    assert_same_termios(&raw, &termios(libc::STDIN_FILENO));
}

#[path = "mouse_tests.rs"]
mod mouse_tests;
