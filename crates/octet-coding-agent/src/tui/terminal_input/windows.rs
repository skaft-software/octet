//! The Windows console reader: VT input decoded by the Unix parser.
//!
//! With `ENABLE_VIRTUAL_TERMINAL_INPUT`, each key record carries one UTF-16
//! unit of the byte stream the terminal sent, so bracketed pastes, SGR mouse
//! reports and key sequences arrive exactly as a Unix tty delivers them. They
//! are reassembled here and decoded by the same [`codec::Decoder`], which keeps
//! a paste one event (newlines do not submit, a dropped path can attach) and
//! keeps each event's raw spelling for extension interception. A console host
//! that refused VT input keeps crossterm's record decoder.
#[cfg(windows)]
use super::super::windows_console;
use super::*;

/// One console input record, reduced to what the VT decoder needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConsoleInput {
    /// One UTF-16 code unit of the terminal's input stream (a key-down record;
    /// repeats are expanded). Key-up records carry no new input.
    Unit(u16),
    /// A carriage return whose record still names Shift or Ctrl. VT input
    /// spells Shift+Enter and Ctrl+Enter as a bare CR, which would submit
    /// instead of inserting a newline.
    ModifiedEnter { shift: bool, control: bool },
    /// The console window changed size.
    Resize,
    /// The console gained or lost focus.
    Focus(bool),
}

/// How long a trailing ESC waits for the rest of a sequence (as on Unix).
#[cfg(windows)]
const ESCAPE_LOOKAHEAD: Duration = Duration::from_millis(1);

#[derive(Default)]
pub(super) struct ConsoleReader {
    decoder: codec::Decoder,
    ready: VecDeque<Packet>,
    records: Vec<ConsoleInput>,
    /// A high surrogate whose low half is in a later record.
    high_surrogate: Option<u16>,
}

impl ConsoleReader {
    #[cfg(windows)]
    pub(super) fn read(&mut self) -> io::Result<Option<Packet>> {
        if let Some(packet) = self.ready.pop_front() {
            return Ok(Some(packet));
        }
        if !windows_console::vt_input_active() {
            return if crossterm::event::poll(Duration::from_millis(1))? {
                crossterm::event::read().map(|event| Some(event.into()))
            } else {
                Ok(None)
            };
        }
        self.records.clear();
        if !windows_console::read_input(&mut self.records)? {
            if !self.decoder.has_pending_escape() {
                return Ok(None);
            }
            // A lone ESC may begin a sequence whose rest is a record away.
            std::thread::sleep(ESCAPE_LOOKAHEAD);
            if !windows_console::read_input(&mut self.records)? {
                if let Some(packet) = self.decoder.finish_pending_escape() {
                    self.ready.push_back(packet);
                }
                return Ok(self.ready.pop_front());
            }
        }
        let records = std::mem::take(&mut self.records);
        let result = self.decode(&records);
        self.records = records;
        result?;
        Ok(self.ready.pop_front())
    }

    fn decode(&mut self, records: &[ConsoleInput]) -> io::Result<()> {
        let mut bytes = Vec::new();
        for record in records {
            match *record {
                ConsoleInput::Unit(unit) => self.push_unit(unit, &mut bytes),
                ConsoleInput::ModifiedEnter { shift, control } => {
                    self.decode_bytes(&std::mem::take(&mut bytes))?;
                    if self.decoder.is_idle() {
                        let mut modifiers = KeyModifiers::NONE;
                        modifiers.set(KeyModifiers::SHIFT, shift);
                        modifiers.set(KeyModifiers::CONTROL, control);
                        self.ready.push_back(Packet {
                            event: Event::Key(crossterm::event::KeyEvent::new(
                                KeyCode::Enter,
                                modifiers,
                            )),
                            raw: Some("\r".into()),
                        });
                    } else {
                        // Inside a paste or escape sequence a CR is data.
                        self.decode_bytes(b"\r")?;
                    }
                }
                ConsoleInput::Resize => {
                    self.decode_bytes(&std::mem::take(&mut bytes))?;
                    let (columns, rows) = crossterm::terminal::size()?;
                    self.ready.push_back(Event::Resize(columns, rows).into());
                }
                ConsoleInput::Focus(gained) => {
                    self.decode_bytes(&std::mem::take(&mut bytes))?;
                    self.ready.push_back(
                        if gained {
                            Event::FocusGained
                        } else {
                            Event::FocusLost
                        }
                        .into(),
                    );
                }
            }
        }
        self.decode_bytes(&bytes)
    }

