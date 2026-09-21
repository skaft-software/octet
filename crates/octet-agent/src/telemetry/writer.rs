//! Ordered, best-effort telemetry transport; never used for session accounting.
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const MAX_QUEUE_BYTES: usize = 1024 * 1024;
const MAX_QUEUE_RECORDS: usize = 256;
pub(super) const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

/// Snapshot of optional telemetry delivery, not authoritative usage accounting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TelemetryStatus {
    /// Accepted records not yet written, including the in-flight record.
    pub pending_records: usize,
    /// Bytes reserved by pending records, including the in-flight write.
    pub pending_bytes: usize,
    /// Records rejected by the byte/count budget or after a write failure.
    pub rejected_records: u64,
    /// First output failure. No more writes are attempted after this failure.
    pub write_error: Option<io::ErrorKind>,
    /// Admission was closed by explicit shutdown or last-owner drop.
    pub closed: bool,
    /// A drain deadline expired; pending records are not confirmed written.
    pub drain_timed_out: bool,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Vec<u8>>,
    status: TelemetryStatus,
    accepted: u64,
    completed: u64,
    finished: bool,
}

#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

pub(super) struct Writer {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Writer {
    pub(super) fn new(mut file: File) -> io::Result<Self> {
        Self::start(move |bytes| {
            // Advisory-lock contention must not strand a writer forever.
            let deadline = Instant::now() + DEFAULT_DRAIN_TIMEOUT;
            loop {
                match fs2::FileExt::try_lock_exclusive(&file) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error),
                }
            }
            let result = file.write_all(bytes).and_then(|_| file.flush());
            let unlock = fs2::FileExt::unlock(&file);
            result.and(unlock)
        })
    }

    fn start(mut write: impl FnMut(&[u8]) -> io::Result<()> + Send + 'static) -> io::Result<Self> {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        let thread = std::thread::Builder::new()
            .name("octet-telemetry".into())
            .spawn(move || {
                loop {
                    let bytes = {
                        let mut state = worker.state.lock().unwrap();
                        while state.queue.is_empty() && !state.status.closed {
                            state = worker.changed.wait(state).unwrap();
                        }
                        let Some(bytes) = state.queue.pop_front() else {
                            break;
                        };
                        bytes
                    };
                    // No observer or queue lock is held across filesystem I/O.
                    let result = write(&bytes);
                    let mut state = worker.state.lock().unwrap();
                    state.status.pending_bytes -= bytes.len();
                    state.status.pending_records -= 1;
                    state.completed += 1;
                    if let Err(error) = result {
                        state.status.write_error = Some(error.kind());
                        state.queue.clear();
                        state.status.pending_bytes = 0;
                        state.status.pending_records = 0;
                        worker.changed.notify_all();
                        break;
                    }
                    worker.changed.notify_all();
                }
                worker.state.lock().unwrap().finished = true;
                worker.changed.notify_all();
            })?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub(super) fn enqueue(&self, bytes: Vec<u8>) {
        let mut state = self.shared.state.lock().unwrap();
        if state.status.closed
            || state.status.write_error.is_some()
            || state.status.pending_records >= MAX_QUEUE_RECORDS
            || bytes.len() > MAX_QUEUE_BYTES.saturating_sub(state.status.pending_bytes)
        {
            state.status.rejected_records = state.status.rejected_records.saturating_add(1);
            return;
        }
        state.status.pending_bytes += bytes.len();
        state.status.pending_records += 1;
        state.accepted += 1;
        state.queue.push_back(bytes);
        self.shared.changed.notify_one();
    }

    pub(super) fn status(&self) -> TelemetryStatus {
        self.shared.state.lock().unwrap().status
    }

    /// Barrier for records accepted before this call. Does not fsync. Reports
    /// any historical rejection, even if the remaining records drained cleanly.
    pub(super) fn flush(&self, timeout: Duration) -> io::Result<()> {
        let mut state = self.shared.state.lock().unwrap();
        let target = state.accepted;
        let (next, waited) = self
            .shared
            .changed
            .wait_timeout_while(state, timeout, |state| {
                state.completed < target && state.status.write_error.is_none()
            })
            .unwrap();
        state = next;
        if let Some(kind) = state.status.write_error {
            return Err(io::Error::new(
                kind,
                "telemetry write failed; log is incomplete",
            ));
        }
        if waited.timed_out() && state.completed < target {
            state.status.drain_timed_out = true;
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "telemetry drain timed out; pending records are unconfirmed",
            ));
        }
        if state.status.rejected_records != 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "telemetry admission rejected records; log is incomplete",
            ));
        }
        Ok(())
    }

    pub(super) fn close(&self) {
        let mut state = self.shared.state.lock().unwrap();
        state.status.closed = true;
        self.shared.changed.notify_one();
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.close();
        let state = self.shared.state.lock().unwrap();
        let (mut state, _) = self
            .shared
            .changed
            .wait_timeout_while(state, Duration::from_millis(100), |state| !state.finished)
            .unwrap();
        if state.finished {
            drop(state);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        } else {
            // std filesystem writes cannot be cancelled safely. Detach rather
            // than blocking UI teardown forever. The worker still owns and
            // drains only its bounded queue; process exit may lose that tail.
            state.status.drain_timed_out = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn blocked_writer_bounds_bytes_including_in_flight_and_drains_in_order() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let mut first = true;
        let writer = Writer::start(move |bytes| {
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            captured.lock().unwrap().push(bytes[0]);
            Ok(())
        })
        .unwrap();
        writer.enqueue(vec![1; MAX_QUEUE_BYTES / 2]);
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        writer.enqueue(vec![2; MAX_QUEUE_BYTES / 2]);
        writer.enqueue(vec![3]);
        assert_eq!(writer.status().pending_bytes, MAX_QUEUE_BYTES);
        assert_eq!(writer.status().rejected_records, 1);
        release_tx.send(()).unwrap();
        assert_eq!(
            writer.flush(DEFAULT_DRAIN_TIMEOUT).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(writer);
        assert_eq!(*output.lock().unwrap(), vec![1, 2]);
    }

    #[test]
    fn count_budget_and_shutdown_deadline_do_not_wait_for_blocked_io() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut first = true;
        let writer = Writer::start(move |_| {
            if first {
                first = false;
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            }
            Ok(())
        })
        .unwrap();
        writer.enqueue(vec![0]);
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 1..MAX_QUEUE_RECORDS {
            writer.enqueue(vec![0]);
        }
        writer.enqueue(vec![0]);
        assert_eq!(writer.status().pending_records, MAX_QUEUE_RECORDS);
        assert_eq!(writer.status().rejected_records, 1);
        writer.close();
        assert_eq!(
            writer.flush(Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert!(writer.status().closed && writer.status().drain_timed_out);
        writer.enqueue(vec![1]);
        assert_eq!(writer.status().rejected_records, 2);
        let (dropped_tx, dropped_rx) = mpsc::channel();
        std::thread::spawn(move || {
            drop(writer);
            dropped_tx.send(()).unwrap();
        });
        let dropped = dropped_rx.recv_timeout(Duration::from_secs(5));
        // Always release the fake I/O even if the bounded-drop assertion fails.
        release_tx.send(()).unwrap();
        dropped.expect("drop must not join indefinitely behind blocked I/O");
    }

    #[test]
    fn shutdown_drains_and_failure_is_observable() {
        let output = Arc::new(Mutex::new(Vec::new()));
        let captured = output.clone();
        let writer = Writer::start(move |bytes| {
            captured.lock().unwrap().extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();
        for value in 0..100u8 {
            writer.enqueue(vec![value]);
        }
        drop(writer);
        assert_eq!(*output.lock().unwrap(), (0..100u8).collect::<Vec<_>>());
        let writer =
            Writer::start(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied))).unwrap();
        writer.enqueue(vec![1]);
        assert_eq!(
            writer.flush(DEFAULT_DRAIN_TIMEOUT).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        writer.enqueue(vec![2]);
        assert_eq!(writer.status().rejected_records, 1);
    }
}
