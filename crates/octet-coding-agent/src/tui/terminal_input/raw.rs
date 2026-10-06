//! The sole Unix byte reader. No background read survives a cancelled poll or cede.
use super::*;
use std::io::Read;
use std::os::fd::AsRawFd;

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
        let tty = self.tty.as_mut().unwrap();
        let mut fd = libc::pollfd {
            fd: tty.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid owned fd; a zero-timeout readiness check only. We
        // never change stdin flags and there is no competing crossterm reader.
        let result = unsafe { libc::poll(&mut fd, 1, 0) };
        if result < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(error)
            };
        }
        if fd.revents & libc::POLLIN == 0 {
            return Ok(None);
        }
        let mut bytes = [0; 256];
        let count = match tty.read(&mut bytes) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(None)
            }
            result => result?,
        };
        for (index, byte) in bytes[..count].iter().enumerate() {
            // As in crossterm, malformed byte sequences are discarded, not
            // converted to printable escape text. Valid events retain spelling.
            match self.decoder.push(*byte, index + 1 < count) {
                Ok(Some(packet)) => self.ready.push_back(packet),
                Err(error) if error.kind() == io::ErrorKind::InvalidData => return Err(error),
                _ => {}
            }
        }
        Ok(self.ready.pop_front())
    }
}
