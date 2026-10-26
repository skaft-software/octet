//! Paste recovery for the legacy Windows console input API.
//!
//! Crossterm reads Windows input as console records and never reports a
//! paste: bracketed paste is unimplemented there, and a pasted line break
//! arrives as an ordinary Enter that submits a half-pasted prompt. Consoles
//! queue every record of one paste at once, while typed keys arrive one by one,
//! so text keys that are already waiting behind a text key are one paste.
//! Line breaks inside such a burst are pasted text; a break that ends it is
//! delivered as Enter, so input typed ahead while octet was busy still submits.
//!
//! Terminal replies (cursor reports, colour answers) also arrive as one queued
//! run of key records, starting with Esc. The reply filter reassembles them key
//! by key, so a run that starts with Esc is delivered unchanged until the
//! console queue drains.
use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// How long each follow-up read waits. Queued paste records are ready at once;
/// no one types two keys within this window, so typed keys stay keys. It must
/// be positive: a zero-timeout read consumes at most one record, and the
/// modifier records a host synthesizes around pasted capitals carry no event,
/// so a zero wait would end the burst at the first capital letter.
pub(super) const FOLLOW_UP: Duration = Duration::from_millis(3);
/// Bound one coalesced event; the remainder of a larger paste follows as the
/// next paste.
const MAX_PASTE_BYTES: usize = 256 * 1024;
/// How long the first read of each poll waits, as the crossterm reader did.
pub(super) const READ_WAIT: Duration = Duration::from_millis(1);

/// Console input with pastes rebuilt and terminal replies left intact.
#[derive(Default)]
pub(super) struct ConsoleInput {
    /// A non-text key that ended a coalesced paste, delivered next.
    held: Option<Event>,
    /// Inside a queued run that began with Esc; cleared once the queue drains.
    escape_run: bool,
}

impl ConsoleInput {
    /// The next event, or `None` when nothing is queued. `read` waits at most
    /// the given duration for one event.
    pub(super) fn next(
        &mut self,
        mut read: impl FnMut(Duration) -> io::Result<Option<Event>>,
    ) -> io::Result<Option<Event>> {
        let first = match self.held.take() {
            Some(event) => event,
            None => match read(READ_WAIT)? {
                Some(event) => event,
                None => {
                    self.escape_run = false;
                    return Ok(None);
                }
            },
        };
        if self.escape_run || is_escape(&first) {
            self.escape_run = true;
            return Ok(Some(first));
        }
        coalesce(first, &mut self.held, read).map(Some)
    }
}

/// Esc, or Alt+char: a console host delivers `ESC x` from a reply as Alt+x.
/// AltGr (reported as Ctrl+Alt) produces ordinary text and does not count.
fn is_escape(event: &Event) -> bool {
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press | KeyEventKind::Repeat,
        ..
    }) = event
    else {
        return false;
    };
    match code {
        KeyCode::Esc => true,
        KeyCode::Char(_) => {
            modifiers.contains(KeyModifiers::ALT) && !modifiers.contains(KeyModifiers::CONTROL)
        }
        _ => false,
    }
}

/// Coalesce `first` with the text keys already queued behind it.
///
/// `queued` reads the next event, waiting at most the given duration. A
/// non-text event ends the burst and is returned in `held` for the caller to
/// deliver next. A lone key is returned unchanged.
pub(super) fn coalesce(
    first: Event,
    held: &mut Option<Event>,
    mut queued: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> io::Result<Event> {
    let mut paste = String::new();
    if !push_pasted(&first, &mut paste) {
        return Ok(first);
    }
    let mut keys = 1usize;
    while paste.len() < MAX_PASTE_BYTES {
        let Some(next) = queued(FOLLOW_UP)? else {
            break;
        };
        if is_release(&next) {
            continue;
        }
        if !push_pasted(&next, &mut paste) {
            *held = Some(next);
            break;
        }
        keys += 1;
    }
    if keys == 1 {
        return Ok(first);
    }
    // Pasted CR LF arrives as Enter then Ctrl+Enter; keep one line break.
    let mut paste = paste.replace("\r\n", "\n").replace('\r', "\n");
    if held.is_none() && paste.ends_with('\n') {
        paste.truncate(paste.trim_end_matches('\n').len());
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        if paste.is_empty() {
            return Ok(enter);
        }
        *held = Some(enter);
    }
    Ok(Event::Paste(paste))
}

fn is_release(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            kind: KeyEventKind::Release,
            ..
        })
    )
}

