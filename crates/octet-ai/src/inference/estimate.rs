//! Usage-calibrated robust streaming fit. No tokenizer, response text, or
//! provider payload is retained. Fit stability is NOT GPU accuracy confidence.

use super::{ClientTimingScope, DecodeEstimate, DecodeEstimateUnavailable};
use crate::Usage;

const MAX_SAMPLES: usize = 256;
const BURST_NS: u64 = 2_000_000;
const MIN_SPAN_NS: u64 = 250_000_000;
const MIN_TOKENS: u64 = 32;
const MIN_SAMPLES: usize = 8;
const INITIAL_BURST_MULTIPLE: u64 = 2;

#[derive(Clone, Copy)]
struct Point {
    ns: u64,
    bytes: u64,
}

#[derive(Default)]
pub(super) struct DecodeFit {
    points: Vec<Point>,
    bytes: u64,
    first: Option<Point>,
    last: Option<Point>,
    seen: u64,
    burst_start: u64,
    first_burst_bytes: u64,
}

impl DecodeFit {
    pub(super) fn observe(&mut self, ns: u64, bytes: usize) {
        self.bytes += bytes as u64;
        let point = Point {
            ns,
            bytes: self.bytes,
        };
        if self.last.is_some() && ns.saturating_sub(self.burst_start) < BURST_NS {
            // Multiple deltas decoded from one buffered arrival are one burst,
            // not independently timed tokens. Keep its cumulative mass.
            if let Some(last) = self.points.last_mut() {
                *last = point;
            }
            if self.seen == 1 {
                self.first = Some(point);
                self.first_burst_bytes = point.bytes;
            }
            self.last = Some(point);
            return;
        }
        self.burst_start = ns;
        if self.seen == 0 {
            self.first_burst_bytes = point.bytes;
        }
        self.first.get_or_insert(point);
        self.last = Some(point);
        self.seen += 1;
        if self.points.len() == MAX_SAMPLES {
            // Deterministic decimation bounds retention and pair-fitting work.
            // First and final observations are kept independently of decimation.
            let mut i = 0;
            self.points.retain(|_| {
                let keep = i % 2 == 0;
                i += 1;
                keep
            });
        }
        self.points.push(point);
    }

