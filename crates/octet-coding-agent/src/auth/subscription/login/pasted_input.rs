//! Cancellation-aware terminal/redirect input. No stdin worker outlives its
//! future: pipe/terminal reads follow a nonblocking readiness probe, then yield
//! while no input is available. Regular files and the null device need no probe.
//! The login driver has exclusive input ownership while the renderer is suspended.

use std::io;
use std::time::Duration;

pub(super) async fn read_line(limit: usize) -> io::Result<String> {
    #[cfg(unix)]
    {
        read_unix_line(libc::STDIN_FILENO, limit).await
    }
    #[cfg(windows)]
    {
        read_windows_line(limit).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = limit;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "pasted login input is unavailable on this platform",
        ))
    }
}

#[cfg(unix)]
async fn read_unix_line(fd: libc::c_int, limit: usize) -> io::Result<String> {
    let mut bytes = Vec::new();
    // Darwin's poll reports POLLNVAL for regular files and /dev/null. These
    // descriptors cannot wait for a producer, unlike a pipe or terminal.
    let immediately_readable = unix_immediately_readable(fd)?;
    while bytes.len() < limit {
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let polled = if immediately_readable {
            1
        } else {
            // SAFETY: ready is one initialized pollfd, and the probe never waits.
            unsafe { libc::poll(&mut ready, 1, 0) }
        };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        } else if polled > 0 {
            if ready.revents & libc::POLLNVAL != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "stdin is unavailable",
                ));
            }
            let mut byte = 0_u8;
            // SAFETY: this sole reader consumes one byte only after readiness
            // (or EOF/HUP), or from a regular file/null device, without changing
            // shared stdin descriptor flags.
            let read = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
            if read == 0 {
                break;
            }
            if read < 0 {
                let error = io::Error::last_os_error();
                if !matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) {
                    return Err(error);
                }
            } else {
                bytes.push(byte);
                if byte == b'\n' {
                    break;
                }
                continue;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pasted redirect is not UTF-8"))
}

#[cfg(unix)]
fn unix_immediately_readable(fd: libc::c_int) -> io::Result<bool> {
    // SAFETY: fstat initializes the stat buffer for a live descriptor.
    let mut metadata: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut metadata) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if metadata.st_mode & libc::S_IFMT == libc::S_IFREG {
        return Ok(true);
    }
    if metadata.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return Ok(false);
    }
    // Only the null character device bypasses readiness; other devices may
    // block and must never be treated as regular input.
    let mut null: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: the C string and stat buffer are valid for the duration of stat.
    Ok(unsafe { libc::stat(c"/dev/null".as_ptr(), &mut null) } == 0
        && null.st_mode & libc::S_IFMT == libc::S_IFCHR
        && metadata.st_rdev == null.st_rdev)
}

