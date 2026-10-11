//! Attempt-scoped inference observations, not reconstructed GPU execution time.
//!
//! Client clocks are taken while polling the canonical stream. They include
//! buffering, consumer backpressure and local decoding. Server counters retain
//! their own generation-interval definition; neither clock is terminal paint.

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

use crate::stream::{ResponseStream, StreamEvent};

mod estimate;
mod reader;
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

pub(crate) fn reasoning_may_be_unreported(
    reasoning: &crate::ReasoningConfig,
    capability: Option<&crate::types::ReasoningCapability>,
) -> bool {
    let Some(capability) = capability else {
        // Missing discovered reasoning metadata is unknown, not evidence that
        // this endpoint cannot emit reasoning outside visible output.
        return true;
    };
    *reasoning != crate::ReasoningConfig::Off
        || capability.control == crate::types::ReasoningControl::AlwaysOn
        // OpenRouter's Chat Off selection intentionally leaves the provider
        // default in force, so hidden reasoning may still be generated.
        || capability.openai_chat_mode == crate::types::OpenAiChatReasoningMode::OpenRouter
}

/// Usage-calibrated robust estimate of visible-output decode cadence.
/// This excludes request/prefill and completion-tail time. Stable arrival
/// cadence cannot authenticate GPU execution or bound hidden provider buffering.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecodeEstimate {
    /// Median long-baseline streaming slope, calibrated to terminal visible usage.
    pub tokens_per_second: f64,
    /// Provider output usage minus its reported reasoning subset.
    pub reported_visible_tokens: u64,
    /// First-to-last answer/tool-argument observation, excluding completion tail.
    pub observed_ns: u64,
    /// Retained arrival bursts used by the bounded robust fit.
    pub samples: u32,
    /// Relative 10th-to-90th percentile pair-slope spread; not accuracy confidence.
    pub relative_dispersion: f64,
    /// Hidden/summary reasoning usage excluded from visible timing calibration.
    pub reasoning_tokens_excluded: u64,
}

/// Why client evidence cannot support a decode estimate. No E2E fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeEstimateUnavailable {
    /// No positive output-minus-reasoning usage counter.
    MissingVisibleUsage,
    /// Reasoning was observed but usage supplies no separable reasoning count.
    UnknownReasoningSplit,
    /// Fewer than 32 reported visible-output tokens.
    InsufficientOutput,
    /// Fewer than eight independently observed arrival bursts.
    InsufficientSamples,
    /// Arrival window is too short or fully buffered to resolve cadence.
    BufferedOutput,
    /// Pair slopes disagree too strongly to report a stable estimate.
    UnstableCadence,
    /// Deferred submission/retrieval is not live generation.
    DeferredOperation,
    /// Media output does not have a comparable text token basis.
    GeneratedMedia,
    /// The bounded reader hit local queue backpressure.
    LocalBackpressure,
    /// Provider usage includes structured tool-call framing that has no
    /// byte-progress counterpart in the client estimate.
    ToolCallFraming,
    /// A large first byte burst can dominate calibration while lying outside
    /// the fitted interval, and its token density is not independently known.
    UnrepresentativeInitialBurst,
    /// Reasoning may have been generated but neither streamed nor separated in
    /// terminal usage, so output-token calibration has no visible-only basis.
    UnreportedReasoningPossible,
}

/// Independent client and server measurements carried on a completed response.
/// Absent client timing is possible for direct codec/host assembly, never zero
/// inference latency. These observations are not durable usage authority.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InferenceMetrics {
    /// Canonical-stream client observations, when measured by the AI client.
    pub client: Option<ClientInferenceMetrics>,
    /// A matching authoritative terminal server generation pair, when available.
    pub server: Option<ServerGenerationMetrics>,
    /// Independent client-derived decode estimate; native server timing wins UI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_estimate: Option<DecodeEstimate>,
    /// Explicit reason an estimate could not be resolved, never a fake E2E rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decode_unavailable: Option<DecodeEstimateUnavailable>,
    /// Explicit reason for absence; omitted when a valid server pair is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_unavailable: Option<ServerTimingUnavailable>,
}

