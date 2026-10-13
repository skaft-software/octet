//! Minimal unix tty control: raw mode, poll, blocking read.
//!
//! Kept separate so the rest of the crate stays free of `unsafe`.

#![allow(unsafe_code)]

use std::io;

/// Puts a tty into raw mode and restores the previous settings on drop.
#[cfg(unix)]
pub struct RawGuard {
    fd: i32,
    saved: libc::termios,
}

#[cfg(unix)]
impl RawGuard {
    /// Enter raw mode on `fd` (normally stdin). No-op when `fd` is not a tty.
    pub fn enable(fd: i32) -> io::Result<Option<RawGuard>> {
        // SAFETY: `termios` is a plain-old-data struct zero-initialized here and
        // filled by `tcgetattr` before any use.
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: `fd` is a valid descriptor for the duration of the call.
        if unsafe { libc::tcgetattr(fd, &mut saved) } != 0 {
            return Ok(None);
        }
        let mut raw = saved;
        // SAFETY: `raw` is an initialized `termios`.
        unsafe { libc::cfmakeraw(&mut raw) };
        // SAFETY: same as above; failure leaves the terminal untouched.
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            return Ok(None);
        }
        Ok(Some(RawGuard { fd, saved }))
    }
}

#[cfg(unix)]
impl Drop for RawGuard {
    fn drop(&mut self) {
        // SAFETY: restoring the settings captured by `enable`.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
        }
    }
}

/// Wait until `fd` is readable or `timeout_ms` elapses. Returns whether it is readable.
#[cfg(unix)]
pub fn poll_readable(fd: i32, timeout_ms: i32) -> bool {
    // SAFETY: a zeroed `pollfd` with a valid descriptor and the `POLLIN` flag.
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is one valid entry.
    let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    rc > 0 && (pfd.revents & libc::POLLIN) != 0
}

/// Read up to `buf.len()` bytes, returning the count (0 on EOF).
#[cfg(unix)]
pub fn read(fd: i32, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `buf` is a valid writable region of `buf.len()` bytes.
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

/// A guard that is absent on non-unix targets.
#[cfg(not(unix))]
pub struct RawGuard;

#[cfg(not(unix))]
impl RawGuard {
    /// No-op on non-unix targets.
    pub fn enable(_fd: i32) -> io::Result<Option<RawGuard>> {
        Ok(None)
    }
}

/// Non-unix targets never report a readable wait.
#[cfg(not(unix))]
pub fn poll_readable(_fd: i32, _timeout_ms: i32) -> bool {
    false
}

/// Non-unix targets fail reads.
#[cfg(not(unix))]
pub fn read(_fd: i32, _buf: &mut [u8]) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "tty reads need a unix target",
    ))
}

/// Extract complete APC sequences (`ESC _ … ESC \` or BEL-terminated) from
/// `buffer`, leaving any trailing partial sequence in place.
pub fn extract_apc(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i + 1 < buffer.len() {
        if buffer[i] == 0x1b && buffer[i + 1] == 0x5f {
            // Find the terminator.
            let mut j = i + 2;
            let mut end = None;
            while j < buffer.len() {
                if buffer[j] == 0x07 {
                    end = Some((j, 1));
                    break;
                }
                if buffer[j] == 0x1b && j + 1 < buffer.len() && buffer[j + 1] == 0x5c {
                    end = Some((j, 2));
                    break;
                }
                j += 1;
            }
            match end {
                Some((stop, width)) => {
                    if let Ok(text) = std::str::from_utf8(&buffer[i..stop + width]) {
                        out.push(text.to_owned());
                    }
                    i = stop + width;
                    start = i;
                }
                None => {
                    // Incomplete: drop only what was already consumed, keeping
                    // the partial sequence (and anything after it) buffered.
                    if start > 0 {
                        buffer.drain(..start);
                    }
                    return out;
                }
            }
        } else {
            i += 1;
        }
    }
    buffer.drain(..start.min(buffer.len()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_apc_and_keeps_partial_tail() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(b"abc\x1b_tsp;e;{\"ev\":\"theme\",\"dark\":true}\x1b\\");
        buffer.extend_from_slice(b"tail\x1b_tsp;e;{\"ev\":\"mo");
        let found = extract_apc(&mut buffer);
        assert_eq!(
            found,
            vec!["\x1b_tsp;e;{\"ev\":\"theme\",\"dark\":true}\x1b\\".to_owned()]
        );
        assert_eq!(buffer, b"tail\x1b_tsp;e;{\"ev\":\"mo".to_vec());
    }
}