#[cfg(windows)]
async fn read_windows_line(limit: usize) -> io::Result<String> {
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    use windows_sys::Win32::Foundation::{ERROR_BROKEN_PIPE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, ReadFile, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetNumberOfConsoleInputEvents, GetStdHandle, ReadConsoleInputW,
        INPUT_RECORD, KEY_EVENT, STD_INPUT_HANDLE,
    };
    // PeekNamedPipe is the only pipe primitive needed here. Declare the native
    // ABI locally rather than expanding the product dependency's feature set.
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn PeekNamedPipe(
            handle: *mut std::ffi::c_void,
            buffer: *mut std::ffi::c_void,
            size: u32,
            read: *mut u32,
            available: *mut u32,
            left: *mut u32,
        ) -> i32;
    }
    let handle = {
        // SAFETY: GetStdHandle returns the process's standard input handle.
        let raw = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if raw.is_null() || raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::other("stdin is unavailable"));
        }
        // SAFETY: login exclusively owns input while the renderer is suspended;
        // the process keeps this handle live for the read. Borrowing never closes
        // stdin on completion or cancellation. BorrowedHandle is Send, so only
        // this typed borrow (not a raw pointer) is retained across the await.
        unsafe { BorrowedHandle::borrow_raw(raw) }
    };
    let mut mode = 0;
    // SAFETY: mode is writable, and the borrowed handle remains live.
    let console = unsafe { GetConsoleMode(handle.as_raw_handle(), &mut mode) } != 0;
    // SAFETY: GetFileType accepts the borrowed standard handle.
    let pipe = unsafe { GetFileType(handle.as_raw_handle()) } == FILE_TYPE_PIPE;
    let mut bytes = Vec::new();
    let mut wide = Vec::new();
    while bytes.len() < limit && wide.len() < limit {
        if console {
            let mut available = 0;
            // SAFETY: available is writable; this query never waits for input.
            if unsafe { GetNumberOfConsoleInputEvents(handle.as_raw_handle(), &mut available) } == 0
            {
                return Err(io::Error::last_os_error());
            }
            if available > 0 {
                let mut record = INPUT_RECORD::default();
                let mut read = 0;
                // SAFETY: a pending record is available and login exclusively
                // owns console input. Reading a record doesn't wait for Enter,
                // unlike ReadFile in console line-input mode.
                if unsafe { ReadConsoleInputW(handle.as_raw_handle(), &mut record, 1, &mut read) }
                    == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if read > 0 && record.EventType == KEY_EVENT as u16 {
                    // SAFETY: EventType identifies the initialized union field.
                    let key = unsafe { record.Event.KeyEvent };
                    if key.bKeyDown != 0 {
                        // SAFETY: UnicodeChar is initialized by ReadConsoleInputW.
                        let character = unsafe { key.uChar.UnicodeChar };
                        match character {
                            0 => {}
                            3 => {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "login cancelled",
                                ))
                            }
                            8 => {
                                wide.pop();
                            }
                            13 => {
                                wide.push(10);
                                break;
                            }
                            _ => wide.push(character),
                        }
                    }
                }
                continue;
            }
        } else {
            let mut available = 1;
            if pipe {
                // SAFETY: this is a size-only, nonblocking query on the borrowed
                // pipe; null output buffers request no bytes to be copied.
                if unsafe {
                    PeekNamedPipe(
                        handle.as_raw_handle(),
                        std::ptr::null_mut(),
                        0,
                        std::ptr::null_mut(),
                        &mut available,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                        break;
                    }
                    return Err(error);
                }
            }
            if available > 0 {
                let mut byte = 0_u8;
                let mut read = 0;
                // SAFETY: one writable byte; pipe readiness was just checked.
                // Disk input is a bounded synchronous one-byte read, not a
                // terminal/pipe wait or a detached background stdin worker.
                if unsafe {
                    ReadFile(
                        handle.as_raw_handle(),
                        (&mut byte as *mut u8).cast(),
                        1,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if read == 0 {
                    break;
                }
                bytes.push(byte);
                if byte == b'\n' {
                    break;
                }
                continue;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    if console {
        if wide.len() >= limit && wide.last() != Some(&10) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pasted redirect is too long",
            ));
        }
        let text = String::from_utf16(&wide).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "pasted redirect is not Unicode")
        })?;
        if text.len() > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pasted redirect is too long",
            ));
        }
        Ok(text)
    } else {
        String::from_utf8(bytes)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pasted redirect is not UTF-8"))
    }
}

#[cfg(test)]
mod send_tests {
    #[test]
    fn pasted_input_future_is_send() {
        fn assert_send(_: impl std::future::Future + Send) {}
        // Construct but never poll: this checks the native platform future
        // without reading stdin or needing a console, runtime, or credentials.
        assert_send(super::read_line(1024));
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};

    #[tokio::test]
    async fn redirected_regular_file_and_null_device_need_no_poll_support() {
        use std::io::{Seek, SeekFrom};
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"https://localhost/callback?code=pasted\nnext line")
            .unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(
            read_unix_line(file.as_raw_fd(), 1024).await.unwrap(),
            "https://localhost/callback?code=pasted\n"
        );
        let null = std::fs::File::open("/dev/null").unwrap();
        assert_eq!(read_unix_line(null.as_raw_fd(), 1024).await.unwrap(), "");
    }

    #[tokio::test]
    async fn dropping_pending_pipe_input_leaves_no_reader_and_runtime_can_stop() {
        let mut fds = [0; 2];
        // SAFETY: fds holds two writable descriptors; each is immediately owned.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: each valid pipe descriptor is transferred exactly once.
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        // SAFETY: the other pipe descriptor is transferred exactly once.
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        assert!(tokio::time::timeout(
            Duration::from_millis(40),
            read_unix_line(reader.as_raw_fd(), 1024)
        )
        .await
        .is_err());
        writer.write_all(b"next login\n").unwrap();
        assert_eq!(
            read_unix_line(reader.as_raw_fd(), 1024).await.unwrap(),
            "next login\n"
        );
    }

    #[tokio::test]
    async fn real_pty_pending_paste_is_cancellable_without_a_detached_reader() {
        let mut master = 0;
        let mut slave = 0;
        // SAFETY: openpty initializes both descriptor outputs; default terminal
        // settings and window size use null optional inputs.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0
        );
        // SAFETY: each returned descriptor is transferred exactly once.
        let mut master = unsafe { std::fs::File::from_raw_fd(master) };
        // SAFETY: each returned descriptor is transferred exactly once.
        let slave = unsafe { std::fs::File::from_raw_fd(slave) };
        assert!(tokio::time::timeout(
            Duration::from_millis(40),
            read_unix_line(slave.as_raw_fd(), 1024)
        )
        .await
        .is_err());
        master.write_all(b"redirect\n").unwrap();
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                read_unix_line(slave.as_raw_fd(), 1024)
            )
            .await
            .unwrap()
            .unwrap(),
            "redirect\n"
        );
    }
}
