//! Rate-limited presentation updates.

use super::*;

pub(super) struct PresentationUpdateRate {
    pub(super) window_started: Instant,
    pub(super) accepted: usize,
    pub(super) warned: bool,
}

impl Default for PresentationUpdateRate {
    fn default() -> Self {
        Self {
            window_started: Instant::now(),
            accepted: 0,
            warned: false,
        }
    }
}

impl PresentationUpdateRate {
    pub(super) fn admit(&mut self) -> (bool, bool) {
        if self.window_started.elapsed() >= Duration::from_secs(1) {
            *self = Self::default();
        }
        if self.accepted < MAX_PRESENTATION_UPDATES_PER_SECOND {
            self.accepted += 1;
            return (true, false);
        }
        let first_rejection = !self.warned;
        self.warned = true;
        (false, first_rejection)
    }
}

pub(super) type PresentationDispatch = (
    u64,
    Option<ExtensionResourceOwner>,
    ExtensionPresentationSnapshot,
);

pub(super) async fn dispatch_presentation_updates(
    mut updates: watch::Receiver<Option<PresentationDispatch>>,
    events: broadcast::Sender<ExtensionEvent>,
    generation: u64,
) {
    let mut emitted_sequence = 0_u64;
    let mut window_started = tokio::time::Instant::now();
    let mut accepted = 0_usize;
    let mut warned = false;
    loop {
        if updates.changed().await.is_err() {
            return;
        }
        loop {
            let latest = updates.borrow_and_update().clone();
            let Some((sequence, resource_owner, snapshot)) = latest else {
                break;
            };
            if sequence <= emitted_sequence {
                break;
            }
            let now = tokio::time::Instant::now();
            if now.duration_since(window_started) >= Duration::from_secs(1) {
                window_started = now;
                accepted = 0;
                warned = false;
            }
            if accepted < MAX_PRESENTATION_UPDATES_PER_SECOND {
                accepted += 1;
                emitted_sequence = sequence;
                let _ = events.send(ExtensionEvent::PresentationUpdated {
                    generation,
                    resource_owner,
                    snapshot,
                });
                break;
            }
            if !warned {
                warned = true;
                let _ = events.send(ExtensionEvent::Diagnostic {
                    message: format!(
                        "semantic presentation update rate exceeded {MAX_PRESENTATION_UPDATES_PER_SECOND}/s; coalescing to the latest complete snapshot"
                    ),
                });
            }
            let deadline = window_started + Duration::from_secs(1);
            tokio::select! {
                changed = updates.changed() => {
                    if changed.is_err() {
                        return;
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    window_started = tokio::time::Instant::now();
                    accepted = 0;
                    warned = false;
                }
            }
        }
    }
}
