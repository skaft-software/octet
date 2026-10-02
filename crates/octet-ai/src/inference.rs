//! Attempt-scoped inference observations, not reconstructed GPU execution time.
//!
//! Client clocks are taken while polling the canonical stream. They include
//! buffering, consumer backpressure and local decoding. Server counters retain
//! their own generation-interval definition; neither clock is terminal paint.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::stream::{ResponseStream, StreamEvent};

pub(crate) mod wire;

/// Origin of the client clock. Response segments and deferred polls are not
/// interchangeable with initial request submission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientTimingScope {
    /// Entry into the one-shot client request, including preparation and opening.
    Request,
    /// Observation of a native-steering successor's response-created event.
    ResponseSegment,
    /// Entry into one deferred submission, excluding later parked time/polls.
    DeferredSubmit,
    /// Entry into one deferred poll, not the original parked generation.
    DeferredPoll,
}

/// A supported wire timing envelope, not an assertion about hardware identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerTimingSource {
    /// `timings.predicted_n` paired with `timings.predicted_ms`.
    TimingsPredicted,
    /// Same-frame completion usage paired with `time_info.completion_time`.
    TimeInfoCompletion,
    /// `usage.completion_tokens` paired with `usage.completion_time`.
    UsageCompletion,
    /// The same pair in a streaming `x_groq.usage` envelope.
    XGroqUsageCompletion,
}

/// Original duration unit. Normalizing to nanoseconds does not improve the
/// precision of the provider's original measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedTimingUnit {
    /// Provider-reported seconds, possibly fractional.
    Seconds,
    /// Provider-reported milliseconds, possibly fractional.
    Milliseconds,
}

/// Why server generation throughput cannot be reported for this response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerTimingUnavailable {
    /// No supported matching server count/duration was supplied.
    #[default]
    NotReported,
    /// A recognized envelope was malformed, zero, negative or unrepresentable.
    Invalid,
    /// Only a provisional stream snapshot was supplied.
    Provisional,
    /// Multiple recognized envelopes supplied different count/duration pairs.
    Conflicting,
}

/// Server-reported generation throughput under the envelope's native definition.
/// It is not normalized to post-first-token TPOT or pure accelerator time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerGenerationMetrics {
    /// Wire contract used to obtain the matching pair.
    pub source: ServerTimingSource,
    /// Counter reported by that contract, not retokenized visible text.
    /// Completion-usage sources can include billed, non-visible output.
    pub tokens: u64,
    /// Matching generation duration, normalized to nanoseconds.
    pub generation_ns: u64,
    /// Unit in which the provider reported its duration.
    pub reported_unit: ReportedTimingUnit,
    /// Provider prompt duration, when valid. Not inferred from other clocks.
    pub prompt_ns: Option<u64>,
    /// Provider queue duration, when valid.
    pub queue_ns: Option<u64>,
    /// Provider total duration, when valid. Its inclusion of queue time remains
    /// source-specific; it is never subtracted from client time as network RTT.
    pub total_ns: Option<u64>,
}

impl ServerGenerationMetrics {
    /// Native server generation rate. No first-token correction is invented.
    pub fn tokens_per_second(&self) -> Option<f64> {
        rate(self.tokens, self.generation_ns)
    }
}

/// Constant-space canonical-output observations for one physical client call.
/// Empty deltas, part starts/ends, signatures and usage frames are not output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInferenceMetrics {
    /// Request/segment/poll origin used for every offset below.
    pub scope: Option<ClientTimingScope>,
    /// Origin to availability of the guarded stream handle; not wire headers.
    pub stream_ready_ns: u64,
    /// First canonical event, which may be a synthetic Started event.
    pub first_event_ns: Option<u64>,
    /// Origin to guarded terminal response, before agent persistence/settlement.
    pub elapsed_ns: u64,
    /// First nonempty text, reasoning, tool-argument delta or completed media.
    pub first_output_ns: Option<u64>,
    /// First nonempty answer-text delta, independently of reasoning.
    pub first_text_ns: Option<u64>,
    /// First nonempty reasoning delta (possibly a summary, not hidden thinking).
    pub first_reasoning_ns: Option<u64>,
    /// First nonempty tool-argument delta, before agent tool admission.
    pub first_tool_arguments_ns: Option<u64>,
    /// First generated-media completion; not a token-generation timestamp.
    pub first_media_ns: Option<u64>,
    /// Last observed canonical output, excluding trailing usage/terminal frames.
    pub last_output_ns: Option<u64>,
    /// Number of nonempty canonical output events, never a token count.
    pub output_events: u64,
    /// UTF-8 bytes in observed answer-text deltas.
    pub text_bytes: u64,
    /// UTF-8 bytes in observed reasoning deltas.
    pub reasoning_bytes: u64,
    /// UTF-8 bytes in observed tool-argument deltas.
    pub tool_argument_bytes: u64,
    /// Number of completed generated media parts.
    pub media_parts: u64,
    /// Largest canonical-output gap, including stalls and backpressure.
    pub max_output_gap_ns: Option<u64>,
    /// Terminal provider-usage output counter; never divided by visible time.
    pub reported_output_tokens: u64,
}

