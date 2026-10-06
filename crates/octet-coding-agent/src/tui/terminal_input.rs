//! One input owner for the background probe, editor, panels and lifecycle waits.
//!
//! Crossterm 0.28 decodes OSC as ordinary keys on Unix: Alt+], literal text,
//! then Ctrl+G (BEL) or Alt+backslash (ST). A read boundary can also split either
//! escape into Esc plus a character. Recognize only a complete OSC 11 color
//! reply to our own outstanding query; never filter Paste or discard a timeout's
//! worth of input. Query/identified-reply state has memory bounds, not an expiry:
//! a terminal's response can arrive arbitrarily later than the startup wait.
//!
//! Windows consoles decode a reply as one key-down and one key-up per byte
//! (ConPTY synthesizes both). Releases are never reply bytes; they stay with the
//! held fragment so they neither flush an unconfirmed prefix into the composer
//! nor survive a recognized reply.
//!
//! Before the full OSC 11 header is recognized, legacy Esc/Alt+] is inherently
//! ambiguous with genuine input. Only that unconfirmed prefix has a 250 ms idle
//! deadline, after which its original events are replayed. An opener split more
//! slowly than that can therefore reach input. Once ESC ] 11 ; is recognized,
//! no timer replays its body or a fragmented ST. Original events remain bounded
//! and are replayed only on a mismatch, overflow, input error or EOF.
//!
//! Unix bytes are decoded here rather than by crossterm's global reader, because
//! a Pi extension must receive the *raw* spelling the terminal produced (see
//! [`codec`]). Every non-protocol event therefore keeps its original bytes and
//! passes the bounded pre-native lane in [`intercept`] before any native owner
//! sees it; ordinary octet input is unchanged because that lane is empty unless
//! an extension bound the pre-native channel to this frontend.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
#[cfg(unix)]
use std::os::{fd::OwnedFd, unix::fs::OpenOptionsExt};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use sexy_tui_rs::terminal_colors::{parse_osc11_background_color, RgbColor};
#[cfg(unix)]
use tokio::io::{unix::AsyncFd, Interest};
use tokio::time::Sleep;

// A valid reply remains recognizable until the outstanding query is answered.
// Only an ambiguous, unconfirmed header is subject to an input-latency bound.
const PREFIX_AMBIGUITY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_REPLY_BYTES: usize = 128;
// Held original events: every reply byte plus, on Windows, its key-up.
const MAX_HELD_EVENTS: usize = MAX_REPLY_BYTES * 2;
#[path = "terminal_input/codec.rs"]
mod codec;
#[path = "terminal_input/intercept.rs"]
mod intercept;
#[cfg(unix)]
#[path = "terminal_input/raw.rs"]
mod raw;
pub use codec::Packet;
pub(crate) use intercept::InputInterceptors;

const OSC11_PREFIX: &str = "\x1b]11;";

#[derive(Default)]
struct BackgroundReplies {
    pending: bool,
    prefix_updated: Option<Instant>,
    text: String,
    held: Vec<Packet>,
    ready: VecDeque<Packet>,
    color: Option<RgbColor>,
    /// Key-up still owed by a Windows console for the reply's final byte.
    trailing_release: Option<(KeyCode, KeyModifiers)>,
    /// Always-on terminal → program TSP message filter (Tern surfaces).
    apc: ApcFilter,
}

impl BackgroundReplies {
    fn begin_query(&mut self, now: Instant) -> bool {
        self.expire(now);
        if self.pending {
            // There is no safe timeout after which an outstanding terminal
            // reply becomes user input. Keep one request, however late it is.
            return false;
        }
        self.pending = true;
        self.color = None;
        true
    }

    fn deadline(&self) -> Option<Instant> {
        let osc = if self.text.starts_with(OSC11_PREFIX) {
            None
        } else {
            self.prefix_updated
                .map(|updated| updated + PREFIX_AMBIGUITY_TIMEOUT)
        };
        match (osc, self.apc.deadline()) {
            (Some(osc), Some(apc)) => Some(osc.min(apc)),
            (osc, apc) => osc.or(apc),
        }
    }

    fn replay(&mut self) {
        self.ready.extend(self.held.drain(..));
        self.ready.extend(self.apc.flush());
        self.clear_fragment();
    }

    /// Route one event through the always-on APC/TSP filter first: terminal →
    /// program TSP messages (`ESC _ tsp;… ESC \`) are protocol traffic, never
    /// octet input. Released events continue through the OSC 11 filter.
    fn push_filtered(&mut self, event: impl Into<Packet>, now: Instant) {
        for event in self.apc.push(event.into(), now) {
            self.push(event, now);
        }
    }

    fn clear_fragment(&mut self) {
        self.text.clear();
        self.held.clear();
        self.prefix_updated = None;
    }

