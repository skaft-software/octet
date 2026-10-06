//! Byte provenance for Unix input and a console-event encoding on Windows.
use super::*;
use crossterm::event::KeyboardEnhancementFlags;

#[path = "parse.rs"]
mod parse;

#[derive(Debug, PartialEq, Eq)]
#[allow(dead_code)] // protocol replies are parsed, but never dispatched as user keys
pub(super) enum InternalEvent {
    Event(Event),
    CursorPosition(u16, u16),
    KeyboardEnhancementFlags(KeyboardEnhancementFlags),
    PrimaryDeviceAttributes,
}

/// A decoded event retaining its original UTF-8 terminal spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    pub(super) event: Event,
    /// Exact bytes the terminal produced, or `None` for events octet derived
    /// from another source (protocol traffic, a console API without bytes, a
    /// synthesized resize). Only the real spelling may reach a Pi listener: a
    /// derived event would report bytes the terminal never sent.
    pub(super) raw: Option<String>,
}

impl Packet {
    /// An event octet synthesized, which has no terminal byte spelling.
    pub(super) fn synthetic(event: Event) -> Self {
        Self { event, raw: None }
    }
}

impl From<Event> for Packet {
    /// Console key records and synthesized events have no byte stream, so they
    /// carry the legacy spelling a terminal reply can be made of. This is the
    /// one key→bytes table; Unix input never comes through here, because the
    /// decoder in this module already produced the terminal's real bytes.
    fn from(event: Event) -> Self {
        let raw = legacy_spelling(&event);
        Self { event, raw }
    }
}

/// Legacy key spelling for a record without byte provenance.
///
/// Do not turn paste text, enhanced key releases/repeats, or arbitrary shortcuts
/// into protocol bytes. These are exactly the legacy key forms the reply filters
/// must recognize (plus literal control characters on other platforms); a key
/// with no legacy spelling has no spelling at all rather than an invented one.
fn legacy_spelling(event: &Event) -> Option<String> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    match (key.code, key.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => Some("\x1b".into()),
        (KeyCode::Char(']'), KeyModifiers::ALT) => Some("\x1b]".into()),
        // APC opener (`ESC _`), the prefix of a Tern Surface Protocol message.
        (KeyCode::Char('_'), KeyModifiers::ALT) => Some("\x1b_".into()),
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

#[cfg(test)]
impl PartialEq<Event> for Packet {
    fn eq(&self, other: &Event) -> bool {
        self.event == *other
    }
}

#[derive(Default)]
pub(super) struct Decoder {
    buffer: Vec<u8>,
}

impl Decoder {
    pub(super) fn push(&mut self, byte: u8, more: bool) -> io::Result<Option<Packet>> {
        self.buffer.push(byte);
        // The wire interception bound is much smaller. This is the existing
        // native protocol-frame ceiling; oversized incomplete sequences fail.
        if self.buffer.len() > ApcFilter::MAX_BYTES {
            self.buffer.clear();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "terminal sequence exceeded native frame bound",
            ));
        }
        match parse::parse_event(&self.buffer, more) {
            Ok(None) => Ok(None),
            Ok(Some(event)) => {
                let raw = String::from_utf8(std::mem::take(&mut self.buffer)).ok();
                Ok(match event {
                    InternalEvent::Event(event) => Some(Packet { event, raw }),
                    _ => None,
                })
            }
            Err(error) => {
                self.buffer.clear();
                Err(error)
            }
        }
    }
}

