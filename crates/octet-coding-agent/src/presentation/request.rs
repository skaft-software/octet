#![allow(missing_docs)]

//! Presentation observations and explicitly end-to-end request throughput.
//!
//! First-to-last output is a client observation, not active server decode.
//! Completed AI-client measurements take precedence over presentation clocks;
//! neither visible characters nor assembled tool calls estimate token timing.

use std::time::{Duration, Instant};

/// Monotonic boundaries captured for one physical provider request attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestTimingSample {
    submitted_at: Instant,
    stream_opened_at: Option<Instant>,
    first_provider_event_at: Option<Instant>,
    first_generated_at: Option<Instant>,
    last_generated_at: Option<Instant>,
    provider_finished_at: Option<Instant>,
    committed_at: Option<Instant>,
}

impl RequestTimingSample {
    #[cfg(test)]
    pub fn submitted_at(&self) -> Instant {
        self.submitted_at
    }

    #[cfg(test)]
    pub fn stream_opened_at(&self) -> Option<Instant> {
        self.stream_opened_at
    }

    #[cfg(test)]
    pub fn first_provider_event_at(&self) -> Option<Instant> {
        self.first_provider_event_at
    }

    #[cfg(test)]
    pub fn first_generated_at(&self) -> Option<Instant> {
        self.first_generated_at
    }

    #[cfg(test)]
    pub fn provider_finished_at(&self) -> Option<Instant> {
        self.provider_finished_at
    }

    #[cfg(test)]
    pub fn committed_at(&self) -> Option<Instant> {
        self.committed_at
    }

    /// First-to-last presentation-observed output interval. This does not
    /// recover hidden server generation or token-level timing.
    #[cfg(test)]
    pub fn generation_elapsed(&self) -> Option<Duration> {
        Some(
            self.last_generated_at?
                .saturating_duration_since(self.first_generated_at?),
        )
    }
}

/// Mutable timing state for one physical request attempt.
#[derive(Clone, Debug)]
pub struct RequestTiming {
    sample: RequestTimingSample,
}

impl RequestTiming {
    pub fn started_at(now: Instant) -> Self {
        Self {
            sample: RequestTimingSample {
                submitted_at: now,
                stream_opened_at: None,
                first_provider_event_at: None,
                first_generated_at: None,
                last_generated_at: None,
                provider_finished_at: None,
                committed_at: None,
            },
        }
    }

    pub fn new() -> Self {
        Self::started_at(Instant::now())
    }

    pub fn sample(&self) -> RequestTimingSample {
        self.sample
    }

    pub fn stream_opened_at(&mut self, now: Instant) {
        self.sample.stream_opened_at = Some(now);
    }

    pub fn provider_event_at(&mut self, now: Instant) {
        self.sample.first_provider_event_at.get_or_insert(now);
    }

    /// Record text, reasoning, tool-argument, or generated-media activity.
    pub fn generated_at(&mut self, now: Instant) {
        self.sample.first_generated_at.get_or_insert(now);
        self.sample.last_generated_at = Some(now);
    }

    pub fn provider_finished_at(&mut self, now: Instant) {
        self.sample.provider_finished_at = Some(now);
    }

    pub fn committed_at(&mut self, now: Instant) {
        self.sample.committed_at = Some(now);
    }

    /// Presentation-observed E2E sample, only for legacy event producers without
    /// frozen AI-client metrics. Never divide billing by a visible-output window.
    pub fn throughput(&self, output_tokens: u64) -> Option<RequestThroughput> {
        let elapsed = self
            .sample
            .provider_finished_at?
            .saturating_duration_since(self.sample.submitted_at);
        RequestThroughput::new(output_tokens, elapsed, self.sample)
    }
}

impl Default for RequestTiming {
    fn default() -> Self {
        Self::new()
    }
}

/// The latest completed request's end-to-end sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestThroughput {
    output_tokens: u64,
    request_elapsed: Duration,
    timing: RequestTimingSample,
}