    fn expire(&mut self, now: Instant) {
        let released = self.apc.expire(now);
        self.ready.extend(released);
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.replay();
        }
    }

    fn push(&mut self, event: impl Into<Packet>, now: Instant) {
        let event = event.into();
        self.expire(now);
        if let Some((code, modifiers)) = self.trailing_release.take() {
            if matches!(&event.event, Event::Key(key)
                if key.kind == KeyEventKind::Release
                    && key.code == code
                    && key.modifiers == modifiers)
            {
                return;
            }
        }
        if !self.pending {
            self.ready.push_back(event);
            return;
        }
        // Resize/focus/mouse notifications can arrive between reply fragments.
        // They are not part of the byte protocol and must remain responsive.
        if matches!(
            event.event,
            Event::Resize(..) | Event::FocusGained | Event::FocusLost | Event::Mouse(_)
        ) {
            self.ready.push_back(event);
            return;
        }
        let Some(fragment) = reply_fragment(&event) else {
            if is_key_release(&event.event) && !self.text.is_empty() {
                // A Windows key-up for a held reply byte. Keep it in order so a
                // mismatch replays exactly what arrived and a match discards it.
                self.held.push(event);
                if self.held.len() > MAX_HELD_EVENTS {
                    self.replay();
                }
                return;
            }
            // A bracketed paste or a shortcut can arrive between identified
            // reply fragments. It remains genuine input, not payload, and
            // must not flush terminal-response text into the next owner.
            if !self.text.starts_with(OSC11_PREFIX) {
                self.replay();
            }
            self.ready.push_back(event);
            return;
        };
        let mut candidate = self.text.clone();
        candidate.push_str(fragment);
        match candidate_status(&candidate) {
            Candidate::Prefix => {
                self.text = candidate;
                self.held.push(event);
                self.prefix_updated = Some(now);
            }
            Candidate::Color(color) => {
                self.color = Some(color);
                self.pending = false;
                self.clear_fragment();
                if let Event::Key(key) = &event.event {
                    self.trailing_release = Some((key.code, key.modifiers));
                }
            }
            Candidate::NotReply => {
                self.replay();
                // A mismatch may itself begin a new reply (e.g. Esc, Alt+]).
                if matches!(candidate_status(fragment), Candidate::Prefix) {
                    self.text = fragment.to_owned();
                    self.held.push(event);
                    self.prefix_updated = Some(now);
                } else {
                    self.ready.push_back(event);
                }
            }
        }
    }
}

/// Decode TSP traffic under the same input owner as OSC 11. Only ambiguous
/// openers expire. An identified reply must never become typed composer text.
#[derive(Default)]
struct ApcFilter {
    text: String,
    held: Vec<Packet>,
    active: bool,
    updated: Option<Instant>,
    discarding: bool,
    trailing_release: Option<(KeyCode, KeyModifiers)>,
    reader: octet_tern::frame::Reader,
    handler: Option<crate::tui::view::tern_input::Handler>,
    #[cfg(test)]
    force_enabled: bool,
}

impl ApcFilter {
    const PREFIX: &'static str = "\x1b_tsp;";
    const MAX_BYTES: usize = 1 << 20;

    fn enabled(&self) -> bool {
        #[cfg(test)]
        if self.force_enabled {
            return true;
        }
        crate::tui::view::tern::enabled_cached()
    }

    fn identified(&self) -> bool {
        self.discarding || self.text.starts_with(Self::PREFIX)
    }

    fn push(&mut self, event: impl Into<Packet>, now: Instant) -> Vec<Packet> {
        let event = event.into();
        if !self.enabled() {
            return vec![event];
        }
        if let Some((code, modifiers)) = self.trailing_release.take() {
            if matches!(&event.event, Event::Key(key) if key.kind == KeyEventKind::Release && key.code == code && key.modifiers == modifiers)
            {
                return Vec::new();
            }
        }
        if !self.active {
            if reply_fragment(&event).is_some_and(|s| s == "\x1b" || s == "\x1b_") {
                self.active = true;
                self.updated = Some(now);
                self.text = reply_fragment(&event)
                    .expect("recognized opener")
                    .to_owned();
                self.held.push(event);
                return Vec::new();
            }
            return vec![event];
        }
        match reply_fragment(&event) {
            Some(fragment) => {
                self.updated = Some(now);
                self.text.push_str(fragment);
                if !self.identified() {
                    self.held.push(event.clone());
                }
                if Self::PREFIX.starts_with(&self.text) {
                    return Vec::new();
                }
                if !self.identified() {
                    // The held bytes are not a Tern opener, so they are the
                    // user's keys. The event that proved it may itself open a
                    // message (an Esc held for an interrupt, then `Alt+_`), so
                    // it is evaluated on its own rather than released with them;
                    // otherwise the message body leaks into the draft.
                    self.held.pop();
                    let mut released = self.flush();
                    released.extend(self.push(event, now));
                    return released;
                }
                self.held.clear();
                if self.text.ends_with('\x07') || self.text.ends_with("\x1b\\") {
                    if let Event::Key(key) = event.event {
                        self.trailing_release = Some((key.code, key.modifiers));
                    }
                    let sequence = if self.text.ends_with('\x07') {
                        format!("{}\x1b\\", self.text.trim_end_matches('\x07'))
                    } else {
                        std::mem::take(&mut self.text)
                    };
                    let message = (!self.discarding)
                        .then(|| self.reader.feed(&sequence))
                        .flatten();
                    self.reset();
                    return message
                        .and_then(|message| self.handler.as_ref()?.as_ref()(message))
                        // Protocol traffic becomes native input; it has no
                        // terminal spelling and must never enter the raw lane.
                        .map(Packet::synthetic)
                        .into_iter()
                        .collect();
                }
                if self.text.len() > Self::MAX_BYTES || self.discarding {
                    // Keep only enough tail to recognize a split ST. Oversized
                    // protocol traffic is rejected, never replayed as input.
                    self.discarding = true;
                    self.text = if self.text.ends_with('\x1b') {
                        "\x1b".into()
                    } else {
                        String::new()
                    };
                }
                Vec::new()
            }
            None if is_key_release(&event.event) => {
                if !self.identified() {
                    self.held.push(event);
                }
                Vec::new()
            }
            None if self.identified() => vec![event],
            None => {
                let mut released = self.flush();
                released.push(event);
                released
            }
        }
    }

    fn reset(&mut self) {
        self.active = false;
        self.discarding = false;
        self.text.clear();
        self.held.clear();
        self.updated = None;
    }

    fn flush(&mut self) -> Vec<Packet> {
        let released = if self.identified() {
            Vec::new()
        } else {
            std::mem::take(&mut self.held)
        };
        self.reset();
        released
    }

    fn deadline(&self) -> Option<Instant> {
        if self.active && !self.identified() {
            self.updated.map(|at| at + PREFIX_AMBIGUITY_TIMEOUT)
        } else {
            None
        }
    }

    fn expire(&mut self, now: Instant) -> Vec<Packet> {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.flush()
        } else {
            Vec::new()
        }
    }
}

