//! Compatibility facade for octet's split terminal frontend.
//!
//! Public construction and capability APIs remain here for existing call
//! sites, while backend rendering, capability policy, lifecycle restoration,
//! signal forwarding, and filtered input ownership live in focused modules.
#![allow(missing_docs)]

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::terminal;

mod backend;
mod capabilities;
#[path = "terminal_input.rs"]
mod input;
mod lifecycle;
mod signal;

pub use backend::OctetTerminal;
pub(crate) use backend::TerminalImageStore;
pub use capabilities::{ColorDepth, ColorMode, TerminalCapabilities};
pub use input::TerminalInput;
pub use lifecycle::{force_restore, install_panic_hook};
pub use signal::{
    exit_if_signaled, install_signal_restore, received_shutdown_signal,
    request_coordinated_shutdown, wait_for_shutdown_signal,
};

/// Shared dimensions reachable by both the boxed terminal and the shell.
pub type TerminalSize = Arc<Mutex<(u16, u16)>>;

/// Query the terminal's default background via OSC 11 while raw mode is active.
/// The same filtered input owner serves the probe and every interactive loop,
/// preserving real input and incomplete replies across the startup timeout.
/// Environment/config wins; timeout still falls back to Unknown.
pub(crate) async fn query_terminal_background_color<S>(
    input: &mut TerminalInput<S>,
    timeout: Duration,
) -> Option<(u8, u8, u8)>
where
    S: futures_util::Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
{
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return None;
    }
    if !terminal::is_raw_mode_enabled().ok()? {
        return None;
    }
    let color = input.query_background_color(timeout).await.ok()??;
    Some((color.r, color.g, color.b))
}
