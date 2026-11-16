//! Opt-in, thread-local frame work accounting for offline renderer qualification.
//! No samples or payloads are retained; callers reset at their frame boundary.
use std::cell::Cell;
use std::time::Instant;

/// Counted work, not total memory or physical display activity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    /// Transcript rows regenerated.
    pub laid_out: u64,
    /// Copy rows aligned with source text.
    pub mapped: u64,
    /// Input lines submitted to the ANSI wrapper (including chrome).
    pub wrapped: u64,
    /// Syntax highlighter invocations, excluding cache hits.
    pub highlights: u64,
    /// Visible block slots visited by geometry capture.
    pub geometry: u64,
    /// Rust allocation/reallocation requests; requires the profiling executable.
    pub allocations: u64,
    /// Requested Rust allocation bytes, not live bytes.
    pub allocated_bytes: u64,
    /// Publication capture, private acceptance, viewport paint and geometry time.
    pub stage_us: [u64; 4],
}
thread_local! {
    static ENABLED: Cell<bool> = const { Cell::new(false) };
    static WORK: Cell<Work> = Cell::new(Work::default());
}
/// Enable counters on this thread only. Disabled sessions do not sample clocks.
pub fn enable(enabled: bool) {
    ENABLED.with(|slot| slot.set(enabled));
}
/// Reset one frame's accounting.
pub fn reset() {
    WORK.with(|slot| slot.set(Work::default()));
}
/// Read one frame's accounting without allocation.
pub fn snapshot() -> Work {
    WORK.with(Cell::get)
}
/// Update counters if enabled. Also safe from a profiling allocator/TLS teardown.
pub fn record(update: impl FnOnce(&mut Work)) {
    let _ = ENABLED.try_with(|enabled| {
        if enabled.get() {
            let _ = WORK.try_with(|slot| {
                let mut work = slot.get();
                update(&mut work);
                slot.set(work);
            });
        }
    });
}
/// A coarse native render stage, not a sampling profiler or exclusive CPU time.
pub struct Span {
    stage: usize,
    start: Option<Instant>,
}
impl Span {
    /// Stage 0: capture; 1: acceptance; 2: viewport; 3: geometry.
    pub fn new(stage: usize) -> Self {
        Self {
            stage,
            start: ENABLED.with(|enabled| enabled.get().then(Instant::now)),
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(start) = self.start {
            let elapsed = start.elapsed().as_micros() as u64;
            record(|work| work.stage_us[self.stage] += elapsed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accounting_is_opt_in_and_frame_local() {
        enable(false);
        reset();
        record(|work| work.mapped += 5);
        assert_eq!(snapshot(), Work::default());
        enable(true);
        record(|work| work.mapped += 5);
        assert_eq!(snapshot().mapped, 5);
        std::thread::spawn(|| assert_eq!(snapshot(), Work::default()))
            .join()
            .unwrap();
        reset();
        assert_eq!(snapshot(), Work::default());
        enable(false);
    }
}
