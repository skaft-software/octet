//! Paste recovery for the legacy Windows console input API.
//!
//! Crossterm reads Windows input as console records and never reports a
//! paste: bracketed paste is unimplemented there, and a pasted line break
//! arrives as an ordinary Enter that submits a half-pasted prompt. Consoles
//! queue every record of one paste at once, while typed keys arrive one by one,
//! so text keys that are already waiting behind a text key are one paste.
use std::io;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// How long the first follow-up read waits. Queued paste records are ready at
/// once; no one types two keys within this window, so a keystroke stays a key.
/// It must be positive: a zero-timeout read consumes at most one record, and
/// the modifier records a host synthesizes around pasted capitals carry no
/// event, so a zero wait would end the burst at the first capital letter.
pub(super) const FIRST_FOLLOW_UP: Duration = Duration::from_millis(3);
/// Once a burst is established, tolerate the gaps a console host leaves while
/// converting a large paste in chunks, so one paste is not split at an Enter.
pub(super) const BURST_FOLLOW_UP: Duration = Duration::from_millis(20);
/// Bound one coalesced event; the remainder of a larger paste follows as the
/// next paste.
const MAX_PASTE_BYTES: usize = 256 * 1024;

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
        let wait = if keys == 1 {
            FIRST_FOLLOW_UP
        } else {
            BURST_FOLLOW_UP
        };
        let Some(next) = queued(wait)? else { break };
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
    Ok(Event::Paste(
        paste.replace("\r\n", "\n").replace('\r', "\n"),
    ))
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
    fn a_lone_keystroke_is_delivered_unchanged_after_one_short_wait() {
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let (event, held, waits, _) = run(vec![enter.clone()]);
        assert_eq!(event, enter);
        assert_eq!(held, None);
        assert_eq!(waits, [FIRST_FOLLOW_UP]);

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

    #[test]
    fn bursts_wait_longer_than_the_first_follow_up_and_stay_bounded() {
        // `a` and its release are read in the short window; once `b` joins,
        // the burst tolerates longer gaps.
        let (_, _, waits, _) = run(text("abc"));
        assert_eq!(waits[..2], [FIRST_FOLLOW_UP, FIRST_FOLLOW_UP]);
        assert!(waits[2..].iter().all(|wait| *wait == BURST_FOLLOW_UP));

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