    fn push_unit(&mut self, unit: u16, bytes: &mut Vec<u8>) {
        let character = match (self.high_surrogate.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high_surrogate = Some(unit);
                return;
            }
            (Some(high), 0xDC00..=0xDFFF) => char::decode_utf16([high, unit])
                .next()
                .and_then(Result::ok)
                .unwrap_or(char::REPLACEMENT_CHARACTER),
            // An unpaired half becomes U+FFFD; the current unit still counts.
            (Some(_), _) => {
                bytes.extend_from_slice("\u{FFFD}".as_bytes());
                return self.push_unit(unit, bytes);
            }
            (None, unit) => char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
        };
        let mut encoded = [0; 4];
        bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
    }

    fn decode_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        for (index, byte) in bytes.iter().enumerate() {
            // A trailing ESC may begin a sequence in the next records; keep it
            // ambiguous for the bounded lookahead, exactly as the Unix reader.
            let more = index + 1 < bytes.len() || *byte == b'\x1b';
            match self.decoder.push(*byte, more) {
                Ok(Some(packet)) => self.ready.push_back(packet),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(units: &str) -> Vec<Event> {
        let mut reader = ConsoleReader::default();
        let records = units
            .encode_utf16()
            .map(ConsoleInput::Unit)
            .collect::<Vec<_>>();
        reader.decode(&records).unwrap();
        reader
            .ready
            .into_iter()
            .map(|packet| packet.event)
            .collect()
    }

    #[test]
    fn a_bracketed_paste_is_one_event_with_its_newlines() {
        assert_eq!(
            decode("\x1b[200~C:\\Users\\me\\Quarterly Report.pdf\r\nline two\x1b[201~"),
            vec![Event::Paste(
                "C:\\Users\\me\\Quarterly Report.pdf\r\nline two".into()
            )]
        );
    }

    #[test]
    fn modified_enter_inserts_a_newline_but_stays_data_inside_a_paste() {
        let mut reader = ConsoleReader::default();
        let mut records = vec![ConsoleInput::ModifiedEnter {
            shift: true,
            control: false,
        }];
        records.extend("\x1b[200~a".encode_utf16().map(ConsoleInput::Unit));
        records.push(ConsoleInput::ModifiedEnter {
            shift: true,
            control: false,
        });
        records.extend("b\x1b[201~".encode_utf16().map(ConsoleInput::Unit));
        reader.decode(&records).unwrap();
        let events = reader
            .ready
            .into_iter()
            .map(|packet| packet.event)
            .collect::<Vec<_>>();
        assert!(
            matches!(&events[0], Event::Key(key)
                if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::SHIFT),
            "{events:?}"
        );
        assert_eq!(events[1], Event::Paste("a\rb".into()));
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn keys_and_surrogate_pairs_decode_like_a_unix_tty() {
        let events = decode("a\u{1F600}\r\x1b[A");
        assert_eq!(events.len(), 4, "{events:?}");
        assert!(matches!(&events[1], Event::Key(key) if key.code == KeyCode::Char('\u{1F600}')));
        assert!(matches!(&events[2], Event::Key(key) if key.code == KeyCode::Enter));
        assert!(matches!(&events[3], Event::Key(key) if key.code == KeyCode::Up));
    }
}
