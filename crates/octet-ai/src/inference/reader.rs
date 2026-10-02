//! Bounded receive-side observation, independent of terminal/agent polling.
//! Cancellation drops the reader task; no detached generation reader survives.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use futures_core::Stream;
use futures_util::{FutureExt, StreamExt};
use tokio::sync::{mpsc, Semaphore};

use super::{DecodeEstimateUnavailable, InferenceMetrics};
use crate::{AiError, StreamEvent};

const QUEUE_EVENTS: usize = 256;
const QUEUE_BYTES: u32 = 16 * 1024 * 1024;

type EventStream<T> = Pin<Box<dyn Stream<Item = Result<T, AiError>> + Send>>;

struct Reader<T> {
    task: Option<tokio::task::JoinHandle<EventStream<T>>>,
    _ready_stream: Option<EventStream<T>>,
}
impl<T> Drop for Reader<T> {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// An oversized guarded terminal/media event fills the admission budget; the
/// inline ready path can share its budget with preceding events. Admission
/// accounts for queued observations, not total heap RSS or caller-owned events.
pub(super) fn event_weight(event: &StreamEvent) -> u32 {
    let bytes = match event {
        StreamEvent::TextDelta { delta, .. }
        | StreamEvent::ReasoningDelta { delta, .. }
        | StreamEvent::ToolCallArgsDelta { delta, .. } => delta.len(),
        StreamEvent::Started { response_id } => response_id.as_ref().map_or(0, String::len),
        StreamEvent::ToolCallStart { id, name, .. } => id.0.len() + name.len(),
        StreamEvent::Finished(_) | StreamEvent::MediaCompleted { .. } => QUEUE_BYTES as usize,
        _ => 256,
    };
    (bytes.saturating_add(std::mem::size_of::<StreamEvent>())).min(QUEUE_BYTES as usize) as u32
}

pub(super) fn finish_feedback(event: &mut StreamEvent, blocked: bool) -> bool {
    if let StreamEvent::Finished(response) = event {
        if blocked {
            let metrics = response
                .inference
                .get_or_insert_with(InferenceMetrics::default);
            metrics.decode_estimate = None;
            metrics.decode_unavailable = Some(DecodeEstimateUnavailable::LocalBackpressure);
        }
        true
    } else {
        false
    }
}

/// Lazy start preserves the stream's cancellation/dispatch lifetime. After the
/// first poll, the reader observes ahead of presentation within bounded budgets.
pub(crate) fn independent_stream<T: Send + 'static>(
    stream: EventStream<T>,
    feedback: fn(&mut T, bool) -> bool,
    weight: fn(&T) -> u32,
) -> EventStream<T> {
    // Keep source ownership through EOF, not just through reader completion.
    // In particular, a temporary client's cached socket must not be closed by
    // eager observation while the caller still owns its steering session.
    let owner = Arc::new(Mutex::new(None));
    let keepalive = owner.clone();
    let observed = async_stream::stream! {
        let (tx, mut rx) = mpsc::channel(QUEUE_EVENTS);
        let bytes = Arc::new(Semaphore::new(QUEUE_BYTES as usize));
        let mut stream = stream;
        let mut pending = None;
        let mut ended = false;
        // Preserve same-poll completion/settlement: already-ready responses must
        // not become Pending merely because an observer task was introduced.
        // After the first Pending or admission limit, receive ahead in the task.
        for _ in 0..QUEUE_EVENTS {
            if bytes.available_permits() == 0 { break; }
            match stream.next().now_or_never() {
                Some(Some(mut item)) => {
                    let terminal = item.as_mut().is_ok_and(|event| feedback(event, false));
                    let units = item.as_ref().map_or(256, weight).min(bytes.available_permits() as u32);
                    let permit = bytes.clone().try_acquire_many_owned(units).expect("inline admission has capacity");
                    assert!(tx.try_send((item, permit)).is_ok());
                    if terminal {
                        // A ready EOF also stays ready, including when the
                        // terminal response occupies the whole byte budget.
                        match stream.next().now_or_never() {
                            Some(None) => ended = true,
                            Some(Some(item)) => pending = Some(item),
                            None => {},
                        }
                        break;
                    }
                },
                Some(None) => { ended = true; break; },
                None => break,
            }
        }
        let reader = if ended {
            drop(tx);
            Reader { task: None, _ready_stream: Some(stream) }
        } else {
            Reader { _ready_stream: None, task: Some(tokio::spawn(async move {
            let mut blocked = false;
            while let Some(mut item) = match pending.take() {
                Some(item) => Some(item),
                None => stream.next().await,
            } {
                let terminal = item.as_mut().is_ok_and(|event| feedback(event, blocked));
                let units = item.as_ref().map_or(256, weight);
                let permit = match bytes.clone().try_acquire_many_owned(units) {
                    Ok(permit) => permit,
                    Err(_) => {
                        blocked = true;
                        match bytes.clone().acquire_many_owned(units).await {
                            Ok(permit) => permit,
                            Err(_) => break,
                        }
                    }
                };
                match tx.try_send((item, permit)) {
                    Ok(()) => {},
                    Err(mpsc::error::TrySendError::Full(item)) => {
                        blocked = true;
                        if tx.send(item).await.is_err() { break; }
                    },
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
                if terminal { blocked = false; }
            }
            stream
            })) }
        };
        *owner.lock().expect("reader owner is not poisoned") = Some(reader);
        while let Some((item, permit)) = rx.recv().await {
            drop(permit);
            yield item;
        }
    };
    Box::pin(observed.map(move |item| {
        let _ = &keepalive;
        item
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[test]
    fn ready_terminal_and_eof_remain_ready_in_the_same_poll() {
        let source = Box::pin(futures_util::stream::iter([
            Ok((0_u32, false)),
            Ok((1_u32, true)),
        ]));
        let stream = independent_stream(
            source,
            |event, _| event.1,
            |event| {
                if event.1 {
                    QUEUE_BYTES
                } else {
                    256
                }
            },
        );
        let result = stream
            .collect::<Vec<_>>()
            .now_or_never()
            .expect("ready acceptance/EOF must not acquire a task scheduling boundary");
        assert_eq!(result.len(), 2);
        assert_eq!(result[1].as_ref().unwrap().0, 1);
    }

    #[tokio::test]
    async fn receiver_stall_does_not_pause_observation_until_bounded_queue_fills() {
        let reached = Arc::new(Notify::new());
        let seen = Arc::new(AtomicUsize::new(0));
        let notify = reached.clone();
        let count = seen.clone();
        let source = Box::pin(async_stream::stream! {
            for n in 0..300 {
                count.store(n, Ordering::SeqCst);
                if n == 257 { notify.notify_one(); }
                yield Ok((n, false));
            }
        });
        let mut stream = independent_stream(
            source,
            |event, blocked| {
                if event.0 == 299 {
                    event.1 = blocked;
                    true
                } else {
                    false
                }
            },
            |_| 256,
        );
        assert_eq!(stream.next().await.unwrap().unwrap().0, 0);
        tokio::time::timeout(std::time::Duration::from_secs(5), reached.notified())
            .await
            .unwrap();
        assert!(
            seen.load(Ordering::SeqCst) <= 258,
            "reader must remain bounded"
        );
        let mut last = None;
        while let Some(item) = stream.next().await {
            last = Some(item.unwrap());
        }
        assert_eq!(
            last,
            Some((299, true)),
            "saturation must suppress false precision"
        );
    }

    #[tokio::test]
    async fn source_ownership_survives_eof_until_the_caller_drops_the_stream() {
        struct Dropped(Arc<AtomicUsize>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        for tasked in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let guard = Dropped(dropped.clone());
            let source: EventStream<u32> = if tasked {
                Box::pin(futures_util::stream::once(async {
                    tokio::task::yield_now().await;
                    Ok(1)
                }))
            } else {
                Box::pin(futures_util::stream::iter([Ok(1)]))
            };
            let source = Box::pin(source.map(move |item| {
                let _ = &guard;
                item
            }));
            let mut stream = independent_stream(source, |_, _| false, |_| 256);
            assert_eq!(stream.next().await.unwrap().unwrap(), 1);
            assert!(stream.next().await.is_none());
            assert_eq!(dropped.load(Ordering::SeqCst), 0);
            drop(stream);
            assert_eq!(dropped.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn dropping_stream_aborts_the_independent_reader() {
        struct Dropped(Arc<Notify>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.notify_one();
            }
        }
        let cancelled = Arc::new(Notify::new());
        let flag = cancelled.clone();
        let source = Box::pin(async_stream::stream! {
            let _guard = Dropped(flag);
            yield Ok(1_u32);
            std::future::pending::<()>().await;
        });
        let mut stream = independent_stream(source, |_, _| false, |_| 256);
        assert_eq!(stream.next().await.unwrap().unwrap(), 1);
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(5), cancelled.notified())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn large_events_reserve_the_byte_budget_alone_and_preserve_order() {
        let source = Box::pin(futures_util::stream::iter((0_u32..10).map(Ok)));
        let mut stream = independent_stream(source, |_, _| false, |_| QUEUE_BYTES);
        for n in 0..10 {
            assert_eq!(stream.next().await.unwrap().unwrap(), n);
        }
        assert!(stream.next().await.is_none());
    }
}
