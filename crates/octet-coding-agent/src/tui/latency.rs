//! Opt-in, payload-free native input/publication/completed-write timing.
//! The terminal is process-owned; accepted input has one monotonically ordered clock.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};

static RECEIVED: AtomicU64 = AtomicU64::new(0);
static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();
const CAPACITY: usize = 1024;

#[derive(Default)]
struct Trace {
    pending: VecDeque<Sample>,
    dropped: u64,
}
struct Sample {
    sequence: u64,
    kind: &'static str,
    received: Instant,
    updated: Option<Instant>,
}
fn trace() -> Option<&'static Mutex<Trace>> {
    TRACE
        .get_or_init(|| {
            (std::env::var("OCTET_STARTUP_TRACE").as_deref() == Ok("latency"))
                .then(|| Mutex::new(Trace::default()))
        })
        .as_ref()
}

pub(crate) fn received(event: &Event) {
    let kind = match event {
        Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => "arrow",
            KeyCode::Char(_) | KeyCode::Backspace
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                "typing"
            }
            _ => "key",
        },
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => "wheel",
            MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => {
                "selection"
            }
            _ => return,
        },
        Event::Paste(_) => "paste",
        _ => return,
    };
    let sequence = RECEIVED.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(trace) = trace() {
        let mut trace = trace.lock().expect("input trace poisoned");
        trace.receive(sequence, kind, Instant::now());
    }
}

/// First render publication following input admission is the state-update fence.
pub(crate) fn published() -> u64 {
    let sequence = RECEIVED.load(Ordering::Acquire);
    if let Some(trace) = trace() {
        let now = Instant::now();
        trace
            .lock()
            .expect("input trace poisoned")
            .publish(sequence, now);
    }
    sequence
}

/// A completed terminal write, not physical emulator display. The sequence was
/// captured before rendering, so input arriving during a write waits for another frame.
pub(crate) fn painted(sequence: u64) {
    if let Some(trace) = trace() {
        let now = Instant::now();
        let mut trace = trace.lock().expect("input trace poisoned");
        while let Some(sample) = trace.complete(sequence) {
            eprintln!(
                "octet-latency: kind={} sequence={} updated_us={} painted_us={} dropped={}",
                sample.kind,
                sample.sequence,
                sample
                    .updated
                    .unwrap()
                    .duration_since(sample.received)
                    .as_micros(),
                now.duration_since(sample.received).as_micros(),
                trace.dropped,
            );
        }
    }
}

impl Trace {
    fn receive(&mut self, sequence: u64, kind: &'static str, received: Instant) {
        if self.pending.len() == CAPACITY {
            self.pending.pop_front();
            self.dropped += 1;
        }
        self.pending.push_back(Sample {
            sequence,
            kind,
            received,
            updated: None,
        });
    }

    fn publish(&mut self, sequence: u64, now: Instant) {
        for sample in &mut self.pending {
            if sample.sequence <= sequence && sample.updated.is_none() {
                sample.updated = Some(now);
            }
        }
    }

    fn complete(&mut self, sequence: u64) -> Option<Sample> {
        if self
            .pending
            .front()
            .is_some_and(|sample| sample.sequence <= sequence && sample.updated.is_some())
        {
            self.pending.pop_front()
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_trace_fences_publication_and_input_during_write() {
        let mut trace = Trace::default();
        let now = Instant::now();
        trace.receive(1, "wheel", now);
        assert!(trace.complete(1).is_none());
        trace.publish(1, now);
        trace.receive(2, "arrow", now);
        trace.publish(2, now);
        assert_eq!(trace.complete(1).unwrap().sequence, 1);
        assert!(trace.complete(1).is_none());
        assert_eq!(trace.complete(2).unwrap().sequence, 2);
        assert!(trace.complete(2).is_none());
    }

    #[test]
    fn input_trace_bounds_unpainted_events_and_retains_first_publication() {
        let mut trace = Trace::default();
        let now = Instant::now();
        for sequence in 0..=CAPACITY as u64 {
            trace.receive(sequence, "typing", now);
        }
        assert_eq!(trace.pending.len(), CAPACITY);
        assert_eq!(trace.dropped, 1);
        trace.publish(CAPACITY as u64, now);
        trace.publish(CAPACITY as u64, now + std::time::Duration::from_secs(1));
        let sample = trace.complete(CAPACITY as u64).unwrap();
        assert_eq!(sample.sequence, 1);
        assert_eq!(sample.updated, Some(now));
    }
}
