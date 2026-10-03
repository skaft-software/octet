//! Live preview pacing, tool progress and partial output checkpoints.

use super::*;

/// Bounded pacing for the live panel's *replaceable* publications (row 4.8).
///
/// The panel feed is `ToolProgress`: append-only `Output`/`Status` chunks are
/// load-bearing (dropping one breaks the `complete_<stream>=true` contract) and
/// stay verbatim, while a [`ToolProgressDecoration`] replaces the previous
/// annotation, so an intermediate one carries nothing the latest does not.
/// This is the run-path consumer of [`AdaptivePreviewCoalescer`]:
///
/// * the first decoration of a call is published immediately,
/// * later ones are paced to the coalescer's interval/rate policy and collapsed
///   to the latest state,
/// * [`LivePreviewPacer::settle`] forces the held state at the call's terminal
///   boundary, so a finished call can never leave the panel on stale state.
pub(super) struct LivePreviewPacer {
    pub(super) coalescer: AdaptivePreviewCoalescer,
    pub(super) pending: Option<ToolProgressDecoration>,
    /// Publications observed; read by [`Self::stats`] in tests only.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) published: u64,
    /// Intermediate states collapsed away; read by [`Self::stats`] in tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) coalesced: u64,
}

impl LivePreviewPacer {
    pub(super) fn new() -> Self {
        Self {
            coalescer: AdaptivePreviewCoalescer::new(),
            pending: None,
            published: 0,
            coalesced: 0,
        }
    }

    /// Routes one replaceable update, returning the publication to forward now.
    pub(super) fn observe(
        &mut self,
        decoration: ToolProgressDecoration,
        now: std::time::Instant,
    ) -> Option<ToolProgressDecoration> {
        let encoded_bytes = decoration.label().len() + decoration.detail().map_or(0, str::len);
        match self.coalescer.record(encoded_bytes, now) {
            PreviewPublication::Immediate => {
                self.pending = None;
                self.published = self.published.saturating_add(1);
                Some(decoration)
            }
            PreviewPublication::Scheduled(_) => {
                if self.pending.replace(decoration).is_some() {
                    self.coalesced = self.coalesced.saturating_add(1);
                }
                None
            }
        }
    }

    /// The instant at which held state becomes publishable, if any is held.
    pub(super) fn flush_deadline(&self, now: std::time::Instant) -> Option<std::time::Instant> {
        self.pending.as_ref()?;
        self.coalescer
            .deadline_in(now)
            .map(|remaining| now + remaining)
    }

    /// Publishes held state if its pace deadline has passed.
    pub(super) fn take_due(&mut self, now: std::time::Instant) -> Option<ToolProgressDecoration> {
        self.coalescer.take_due(now)?;
        self.published = self.published.saturating_add(1);
        self.pending.take()
    }

    /// Publishes held state unconditionally (completion, error, cancellation).
    pub(super) fn settle(&mut self, now: std::time::Instant) -> Option<ToolProgressDecoration> {
        let pending = self.pending.take()?;
        self.coalescer.force(now, 0);
        self.published = self.published.saturating_add(1);
        Some(pending)
    }

    /// Decorations published and intermediate states collapsed away.
    ///
    /// Test-only observability: the live path forwards the surviving
    /// decoration itself, so production builds never read the counters.
    #[cfg(test)]
    pub(super) fn stats(&self) -> (u64, u64) {
        (self.published, self.coalesced)
    }
}

/// Routes one accepted progress item to the live panel.
///
/// Replaceable decorations go through `pacer`; every append-only flavor is
/// forwarded unchanged.
pub(super) fn forward_tool_progress(
    progress: ToolProgress,
    pacer: &mut LivePreviewPacer,
    now: std::time::Instant,
) -> Option<ToolProgress> {
    match progress {
        ToolProgress::Decoration(decoration) => {
            pacer.observe(decoration, now).map(ToolProgress::Decoration)
        }
        verbatim => Some(verbatim),
    }
}

/// Opt-in durable partial-output checkpointing for one tool's live calls (row
/// 4.7).
///
/// The host names the tool, supplies the durable replacement sink, and chooses
/// the cadence; the run path owns publishing. A name that matches no registered
/// tool costs checkpoints and nothing else — it can never change a result.
#[cfg(any(unix, windows))]
#[derive(Clone)]
pub(super) struct PartialOutputCheckpointConfig {
    pub(super) tool: String,
    // None binds the current session invocation at dispatch, never a global
    // sink shared by different provider calls.
    pub(super) sink: Option<Arc<dyn PartialOutputCheckpointSink>>,
    pub(super) interval: Duration,
    pub(super) totals: Arc<PartialOutputCheckpointTotals>,
}

