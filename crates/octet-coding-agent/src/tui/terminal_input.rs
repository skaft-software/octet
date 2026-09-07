//! One input owner for the background probe, editor, panels and lifecycle waits.
//!
//! Crossterm 0.28 decodes OSC as ordinary keys on Unix: Alt+], literal text,
//! then Ctrl+G (BEL) or Alt+backslash (ST). A read boundary can also split either
//! escape into Esc plus a character. Recognize only a complete OSC 11 color
//! reply to our own outstanding query; never filter Paste or discard a timeout's
//! worth of input. Query/identified-reply state has memory bounds, not an expiry:
//! a terminal's response can arrive arbitrarily later than the startup wait.
//!
//! Before the full OSC 11 header is recognized, legacy Esc/Alt+] is inherently
//! ambiguous with genuine input. Only that unconfirmed prefix has a 250 ms idle
//! deadline, after which its original events are replayed. An opener split more
//! slowly than that can therefore reach input. Once ESC ] 11 ; is recognized,
//! no timer replays its body or a fragmented ST. Original events remain bounded
//! and are replayed only on a mismatch, overflow, input error or EOF.

use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use futures_util::{Stream, StreamExt};
use sexy_tui_rs::terminal_colors::{parse_osc11_background_color, RgbColor};
use tokio::time::Sleep;

// A valid reply remains recognizable until the outstanding query is answered.
// Only an ambiguous, unconfirmed header is subject to an input-latency bound.
const PREFIX_AMBIGUITY_TIMEOUT: Duration = Duration::from_millis(250);
const MAX_REPLY_BYTES: usize = 128;
const OSC11_PREFIX: &str = "\x1b]11;";

#[derive(Default)]
struct BackgroundReplies {
    pending: bool,
    prefix_updated: Option<Instant>,
    text: String,
    held: Vec<Event>,
    ready: VecDeque<Event>,
    color: Option<RgbColor>,
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
        if self.text.starts_with(OSC11_PREFIX) {
            None
        } else {
            self.prefix_updated
                .map(|updated| updated + PREFIX_AMBIGUITY_TIMEOUT)
        }
    }

    fn replay(&mut self) {
        self.ready.extend(self.held.drain(..));
        self.clear_fragment();
    }

    fn clear_fragment(&mut self) {
        self.text.clear();
        self.held.clear();
        self.prefix_updated = None;
    }

    fn expire(&mut self, now: Instant) {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.replay();
        }
    }

    fn push(&mut self, event: Event, now: Instant) {
        self.expire(now);
        if !self.pending {
            self.ready.push_back(event);
            return;
        }
        // Resize/focus/mouse notifications can arrive between reply fragments.
        // They are not part of the byte protocol and must remain responsive.
        if matches!(
            event,
            Event::Resize(..) | Event::FocusGained | Event::FocusLost | Event::Mouse(_)
        ) {
            self.ready.push_back(event);
            return;
        }
        let Some(fragment) = reply_fragment(&event) else {
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
        candidate.push_str(&fragment);
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
            }
            Candidate::NotReply => {
                self.replay();
                // A mismatch may itself begin a new reply (e.g. Esc, Alt+]).
                if matches!(candidate_status(&fragment), Candidate::Prefix) {
                    self.text = fragment;
                    self.held.push(event);
                    self.prefix_updated = Some(now);
                } else {
                    self.ready.push_back(event);
                }
            }
        }
    }
}

// Do not turn paste text, enhanced key releases/repeats, or arbitrary shortcuts
// into protocol bytes. These are exactly the legacy key forms emitted by
// crossterm's Unix parser (plus literal control characters on other platforms).
fn reply_fragment(event: &Event) -> Option<String> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => Some("\x1b".into()),
        (KeyCode::Char(']'), KeyModifiers::ALT) => Some("\x1b]".into()),
        (KeyCode::Char('\\'), KeyModifiers::ALT) => Some("\x1b\\".into()),
        (KeyCode::Char('g'), KeyModifiers::CONTROL) => Some("\x07".into()),
        (KeyCode::Char(character), modifiers)
            if modifiers.is_empty() || modifiers == KeyModifiers::SHIFT =>
        {
            Some(character.to_string())
        }
        _ => None,
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

/// Filtered, cancellation-safe interactive input. Pass this same owner to every
/// panel and lifecycle loop; raw crossterm events must never reach extensions.
pub struct TerminalInput<S = crossterm::event::EventStream> {
    source: S,
    replies: BackgroundReplies,
    timer: Option<Pin<Box<Sleep>>>,
    timer_deadline: Option<Instant>,
    ended: bool,
    input_error: Option<io::Error>,
}

impl TerminalInput {
    pub fn new() -> Self {
        Self::from_stream(crossterm::event::EventStream::new())
    }
}

impl Default for TerminalInput {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> TerminalInput<S> {
    pub fn from_stream(source: S) -> Self {
        Self {
            source,
            replies: BackgroundReplies::default(),
            timer: None,
            timer_deadline: None,
            ended: false,
            input_error: None,
        }
    }
}

impl<S: Stream<Item = io::Result<Event>> + Unpin> TerminalInput<S> {
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
            self.replies.push(event, Instant::now());
            if let Some(color) = self.replies.color.take() {
                return Some(color);
            }
        }
    }
}

impl<S: Stream<Item = io::Result<Event>> + Unpin> Stream for TerminalInput<S> {
    type Item = io::Result<Event>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        // The state and timer belong to the stream, not next()'s future. A
        // select! cancellation or an input-owner change cannot lose a prefix.
        for _ in 0..256 {
            this.replies.expire(Instant::now());
            if let Some(event) = this.replies.ready.pop_front() {
                return Poll::Ready(Some(Ok(event)));
            }
            if let Some(error) = this.input_error.take() {
                return Poll::Ready(Some(Err(error)));
            }
            if this.ended {
                return Poll::Ready(None);
            }
            match Pin::new(&mut this.source).poll_next(cx) {
                Poll::Ready(Some(Ok(event))) => this.replies.push(event, Instant::now()),
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

    // Model the real crossterm parser at arbitrary read boundaries. The PTY
    // lane below separately verifies these assumptions against actual bytes.
    fn decoded(bytes: &str, split_escapes: bool) -> Vec<Event> {
        let mut events = Vec::new();
        let mut chars = bytes.chars().peekable();
        while let Some(character) = chars.next() {
            events.push(match character {
                '\x1b' if !split_escapes && matches!(chars.peek(), Some(']' | '\\')) => {
                    key(KeyCode::Char(chars.next().unwrap()), KeyModifiers::ALT)
                }
                '\x1b' => key(KeyCode::Esc, KeyModifiers::NONE),
                '\x07' => key(KeyCode::Char('g'), KeyModifiers::CONTROL),
                character => key(
                    KeyCode::Char(character),
                    if character.is_uppercase() {
                        KeyModifiers::SHIFT
                    } else {
                        KeyModifiers::NONE
                    },
                ),
            });
        }
        events
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
                assert_eq!(replies.ready.pop_front(), Some(escape.clone()));
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
            assert_eq!(replies.ready.pop_front(), Some(event.clone()));
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
        assert_eq!(replies.ready.pop_front(), Some(escape));
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
