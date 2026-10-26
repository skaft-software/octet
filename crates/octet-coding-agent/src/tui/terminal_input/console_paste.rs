//! Multi-line paste recovery for the legacy Windows console input API.
//!
//! Crossterm reads Windows input as console records and never reports a
//! paste: bracketed paste is unimplemented there, and a pasted line break
//! arrives as an ordinary Enter that submits a half-pasted prompt. Consoles
//! queue every record of one paste at once, while typed keys arrive one by one.
//!
//! Only a queued run of text keys with a line break followed by more text is
//! rebuilt, as one paste. Everything else is replayed as the original records:
//! single-line pastes, typing that queued while octet was busy and its Enter,
//! and every panel, filter and shortcut that reads keys keep working exactly as
//! without this layer. A line break that ends a rebuilt paste still arrives as
//! Enter after it.
//!
//! Terminal replies (cursor reports, colour answers) also arrive as one queued
//! run of key records, starting with Esc or Alt+char. The reply filter
//! reassembles them key by key, so such a run is delivered unchanged until the
//! console queue drains.
use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// How long each follow-up read waits. Queued paste records are ready at once;
/// no one types two keys within this window, so typed keys stay separate. It
/// must be positive: a zero-timeout read consumes at most one record, and the
/// modifier records a host synthesizes around pasted capitals carry no event,
/// so a zero wait would end the run at the first capital letter.
pub(super) const FOLLOW_UP: Duration = Duration::from_millis(3);
/// Bound one rebuilt paste; the remainder of a larger paste follows next.
const MAX_PASTE_BYTES: usize = 256 * 1024;
/// How long the first read of each poll waits, as the crossterm reader did.
pub(super) const READ_WAIT: Duration = Duration::from_millis(1);

/// Console input with multi-line pastes rebuilt and everything else intact.
#[derive(Default)]
pub(super) struct ConsoleInput {
    /// Events already read and decided, delivered verbatim in order.
    pending: VecDeque<Event>,
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
        if let Some(event) = self.pending.pop_front() {
            self.escape_run |= is_escape(&event);
            return Ok(Some(event));
        }
        let Some(first) = read(READ_WAIT)? else {
            self.escape_run = false;
            return Ok(None);
        };
        if self.escape_run || is_escape(&first) {
            self.escape_run = true;
            return Ok(Some(first));
        }
        self.pending = coalesce(first, read)?.into();
        Ok(self.pending.pop_front())
    }
}

