//! The two stdio pumps own blocking OS handles, not Tokio blocking-pool jobs:
//! an idle stdin must not prevent runtime shutdown after SIGTERM. Protocol and
//! runner work stays on the current-thread Tokio executor. Pumps have bounded
//! queues; only the writer can touch stdout, and cancellation never interrupts
//! a frame that has begun writing. The process exit closes idle pump handles.
use super::boundaries::{RpcError, FRAME_BYTES};
use anyhow::{bail, Result};
use serde_json::Value;
use std::cell::RefCell;
use std::io::{BufRead, Read, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc as blocking, Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Queued,
    Sent,
    Cancelled,
    Settled,
}

pub(super) struct Delivery(Mutex<State>);
impl Delivery {
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(State::Queued)))
    }
    fn begin(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        if *state != State::Queued {
            return false;
        }
        *state = State::Sent;
        true
    }
    // Exactly one caller is responsible for the reverse cancellation, and only
    // if the writer has crossed the begin-write boundary. The frame may still
    // be blocked writing; the serialized writer finishes it before the cancel.
    pub fn abandon(&self) -> bool {
        let mut state = self.0.lock().unwrap();
        let sent = *state == State::Sent;
        *state = State::Cancelled;
        sent
    }
    pub fn settle(&self) {
        *self.0.lock().unwrap() = State::Settled;
    }
}
enum Frame {
    Message(Vec<u8>, Option<Arc<Delivery>>),
    Progress,
}

const MESSAGE_SLOTS: usize = 128;

#[derive(Default)]
struct Outbox {
    messages: AtomicUsize,
    bytes: AtomicUsize,
    // A single replaceable snapshot and FIFO wakeup, separate from the 128
    // reliable slots / 8 MiB. Never hold the lock during a pipe write.
    progress: Mutex<Option<Value>>,
}
impl Outbox {
    fn write(&self, frame: Frame, output: &mut impl Write) -> std::io::Result<()> {
        let mut write = |data: &[u8]| output.write_all(data).and_then(|_| output.flush());
        match frame {
            Frame::Message(data, delivery) => {
                self.messages.fetch_sub(1, Ordering::Relaxed);
                let result = if delivery.is_none_or(|d| d.begin()) {
                    write(&data)
                } else {
                    Ok(())
                };
                self.bytes.fetch_sub(data.len(), Ordering::Relaxed);
                result
            }
            Frame::Progress => {
                let latest = self.progress.lock().unwrap().take().unwrap();
                let mut data = serde_json::to_vec(&latest)?;
                data.push(b'\n');
                write(&data)
            }
        }
    }
}

pub(super) fn reader() -> Result<mpsc::Receiver<Result<Vec<u8>>>> {
    let (sender, receiver) = mpsc::channel(1);
    std::thread::Builder::new()
        .name("codemode-stdin".into())
        .spawn(move || {
            let input = std::io::stdin();
            let mut input = input.lock();
            loop {
                let result = (|| {
                    let mut data = Vec::new();
                    let count = Read::take(&mut input, (FRAME_BYTES + 2) as u64)
                        .read_until(b'\n', &mut data)?;
                    if count == 0 {
                        return Ok(None);
                    }
                    if count > FRAME_BYTES + 1 || data.pop() != Some(b'\n') {
                        bail!("Incoming JSON-RPC frame exceeds 1 MiB or is truncated");
                    }
                    Ok(Some(data))
                })();
                match result {
                    Ok(Some(data)) => {
                        if sender.blocking_send(Ok(data)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error));
                        break;
                    }
                }
            }
        })?;
    Ok(receiver)
}

