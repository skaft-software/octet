//! Opt-in, off-screen startup attribution across the extension process boundary.
//!
//! The coding frontend owns the user-visible `OCTET_STARTUP_TRACE` contract:
//! when `OCTET_STARTUP_TRACE` is set to anything other than empty/`0`, startup
//! writes one `octet-startup: <phase> elapsed=<micros>us` line per phase
//! boundary to stderr and renders nothing. Extension processes are separate
//! operating-system processes, so their handshake can only be attributed from
//! inside that process (Node startup and Pi factory imports) and from the host
//! side (spawn and initialize). This module provides the host half of that
//! contract:
//!
//! * the child environment receives `OCTET_STARTUP_TRACE` only while tracing is
//!   on, so an adapter can emit its own phases;
//! * a child stderr line that already carries the `octet-startup:` prefix is
//!   forwarded to the host's stderr instead of being folded into a diagnostic;
//! * host-side steps (spawn, initialize) emit the same line shape.
//!
//! Trace lines go through a sink installed by the process that owns the
//! terminal ([`set_startup_trace_sink`]). The coding frontend routes them
//! through its control-safe stderr boundary, so an enabled trace can never
//! corrupt a rendered frame. Without an installed sink the lines are dropped:
//! a library user who does not own a frontend never receives stray stderr.
//!
//! Semantics of `elapsed=`: the frontend's own phase lines report micros since
//! the Octet process entered its runtime. The per-process lines emitted here
//! are *durations* of the named step (spawn, initialize) because they are
//! written by independent processes with independent clocks; they are not
//! offsets into the Octet process. Adapter-side lines report micros since the
//! Node process started, which is the only shared origin available in the
//! child.

use std::sync::OnceLock;
use std::time::Instant;

/// The one startup trace environment variable, shared with the frontend.
pub(crate) const STARTUP_TRACE_ENV: &str = "OCTET_STARTUP_TRACE";

type Sink = Box<dyn Fn(&str) + Send + Sync>;

static TRACE_SINK: OnceLock<Sink> = OnceLock::new();

/// Installs the process-wide trace sink.
///
/// The first installation wins; a frontend installs it once at startup. A
/// process that never installs one (a library embedding, a unit test) keeps the
/// trace off even if the environment variable is set.
pub fn set_startup_trace_sink(sink: fn(&str)) {
    let _ = TRACE_SINK.set(Box::new(sink));
}

/// Whether the startup trace is enabled for the current process environment.
///
/// Off unless the switch is explicitly set to something other than empty/`0`,
/// matching the frontend so a child is traced exactly when its host is.
pub(crate) fn enabled() -> bool {
    TRACE_SINK.get().is_some()
        && std::env::var_os(STARTUP_TRACE_ENV)
            .is_some_and(|value| !value.is_empty() && value != "0")
}

fn emit(line: &str) {
    if let Some(sink) = TRACE_SINK.get() {
        sink(line);
    }
}

/// The environment entry handed to an extension child while tracing is on.
pub(crate) fn child_environment() -> Option<(&'static str, &'static str)> {
    enabled().then_some((STARTUP_TRACE_ENV, "1"))
}

/// Whether one child stderr line is an `octet-startup:` trace line.
fn is_trace_line(line: &str) -> bool {
    line.starts_with("octet-startup: ")
}

/// One process step attributed to the extension that requested it.
pub(crate) fn process_phase(step: &str, extension: &str, started: Instant) {
    if !enabled() {
        return;
    }
    emit(&format!(
        "octet-startup: extensions.process.{step}:{extension} elapsed={}us",
        started.elapsed().as_micros()
    ));
}

/// Forwards one child stderr line that already carries the trace prefix.
///
/// Returns `true` when the line was consumed by the trace channel. Control
/// bytes are dropped: a child must not be able to inject terminal control
/// sequences through the trace channel. The line is written verbatim otherwise,
/// so every trace consumer parses one shape regardless of which process
/// produced it.
pub(crate) fn forward_child_line(line: &str) -> bool {
    if !is_trace_line(line) {
        return false;
    }
    if !enabled() {
        return false;
    }
    let sanitized: String = line
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    emit(&sanitized);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_lines_are_recognized_by_prefix_only() {
        assert!(is_trace_line(
            "octet-startup: adapter.load.ready elapsed=1us"
        ));
        assert!(!is_trace_line("octet-startup:adapter.load.ready"));
        assert!(!is_trace_line("[pi-compat] skipped factory"));
    }

    #[test]
    fn tracing_needs_an_installed_sink() {
        // No sink is installed in this unit-test process, so the switch alone
        // must never change library behavior.
        assert!(!enabled());
        assert!(child_environment().is_none());
    }
}