/// `first` and the records queued behind it, decided as one unit: a rebuilt
/// multi-line paste, or the original records. A non-text key ends the run and
/// comes last, unchanged.
fn coalesce(
    first: Event,
    mut queued: impl FnMut(Duration) -> io::Result<Option<Event>>,
) -> io::Result<Vec<Event>> {
    let mut text = String::new();
    if !push_pasted(&first, &mut text) {
        return Ok(vec![first]);
    }
    let mut records = vec![first];
    let mut ended_by = None;
    while text.len() < MAX_PASTE_BYTES {
        let Some(next) = queued(FOLLOW_UP)? else {
            break;
        };
        if !is_release(&next) && !push_pasted(&next, &mut text) {
            ended_by = Some(next);
            break;
        }
        records.push(next);
    }
    // Pasted CR LF arrives as Enter then Ctrl+Enter; keep one line break.
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let body = text.trim_end_matches('\n');
    let mut out = if body.contains('\n') {
        let mut paste = vec![Event::Paste(body.to_owned())];
        if body.len() < text.len() {
            paste.push(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )));
        }
        paste
    } else {
        records
    };
    out.extend(ended_by);
    Ok(out)
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

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Press,
        ))
    }
    fn release(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new_with_kind(
            code,
            modifiers,
            KeyEventKind::Release,
        ))
    }
    /// Console records for `s`: a press and a release per key, as ConPTY sends.
    fn text(s: &str) -> Vec<Event> {
        s.chars()
            .flat_map(|c| {
                let (code, modifiers) = match c {
                    '\r' => (KeyCode::Enter, KeyModifiers::NONE),
                    '\n' => (KeyCode::Enter, KeyModifiers::CONTROL),
                    '\t' => (KeyCode::Tab, KeyModifiers::NONE),
                    c if c.is_uppercase() => (KeyCode::Char(c), KeyModifiers::SHIFT),
                    c => (KeyCode::Char(c), KeyModifiers::NONE),
                };
                [key(code, modifiers), release(code, modifiers)]
            })
            .collect()
    }
    fn enter() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    }

    /// Drive `ConsoleInput` over `batches`: each batch is queued at once, and
    /// an empty read separates batches the way a drained console does. Also
    /// returns every wait the reader asked for.
    fn drain(batches: Vec<Vec<Event>>) -> (Vec<Event>, Vec<Duration>) {
        let mut queue: VecDeque<Option<Event>> = VecDeque::new();
        for batch in batches {
            queue.extend(batch.into_iter().map(Some));
            queue.push_back(None);
        }
        let mut input = ConsoleInput::default();
        let mut out = Vec::new();
        let mut waits = Vec::new();
        while !queue.is_empty() || !input.pending.is_empty() {
            let event = input
                .next(|wait| {
                    waits.push(wait);
                    Ok(queue.pop_front().flatten())
                })
                .unwrap();
            out.extend(event);
        }
        (out, waits)
    }
    fn events(batches: Vec<Vec<Event>>) -> Vec<Event> {
        drain(batches).0
    }

    #[test]
    fn a_queued_multiline_paste_becomes_one_paste_instead_of_submitting() {
        assert_eq!(
            events(vec![text("first line\r\nSecond\tline\r\n雪 é")]),
            [Event::Paste("first line\nSecond\tline\n雪 é".into())]
        );
        // A trailing line break still submits after the pasted text.
        assert_eq!(
            events(vec![text("one\r\ntwo\r\n\r\n")]),
            [Event::Paste("one\ntwo".into()), enter()]
        );
    }

    #[test]
    fn queued_single_line_input_is_replayed_as_the_original_keys() {
        // Typing that queued while octet was busy, then Enter: every record,
        // releases included, arrives unchanged, so pickers and filters that
        // read keys see exactly what was typed and Enter still submits.
        for typed in ["Continue without\r", "ab", "\r\r", "a single pasted line"] {
            assert_eq!(events(vec![text(typed)]), text(typed), "{typed:?}");
        }
    }

    #[test]
    fn non_text_keys_are_never_absorbed_and_keep_their_order() {
        let ctrl_c = key(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let x = key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(events(vec![vec![ctrl_c.clone(), x.clone()]]), [ctrl_c, x]);

        let up = key(KeyCode::Up, KeyModifiers::NONE);
        let mut batch = text("a\r\nb");
        batch.push(up.clone());
        batch.extend(text("z"));
        let mut expected = vec![Event::Paste("a\nb".into()), up];
        expected.extend(text("z"));
        assert_eq!(events(vec![batch]), expected);
    }

    #[test]
    fn altgr_characters_are_text_but_alt_chords_are_not() {
        let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
        let at = key(KeyCode::Char('@'), altgr);
        let mut batch = text("a\r\n");
        batch.push(at);
        assert_eq!(events(vec![batch]), [Event::Paste("a\n@".into())]);
    }

    #[test]
    fn a_terminal_reply_run_reaches_the_reply_filter_key_by_key() {
        // A cursor-position reply as console key records, with key-ups.
        let mut reply = vec![key(KeyCode::Esc, KeyModifiers::NONE)];
        reply.extend(text("[1;1R"));
        let mut expected = reply.clone();
        expected.push(Event::Paste("pasted\nlines".into()));
        assert_eq!(
            events(vec![reply, text("pasted\r\nlines")]),
            expected,
            "the reply is unchanged and a later paste is still rebuilt"
        );
    }

    #[test]
    fn an_alt_led_reply_run_is_also_left_unchanged() {
        // ConPTY folds `ESC ]` into Alt+] before the OSC 11 colour reply.
        let mut reply = vec![key(KeyCode::Char(']'), KeyModifiers::ALT)];
        reply.extend(text("11;rgb:0c0c/0c0c/0c0c\r\nx"));
        assert_eq!(events(vec![reply.clone()]), reply);
    }

    #[test]
    fn an_escape_that_ends_a_paste_starts_an_unchanged_run() {
        let escape = key(KeyCode::Esc, KeyModifiers::NONE);
        let mut batch = text("a\r\nb");
        batch.push(escape.clone());
        batch.extend(text("]11;rgb\r\nz"));
        let mut expected = vec![Event::Paste("a\nb".into()), escape];
        expected.extend(text("]11;rgb\r\nz"));
        assert_eq!(events(vec![batch]), expected);
    }

    #[test]
    fn every_follow_up_is_short_and_pastes_stay_bounded() {
        let (_, waits) = drain(vec![text("ab")]);
        assert_eq!(waits[0], READ_WAIT);
        assert!(waits[1..]
            .iter()
            .all(|wait| *wait == FOLLOW_UP || *wait == READ_WAIT));

        let mut big = "x\n".repeat(MAX_PASTE_BYTES / 2 + 10);
        big.push('y');
        let records: Vec<Event> = big
            .chars()
            .map(|c| match c {
                '\n' => key(KeyCode::Enter, KeyModifiers::CONTROL),
                c => key(KeyCode::Char(c), KeyModifiers::NONE),
            })
            .collect();
        let out = events(vec![records]);
        let Event::Paste(first) = &out[0] else {
            panic!("expected a paste")
        };
        assert!(first.len() < MAX_PASTE_BYTES);
        assert!(matches!(out.last(), Some(Event::Paste(rest)) if rest.ends_with('y')));
    }
}