pub(super) struct Writer {
    sender: RefCell<Option<blocking::SyncSender<Frame>>>,
    outbox: Arc<Outbox>,
    done: RefCell<Option<oneshot::Receiver<()>>>,
    failure: watch::Sender<Option<String>>,
}
impl Writer {
    pub fn new() -> Result<(Self, watch::Receiver<Option<String>>)> {
        let (sender, receiver) = blocking::sync_channel(MESSAGE_SLOTS + 1);
        let (done_tx, done) = oneshot::channel();
        let (failure, failures) = watch::channel(None);
        let outbox = Arc::new(Outbox::default());
        let pump = outbox.clone();
        let failed = failure.clone();
        std::thread::Builder::new()
            .name("codemode-stdout".into())
            .spawn(move || {
                let output = std::io::stdout();
                let mut output = output.lock();
                while let Ok(frame) = receiver.recv() {
                    if let Err(error) = pump.write(frame, &mut output) {
                        failed.send_replace(Some(error.to_string()));
                        break;
                    }
                }
                let _ = done_tx.send(());
            })?;
        Ok((
            Self {
                sender: RefCell::new(Some(sender)),
                outbox,
                done: RefCell::new(Some(done)),
                failure,
            },
            failures,
        ))
    }
    #[cfg(test)]
    pub fn gated_for_test() -> (Self, TestWriter) {
        let (sender, receiver) = blocking::sync_channel(MESSAGE_SLOTS + 1);
        let (failure, _) = watch::channel(None);
        let outbox = Arc::new(Outbox::default());
        (
            Self {
                sender: RefCell::new(Some(sender)),
                outbox: outbox.clone(),
                done: RefCell::new(None),
                failure,
            },
            TestWriter { receiver, outbox },
        )
    }
    pub fn send(&self, value: &Value, delivery: Option<Arc<Delivery>>) -> Result<()> {
        let mut data = serde_json::to_vec(value)?;
        if data.len() > FRAME_BYTES {
            return Err(RpcError(-32002, "Outgoing JSON-RPC frame exceeds 1 MiB".into()).into());
        }
        data.push(b'\n');
        let size = data.len();
        if self.outbox.messages.load(Ordering::Relaxed) >= MESSAGE_SLOTS {
            return self.exhausted();
        }
        let reserve = self
            .outbox
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n + size <= 8 * FRAME_BYTES).then_some(n + size)
            });
        if reserve.is_err() {
            return self.exhausted();
        }
        self.outbox.messages.fetch_add(1, Ordering::Relaxed);
        let sender = self.sender.borrow();
        let sent = sender
            .as_ref()
            .is_some_and(|s| s.try_send(Frame::Message(data, delivery)).is_ok());
        if !sent {
            self.outbox.messages.fetch_sub(1, Ordering::Relaxed);
            self.outbox.bytes.fetch_sub(size, Ordering::Relaxed);
            return self.exhausted();
        }
        Ok(())
    }
    pub fn progress(&self, value: Value) {
        let mut pending = self.outbox.progress.lock().unwrap();
        if pending.is_some()
            || self
                .sender
                .borrow()
                .as_ref()
                .is_some_and(|s| s.try_send(Frame::Progress).is_ok())
        {
            *pending = Some(value);
        }
    }
    fn exhausted(&self) -> Result<()> {
        self.failure.send_replace(Some(
            "Protocol writer queue exhausted (128 frames/8 MiB)".into(),
        ));
        Err(RpcError(-32002, "Protocol writer queue exhausted".into()).into())
    }
    pub async fn close(&self) -> Result<()> {
        self.sender.borrow_mut().take();
        let done = self.done.borrow_mut().take();
        if let Some(done) = done {
            tokio::time::timeout(Duration::from_millis(500), done)
                .await
                .map_err(|_| anyhow::anyhow!("Protocol writer did not drain within 500 ms"))??;
        }
        if let Some(error) = self.failure.borrow().as_ref() {
            bail!("Protocol writer failed: {error}");
        }
        Ok(())
    }
}

// A manual begin-write gate for deterministic request/cleanup race tests. It
// consumes the real bounded Writer::send queue and the same Delivery fence,
// without letting an OS thread choose the test's scheduling interleaving.
#[cfg(test)]
pub(super) struct TestWriter {
    receiver: blocking::Receiver<Frame>,
    outbox: Arc<Outbox>,
}
#[cfg(test)]
impl TestWriter {
    pub fn drain(&self) -> Vec<Value> {
        self.receiver
            .try_iter()
            .filter_map(|frame| {
                let mut bytes = Vec::new();
                self.outbox.write(frame, &mut bytes).unwrap();
                (!bytes.is_empty()).then(|| serde_json::from_slice(&bytes).unwrap())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_burst_coalesces_without_taking_any_message_slots() {
        let (writer, gate) = Writer::gated_for_test();
        for update in 0..512 {
            writer.progress(Value::from(update));
        }
        for _ in 0..MESSAGE_SLOTS {
            writer.send(&Value::Null, None).unwrap();
        }
        let frames = gate.drain();
        assert_eq!(frames.len(), MESSAGE_SLOTS + 1);
        assert_eq!(frames[0], 511);
        assert!(frames[1..].iter().all(Value::is_null));
    }
    #[test]
    fn cancellation_skips_queued_frames_and_follows_started_frames_once() {
        let queued = Delivery::new();
        assert!(!queued.abandon());
        assert!(!queued.begin());
        let sent = Delivery::new();
        assert!(sent.begin());
        assert!(sent.abandon());
        assert!(!sent.abandon());
        let settled = Delivery::new();
        assert!(settled.begin());
        settled.settle();
        assert!(!settled.abandon());
    }
}