impl ClientInferenceMetrics {
    /// Latest physical client's E2E usage throughput, not decode speed. Parked
    /// polls and successor segments deliberately do not manufacture this rate.
    pub fn end_to_end_tokens_per_second(&self) -> Option<f64> {
        (self.scope == Some(ClientTimingScope::Request))
            .then(|| rate(self.reported_output_tokens, self.elapsed_ns))
            .flatten()
    }

    /// First-to-last observed output interval. This is not server decode time.
    pub fn output_interval_ns(&self) -> Option<u64> {
        Some(self.last_output_ns?.saturating_sub(self.first_output_ns?))
    }

    /// Last observed output to terminal response, including framing/local work.
    pub fn completion_tail_ns(&self) -> Option<u64> {
        Some(self.elapsed_ns.saturating_sub(self.last_output_ns?))
    }
}

/// Independent client and server measurements carried on a completed response.
/// Absent client timing is possible for direct codec/host assembly, never zero
/// inference latency. These observations are not durable usage authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InferenceMetrics {
    /// Canonical-stream client observations, when measured by the AI client.
    pub client: Option<ClientInferenceMetrics>,
    /// A matching authoritative terminal server generation pair, when available.
    pub server: Option<ServerGenerationMetrics>,
    /// Explicit reason for absence; omitted when a valid server pair is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_unavailable: Option<ServerTimingUnavailable>,
}

impl Default for InferenceMetrics {
    fn default() -> Self {
        Self {
            client: None,
            server: None,
            server_unavailable: Some(ServerTimingUnavailable::NotReported),
        }
    }
}

pub(crate) fn rate(tokens: u64, nanos: u64) -> Option<f64> {
    (tokens > 0 && nanos > 0)
        .then(|| tokens as f64 / Duration::from_nanos(nanos).as_secs_f64())
        .filter(|value| value.is_finite() && *value > 0.0)
}

fn ns(duration: Duration) -> u64 {
    duration.as_nanos().try_into().unwrap_or(u64::MAX)
}

pub(crate) struct ClientTiming {
    started: Instant,
    metrics: ClientInferenceMetrics,
}

impl ClientTiming {
    pub(crate) fn new(started: Instant, scope: ClientTimingScope) -> Self {
        Self {
            started,
            metrics: ClientInferenceMetrics {
                scope: Some(scope),
                ..Default::default()
            },
        }
    }

    fn observe_at(&mut self, event: &StreamEvent, now: Instant) {
        let elapsed = ns(now.saturating_duration_since(self.started));
        self.metrics.first_event_ns.get_or_insert(elapsed);
        match event {
            StreamEvent::TextDelta { delta, .. } if !delta.is_empty() => {
                self.metrics.first_text_ns.get_or_insert(elapsed);
                self.metrics.text_bytes += delta.len() as u64;
            }
            StreamEvent::ReasoningDelta { delta, .. } if !delta.is_empty() => {
                self.metrics.first_reasoning_ns.get_or_insert(elapsed);
                self.metrics.reasoning_bytes += delta.len() as u64;
            }
            StreamEvent::ToolCallArgsDelta { delta, .. } if !delta.is_empty() => {
                self.metrics.first_tool_arguments_ns.get_or_insert(elapsed);
                self.metrics.tool_argument_bytes += delta.len() as u64;
            }
            StreamEvent::MediaCompleted { .. } => {
                self.metrics.first_media_ns.get_or_insert(elapsed);
                self.metrics.media_parts += 1;
            }
            _ => return,
        }
        self.metrics.first_output_ns.get_or_insert(elapsed);
        if let Some(last) = self.metrics.last_output_ns {
            let gap = elapsed.saturating_sub(last);
            self.metrics.max_output_gap_ns =
                Some(self.metrics.max_output_gap_ns.unwrap_or(0).max(gap));
        }
        self.metrics.last_output_ns = Some(elapsed);
        self.metrics.output_events += 1;
    }