/// Append the text a pasted key record carries; false if it carries none.
fn push_pasted(event: &Event, paste: &mut String) -> bool {
    let Event::Key(KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press | KeyEventKind::Repeat,
        ..
    }) = event
    else {
        return false;
    };
    let text = match code {
        // Conhost synthesizes LF as Ctrl+Enter and CR as Enter.
        KeyCode::Enter if modifiers.contains(KeyModifiers::CONTROL) => '\n',
        KeyCode::Enter if modifiers.difference(KeyModifiers::SHIFT).is_empty() => '\r',
        KeyCode::Tab if modifiers.is_empty() => '\t',
        KeyCode::Char(c) if !c.is_control() && is_text_modifiers(*modifiers) => *c,
        _ => return false,
    };
    paste.push(text);
    true
}

/// Shift alone, or AltGr (reported as Ctrl+Alt) on layouts that need it.
fn is_text_modifiers(modifiers: KeyModifiers) -> bool {
    let chord = modifiers.difference(KeyModifiers::SHIFT);
    chord.is_empty() || chord == KeyModifiers::CONTROL | KeyModifiers::ALT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }
    fn release(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))
    }
    fn text(s: &str) -> Vec<Event> {
        s.chars()
            .flat_map(|c| match c {
                '\r' => vec![key(KeyCode::Enter, KeyModifiers::NONE)],
                '\n' => vec![key(KeyCode::Enter, KeyModifiers::CONTROL)],
                '\t' => vec![key(KeyCode::Tab, KeyModifiers::NONE)],
                c if c.is_uppercase() => vec![key(KeyCode::Char(c), KeyModifiers::SHIFT)],
                c => vec![
                    key(KeyCode::Char(c), KeyModifiers::NONE),
                    release(KeyCode::Char(c)),
                ],
            })
            .collect()
    }
    /// Feed `events` as already queued; record each wait the coalescer asked for.
    fn run(events: Vec<Event>) -> (Event, Option<Event>, Vec<Duration>, usize) {
        let mut queue: VecDeque<Event> = events.into();
        let first = queue.pop_front().unwrap();
        let mut held = None;
        let mut waits = Vec::new();
        let event = coalesce(first, &mut held, |wait| {
            waits.push(wait);
            Ok(queue.pop_front())
        })
        .unwrap();
        (event, held, waits, queue.len())
    }

    #[test]
    fn a_queued_multiline_paste_becomes_one_paste_instead_of_submitting() {
        let (event, held, _, left) = run(text("first line\r\nSecond\tline\r\n雪 é"));
        assert_eq!(event, Event::Paste("first line\nSecond\tline\n雪 é".into()));
        assert_eq!((held, left), (None, 0));
    }

    #[test]
    fn a_line_break_that_ends_the_burst_still_submits() {
        // Typed ahead while octet was busy: the text is inserted and the
        // Enter that followed it still submits.
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let (event, held, ..) = run(text("Continue without\r"));
        assert_eq!(event, Event::Paste("Continue without".into()));
        assert_eq!(held, Some(enter.clone()));

        let (event, held, ..) = run(text("one\r\ntwo\r\n\r\n"));
        assert_eq!(event, Event::Paste("one\ntwo".into()));
        assert_eq!(held, Some(enter.clone()));

        let (event, held, ..) = run(text("\r\r"));
        assert_eq!((event, held), (enter, None));
    }

    #[test]
    fn a_lone_keystroke_is_delivered_unchanged_after_one_short_wait() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let (event, held, waits, _) = run(vec![enter.clone()]);
        assert_eq!(event, enter);
        assert_eq!(held, None);
        assert_eq!(waits, [FOLLOW_UP]);

        let a = key(KeyCode::Char('a'), KeyModifiers::NONE);
        let (event, ..) = run(vec![a.clone(), release(KeyCode::Char('a'))]);
        assert_eq!(event, a, "a key's own release is not a second key");
    }

    #[test]
    fn non_text_keys_are_never_absorbed_and_end_the_burst_in_order() {
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let (event, ..) = run(vec![
            ctrl_c.clone(),
            key(KeyCode::Char('x'), KeyModifiers::NONE),
        ]);
        assert_eq!(event, ctrl_c);

        let mut events = text("ab");
        let up = key(KeyCode::Up, KeyModifiers::NONE);
        events.push(up.clone());
        events.push(key(KeyCode::Char('z'), KeyModifiers::NONE));
        let (event, held, _, left) = run(events);
        assert_eq!(event, Event::Paste("ab".into()));
        assert_eq!(held, Some(up));
        assert_eq!(left, 1, "events after the held key stay queued");
    }

    #[test]
    fn altgr_characters_join_but_ctrl_and_alt_chords_do_not() {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let (event, ..) = run(vec![
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            key(KeyCode::Char('@'), altgr),
        ]);
        assert_eq!(event, Event::Paste("a@".into()));

        let alt_b = key(KeyCode::Char('b'), KeyModifiers::ALT);
        let (_, held, ..) = run(vec![
            key(KeyCode::Char('a'), KeyModifiers::NONE),
            alt_b.clone(),
        ]);
        assert_eq!(held, Some(alt_b));
    }

    /// Drive `ConsoleInput` over `batches`: each batch is queued at once, and
    /// an empty read separates batches the way a drained console does.
    fn drain(batches: Vec<Vec<Event>>) -> Vec<Event> {
        let mut queue: VecDeque<Option<Event>> = VecDeque::new();
        for batch in batches {
            queue.extend(batch.into_iter().map(Some));
            queue.push_back(None);
        }
        let mut input = ConsoleInput::default();
        let mut out = Vec::new();
        while !queue.is_empty() {
            if let Some(event) = input.next(|_| Ok(queue.pop_front().flatten())).unwrap() {
                out.push(event);
            }
        }
        out
    }

    #[test]
    fn a_terminal_reply_run_reaches_the_reply_filter_key_by_key() {
        // A cursor-position reply as console key records, with key-ups.
        let mut reply = vec![key(KeyCode::Esc, KeyModifiers::NONE)];
        reply.extend(text("[1;1R"));
        let events = drain(vec![reply.clone(), text("pasted\r\nlines")]);
        assert_eq!(events[..reply.len()], reply[..], "the reply is unchanged");
        assert_eq!(
            events[reply.len()..],
            [Event::Paste("pasted\nlines".into())],
            "once the queue drains, a later paste is rebuilt again"
        );
    }

    #[test]
    fn an_alt_led_reply_run_is_also_left_unchanged() {
        // ConPTY folds `ESC ]` into Alt+] before the OSC 11 colour reply.
        let mut reply = vec![key(KeyCode::Char(']'), KeyModifiers::ALT)];
        reply.extend(text("11;rgb:0c0c/0c0c/0c0c"));
        reply.push(key(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert_eq!(drain(vec![reply.clone()]), reply);

        // AltGr text is not a reply and still pastes.
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let events = drain(vec![vec![
            key(KeyCode::Char('@'), altgr),
            key(KeyCode::Char('x'), KeyModifiers::NONE),
        ]]);
        assert_eq!(events, [Event::Paste("@x".into())]);
    }

    #[test]
    fn an_escape_that_ends_a_paste_starts_an_unchanged_run() {
        let mut batch = text("ab");
        batch.push(key(KeyCode::Esc, KeyModifiers::NONE));
        batch.extend(text("]11;rgb"));
        let events = drain(vec![batch]);
        assert_eq!(events[0], Event::Paste("ab".into()));
        assert_eq!(events[1], key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(
            events[2..]
                .iter()
                .all(|event| matches!(event, Event::Key(_))),
            "{events:?}"
        );
    }

    #[test]
    fn every_follow_up_is_short_and_bursts_stay_bounded() {
        // A paste is queued at once, so no follow-up waits longer than a gap
        // between keys a person or a typing script could produce.
        let (_, _, waits, _) = run(text("abc"));
        assert!(waits.iter().all(|wait| *wait == FOLLOW_UP));

        let big = "x".repeat(MAX_PASTE_BYTES + 10);
        let events: Vec<Event> = big
            .chars()
            .map(|c| key(KeyCode::Char(c), KeyModifiers::NONE))
            .collect();
        let (event, _, _, left) = run(events);
        let Event::Paste(first) = event else {
            panic!("expected a paste")
        };
        assert_eq!(first.len(), MAX_PASTE_BYTES);
        assert_eq!(left, 10, "the rest follows as the next paste");
    }
}