fn is_key_release(event: &Event) -> bool {
    matches!(event, Event::Key(key) if key.kind == KeyEventKind::Release)
}

/// One reply-byte fragment, or `None` when this packet is not part of the reply
/// byte protocol.
///
/// The decoder already produced the terminal's own bytes, so this classifies
/// them instead of rebuilding them from a key event: only a key press whose
/// spelling is a single text character or one of the legacy escape forms can be
/// reply payload. Console records (no byte stream) arrive with the same fields
/// filled from [`codec::legacy_spelling`], so both platforms share one path.
fn reply_fragment(packet: &Packet) -> Option<&str> {
    if matches!(&packet.event, Event::Key(key) if key.kind != KeyEventKind::Press) {
        return None;
    }
    let raw = packet.raw.as_deref()?;
    match raw {
        "\x1b" | "\x1b]" | "\x1b_" | "\x1b\\" | "\x07" => Some(raw),
        _ => {
            let mut characters = raw.chars();
            match (characters.next(), characters.next()) {
                (Some(character), None) if !character.is_control() => Some(raw),
                _ => None,
            }
        }
    }
}

enum Candidate {
    Prefix,
    Color(RgbColor),
    NotReply,
}

fn candidate_status(text: &str) -> Candidate {
    if text.len() > MAX_REPLY_BYTES {
        return Candidate::NotReply;
    }
    if OSC11_PREFIX.starts_with(text) {
        return Candidate::Prefix;
    }
    let Some(body) = text.strip_prefix(OSC11_PREFIX) else {
        return Candidate::NotReply;
    };
    if body.ends_with('\x07') || body.ends_with("\x1b\\") {
        return parse_osc11_background_color(text)
            .map(Candidate::Color)
            .unwrap_or(Candidate::NotReply);
    }
    // Allow a fragmented ST, but no embedded escapes/controls. A non-color
    // payload is replayed, not silently discarded, when it ends or overflows.
    let body = body.strip_suffix('\x1b').unwrap_or(body);
    if body
        .bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        Candidate::Prefix
    } else {
        Candidate::NotReply
    }
}

/// Foreground polling, with no detached or armed terminal reader.
///
/// Crossterm's EventStream keeps a background read armed after next() is
/// cancelled. A terminal grant cannot quiesce that reader with an atomic flag.
/// Poll synchronously for at most 1 ms on the frontend thread instead: when we
/// yield, the terminal has no pending host read and may be handed to another
/// owner. The supported level-triggered tty reader needs a positive timeout;
/// its zero-timeout path does not inspect even already-buffered input.
///
/// On Unix a separate descriptor watches readiness ONLY, so incoming bytes can
/// wake the foreground poll before its 10 ms fallback timer. It never reads,
/// including while TerminalInput is ceded. The timer still discovers resize,
/// crossterm-internal events and input on platforms without this optional wake.
#[derive(Default)]
pub struct ForegroundEvents {
    #[cfg(unix)]
    raw: raw::RawReader,
    wake: Option<Pin<Box<Sleep>>>,
    // Open/register lazily: construction must not need a tty or Tokio runtime.
    #[cfg(unix)]
    tty: std::sync::OnceLock<Option<AsyncFd<OwnedFd>>>,
}

impl ForegroundEvents {
    #[cfg(unix)]
    fn open_tty_notifier() -> Option<OwnedFd> {
        use std::os::fd::FromRawFd;

        // Match crossterm 0.29's tty_fd(): tty stdin is authoritative even
        // when it is NOT the controlling terminal; otherwise use /dev/tty.
        // SAFETY: isatty only inspects the process's standard input descriptor.
        if unsafe { libc::isatty(libc::STDIN_FILENO) } == 1 {
            // macOS kqueue rejects the /dev/tty alias with EINVAL. Duplicating
            // the actual reader descriptor avoids both that alias and a named
            // device reopen/identity race. This shares stdin's file description:
            // NEVER change its status flags (especially O_NONBLOCK) or termios.
            // AsyncFd is used ONLY for readiness, never read/try_io, so observing
            // a blocking descriptor cannot block the frontend on a byte read.
            // SAFETY: fcntl creates a new owned, close-on-exec descriptor.
            let fd = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_DUPFD_CLOEXEC, 0) };
            if fd < 0 {
                return None;
            }
            // SAFETY: fd is the newly owned descriptor returned by fcntl.
            return Some(unsafe { OwnedFd::from_raw_fd(fd) });
        }
        // Only redirected stdin uses crossterm's /dev/tty source. This is an
        // independent open; its flags do not affect another reader descriptor.
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open("/dev/tty")
            .ok()
            .map(Into::into)
    }

    #[cfg(unix)]
    fn tty_readable(&mut self, cx: &mut Context<'_>) -> bool {
        let tty = self.tty.get_or_init(|| {
            let fd = Self::open_tty_notifier()?;
            // Polling runs on the frontend's I/O-enabled Tokio runtime.
            // OwnedFd closes only after AsyncFd deregisters it.
            AsyncFd::with_interest(fd, Interest::READABLE).ok()
        });
        let Some(tty) = tty else { return false };
        match Self::poll_tty_readiness(tty, cx) {
            Poll::Ready(Ok(())) => true,
            Poll::Pending => false,
            Poll::Ready(Err(_)) => {
                // A failed/hung-up notifier is not an input error. Keep the
                // original crossterm/timer path, without repeated ready wakes.
                self.tty.get_mut().expect("initialized above").take();
                false
            }
        }
    }

    #[cfg(not(unix))]
    fn tty_readable(&mut self, _cx: &mut Context<'_>) -> bool {
        false
    }

    #[cfg(unix)]
    fn poll_tty_readiness(tty: &AsyncFd<OwnedFd>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        use std::os::fd::AsRawFd;

        // Crossterm (or a ceded owner) may already have drained the bytes that
        // produced Tokio's cached readiness. Check without consuming anything,
        // then clear only the observed stale generation and register the waker
        // again. Bound retries if readiness changes during this check.
        for _ in 0..2 {
            let mut ready = std::task::ready!(tty.poll_read_ready(cx))?;
            let mut fd = libc::pollfd {
                fd: tty.get_ref().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: fd is one initialized pollfd, owned for this call; timeout
            // zero only inspects readiness and cannot read or wait for input.
            if unsafe { libc::poll(&mut fd, 1, 0) } < 0 {
                return Poll::Ready(Err(io::Error::last_os_error()));
            }
            if fd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Poll::Ready(Err(io::Error::other("terminal notifier closed")));
            }
            if fd.revents & libc::POLLIN != 0 {
                return Poll::Ready(Ok(()));
            }
            ready.clear_ready();
        }
        Poll::Pending
    }

    // The injected synchronous read keeps tests off process-global stdin and
    // crossterm state; production still has exactly one poll/read byte owner.
    fn poll_next_with<T>(
        &mut self,
        cx: &mut Context<'_>,
        read_event: impl FnOnce() -> io::Result<Option<T>>,
    ) -> Poll<Option<io::Result<T>>> {
        let readable = self.tty_readable(cx);
        if let Some(wake) = &mut self.wake {
            if wake.as_mut().poll(cx).is_pending() && !readable {
                return Poll::Pending;
            }
            self.wake = None;
        }
        match read_event() {
            Ok(Some(event)) => Poll::Ready(Some(Ok(event))),
            Err(error) => Poll::Ready(Some(Err(error))),
            Ok(None) => {
                let mut wake = Box::pin(tokio::time::sleep(Duration::from_millis(10)));
                let _ = wake.as_mut().poll(cx);
                self.wake = Some(wake);
                // Rearm after a partial/filtered read drained the tty. If bytes
                // arrived during the read, retry once; a readiness wake that
                // made no progress must fall back to the timer, not self-spin.
                if self.tty_readable(cx) && !readable {
                    cx.waker().wake_by_ref();
                }
                Poll::Pending
            }
        }
    }
}

