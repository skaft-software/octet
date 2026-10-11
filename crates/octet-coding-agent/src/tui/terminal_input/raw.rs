//! The sole Unix byte reader. No background read survives a cancelled poll or cede.
use super::*;
use std::io::Read;
use std::os::fd::AsRawFd;

const ESCAPE_LOOKAHEAD: std::time::Duration = std::time::Duration::from_millis(1);

#[derive(Default)]
pub(super) struct RawReader {
    tty: Option<std::fs::File>,
    decoder: codec::Decoder,
    ready: VecDeque<Packet>,
    size: Option<(u16, u16)>,
}

impl RawReader {
    #[cfg(test)]
    pub(super) fn from_file(file: std::fs::File) -> Self {
        Self {
            tty: Some(file),
            ..Default::default()
        }
    }

    fn poll(&self, timeout_ms: i32) -> io::Result<(i32, libc::pollfd)> {
        let mut fd = libc::pollfd {
            fd: self.tty.as_ref().unwrap().as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid owned fd; this readiness check never reads or
        // changes stdin flags. The short timeout is used only for a lone ESC.
        let result = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if result < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok((result, fd))
        }
    }

    fn decode_chunk(&mut self, bytes: &[u8]) -> io::Result<()> {
        for (index, byte) in bytes.iter().enumerate() {
            // A trailing ESC may be the first byte of a report arriving in the
            // next read chunk. Keep it ambiguous for the bounded lookahead.
            let more = index + 1 < bytes.len() || *byte == b'\x1b';
            // As in crossterm, malformed byte sequences are discarded, not
            // converted to printable escape text. Valid events retain spelling.
            match self.decoder.push(*byte, more) {
                Ok(Some(packet)) => self.ready.push_back(packet),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(error),
                _ => {}
            }
        }
        Ok(())
    }

    fn read_chunk(&mut self) -> io::Result<()> {
        let mut bytes = [0; 256];
        let count = match self.tty.as_mut().unwrap().read(&mut bytes) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(())
            }
            result => result?,
        };
        if count == 0 {
            return Ok(());
        }
        self.decode_chunk(&bytes[..count])
    }

    fn finish_pending_escape(&mut self) {
        if let Some(packet) = self.decoder.finish_pending_escape() {
            self.ready.push_back(packet);
        }
    }

    fn resolve_pending_escape(&mut self) -> io::Result<()> {
        let deadline = std::time::Instant::now() + ESCAPE_LOOKAHEAD;
        while self.decoder.has_pending_escape() {
            let timeout_ms = if std::time::Instant::now() < deadline {
                1
            } else {
                0
            };
            let (result, fd) = match self.poll(timeout_ms) {
                Ok(readiness) => readiness,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                    if std::time::Instant::now() >= deadline {
                        self.finish_pending_escape();
                        return Ok(());
                    }
                    continue;
                }
                Err(error) => return Err(error),
            };
            if result == 0 || fd.revents & libc::POLLIN == 0 {
                self.finish_pending_escape();
                return Ok(());
            }
            self.read_chunk()?;
        }
        Ok(())
    }

    pub(super) fn read(&mut self) -> io::Result<Option<Packet>> {
        if let Some(packet) = self.ready.pop_front() {
            return Ok(Some(packet));
        }
        if let Ok(size) = crossterm::terminal::size() {
            if self
                .size
                .replace(size)
                .is_some_and(|previous| previous != size)
            {
                return Ok(Some(Event::Resize(size.0, size.1).into()));
            }
        }
        if self.tty.is_none() {
            self.tty = Some(std::fs::File::from(
                ForegroundEvents::open_tty_notifier()
                    .ok_or_else(|| io::Error::other("terminal input unavailable"))?,
            ));
        }
        let (result, fd) = match self.poll(0) {
            Ok(readiness) => readiness,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => return Ok(None),
            Err(error) => return Err(error),
        };
        if result != 0 && fd.revents & libc::POLLIN != 0 {
            self.read_chunk()?;
        }
        if self.decoder.has_pending_escape() {
            self.resolve_pending_escape()?;
        }
        Ok(self.ready.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseEvent, MouseEventKind};
    use std::io::Write;
    use std::os::{fd::OwnedFd, unix::net::UnixStream};

    fn reader_pair() -> (RawReader, UnixStream) {
        let (reader, writer) = UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        (
            RawReader::from_file(std::fs::File::from(OwnedFd::from(reader))),
            writer,
        )
    }

    #[test]
    fn split_sgr_mouse_after_lone_escape_chunk_stays_in_the_decoder() {
        let (mut reader, mut writer) = reader_pair();
        reader.decode_chunk(b"\x1b").unwrap();
        assert!(reader.ready.is_empty());
        assert!(reader.decoder.has_pending_escape());

        writer.write_all(b"[<64;31;11M").unwrap();
        let packet = reader.read().unwrap().unwrap();
        assert_eq!(
            packet.event,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 30,
                row: 10,
                modifiers: KeyModifiers::NONE,
            })
        );
        assert_eq!(packet.raw.as_deref(), Some("\x1b[<64;31;11M"));
    }

    #[test]
    fn standalone_escape_is_emitted_after_short_continuation_lookahead() {
        let (mut reader, mut writer) = reader_pair();
        writer.write_all(b"\x1b").unwrap();

        let packet = reader.read().unwrap().unwrap();
        assert_eq!(packet.event, Event::Key(KeyCode::Esc.into()));
        assert_eq!(packet.raw.as_deref(), Some("\x1b"));
    }
}
