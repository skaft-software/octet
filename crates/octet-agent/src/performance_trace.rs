//! Opt-in, payload-free wall-clock attribution for the offline session benchmark.
use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

pub(crate) struct Span {
    phase: &'static str,
    bytes: usize,
    start: Instant,
}

impl Span {
    pub(crate) fn start(phase: &'static str, bytes: usize) -> Option<Self> {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        ENABLED
            .get_or_init(|| std::env::var("OCTET_PERF_TRACE").is_ok_and(|value| value == "1"))
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