    fn finish_at(mut self, tokens: u64, now: Instant) -> ClientInferenceMetrics {
        self.metrics.elapsed_ns = ns(now.saturating_duration_since(self.started));
        self.metrics.reported_output_tokens = tokens;
        self.metrics
    }
}

/// Wrap only a guarded stream. The terminal sample is frozen before handing it
/// to the caller; drop/error never publishes a successful or inherited sample.
pub(crate) fn measured_stream(
    stream: ResponseStream,
    started: Instant,
    scope: ClientTimingScope,
) -> ResponseStream {
    let stream_ready_ns = ns(started.elapsed());
    Box::pin(async_stream::try_stream! {
        let mut stream = stream;
        let mut timing = ClientTiming::new(started, scope);
        timing.metrics.stream_ready_ns = stream_ready_ns;
        while let Some(event) = stream.next().await {
            let mut event = event?;
            let now = Instant::now();
            timing.observe_at(&event, now);
            if let StreamEvent::Finished(response) = &mut event {
                let inference = response.inference.get_or_insert_with(|| InferenceMetrics {
                    server_unavailable: Some(ServerTimingUnavailable::NotReported),
                    ..Default::default()
                });
                // Host-owned transports are a system boundary. Normalize their
                // advisory availability state without rejecting valid content.
                if inference.server.as_ref().is_some_and(|server| server.tokens_per_second().is_none()) {
                    inference.server = None;
                    inference.server_unavailable = Some(ServerTimingUnavailable::Invalid);
                } else if inference.server.is_some() {
                    inference.server_unavailable = None;
                } else if inference.server_unavailable.is_none() {
                    inference.server_unavailable = Some(ServerTimingUnavailable::NotReported);
                }
                inference.client = Some(timing.finish_at(response.usage.output_tokens, now));
                yield event;
                // Drain the guard: no events may follow Finished.
                while let Some(event) = stream.next().await { yield event?; }
                break;
            }
            yield event;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_separate_reasoning_answer_tools_and_completion_tail() {
        let origin = Instant::now();
        let mut timing = ClientTiming::new(origin, ClientTimingScope::Request);
        let at = |ms| origin + Duration::from_millis(ms);
        timing.observe_at(
            &StreamEvent::TextDelta {
                index: 0,
                delta: String::new(),
            },
            at(10),
        );
        timing.observe_at(
            &StreamEvent::ReasoningDelta {
                index: 0,
                delta: "thinking".into(),
            },
            at(100),
        );
        timing.observe_at(
            &StreamEvent::TextDelta {
                index: 1,
                delta: "answer".into(),
            },
            at(300),
        );
        timing.observe_at(
            &StreamEvent::ToolCallArgsDelta {
                index: 2,
                delta: "{}".into(),
            },
            at(400),
        );
        let sample = timing.finish_at(200, at(1000));
        assert_eq!(sample.first_output_ns, Some(100_000_000));
        assert_eq!(sample.first_text_ns, Some(300_000_000));
        assert_eq!(sample.first_tool_arguments_ns, Some(400_000_000));
        assert_eq!(sample.output_interval_ns(), Some(300_000_000));
        assert_eq!(sample.completion_tail_ns(), Some(600_000_000));
        assert_eq!(sample.max_output_gap_ns, Some(200_000_000));
        assert_eq!(sample.output_events, 3);
        assert_eq!(sample.end_to_end_tokens_per_second(), Some(200.0));
    }

    #[test]
    fn single_chunk_is_not_a_decode_interval_and_polls_are_not_e2e_generation() {
        let origin = Instant::now();
        for scope in [
            ClientTimingScope::Request,
            ClientTimingScope::ResponseSegment,
            ClientTimingScope::DeferredSubmit,
            ClientTimingScope::DeferredPoll,
        ] {
            let mut timing = ClientTiming::new(origin, scope);
            timing.observe_at(
                &StreamEvent::TextDelta {
                    index: 0,
                    delta: "many tokens in one chunk".into(),
                },
                origin + Duration::from_secs(1),
            );
            let sample = timing.finish_at(100, origin + Duration::from_secs(2));
            assert_eq!(sample.output_interval_ns(), Some(0));
            assert_eq!(sample.max_output_gap_ns, None);
            assert_eq!(
                sample.end_to_end_tokens_per_second().is_some(),
                scope == ClientTimingScope::Request
            );
        }
        assert_eq!(rate(0, 100), None);
        assert_eq!(rate(1, 0), None);
    }
}