/// Observable counters for the run path's checkpoint publications.
///
/// `published` is the number of snapshots the sink accepted, `paced` the
/// observations the publisher collapsed away (interval or duplicate), and
/// `failures` the storage faults. A failure is bookkeeping only: it never
/// changes a tool result, and it never becomes durable state.
#[cfg(any(unix, windows))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PartialOutputCheckpointStats {
    /// Bounded snapshots handed to the sink.
    pub published: u64,
    /// Observations suppressed by the interval or by duplicate suppression.
    pub paced: u64,
    /// Sink refusals (storage faults).
    pub failures: u64,
}

#[cfg(any(unix, windows))]
#[derive(Default)]
pub(super) struct PartialOutputCheckpointTotals {
    pub(super) published: AtomicU64,
    pub(super) paced: AtomicU64,
    pub(super) failures: AtomicU64,
}

#[cfg(any(unix, windows))]
impl PartialOutputCheckpointTotals {
    pub(super) fn stats(&self) -> PartialOutputCheckpointStats {
        PartialOutputCheckpointStats {
            published: self.published.load(Ordering::Relaxed),
            paced: self.paced.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

/// Run-path consumer of [`BashCheckpointPublisher`] for one live invocation.
///
/// Pi's durability contract makes the *harness* replace
/// `pendingToolOutput(operationId, invocationId)` while a call is live, and the
/// tool own the cadence and the "this update is a complete bounded snapshot"
/// claim. The cadence, the byte bound and duplicate suppression here are exactly
/// the publisher's; the durable replacement is the host's
/// [`PartialOutputCheckpointSink`]. The values live only as long as the call
/// does, so a settled invocation has nothing left to republish, and only
/// bounded state is retained: at most [`BASH_CHECKPOINT_MAX_BYTES`] per stream,
/// which is also the cap applied to the published snapshot.
#[cfg(any(unix, windows))]
pub(super) struct LivePartialOutput {
    pub(super) sink: Arc<dyn PartialOutputCheckpointSink>,
    pub(super) publisher: BashCheckpointPublisher,
    pub(super) streams: [PartialStream; 2],
    pub(super) totals: Arc<PartialOutputCheckpointTotals>,
}

/// Bytes of one stream the run-path tracker retains.
///
/// Half of the published cap minus a header reserve, so the rendered snapshot
/// (both stream headers plus both retained tails) always fits
/// [`BASH_CHECKPOINT_MAX_BYTES`] with its `stdout: N bytes seen` header intact
/// even after the publisher's final bounding fence.
#[cfg(any(unix, windows))]
pub(super) const PARTIAL_STREAM_CAP: usize = BASH_CHECKPOINT_MAX_BYTES / 2 - PARTIAL_HEADER_RESERVE;

/// Per-stream budget reserved for the snapshot header.
#[cfg(any(unix, windows))]
pub(super) const PARTIAL_HEADER_RESERVE: usize = 512;

/// Bounded, newest-bytes-retaining accumulation of one stream.
#[cfg(any(unix, windows))]
#[derive(Default)]
pub(super) struct PartialStream {
    pub(super) seen: u64,
    pub(super) tail: Vec<u8>,
    pub(super) elided: bool,
}

#[cfg(any(unix, windows))]
impl PartialStream {
    /// Appends live bytes, keeping at most [`PARTIAL_STREAM_CAP`] of the newest
    /// output on a UTF-8 boundary.
    pub(super) fn push(&mut self, bytes: &[u8]) {
        self.seen = self.seen.saturating_add(bytes.len() as u64);
        self.tail.extend_from_slice(bytes);
        if self.tail.len() <= PARTIAL_STREAM_CAP {
            return;
        }
        self.elided = true;
        let mut start = self.tail.len() - PARTIAL_STREAM_CAP;
        while start < self.tail.len() && !is_utf8_boundary(&self.tail, start) {
            start += 1;
        }
        self.tail.drain(..start);
    }

    pub(super) fn render(&self, name: &str) -> String {
        if self.seen == 0 {
            return format!("{name}: 0 bytes seen");
        }
        let text = String::from_utf8_lossy(&self.tail);
        let text = text.trim_end_matches('\n');
        let elided = if self.elided {
            " (earlier bytes elided)"
        } else {
            ""
        };
        format!(
            "{name}: {} bytes seen{elided}, showing the newest {} bytes\n{}",
            self.seen,
            self.tail.len(),
            text
        )
    }
}

/// Whether `index` starts a UTF-8 code point.
#[cfg(any(unix, windows))]
pub(super) fn is_utf8_boundary(bytes: &[u8], index: usize) -> bool {
    index >= bytes.len() || (bytes[index] & 0xC0) != 0x80
}

/// Renders the complete replaceable snapshot for one invocation.
///
/// It shares the tool layer's `"{stream}: {n} bytes seen"` header so recovery
/// reads one shape from either mechanism, keeps the newest bytes of each stream
/// on a code-point boundary, and deliberately never emits
/// `complete_<stream>=true`: a checkpoint must not be readable as proof that the
/// command finished.
#[cfg(any(unix, windows))]
pub(super) fn render_partial_output(streams: &[PartialStream; 2]) -> String {
    format!(
        "{}\n{}",
        streams[0].render("stdout"),
        streams[1].render("stderr")
    )
}

#[cfg(any(unix, windows))]
impl LivePartialOutput {
    /// Creates the tracker for `call` when the host's opt-in names that tool.
    pub(super) fn for_call(config: &PartialOutputCheckpointConfig, call: &str) -> Option<Self> {
        let sink = config.sink.as_ref()?;
        (config.tool == call).then(|| Self {
            sink: Arc::clone(sink),
            publisher: BashCheckpointPublisher::new(config.interval),
            streams: [PartialStream::default(), PartialStream::default()],
            totals: Arc::clone(&config.totals),
        })
    }

    /// Records one drained progress item, publishing only replaceable,
    /// bounded, complete output snapshots.
    pub(super) fn observe_progress(&mut self, progress: &ToolProgress, now: std::time::Instant) {
        if let ToolProgress::Output { stream, bytes } = progress {
            self.observe_output(*stream, bytes, now);
        }
    }

    /// Records live output and publishes the snapshot the publisher admits.
    ///
    /// Returns the published snapshot for observability/tests. A sink refusal is
    /// counted and dropped: the command's own result is unaffected.
    pub(super) fn observe_output(
        &mut self,
        stream: OutputStream,
        bytes: &[u8],
        now: std::time::Instant,
    ) -> Option<String> {
        let slot = match stream {
            OutputStream::Stdout => &mut self.streams[0],
            OutputStream::Stderr => &mut self.streams[1],
        };
        slot.push(bytes);
        if !self.publisher.is_due(now) {
            self.publisher.note_before_interval();
            self.totals.paced.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        let snapshot =
            BashCheckpointPublisher::bound_snapshot(&render_partial_output(&self.streams));
        let Some(published) = self.publisher.observe(&snapshot, now) else {
            self.totals.paced.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        match self.sink.checkpoint_partial_output(&published) {
            Ok(()) => {
                self.totals.published.fetch_add(1, Ordering::Relaxed);
                Some(published)
            }
            Err(_) => {
                self.publisher.note_failure();
                self.totals.failures.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

/// Result of applying one drained tool-progress item to the run.
pub(super) enum ProgressSettlement {
    /// Cancellation took precedence before the item was accepted; semantic
    /// state was discarded and the caller must stop accepting progress.
    Cancelled,
    /// Consumed internally as a durable session event (persisted, or its
    /// reply resolved with the persistence error).
    Settled,
    /// Pure progress; surface it to observers as a `ToolProgress` event.
    Emit(ToolProgress),
}

/// Apply one drained tool-progress item.
///
/// When `cancelled` won, any queued session event is rejected through its
/// reply channel without touching the session. Otherwise a session event is
/// appended durably and acknowledged; every other progress flavor is returned
/// for the caller to emit.
pub(super) fn settle_tool_progress(
    p: ToolProgress,
    cancelled: bool,
    session: &mut Session,
) -> ProgressSettlement {
    if cancelled {
        // The biased select deliberately gives cancellation
        // precedence. Events already accepted in the loop
        // remain durable, but a queued semantic event must
        // not take effect after the tool was reported as
        // cancelled (notably, it must not activate a skill).
        if let ToolProgress::SessionEvent(_, reply_tx_mutex)
        | ToolProgress::SessionMetadataEvent(_, reply_tx_mutex) = p
        {
            if let Ok(mut opt) = reply_tx_mutex.lock() {
                if let Some(reply_tx) = opt.take() {
                    let _ = reply_tx.send(Err(
                        "session event discarded because cancellation won".to_string()
                    ));
                }
            }
        }
        return ProgressSettlement::Cancelled;
    }
    let (res, reply_tx_mutex) = match p {
        ToolProgress::SessionEvent(event, reply) => (session.append(*event), reply),
        ToolProgress::SessionMetadataEvent(metadata, reply) => (
            session.append_with_metadata(
                EntryValue::Config {
                    model: None,
                    reasoning: None,
                    reasoning_mode: None,
                },
                Some(*metadata),
            ),
            reply,
        ),
        progress => return ProgressSettlement::Emit(progress),
    };
    if let Ok(mut opt) = reply_tx_mutex.lock() {
        if let Some(reply_tx) = opt.take() {
            let _ = reply_tx.send(res.map_err(|e| e.to_string()));
        }
    }
    ProgressSettlement::Settled
}

impl Agent {
    /// Enables durable partial-output checkpoints for live calls of `tool`.
    ///
    /// Row 4.7's harness half: while an invocation of the named tool is live, the
    /// run path republishes a bounded, complete, replaceable snapshot of the
    /// output it has streamed so far to `sink`, at `interval` cadence
    /// ([`BashCheckpointPublisher`] policy: first observation immediate, at most
    /// one publication per interval, identical snapshots suppressed, snapshot
    /// capped at [`BASH_CHECKPOINT_MAX_BYTES`] with the newest bytes kept on a
    /// code-point boundary). The name is matched exactly against the executing
    /// tool's name; an unmatched name costs checkpoints and nothing else.
    ///
    /// A checkpoint is auxiliary observation data: this external-sink variant
    /// does not itself persist it in the session or turn it into a result, and
    /// never claims the command finished (no `complete_<stream>=true`). Nothing
    /// is published before the call starts or after its result is committed, so a
    /// settled invocation is never republished as progress. A host that instead
    /// constructs its own checkpointing tool (for example
    /// [`CheckpointedBashTool`](crate::tools::bash::CheckpointedBashTool))
    /// should not also enable this, so no invocation is published twice.
    ///
    /// Disabled by default: an unopted host publishes no progress snapshots.
    /// Invocation intent and memo records are independent of this opt-in.
    #[cfg(any(unix, windows))]
    pub fn enable_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        sink: Arc<dyn PartialOutputCheckpointSink>,
        interval: Duration,
    ) {
        self.partial_output_checkpoints = Some(PartialOutputCheckpointConfig {
            tool: tool.into(),
            sink: Some(sink),
            interval,
            totals: Arc::new(PartialOutputCheckpointTotals::default()),
        });
    }

    /// Enables per-invocation checkpoints backed by this agent's private
    /// synced session log. No external sink or process-local fixture is used.
    /// Replay preserves the last bounded snapshot only as auxiliary data;
    /// paired-result persistence atomically fences and clears its live value.
    #[cfg(any(unix, windows))]
    pub fn enable_session_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        interval: Duration,
    ) {
        self.partial_output_checkpoints = Some(PartialOutputCheckpointConfig {
            tool: tool.into(),
            sink: None,
            interval,
            totals: Arc::new(PartialOutputCheckpointTotals::default()),
        });
    }

    /// Enables checkpoints for `tool` at Pi's bash cadence.
    #[cfg(any(unix, windows))]
    pub fn enable_default_partial_output_checkpoints(
        &mut self,
        tool: impl Into<String>,
        sink: Arc<dyn PartialOutputCheckpointSink>,
    ) {
        self.enable_partial_output_checkpoints(tool, sink, BASH_CHECKPOINT_INTERVAL);
    }

    /// Disables partial-output checkpoints and drops their counters.
    #[cfg(any(unix, windows))]
    pub fn disable_partial_output_checkpoints(&mut self) {
        self.partial_output_checkpoints = None;
    }

    /// Publication counters of the enabled checkpoint consumer, if any.
    #[cfg(any(unix, windows))]
    pub fn partial_output_checkpoint_stats(&self) -> Option<PartialOutputCheckpointStats> {
        self.partial_output_checkpoints
            .as_ref()
            .map(|config| config.totals.stats())
    }
}
