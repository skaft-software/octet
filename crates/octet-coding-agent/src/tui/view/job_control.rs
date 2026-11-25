//! Job control belongs to the host that owns the physical terminal.
use std::io;
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
static REQUEST: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
pub(super) fn request() {
    REQUEST.store(true, Ordering::Release);
}

pub(super) fn requested() -> bool {
    #[cfg(unix)]
    {
        REQUEST.swap(false, Ordering::AcqRel)
    }
    #[cfg(not(unix))]
    false
}

// Drop order matters: restore the physical terminal before removing its listener.
pub(super) struct OwnedTerminal {
    pub(super) backend: super::OctetTerminal,
    _listener: Listener,
}

impl OwnedTerminal {
    pub(super) fn enter(
        size: super::TerminalSize,
        capture_mouse: bool,
        images: super::TerminalImageStore,
    ) -> anyhow::Result<Self> {
        let listener = Listener::install()?;
        let backend =
            super::OctetTerminal::enter_with_mouse_and_images(size, capture_mouse, images)?;
        Ok(Self {
            backend,
            _listener: listener,
        })
    }
}

struct Listener {
    #[cfg(unix)]
    previous: libc::sigaction,
}

impl Listener {
    // Pi packages/tui/src/tui.ts:1052-1057,1134-1140: subscribe before entering
    // raw mode, and keep the listener until the renderer has restored the tty.
    fn install() -> io::Result<Self> {
        #[cfg(unix)]
        unsafe {
            extern "C" fn handle(_: libc::c_int) {
                request();
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_RESTART;
            action.sa_sigaction = handle as *const () as usize;
            let mut previous = std::mem::MaybeUninit::uninit();
            REQUEST.store(false, Ordering::Release);
            if libc::sigaction(libc::SIGTSTP, &action, previous.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self {
                previous: previous.assume_init(),
            })
        }
        #[cfg(not(unix))]
        Ok(Self {})
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::sigaction(libc::SIGTSTP, &self.previous, std::ptr::null_mut());
        }
    }
}

pub(super) fn stop() {
    #[cfg(unix)]
    unsafe {
        // Pi packages/coding-agent/src/modes/interactive/interactive-mode.ts:4661-4664
        // stops the job after terminal handback. Never group-stop an invoking
        // shell without job control. SIGSTOP also works for an orphaned PTY job.
        let pid = libc::getpid();
        libc::kill(if libc::getpgrp() == pid { 0 } else { pid }, libc::SIGSTOP);
    }
}