impl Default for InferenceMetrics {
    fn default() -> Self {
        Self {
            client: None,
            server: None,
            decode_estimate: None,
            decode_unavailable: None,
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
    decode: estimate::DecodeFit,
    reasoning_may_be_unreported: bool,
}

impl ClientTiming {
    #[cfg(test)]
    pub(crate) fn new(started: Instant, scope: ClientTimingScope) -> Self {
        Self::new_with_reasoning_context(started, scope, false)
    }

    fn new_with_reasoning_context(
        started: Instant,
        scope: ClientTimingScope,
        reasoning_may_be_unreported: bool,
    ) -> Self {
        Self {
            started,
            decode: estimate::DecodeFit::default(),
            reasoning_may_be_unreported,
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
                self.decode.observe(elapsed, delta.len());
            }
            StreamEvent::ReasoningDelta { delta, .. } if !delta.is_empty() => {
                self.metrics.first_reasoning_ns.get_or_insert(elapsed);
                self.metrics.reasoning_bytes += delta.len() as u64;
            }
            StreamEvent::ToolCallArgsDelta { delta, .. } if !delta.is_empty() => {
                self.metrics.first_tool_arguments_ns.get_or_insert(elapsed);
                self.metrics.tool_argument_bytes += delta.len() as u64;
                self.decode.observe(elapsed, delta.len());
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

    fn finish_response(mut self, response: &mut crate::Response, now: Instant) {
        let reasoning_observed = self.metrics.reasoning_bytes > 0 || response.message.content.iter().any(|part| matches!(part, crate::AssistantPart::Reasoning(reasoning) if reasoning.state.is_some() || reasoning.text.as_ref().is_some_and(|text| !text.is_empty())));
        let media_observed = self.metrics.media_parts > 0
            || response
                .message
                .content
                .iter()
                .any(|part| matches!(part, crate::AssistantPart::Media(_)));
        let inference = response
            .inference
            .get_or_insert_with(InferenceMetrics::default);
        // Normalize host advisory availability without rejecting valid content.
        if inference
            .server
            .as_ref()
            .is_some_and(|server| server.tokens_per_second().is_none())
        {
            inference.server = None;
            inference.server_unavailable = Some(ServerTimingUnavailable::Invalid);
        } else if inference.server.is_some() {
            inference.server_unavailable = None;
        } else if inference.server_unavailable.is_none() {
            inference.server_unavailable = Some(ServerTimingUnavailable::NotReported);
        }
        let tool_call_observed = response
            .message
            .content
            .iter()
            .any(|part| matches!(part, crate::AssistantPart::ToolCall(_)));
        let decode_result = if tool_call_observed {
            // Provider completion usage includes call framing (and may include
            // hidden argument tokens) that has no matching UTF-8 byte progress.
            // Do not spread those tokens over answer-byte time as if they did.
            Err(DecodeEstimateUnavailable::ToolCallFraming)
        } else if self.reasoning_may_be_unreported
            && !reasoning_observed
            && response.usage.output_tokens > 0
            && response.usage.reasoning_tokens == 0
        {
            // A false observed-reasoning flag only means no reasoning part or
            // delta reached this client. Requested or always-on reasoning may
            // still be hidden and included in aggregate output usage.
            Err(DecodeEstimateUnavailable::UnreportedReasoningPossible)
        } else {
            std::mem::take(&mut self.decode).finish(
                &response.usage,
                self.metrics.scope,
                media_observed,
                reasoning_observed,
            )
        };
        match decode_result {
            Ok(estimate) => {
                inference.decode_estimate = Some(estimate);
                inference.decode_unavailable = None;
            }
            Err(reason) => {
                inference.decode_estimate = None;
                inference.decode_unavailable = Some(reason);
            }
        }
        inference.client = Some(self.finish_at(response.usage.output_tokens, now));
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
    measured_stream_with_context(stream, started, scope, false)
}

pub(crate) fn measured_stream_with_context(
    stream: ResponseStream,
    started: Instant,
    scope: ClientTimingScope,
    reasoning_may_be_unreported: bool,
) -> ResponseStream {
    let stream_ready_ns = ns(started.elapsed());
    let measured: ResponseStream = Box::pin(async_stream::try_stream! {
        let mut stream = stream;
        let mut timing = ClientTiming::new_with_reasoning_context(
            started,
            scope,
            reasoning_may_be_unreported,
        );
        timing.metrics.stream_ready_ns = stream_ready_ns;
        while let Some(event) = stream.next().await {
            let mut event = event?;
            let now = Instant::now();
            timing.observe_at(&event, now);
            if let StreamEvent::Finished(response) = &mut event {
                timing.finish_response(response, now);
                yield event;
                // Drain the guard: no events may follow Finished.
                while let Some(event) = stream.next().await { yield event?; }
                break;
            }
            yield event;
        }
    });
    reader::independent_stream(measured, reader::finish_feedback, reader::event_weight)
}

pub(crate) fn buffered_steering_stream(
    stream: std::pin::Pin<
        Box<
            dyn futures_core::Stream<Item = Result<crate::steering::SteeringEvent, crate::AiError>>
                + Send,
        >,
    >,
) -> std::pin::Pin<
    Box<
        dyn futures_core::Stream<Item = Result<crate::steering::SteeringEvent, crate::AiError>>
            + Send,
    >,
> {
    use crate::steering::SteeringEvent;
    reader::independent_stream(
        stream,
        |event, blocked| match event {
            SteeringEvent::Response { event, .. } => reader::finish_feedback(event, blocked),
            _ => false,
        },
        |event| match event {
            SteeringEvent::Response { event, .. } => reader::event_weight(event),
            _ => 256,
        },
    )
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
    fn terminal_only_media_cannot_calibrate_text_decode_usage() {
        let origin = Instant::now();
        let mut timing = ClientTiming::new(origin, ClientTimingScope::Request);
        for n in 0..41 {
            timing.observe_at(
                &StreamEvent::TextDelta {
                    index: 0,
                    delta: "text".into(),
                },
                origin + Duration::from_millis(n * 50),
            );
        }
        let mut response = crate::Response {
            message: crate::AssistantMessage {
                model: crate::ModelId("test".into()),
                protocol: crate::Protocol::OpenAiChat,
                content: vec![crate::AssistantPart::Media(crate::Media::image_url(
                    "https://example.test/image.png".parse().unwrap(),
                    None,
                ))],
            },
            usage: crate::Usage {
                output_tokens: 205,
                ..Default::default()
            },
            stop_reason: crate::StopReason::EndTurn,
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        };
        timing.finish_response(&mut response, origin + Duration::from_secs(3));
        let inference = response.inference.unwrap();
        assert!(inference.decode_estimate.is_none());
        assert_eq!(
            inference.decode_unavailable,
            Some(DecodeEstimateUnavailable::GeneratedMedia)
        );
    }

    #[test]
    fn unknown_requested_or_always_on_reasoning_marks_unreported_usage_possible() {
        let capability = crate::test_fixtures::reasoning_capability();
        assert!(!reasoning_may_be_unreported(
            &crate::ReasoningConfig::Off,
            Some(&capability)
        ));
        assert!(reasoning_may_be_unreported(
            &crate::ReasoningConfig::Off,
            None
        ));
        assert!(reasoning_may_be_unreported(
            &crate::ReasoningConfig::On,
            Some(&capability)
        ));
        assert!(reasoning_may_be_unreported(
            &crate::ReasoningConfig::Effort(crate::ReasoningEffort::High),
            None
        ));
        let mut always_on = capability.clone();
        always_on.control = crate::types::ReasoningControl::AlwaysOn;
        assert!(reasoning_may_be_unreported(
            &crate::ReasoningConfig::Off,
            Some(&always_on)
        ));
        let mut openrouter = capability;
        openrouter.openai_chat_mode = crate::types::OpenAiChatReasoningMode::OpenRouter;
        assert!(reasoning_may_be_unreported(
            &crate::ReasoningConfig::Off,
            Some(&openrouter)
        ));
    }

    #[test]
    fn asserted_off_reasoning_context_allows_client_estimate() {
        let origin = Instant::now();
        let capability = crate::test_fixtures::reasoning_capability();
        let reasoning_may_be_unreported =
            reasoning_may_be_unreported(&crate::ReasoningConfig::Off, Some(&capability));
        assert!(!reasoning_may_be_unreported);
        let mut timing = ClientTiming::new_with_reasoning_context(
            origin,
            ClientTimingScope::Request,
            reasoning_may_be_unreported,
        );
        for n in 0..=40 {
            timing.observe_at(
                &StreamEvent::TextDelta {
                    index: 0,
                    delta: "text".into(),
                },
                origin + Duration::from_millis(n * 50),
            );
        }
        let mut response = crate::Response {
            message: crate::AssistantMessage {
                model: crate::ModelId("test".into()),
                protocol: crate::Protocol::OpenAiChat,
                content: Vec::new(),
            },
            usage: crate::Usage {
                output_tokens: 205,
                reasoning_tokens: 0,
                ..Default::default()
            },
            stop_reason: crate::StopReason::EndTurn,
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: None,
            diagnostics: Vec::new(),
        };
        timing.finish_response(&mut response, origin + Duration::from_secs(3));
        let inference = response.inference.unwrap();
        assert!(inference.decode_estimate.is_some());
        assert!(inference.decode_unavailable.is_none());
    }

    #[test]
    fn unknown_off_reasoning_context_suppresses_only_client_estimate() {
        let origin = Instant::now();
        let mut timing = ClientTiming::new_with_reasoning_context(
            origin,
            ClientTimingScope::Request,
            reasoning_may_be_unreported(&crate::ReasoningConfig::Off, None),
        );
        for n in 0..=40 {
            timing.observe_at(
                &StreamEvent::TextDelta {
                    index: 0,
                    delta: "text".into(),
                },
                origin + Duration::from_millis(n * 50),
            );
        }
        let mut response = crate::Response {
            message: crate::AssistantMessage {
                model: crate::ModelId("test".into()),
                protocol: crate::Protocol::OpenAiChat,
                content: Vec::new(),
            },
            usage: crate::Usage {
                output_tokens: 205,
                reasoning_tokens: 0,
                ..Default::default()
            },
            stop_reason: crate::StopReason::EndTurn,
            cost: None,
            response_id: None,
            responses_output: None,
            deferred: None,
            inference: Some(InferenceMetrics {
                server: Some(ServerGenerationMetrics {
                    source: ServerTimingSource::TimingsPredicted,
                    tokens: 300,
                    generation_ns: 1_500_000_000,
                    reported_unit: ReportedTimingUnit::Milliseconds,
                    prompt_ns: None,
                    queue_ns: None,
                    total_ns: None,
                }),
                ..Default::default()
            }),
            diagnostics: Vec::new(),
        };
        timing.finish_response(&mut response, origin + Duration::from_secs(3));
        let inference = response.inference.unwrap();
        assert!(inference.decode_estimate.is_none());
        assert_eq!(
            inference.decode_unavailable,
            Some(DecodeEstimateUnavailable::UnreportedReasoningPossible)
        );
        assert_eq!(
            inference.server.unwrap().tokens_per_second(),
            Some(200.0),
            "native generation timing remains independent"
        );
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
