//! Opt-in, payload-free wall-clock attribution for the offline session benchmark.
use std::io::{IsTerminal, Write};
use std::sync::OnceLock;
use std::time::Instant;

/// One timed phase. Inert unless `OCTET_PERF_TRACE=1` and stderr is not a
/// terminal: trace lines would otherwise be painted into the TUI, so a traced
/// run must redirect stderr (the benchmark does).
pub struct Span {
    phase: &'static str,
    bytes: usize,
    start: Instant,
}

impl Span {
    pub fn start(phase: &'static str, bytes: usize) -> Option<Self> {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        ENABLED
            .get_or_init(|| {
                std::env::var("OCTET_PERF_TRACE").is_ok_and(|value| value == "1")
                    && !std::io::stderr().is_terminal()
            })
            .then(|| Self {
                phase,
                bytes,
                start: Instant::now(),
            })
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        // Diagnostics never change the result of persistence or inference.
        let _ = writeln!(
            std::io::stderr().lock(),
            "octet-perf: {} elapsed={}us bytes={}",
            self.phase,
            self.start.elapsed().as_micros(),
            self.bytes
        );
    }
}