impl RequestThroughput {
    fn new(
        output_tokens: u64,
        request_elapsed: Duration,
        timing: RequestTimingSample,
    ) -> Option<Self> {
        (output_tokens > 0 && !request_elapsed.is_zero()).then_some(Self {
            output_tokens,
            request_elapsed,
            timing,
        })
    }

    #[cfg(test)]
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }

    #[cfg(test)]
    pub fn request_elapsed(&self) -> Duration {
        self.request_elapsed
    }

    #[cfg(test)]
    pub fn timing(&self) -> RequestTimingSample {
        self.timing
    }
}

/// Latest-request tracker used by presentation owners. Starting a request
/// always discards the active attempt, so retry backoff and a prior request can
/// never enter the next sample. Finishing a zero-token/media-only request also
/// clears an older displayed rate.
#[derive(Clone, Debug, Default)]
pub struct RequestThroughputTracker {
    active: Option<RequestTiming>,
    latest: Option<RequestThroughput>,
    latest_timing: Option<RequestTiming>,
    client: Option<octet_ai::inference::ClientInferenceMetrics>,
}

impl RequestThroughputTracker {
    pub fn begin_at(&mut self, now: Instant) {
        // A new physical request owns a fresh displayed sample. This prevents
        // a prior request's rate from surviving while a zero-token or failed
        // replacement is in flight.
        self.active = Some(RequestTiming::started_at(now));
        self.latest = None;
        self.latest_timing = None;
        self.client = None;
    }

    pub fn active(&self) -> Option<&RequestTiming> {
        self.active.as_ref()
    }

    #[cfg(test)]
    pub fn latest(&self) -> Option<&RequestThroughput> {
        self.latest.as_ref()
    }

    #[cfg(test)]
    pub fn latest_timing(&self) -> Option<RequestTimingSample> {
        self.latest_timing.as_ref().map(RequestTiming::sample)
    }

    pub fn generated_at(&mut self, now: Instant) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.generated_at(now);
        true
    }

    pub fn stream_opened_at(&mut self, now: Instant) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.stream_opened_at(now);
        true
    }

    pub fn provider_event_at(&mut self, now: Instant) -> bool {
        let Some(active) = self.active.as_mut() else {
            return false;
        };
        active.provider_event_at(now);
        true
    }

    /// Abandon a failed or rejected provider attempt without carrying its
    /// partial output into the next request. A previously completed request is
    /// retained until a replacement request supplies a new rate.
    pub fn abort_attempt(&mut self) {
        self.active = None;
        self.client = None;
    }

    pub fn observe_client(
        &mut self,
        client: &octet_ai::inference::ClientInferenceMetrics,
        now: Instant,
    ) {
        if let Some(active) = self.active.as_mut() {
            active.provider_finished_at(now);
            self.client = Some(client.clone());
        }
    }

    pub fn finish_at(&mut self, output_tokens: u64, now: Instant) -> Option<RequestThroughput> {
        let mut active = self.active.take()?;
        active.sample.provider_finished_at.get_or_insert(now);
        let throughput = match self.client.take() {
            Some(client)
                if client.scope == Some(octet_ai::inference::ClientTimingScope::Request) =>
            {
                RequestThroughput::new(
                    client.reported_output_tokens,
                    Duration::from_nanos(client.elapsed_ns),
                    active.sample(),
                )
            }
            Some(_) => None,
            None => active.throughput(output_tokens),
        };
        self.latest_timing = Some(active);
        self.latest = throughput;
        throughput
    }

    /// Mark the latest completed request as committed. Provider finish and
    /// persistence are separate boundaries even when an event source reports
    /// them at the same instant.
    pub fn commit_at(&mut self, now: Instant) -> bool {
        let Some(timing) = self.latest_timing.as_mut() else {
            return false;
        };
        timing.committed_at(now);
        let timing = timing.sample();
        if let Some(throughput) = self.latest {
            self.latest = Some(RequestThroughput {
                output_tokens: throughput.output_tokens,
                request_elapsed: throughput.request_elapsed,
                timing,
            });
        }
        true
    }

    pub fn clear_latest(&mut self) {
        self.active = None;
        self.latest = None;
        self.latest_timing = None;
        self.client = None;
    }
}