impl Stream for ForegroundEvents {
    type Item = io::Result<Packet>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        #[cfg(unix)]
        {
            let mut raw = std::mem::take(&mut this.raw);
            let result = this.poll_next_with(cx, || raw.read());
            this.raw = raw;
            result
        }
        #[cfg(not(unix))]
        this.poll_next_with(cx, || {
            if crossterm::event::poll(Duration::from_millis(1))? {
                crossterm::event::read().map(|event| Some(event.into()))
            } else {
                Ok(None)
            }
        })
    }
}

#[cfg(all(test, unix))]
#[path = "terminal_input/foreground_tests.rs"]
mod foreground_tests;

/// Filtered, cancellation-safe interactive input. Pass this same owner to every
/// panel and lifecycle loop; raw crossterm events must never reach extensions.
pub struct TerminalInput<S = ForegroundEvents> {
    source: S,
    replies: BackgroundReplies,
    timer: Option<Pin<Box<Sleep>>>,
    timer_deadline: Option<Instant>,
    ended: bool,
    input_error: Option<io::Error>,
    intercept: intercept::PendingInput,
    /// Set by the shell while an extension grant holds the raw terminal. While
    /// set the stream never polls its source, so a ceded byte stays with the
    /// child that owns `/dev/tty` instead of being stolen by the host.
    ceded: Arc<AtomicBool>,
}

impl TerminalInput {
    pub fn new() -> Self {
        Self::from_source(ForegroundEvents::default())
    }

    #[cfg(all(test, unix))]
    pub(crate) fn from_terminal_file(file: std::fs::File) -> Self {
        let notifier =
            AsyncFd::with_interest(OwnedFd::from(file.try_clone().unwrap()), Interest::READABLE)
                .unwrap();
        Self::from_source(ForegroundEvents {
            raw: raw::RawReader::from_file(file),
            tty: std::sync::OnceLock::from(Some(notifier)),
            ..Default::default()
        })
    }
}

impl Default for TerminalInput {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> TerminalInput<S> {
    /// Build the input owner over an injected event source. Production always
    /// uses [`Self::new`]; this is the test seam that lets one test drive the
    /// real stream with a synthetic source of plain crossterm events.
    #[cfg(test)]
    pub fn from_stream(source: S) -> Self
    where
        S: Stream<Item = io::Result<Event>>,
    {
        Self::from_source(source)
    }

    fn from_source(source: S) -> Self {
        Self {
            source,
            replies: BackgroundReplies::default(),
            timer: None,
            timer_deadline: None,
            ended: false,
            input_error: None,
            intercept: intercept::PendingInput::default(),
            ceded: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(crate) fn with_interceptors(mut self, handle: InputInterceptors) -> Self {
        self.intercept.handle = handle;
        self
    }

    /// Route decoded native protocol messages without giving up stdin ownership.
    pub(crate) fn with_tern_handler(
        mut self,
        handler: crate::tui::view::tern_input::Handler,
    ) -> Self {
        self.replies.apc.handler = Some(handler);
        self
    }

    /// Park this stream on a shared cede flag owned by the shell.
    ///
    /// The flag is the one piece of terminal authority a granted extension can
    /// temporarily hold: while it is set, the host reads no raw bytes and the
    /// ceded child owns them.
    pub fn with_cede_flag(mut self, ceded: Arc<AtomicBool>) -> Self {
        self.ceded = ceded;
        self
    }
}

impl<E: Into<Packet>, S: Stream<Item = io::Result<E>> + Unpin> TerminalInput<S> {
    /// The probe and interactive frontend share a stream from the outset. This
    /// replaces the synchronous raw-stdin read/handoff, which lost typing and
    /// raced crossterm when Auto was selected after startup.
    pub(super) async fn query_background_color(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<RgbColor>> {
        use std::io::Write;

        // Consume a late result once. Otherwise a new explicit Auto selection
        // may probe again, but never overlap an unanswered request.
        if let Some(color) = self.replies.color.take() {
            return Ok(Some(color));
        }
        if self.replies.begin_query(Instant::now()) {
            let mut out = std::io::stdout();
            out.write_all(b"\x1b]11;?\x1b\\")?;
            out.flush()?;
        }
        Ok(self.read_background_color(timeout).await)
    }

    async fn read_background_color(&mut self, timeout: Duration) -> Option<RgbColor> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            // Stop on real input, keeping it for its ordinary owner. This
            // bounds the probe queue even under an unbracketed input flood.
            if !self.replies.ready.is_empty() || self.ended || self.input_error.is_some() {
                return None;
            }
            let event = match tokio::time::timeout_at(deadline, self.source.next()).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(error))) => {
                    self.input_error = Some(error);
                    self.replies.replay();
                    return None;
                }
                Ok(None) => {
                    self.ended = true;
                    self.replies.replay();
                    return None;
                }
                Err(_) => return None,
            };
            self.replies.push_filtered(event, Instant::now());
            if let Some(color) = self.replies.color.take() {
                return Some(color);
            }
        }
    }
}