/// Decode an entire replacement before admitting any part of it. Incomplete or
/// malformed escapes and terminal *replies* are not valid input replacements:
/// the host, not a listener, decides what protocol traffic means.
pub(super) fn replacement(data: &str) -> io::Result<Vec<Event>> {
    let mut buffer = Vec::new();
    let mut events = Vec::new();
    for (index, byte) in data.bytes().enumerate() {
        buffer.push(byte);
        if let Some(event) = parse::parse_event(&buffer, index + 1 < data.len())? {
            match event {
                InternalEvent::Event(event) => events.push(event),
                _ => {
                    return Err(io::Error::other(
                        "terminal protocol reply is not replacement input",
                    ))
                }
            }
            buffer.clear();
        }
    }
    if !buffer.is_empty() {
        return Err(io::Error::other("incomplete terminal replacement"));
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn events(data: &str) -> Vec<Event> {
        replacement(data).expect("admitted replacement")
    }

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn admitted_replacements_decode_to_events_in_order() {
        assert_eq!(events("a"), [key(KeyCode::Char('a'), KeyModifiers::NONE)]);
        assert_eq!(events("\x1bOA"), [key(KeyCode::Up, KeyModifiers::NONE)]);
        assert_eq!(events("\x1b"), [key(KeyCode::Esc, KeyModifiers::NONE)]);
        assert_eq!(
            events("\x1b[200~雪\nq\x1b[201~"),
            [Event::Paste("雪\nq".into())]
        );
        assert!(events("").is_empty(), "an empty replacement consumes input");
    }

    // Terminal *replies* are protocol traffic, not keystrokes: a listener must
    // not be able to inject one as user input. OSC replies are deliberately
    // absent here because the terminal byte stream is inherently ambiguous for
    // them (the native filter treats an unmatched one as typing too).
    #[test]
    fn protocol_replies_and_partial_escapes_are_never_admitted_as_input() {
        for data in [
            "\x1b[3;7R",
            "\x1b[?1;2c",
            "\x1b[",
            "\x1b[0;",
            "\x1b[200~unterminated",
        ] {
            assert!(replacement(data).is_err(), "admitted {data:?}");
        }
    }

    #[test]
    fn console_records_carry_only_spellings_a_terminal_actually_sends() {
        let key = |code, modifiers| Event::Key(KeyEvent::new(code, modifiers));
        // Records without a byte stream get the legacy forms a reply can be made
        // of, and nothing is invented for keys the console protocol has no byte
        // spelling for: the Unix decoder is the only bytes↔events mapping.
        for (event, expected) in [
            (key(KeyCode::Char('a'), KeyModifiers::NONE), Some("a")),
            (key(KeyCode::Char('A'), KeyModifiers::SHIFT), Some("A")),
            (key(KeyCode::Esc, KeyModifiers::NONE), Some("\x1b")),
            (key(KeyCode::Char(']'), KeyModifiers::ALT), Some("\x1b]")),
            (key(KeyCode::Char('g'), KeyModifiers::CONTROL), Some("\x07")),
            (key(KeyCode::Enter, KeyModifiers::NONE), None),
            (key(KeyCode::Up, KeyModifiers::NONE), None),
            (key(KeyCode::Char('c'), KeyModifiers::CONTROL), None),
            (key(KeyCode::Backspace, KeyModifiers::NONE), None),
            (Event::Paste("text".into()), None),
        ] {
            let packet: Packet = event.clone().into();
            assert_eq!(packet.raw.as_deref(), expected, "{event:?}");
        }
        let release = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert_eq!(Packet::from(release).raw, None);
    }

    #[test]
    fn decoder_keeps_the_exact_terminal_spelling_of_each_event() {
        let mut decoder = Decoder::default();
        let mut packets = Vec::new();
        let bytes = "a\x1b[1;5C雪".as_bytes();
        for (index, byte) in bytes.iter().enumerate() {
            if let Some(packet) = decoder.push(*byte, index + 1 < bytes.len()).unwrap() {
                packets.push(packet);
            }
        }
        let raw: Vec<_> = packets
            .iter()
            .map(|packet| packet.raw.clone().unwrap())
            .collect();
        assert_eq!(raw, ["a", "\x1b[1;5C", "雪"]);
        assert_eq!(
            packets
                .iter()
                .map(|packet| packet.event.clone())
                .collect::<Vec<_>>(),
            [
                key(KeyCode::Char('a'), KeyModifiers::NONE),
                key(KeyCode::Right, KeyModifiers::CONTROL),
                key(KeyCode::Char('雪'), KeyModifiers::NONE),
            ]
        );
        assert_eq!(
            Packet::synthetic(key(KeyCode::Char('x'), KeyModifiers::NONE)).raw,
            None
        );
    }
}