    pub(super) fn finish(
        self,
        usage: &Usage,
        scope: Option<ClientTimingScope>,
        media: bool,
        reasoning_observed: bool,
    ) -> Result<DecodeEstimate, DecodeEstimateUnavailable> {
        use DecodeEstimateUnavailable::*;
        if !matches!(
            scope,
            Some(ClientTimingScope::Request | ClientTimingScope::ResponseSegment)
        ) {
            return Err(DeferredOperation);
        }
        if media {
            return Err(GeneratedMedia);
        }
        if reasoning_observed && usage.reasoning_tokens == 0 {
            return Err(UnknownReasoningSplit);
        }
        // Never assign hidden thinking or a streamed reasoning summary's billing
        // to answer-text time. Calibrate only answer/tool-argument byte progress.
        let tokens = usage
            .output_tokens
            .checked_sub(usage.reasoning_tokens)
            .filter(|n| *n > 0)
            .ok_or(MissingVisibleUsage)?;
        if tokens < MIN_TOKENS {
            return Err(InsufficientOutput);
        }
        let first = self.first.ok_or(InsufficientSamples)?;
        let last = self.last.ok_or(InsufficientSamples)?;
        let span = last.ns.saturating_sub(first.ns);
        if self.seen < MIN_SAMPLES as u64 || self.points.len() < MIN_SAMPLES {
            return Err(InsufficientSamples);
        }
        if span < MIN_SPAN_NS {
            return Err(BufferedOutput);
        }
        let extent = last.bytes.saturating_sub(first.bytes);
        if extent == 0 {
            return Err(InsufficientOutput);
        }
        // The first burst contributes to terminal usage and total bytes but is
        // outside every fitted time baseline. If its byte mass is over twice
        // the mean later burst, an unobserved density shift can dominate the
        // global calibration while leaving all fitted slopes perfectly stable.
        // There is no token-level evidence to repair that mismatch, so do not
        // publish a precise-looking decode estimate for this schedule.
        let later_bursts = self.seen.saturating_sub(1);
        let later_bytes = self.bytes.saturating_sub(self.first_burst_bytes);
        if later_bursts > 0
            && u128::from(self.first_burst_bytes) * u128::from(later_bursts)
                > u128::from(later_bytes) * u128::from(INITIAL_BURST_MULTIPLE)
        {
            return Err(UnrepresentativeInitialBurst);
        }
        let calibration = tokens as f64 / self.bytes as f64;
        let mut slopes = Vec::with_capacity(self.points.len() * self.points.len() / 2);
        for (i, a) in self.points.iter().enumerate() {
            for b in &self.points[i + 1..] {
                let dx = b.bytes.saturating_sub(a.bytes);
                let dt = b.ns.saturating_sub(a.ns);
                // Long baselines suppress packet jitter and intra-burst spacing.
                // The intercept absorbs request/prefill/first-chunk delay, but
                // global usage calibration still includes first-burst bytes.
                if dx >= (extent / 4).max(1) && dt >= MIN_SPAN_NS / 4 {
                    slopes.push(dx as f64 * calibration * 1e9 / dt as f64);
                }
            }
        }
        if slopes.is_empty() {
            return Err(BufferedOutput);
        }
        slopes.sort_unstable_by(f64::total_cmp);
        let rate = slopes[slopes.len() / 2];
        let lower = slopes[slopes.len() / 10];
        let upper = slopes[slopes.len() * 9 / 10];
        let dispersion = (upper - lower) / rate;
        if !rate.is_finite() || rate <= 0.0 {
            return Err(BufferedOutput);
        }
        // A stable transport delay changes the intercept, not slope. Irregular
        // stalls can be network OR backend scheduling; do not silently remove
        // them, or claim that a trimmed fit identifies pure GPU-active time.
        if dispersion > 0.5 {
            return Err(UnstableCadence);
        }
        Ok(DecodeEstimate {
            tokens_per_second: rate,
            reported_visible_tokens: tokens,
            observed_ns: span,
            samples: self.points.len() as u32,
            relative_dispersion: dispersion,
            reasoning_tokens_excluded: usage.reasoning_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fit(first_batch: u64, delay: u64, jitter: bool) -> DecodeFit {
        let mut fit = DecodeFit::default();
        for n in 0..=40_u64 {
            let tokens = if n == 0 { first_batch } else { 5 };
            let noise = if jitter && n % 7 == 3 { 12_000_000 } else { 0 };
            fit.observe(delay + n * 50_000_000 + noise, (tokens * 4) as usize);
        }
        fit
    }
    fn usage(output: u64, reasoning: u64) -> Usage {
        Usage {
            output_tokens: output,
            reasoning_tokens: reasoning,
            ..Default::default()
        }
    }
    #[test]
    fn robust_fit_excludes_prefill_first_batch_and_final_tail() {
        for batch in [1, 5] {
            for delay in [0, 10_000_000_000] {
                let estimate = fit(batch, delay, false)
                    .finish(
                        &usage(batch + 200, 0),
                        Some(ClientTimingScope::Request),
                        false,
                        false,
                    )
                    .unwrap();
                assert!((estimate.tokens_per_second - 100.0).abs() < 1e-9);
                assert_eq!(estimate.observed_ns, 2_000_000_000);
            }
        }
    }
    #[test]
    fn first_burst_density_counterexamples_are_reproduced_and_suppressed() {
        // These schedules have independently specified token counts: 100
        // tokens in the initial burst, then 40 * 5 tokens at 50-ms intervals.
        // The post-first-burst truth is therefore 100 tokens/s in every case.
        // Reproduce the former whole-output-byte calibration: its fitted byte
        // slope is 600, 600 and 200 bytes/s (zero dispersion), yet the global
        // terminal-usage calibration yields 100, 138.46 and 42.86 tokens/s.
        for (first_burst_bytes, later_burst_bytes, formula_rate) in [
            (600_u64, 30_u64, 100.0_f64),
            (100, 30, 138.461_538_461_538_48),
            (1000, 10, 42.857_142_857_142_854),
        ] {
            let visible_tokens = 300_u64;
            let later_bursts = 40_u64;
            let interval_ns = 50_000_000_u64;
            let mut fit = DecodeFit::default();
            fit.observe(0, first_burst_bytes as usize);
            for burst in 1..=later_bursts {
                fit.observe(burst * interval_ns, later_burst_bytes as usize);
            }

            let first = fit.first.unwrap();
            let last = fit.last.unwrap();
            let byte_slope = last.bytes.saturating_sub(first.bytes) as f64 * 1e9
                / last.ns.saturating_sub(first.ns) as f64;
            let extent = last.bytes.saturating_sub(first.bytes);
            let mut fitted_byte_slopes = Vec::new();
            for (i, a) in fit.points.iter().enumerate() {
                for b in &fit.points[i + 1..] {
                    let dx = b.bytes.saturating_sub(a.bytes);
                    let dt = b.ns.saturating_sub(a.ns);
                    if dx >= (extent / 4).max(1) && dt >= MIN_SPAN_NS / 4 {
                        fitted_byte_slopes.push(dx as f64 * 1e9 / dt as f64);
                    }
                }
            }
            assert!(!fitted_byte_slopes.is_empty());
            let low = fitted_byte_slopes
                .iter()
                .copied()
                .fold(f64::INFINITY, f64::min);
            let high = fitted_byte_slopes
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            assert!((high - low).abs() < 1e-9, "{fitted_byte_slopes:?}");
            let reproduced_rate = visible_tokens as f64 / fit.bytes as f64 * byte_slope;
            assert!((reproduced_rate - formula_rate).abs() < 1e-9);
            assert!((5.0 * 1e9 / interval_ns as f64 - 100.0).abs() < 1e-9);

            assert_eq!(
                fit.finish(
                    &usage(visible_tokens, 0),
                    Some(ClientTimingScope::Request),
                    false,
                    false,
                )
                .unwrap_err(),
                DecodeEstimateUnavailable::UnrepresentativeInitialBurst,
                "first burst {first_burst_bytes} bytes, later bursts {later_burst_bytes} bytes"
            );
        }
    }

    #[test]
    fn burst_sized_first_chunk_retains_a_well_calibrated_estimate() {
        let estimate = fit(5, 0, false)
            .finish(
                &usage(205, 0),
                Some(ClientTimingScope::Request),
                false,
                false,
            )
            .unwrap();
        assert!((estimate.tokens_per_second - 100.0).abs() < 1e-9);
    }

    #[test]
    fn large_initial_byte_mass_is_unavailable_even_with_stable_fit() {
        assert_eq!(
            fit(100, 0, false)
                .finish(
                    &usage(300, 0),
                    Some(ClientTimingScope::Request),
                    false,
                    false,
                )
                .unwrap_err(),
            DecodeEstimateUnavailable::UnrepresentativeInitialBurst
        );
    }

    #[test]
    fn jitter_and_hidden_reasoning_do_not_inflate_answer_decode() {
        let estimate = fit(5, 90_000_000_000, true)
            .finish(
                &usage(10_205, 10_000),
                Some(ClientTimingScope::Request),
                false,
                false,
            )
            .unwrap();
        assert!((estimate.tokens_per_second - 100.0).abs() < 2.0);
        assert!(estimate.relative_dispersion < 0.1);
        assert_eq!(estimate.reasoning_tokens_excluded, 10_000);
    }
    #[test]
    fn burst_deliveries_preserve_mass_without_fabricating_token_times() {
        let mut fit = DecodeFit::default();
        for n in 0..20 {
            for _ in 0..5 {
                fit.observe(n * 50_000_000, 4);
            }
        }
        let estimate = fit
            .finish(
                &usage(100, 0),
                Some(ClientTimingScope::Request),
                false,
                false,
            )
            .unwrap();
        assert!((estimate.tokens_per_second - 100.0).abs() < 1e-9);
        assert_eq!(estimate.samples, 20);
    }
    #[test]
    fn absent_short_buffered_media_and_deferred_evidence_fail_closed() {
        for (usage, scope, media, reason) in [
            (
                usage(0, 0),
                ClientTimingScope::Request,
                false,
                DecodeEstimateUnavailable::MissingVisibleUsage,
            ),
            (
                usage(10, 0),
                ClientTimingScope::Request,
                false,
                DecodeEstimateUnavailable::InsufficientOutput,
            ),
            (
                usage(100, 0),
                ClientTimingScope::DeferredPoll,
                false,
                DecodeEstimateUnavailable::DeferredOperation,
            ),
            (
                usage(100, 0),
                ClientTimingScope::Request,
                true,
                DecodeEstimateUnavailable::GeneratedMedia,
            ),
        ] {
            assert_eq!(
                fit(5, 0, false)
                    .finish(&usage, Some(scope), media, false)
                    .unwrap_err(),
                reason
            );
        }
        assert_eq!(
            fit(5, 0, false)
                .finish(
                    &usage(205, 0),
                    Some(ClientTimingScope::Request),
                    false,
                    true
                )
                .unwrap_err(),
            DecodeEstimateUnavailable::UnknownReasoningSplit
        );
        let mut buffered = DecodeFit::default();
        for n in 0..20 {
            buffered.observe(n * 1000, 4);
        }
        assert!(buffered
            .finish(
                &usage(100, 0),
                Some(ClientTimingScope::Request),
                false,
                false
            )
            .is_err());
    }
    #[test]
    fn irregular_chunks_and_variable_byte_density_track_known_generation_cadence() {
        let mut fit = DecodeFit::default();
        let mut tokens = 0;
        let mut elapsed = 8_000_000_000;
        for n in 0..128 {
            let chunk_tokens = [1, 8, 2, 6, 3, 5, 9][n % 7];
            let bytes_per_token = [2, 3, 4, 5, 6][n % 5];
            tokens += chunk_tokens;
            elapsed += chunk_tokens * 10_000_000;
            fit.observe(elapsed, (chunk_tokens * bytes_per_token) as usize);
        }
        // Synthetic matching native count/time, not live-provider qualification.
        let native_rate = tokens as f64 * 1e9 / (elapsed - 8_000_000_000) as f64;
        let estimate = fit
            .finish(
                &usage(tokens, 0),
                Some(ClientTimingScope::Request),
                false,
                false,
            )
            .unwrap();
        assert!((estimate.tokens_per_second - native_rate).abs() < 2.0);
    }

    #[test]
    fn strongly_changing_arrival_cadence_is_not_a_stable_decode_estimate() {
        let mut fit = DecodeFit::default();
        let mut elapsed = 0;
        for n in 0..80 {
            elapsed += if n < 40 { 20_000_000 } else { 200_000_000 };
            fit.observe(elapsed, 20);
        }
        assert_eq!(
            fit.finish(
                &usage(400, 0),
                Some(ClientTimingScope::Request),
                false,
                false
            )
            .unwrap_err(),
            DecodeEstimateUnavailable::UnstableCadence
        );
    }

    #[test]
    fn retention_and_pair_work_are_bounded_for_long_streams() {
        let mut fit = DecodeFit::default();
        for n in 0..100_000 {
            fit.observe(n * 10_000_000, 4);
        }
        assert!(fit.points.len() <= MAX_SAMPLES);
        let estimate = fit
            .finish(
                &usage(100_000, 0),
                Some(ClientTimingScope::ResponseSegment),
                false,
                false,
            )
            .unwrap();
        assert!((estimate.tokens_per_second - 100.0).abs() < 1e-9);
    }
}