impl<E: Into<Packet>, S: Stream<Item = io::Result<E>> + Unpin> Stream for TerminalInput<S> {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // A ceded foreground grant owns the raw terminal. Returning without
        // polling the source is the whole parking guarantee: no ceded byte is
        // read or buffered by the host. The surrounding loop has its own
        // periodic wakeups, so it still re-polls this stream after release.
        if this.ceded.load(Ordering::SeqCst) {
            return Poll::Pending;
        }
        // The state and timer belong to the stream, not next()'s future. A
        // select! cancellation or an input-owner change cannot lose a prefix.
        for _ in 0..256 {
            // The pre-native lane holds at most one event and everything
            // behind it stays ordered: a queued reply is delivered before the
            // next raw event is admitted.
            match this.intercept.poll(cx) {
                Some(Poll::Ready(event)) => return Poll::Ready(Some(Ok(event))),
                Some(Poll::Pending) => return Poll::Pending,
                None => {}
            }
            this.replies.expire(Instant::now());
            if let Some(packet) = this.replies.ready.pop_front() {
                this.intercept.start(packet);
                continue;
            }
            if let Some(error) = this.input_error.take() {
                return Poll::Ready(Some(Err(error)));
            }
            if this.ended {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.source).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => this.replies.push_filtered(event, Instant::now()),
                Poll::Ready(Some(Err(error))) => {
                    this.input_error = Some(error);
                    this.replies.replay();
                }
                Poll::Ready(None) => {
                    this.ended = true;
                    this.replies.replay();
                }
                Poll::Pending => {
                    let deadline = this.replies.deadline();
                    if this.timer_deadline != deadline {
                        this.timer_deadline = deadline;
                        this.timer = deadline
                            .map(|deadline| Box::pin(tokio::time::sleep_until(deadline.into())));
                    }
                    if let Some(timer) = &mut this.timer {
                        if timer.as_mut().poll(cx).is_ready() {
                            continue;
                        }
                    }
                    return Poll::Pending;
                }
            }
        }
        // Keep source polling bounded for the surrounding select! owner.
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    // Drive the production decoder at arbitrary read boundaries instead of a
    // second hand-written model of it: one contiguous read when `split_escapes`
    // is false, one byte per read when a fragmented sequence is under test. The
    // PTY lane below separately verifies the reader against actual bytes.
    fn decoded(bytes: &str, split_escapes: bool) -> Vec<Event> {
        let mut decoder = codec::Decoder::default();
        let mut events = Vec::new();
        let raw = bytes.as_bytes();
        for (index, byte) in raw.iter().enumerate() {
            let more = !split_escapes && index + 1 < raw.len();
            if let Some(packet) = decoder.push(*byte, more).unwrap() {
                events.push(packet.event);
            }
        }
        events
    }

    #[test]
    fn native_apc_is_routed_once_across_slow_fragments_and_key_releases() {
        for releases in [false, true] {
            let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
            let captured = seen.clone();
            let mut filter = ApcFilter {
                force_enabled: true,
                handler: Some(Arc::new(move |message| {
                    captured.lock().unwrap().push(message);
                    None
                })),
                ..Default::default()
            };
            let wire = octet_tern::frame::encode_json(
                octet_tern::wire::Verb::Event,
                &serde_json::json!({"ev":"ack","sf":"octet.session","s":12}),
                12,
            )
            .unwrap();
            let mut now = Instant::now();
            let events = if releases {
                windows_decoded(&wire)
            } else {
                decoded(&wire, true)
            };
            for event in events {
                now += Duration::from_millis(100);
                assert!(filter.expire(now).is_empty());
                assert!(filter.push(event, now).is_empty());
            }
            assert_eq!(
                seen.lock().unwrap().as_slice(),
                &[octet_tern::frame::Incoming::Event(
                    octet_tern::wire::Event::Ack {
                        sf: "octet.session".into(),
                        s: 12
                    }
                )]
            );
            assert!(filter.text.is_empty());
            assert!(filter.held.is_empty());
        }
    }

    #[test]
    fn escape_held_before_a_native_message_is_released_and_the_message_stays_protocol() {
        // A lone Esc (for example interrupting a run) is held while it could
        // still open a Tern message. When the next read starts a real message,
        // crossterm reports its opener as Alt+_ rather than a bare Esc then `_`.
        // The held Esc is the user's key and must be released, and the new opener
        // must start its own candidate; otherwise the whole message body leaked
        // into the composer as typed text.
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = seen.clone();
        let mut filter = ApcFilter {
            force_enabled: true,
            handler: Some(Arc::new(move |message| {
                captured.lock().unwrap().push(message);
                None
            })),
            ..Default::default()
        };
        let wire = octet_tern::frame::encode_json(
            octet_tern::wire::Verb::Event,
            &serde_json::json!({"ev":"ack","sf":"octet.session","s":7}),
            65536,
        )
        .unwrap();
        let body = wire
            .strip_prefix("\x1b_")
            .and_then(|rest| rest.strip_suffix("\x1b\\"))
            .expect("one APC string");
        let escape = key(KeyCode::Esc, KeyModifiers::NONE);
        let mut now = Instant::now();
        let mut released = filter.push(escape.clone(), now);
        now += Duration::from_millis(40);
        let events = std::iter::once(key(KeyCode::Char('_'), KeyModifiers::ALT))
            .chain(decoded(body, true))
            .chain(std::iter::once(key(KeyCode::Char('\\'), KeyModifiers::ALT)));
        for event in events {
            released.extend(filter.push(event, now));
            now += Duration::from_millis(5);
        }
        assert_eq!(released, vec![escape], "message text leaked as input");
        assert_eq!(seen.lock().unwrap().len(), 1);
        assert!(!filter.active && filter.text.is_empty() && filter.held.is_empty());
    }

    #[test]
    fn native_oversize_and_malformed_messages_never_replay_into_the_draft() {
        let now = Instant::now();
        let mut filter = ApcFilter {
            force_enabled: true,
            ..Default::default()
        };
        for event in decoded("\x1b_tsp;e;malformed\x1b\\", true) {
            assert!(filter.push(event, now).is_empty());
        }
        filter.text = format!("{}{}", ApcFilter::PREFIX, "x".repeat(ApcFilter::MAX_BYTES));
        filter.active = true;
        assert!(filter
            .push(key(KeyCode::Char('x'), KeyModifiers::NONE), now)
            .is_empty());
        assert!(filter.discarding);
        let paste = Event::Paste("genuine pasted 雪".into());
        assert_eq!(filter.push(paste.clone(), now), vec![paste]);
        assert!(filter.expire(now + Duration::from_secs(3600)).is_empty());
        assert!(filter
            .push(key(KeyCode::Char('\\'), KeyModifiers::ALT), now)
            .is_empty());
        assert!(!filter.active);
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(filter.push(enter.clone(), now), vec![enter]);
    }

    #[test]
    fn osc11_late_complete_and_fragmented_bel_st_replies_are_consumed() {
        for ending in ["\x07", "\x1b\\"] {
            for split_escapes in [false, true] {
                for body in ["rgb:1e1e/1e1e/1e1e", "#1e1e1e", "RGB:1E1E/1E1E/1E1E"] {
                    let start = Instant::now();
                    let mut replies = BackgroundReplies::default();
                    assert!(replies.begin_query(start));
                    let wire = format!("\x1b]11;{body}{ending}");
                    let mut now = start + Duration::from_millis(400);
                    for event in decoded(&wire, split_escapes) {
                        replies.push(event, now);
                        now += Duration::from_millis(20);
                        assert!(replies.ready.is_empty(), "partial reply escaped: {wire:?}");
                    }
                    assert_eq!(
                        replies.color,
                        Some(RgbColor {
                            r: 30,
                            g: 30,
                            b: 30
                        })
                    );
                    assert!(replies.held.is_empty());
                }
            }
        }
    }

    /// Windows console decoding: ConPTY turns every reply byte into a key-down
    /// and a key-up record, and crossterm reports both.
    fn windows_decoded(bytes: &str) -> Vec<Event> {
        decoded(bytes, true)
            .into_iter()
            .flat_map(|event| {
                let Event::Key(press) = event else {
                    unreachable!("the decoder yields key events")
                };
                let release = KeyEvent {
                    kind: KeyEventKind::Release,
                    ..press
                };
                [Event::Key(press), Event::Key(release)]
            })
            .collect()
    }

    #[test]
    fn osc11_windows_key_releases_do_not_leak_a_reply_into_input() {
        for ending in ["\x07", "\x1b\\"] {
            let start = Instant::now();
            let mut replies = BackgroundReplies::default();
            assert!(replies.begin_query(start));
            let wire = format!("\x1b]11;rgb:0c0c/0c0c/0c0c{ending}");
            for event in windows_decoded(&wire) {
                replies.push(event, start);
                assert!(replies.ready.is_empty(), "reply byte escaped: {wire:?}");
            }
            assert_eq!(
                replies.color,
                Some(RgbColor {
                    r: 12,
                    g: 12,
                    b: 12
                })
            );
            assert!(replies.held.is_empty());
        }
    }

    #[test]
    fn osc11_windows_mismatch_replays_presses_and_releases_in_order() {
        let start = Instant::now();
        for wire in ["\x1b]user", "\x1b]10;rgb:11/22/33\x07", "\x1bx"] {
            let mut replies = BackgroundReplies::default();
            replies.begin_query(start);
            let events = windows_decoded(wire);
            for event in &events {
                replies.push(event.clone(), start);
            }
            replies.expire(start + PREFIX_AMBIGUITY_TIMEOUT);
            assert_eq!(
                replies.ready.into_iter().collect::<Vec<_>>(),
                events,
                "{wire:?}"
            );
        }
    }

    #[test]
    fn osc11_typing_paste_and_shortcuts_are_preserved_exactly() {
        let now = Instant::now();
        let mut replies = BackgroundReplies::default();
        replies.begin_query(now);
        let mut ordinary = decoded("hello 11;rgb:1e1e/1e1e/1e1e 雪", false);
        ordinary.extend([
            Event::Paste("\x1b]11;rgb:ffff/ffff/ffff\x07 pasted\n雪".into()),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            key(KeyCode::Char('s'), KeyModifiers::CONTROL),
            key(KeyCode::Char('g'), KeyModifiers::CONTROL),
            key(KeyCode::Char('x'), KeyModifiers::ALT),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Left, KeyModifiers::NONE),
            Event::Key(KeyEvent {
                code: KeyCode::Char(']'),
                modifiers: KeyModifiers::ALT,
                kind: KeyEventKind::Release,
                state: KeyEventState::NONE,
            }),
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(']'),
                KeyModifiers::ALT,
                KeyEventKind::Repeat,
            )),
        ]);
        for event in &ordinary {
            replies.push(event.clone(), now);
        }
        assert_eq!(replies.ready.into_iter().collect::<Vec<_>>(), ordinary);
    }

    #[test]
    fn osc11_unqueried_and_mismatched_input_is_replayed() {
        let start = Instant::now();
        for (wire, queried) in [
            ("\x1b]11;rgb:11/22/33\x07", false),
            ("\x1b]10;rgb:11/22/33\x07", true),
            ("\x1b]11;not-a-color\x1b\\", true),
            ("\x1b]user", true),
        ] {
            let mut replies = BackgroundReplies::default();
            if queried {
                replies.begin_query(start);
            }
            let now = start + Duration::from_secs(6);
            let events = decoded(wire, false);
            for event in &events {
                replies.push(event.clone(), now);
            }
            assert_eq!(
                replies.ready.into_iter().collect::<Vec<_>>(),
                events,
                "{wire:?}"
            );
        }
    }

    #[test]
    fn osc11_prefix_ambiguity_and_memory_are_bounded_without_discarding_input() {
        let start = Instant::now();
        for wire in [
            "\x1b",
            "\x1b]",
            "\x1b]1",
            "\x1b]11",
            &format!("\x1b]11;{}", "a".repeat(160)),
        ] {
            let mut replies = BackgroundReplies::default();
            replies.begin_query(start);
            let events = decoded(wire, false);
            for event in &events {
                replies.push(event.clone(), start);
                assert!(replies.text.len() <= MAX_REPLY_BYTES);
                assert!(replies.held.len() <= MAX_REPLY_BYTES);
            }
            replies.expire(start + PREFIX_AMBIGUITY_TIMEOUT);
            assert_eq!(replies.ready.into_iter().collect::<Vec<_>>(), events);
            assert!(replies.held.is_empty());
        }
    }

    #[test]
    fn osc11_complete_reply_after_six_seconds_or_an_hour_is_still_consumed() {
        for delay in [Duration::from_secs(6), Duration::from_secs(3600)] {
            for ending in ["\x07", "\x1b\\"] {
                let start = Instant::now();
                let mut replies = BackgroundReplies::default();
                replies.begin_query(start);
                let escape = key(KeyCode::Esc, KeyModifiers::NONE);
                replies.push(escape.clone(), start);
                replies.expire(start + PREFIX_AMBIGUITY_TIMEOUT);
                assert_eq!(
                    replies.ready.pop_front().map(|packet| packet.event),
                    Some(escape.clone())
                );
                let typed = decoded("typed before a very late reply", false);
                for event in &typed {
                    replies.push(event.clone(), start + delay);
                }
                assert!(!replies.begin_query(start + delay), "no overlapping query");
                for event in decoded(&format!("\x1b]11;rgb:1e1e/1e1e/1e1e{ending}"), false) {
                    replies.push(event, start + delay);
                }
                assert_eq!(replies.ready.into_iter().collect::<Vec<_>>(), typed);
                assert!(replies.color.is_some());
                assert!(!replies.pending);
                assert!(replies.held.is_empty());
            }
        }
    }

    #[test]
    fn osc11_identified_reply_has_no_fragment_idle_or_total_expiry() {
        for ending in ["\x07", "\x1b\\"] {
            let start = Instant::now();
            let mut now = start + Duration::from_secs(6);
            let mut replies = BackgroundReplies::default();
            replies.begin_query(start);
            for event in decoded(OSC11_PREFIX, true) {
                replies.push(event, now);
            }
            assert!(replies.deadline().is_none());
            // Hold an identified header across a long pause, then fragment
            // every body/terminator byte more slowly than the old 250 ms bound.
            now += Duration::from_secs(600);
            replies.expire(now);
            for event in decoded(&format!("rgb:1e1e/1e1e/1e1e{ending}"), true) {
                now += Duration::from_millis(700);
                replies.expire(now);
                assert!(replies.ready.is_empty());
                replies.push(event, now);
                assert!(replies.text.len() <= MAX_REPLY_BYTES);
            }
            assert!(replies.color.is_some());
            assert!(replies.ready.is_empty());
            assert!(replies.held.is_empty());
        }
    }

    #[test]
    fn osc11_paste_and_shortcuts_between_identified_fragments_remain_input() {
        let start = Instant::now();
        let mut replies = BackgroundReplies::default();
        replies.begin_query(start);
        for event in decoded("\x1b]11;rgb:", false) {
            replies.push(event, start);
        }
        let input = [
            Event::Paste("pasted 雪\x1b]11;rgb:ffff/ffff/ffff\x07".into()),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
            key(KeyCode::Char('d'), KeyModifiers::CONTROL),
            key(KeyCode::Left, KeyModifiers::NONE),
        ];
        for event in &input {
            replies.push(event.clone(), start + Duration::from_secs(6));
            assert_eq!(
                replies.ready.pop_front().map(|packet| packet.event),
                Some(event.clone())
            );
        }
        for event in decoded("1e1e/1e1e/1e1e\x1b\\", true) {
            replies.push(event, start + Duration::from_secs(7));
        }
        assert!(replies.ready.is_empty());
        assert!(replies.color.is_some());
    }

    #[test]
    fn osc11_ambiguity_deadline_preserves_escape_then_genuine_header_like_typing() {
        // An isolated Esc could equally be a shortcut or the first byte of a
        // very slowly split OSC opener. Preserve the shortcut at the documented
        // boundary; without byte provenance the following text cannot safely be
        // reclassified as a reply. This intentionally records that limitation.
        let start = Instant::now();
        let mut replies = BackgroundReplies::default();
        replies.begin_query(start);
        let escape = key(KeyCode::Esc, KeyModifiers::NONE);
        replies.push(escape.clone(), start);
        replies.expire(start + PREFIX_AMBIGUITY_TIMEOUT);
        assert_eq!(
            replies.ready.pop_front().map(|packet| packet.event),
            Some(escape)
        );
        let typed = decoded("]11;rgb:1e1e/1e1e/1e1e", false);
        for event in &typed {
            replies.push(event.clone(), start + Duration::from_secs(1));
        }
        assert_eq!(replies.ready.into_iter().collect::<Vec<_>>(), typed);
    }

    #[test]
    fn osc11_query_handoff_retains_typing_and_partial_reply_state() {
        let now = Instant::now();
        let mut replies = BackgroundReplies::default();
        replies.begin_query(now);
        let typed = decoded("draft", false);
        for event in &typed {
            replies.push(event.clone(), now);
        }
        for event in decoded("\x1b]11;rgb:", true) {
            replies.push(event, now + Duration::from_millis(100));
        }
        // The old synchronous 120 ms handoff lost both kinds of input. The
        // shared owner keeps them even when the probe returns no color.
        for event in decoded("1e1e/1e1e/1e1e\x1b\\", true) {
            replies.push(event, now + Duration::from_millis(200));
        }
        assert_eq!(replies.ready.into_iter().collect::<Vec<_>>(), typed);
        assert_eq!(
            replies.color,
            Some(RgbColor {
                r: 30,
                g: 30,
                b: 30
            })
        );
    }

    #[tokio::test]
    async fn osc11_stream_cancellation_keeps_fragment_and_filters_before_observation() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut input = TerminalInput::from_stream(
            tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        );
        input.replies.begin_query(Instant::now());
        for event in decoded("\x1b]11;rgb:", true) {
            sender.send(Ok(event)).unwrap();
        }
        // Poll and drop next(), as select! does when another branch wins.
        assert!(futures_util::poll!(input.next()).is_pending());
        for event in decoded("1e1e/1e1e/1e1e\x1b\\", true) {
            sender.send(Ok(event)).unwrap();
        }
        let typed = key(KeyCode::Char('x'), KeyModifiers::NONE);
        sender.send(Ok(typed.clone())).unwrap();
        assert_eq!(input.next().await.unwrap().unwrap(), typed);
        assert!(futures_util::poll!(input.next()).is_pending());
    }

    #[tokio::test]
    async fn osc11_probe_keeps_real_input_and_errors_for_the_next_owner() {
        let events = [
            Event::Paste("typed during the probe 雪".into()),
            key(KeyCode::Char('x'), KeyModifiers::NONE),
        ];
        let source = tokio_stream::iter(events.clone().map(Ok));
        let mut input = TerminalInput::from_stream(source);
        input.replies.begin_query(Instant::now());
        assert!(input.read_background_color(Duration::ZERO).await.is_none());
        for expected in events {
            assert_eq!(input.next().await.unwrap().unwrap(), expected);
        }
        assert!(input.next().await.is_none());
        assert!(input.next().await.is_none());

        let source = tokio_stream::iter([Err(io::Error::other("input failed"))]);
        let mut input = TerminalInput::from_stream(source);
        input.replies.begin_query(Instant::now());
        assert!(input.read_background_color(Duration::ZERO).await.is_none());
        assert_eq!(
            input.next().await.unwrap().unwrap_err().to_string(),
            "input failed"
        );
    }

    #[tokio::test]
    async fn ceded_terminal_input_is_parked_until_the_grant_is_released() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let ceded = Arc::new(AtomicBool::new(false));
        let mut input = TerminalInput::from_stream(
            tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        )
        .with_cede_flag(ceded.clone());
        let typed = key(KeyCode::Char('x'), KeyModifiers::NONE);
        sender.send(Ok(typed.clone())).unwrap();

        ceded.store(true, Ordering::SeqCst);
        // The source is never polled while ceded, so the byte the child will
        // read on /dev/tty is still queued for the host afterwards.
        for _ in 0..3 {
            assert!(futures_util::poll!(input.next()).is_pending());
        }

        ceded.store(false, Ordering::SeqCst);
        assert_eq!(input.next().await.unwrap().unwrap(), typed);
    }

    #[tokio::test]
    async fn osc11_probe_timeout_keeps_a_fragment_for_the_next_owner() {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut input = TerminalInput::from_stream(
            tokio_stream::wrappers::UnboundedReceiverStream::new(receiver),
        );
        input.replies.begin_query(Instant::now());
        for event in decoded("\x1b]11;rgb:", true) {
            sender.send(Ok(event)).unwrap();
        }
        assert!(input.read_background_color(Duration::ZERO).await.is_none());
        assert_eq!(input.replies.text, "\x1b]11;rgb:");
        for event in decoded("1e1e/1e1e/1e1e\x07", true) {
            sender.send(Ok(event)).unwrap();
        }
        sender.send(Ok(Event::Paste("kept".into()))).unwrap();
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            Event::Paste("kept".into())
        );
        assert!(futures_util::poll!(input.next()).is_pending());
    }

    #[test]
    fn osc11_resize_during_a_fragment_and_overlapping_query_do_not_lose_state() {
        let now = Instant::now();
        let mut replies = BackgroundReplies::default();
        assert!(replies.begin_query(now));
        for event in decoded("\x1b]11;rgb:", false) {
            replies.push(event, now);
        }
        replies.push(Event::Resize(80, 24), now);
        assert!(!replies.begin_query(now + Duration::from_millis(120)));
        for event in decoded("1e1e/1e1e/1e1e\x07", false) {
            replies.push(event, now + Duration::from_millis(200));
        }
        assert_eq!(
            replies.ready.into_iter().collect::<Vec<_>>(),
            [Event::Resize(80, 24)]
        );
        assert!(replies.color.is_some());
    }

    #[tokio::test]
    async fn osc11_stream_replays_an_ambiguous_escape_without_more_input() {
        let source = futures_util::stream::iter([Ok(key(KeyCode::Esc, KeyModifiers::NONE))])
            .chain(futures_util::stream::pending());
        let mut input = TerminalInput::from_stream(source);
        input.replies.begin_query(Instant::now());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), input.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            key(KeyCode::Esc, KeyModifiers::NONE),
        );
    }
}
