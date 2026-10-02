//! Tolerant, allocation-bounded numeric advisory DTOs. Malformed timing is not a
//! malformed assistant response. Unknown payloads are consumed with IgnoredAny;
//! no arbitrary provider strings or JSON trees cross the metrics boundary.

use std::fmt;
use std::time::Duration;

use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use super::{
    InferenceMetrics, ReportedTimingUnit, ServerGenerationMetrics, ServerTimingSource,
    ServerTimingUnavailable,
};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct MetricNumber {
    seen: bool,
    integer: Option<u64>,
    number: Option<f64>,
}

impl MetricNumber {
    fn seconds(self, scale: f64) -> Option<u64> {
        let value = self.number? * scale;
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        Duration::try_from_secs_f64(value)
            .ok()?
            .as_nanos()
            .try_into()
            .ok()
    }
    pub(crate) fn was_reported(self) -> bool {
        self.seen
    }
}

impl<'de> Deserialize<'de> for MetricNumber {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct NumberVisitor;
        impl<'de> Visitor<'de> for NumberVisitor {
            type Value = MetricNumber;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("advisory metric")
            }
            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(MetricNumber {
                    seen: true,
                    integer: Some(value),
                    number: Some(value as f64),
                })
            }
            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(MetricNumber {
                    seen: true,
                    integer: u64::try_from(value).ok(),
                    number: Some(value as f64),
                })
            }
            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(MetricNumber {
                    seen: true,
                    integer: None,
                    number: Some(value),
                })
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(MetricNumber {
                    seen: true,
                    ..Default::default()
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                self.visit_unit()
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                while a.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                self.visit_unit()
            }
        }
        de.deserialize_any(NumberVisitor)
    }
}

/// Only known timing keys are retained. The optional usage member handles the
/// `x_groq` envelope without retaining its request id or arbitrary extensions.
#[derive(Clone, Debug, Default)]
pub(crate) struct RawMetrics {
    present: bool,
    duplicate: bool,
    predicted_n: MetricNumber,
    predicted_ms: MetricNumber,
    prompt_ms: MetricNumber,
    completion_tokens: MetricNumber,
    pub(crate) completion_time: MetricNumber,
    prompt_time: MetricNumber,
    queue_time: MetricNumber,
    total_time: MetricNumber,
    pub(crate) usage: Option<Box<RawMetrics>>,
}

impl RawMetrics {
    pub(crate) fn mark_duplicate(&mut self) {
        self.duplicate = true;
        if let Some(usage) = &mut self.usage {
            usage.duplicate = true;
        }
    }
    pub(crate) fn set_usage_timing(&mut self, key: &str, value: MetricNumber) {
        let target = match key {
            "completion_time" => &mut self.completion_time,
            "prompt_time" => &mut self.prompt_time,
            "queue_time" => &mut self.queue_time,
            "total_time" => &mut self.total_time,
            _ => unreachable!("known usage timing key"),
        };
        self.duplicate |= target.seen;
        *target = value;
        self.present = self.completion_time.was_reported();
    }
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum MetricKey {
    PredictedN,
    PredictedMs,
    PromptMs,
    CompletionTokens,
    CompletionTime,
    PromptTime,
    QueueTime,
    TotalTime,
    Usage,
    #[serde(other)]
    Other,
}

impl<'de> Deserialize<'de> for RawMetrics {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct MetricsVisitor {
            nested: bool,
        }
        impl<'de> Visitor<'de> for MetricsVisitor {
            type Value = RawMetrics;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("advisory timing envelope")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                let mut result = RawMetrics {
                    present: true,
                    ..Default::default()
                };
                while let Some(key) = a.next_key::<MetricKey>()? {
                    let target = match key {
                        MetricKey::PredictedN => &mut result.predicted_n,
                        MetricKey::PredictedMs => &mut result.predicted_ms,
                        MetricKey::PromptMs => &mut result.prompt_ms,
                        MetricKey::CompletionTokens => &mut result.completion_tokens,
                        MetricKey::CompletionTime => &mut result.completion_time,
                        MetricKey::PromptTime => &mut result.prompt_time,
                        MetricKey::QueueTime => &mut result.queue_time,
                        MetricKey::TotalTime => &mut result.total_time,
                        MetricKey::Usage if !self.nested => {
                            use serde::de::DeserializeSeed;
                            struct UsageSeed;
                            impl<'de> DeserializeSeed<'de> for UsageSeed {
                                type Value = RawMetrics;
                                fn deserialize<D: Deserializer<'de>>(
                                    self,
                                    de: D,
                                ) -> Result<RawMetrics, D::Error> {
                                    de.deserialize_any(MetricsVisitor { nested: true })
                                }
                            }
                            result.duplicate |= result.usage.is_some();
                            result.usage = Some(Box::new(a.next_value_seed(UsageSeed)?));
                            continue;
                        }
                        _ => {
                            a.next_value::<IgnoredAny>()?;
                            continue;
                        }
                    };
                    result.duplicate |= target.seen;
                    *target = a.next_value()?;
                }
                if result.duplicate {
                    if let Some(usage) = &mut result.usage {
                        usage.mark_duplicate();
                    }
                }
                Ok(result)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Self::Value, E> {
                self.visit_unit()
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(RawMetrics {
                    present: true,
                    ..Default::default()
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
                while a.next_element::<IgnoredAny>()?.is_some() {}
                self.visit_unit()
            }
        }
        de.deserialize_any(MetricsVisitor { nested: false })
    }
}

#[derive(Debug, Default)]
pub(crate) struct ServerTiming {
    metrics: Option<ServerGenerationMetrics>,
    unavailable: ServerTimingUnavailable,
    conflicted: bool,
    invalid_terminal: bool,
}

impl ServerTiming {
    pub(crate) fn reject_identity(&mut self) {
        self.metrics = None;
        self.conflicted = true;
        self.unavailable = ServerTimingUnavailable::Conflicting;
    }
    pub(crate) fn observe(
        &mut self,
        source: ServerTimingSource,
        raw: &RawMetrics,
        same_frame_tokens: Option<u64>,
        terminal: bool,
    ) {
        if !raw.present {
            return;
        }
        if matches!(
            source,
            ServerTimingSource::UsageCompletion | ServerTimingSource::XGroqUsageCompletion
        ) && !raw.completion_time.was_reported()
        {
            return;
        }
        if self.conflicted || self.invalid_terminal {
            return;
        }
        if !terminal {
            if self.metrics.is_none() {
                self.unavailable = ServerTimingUnavailable::Provisional;
            }
            return;
        }
        let (tokens, generation_ns, unit, prompt_ns) = match source {
            ServerTimingSource::TimingsPredicted => (
                raw.predicted_n.integer,
                raw.predicted_ms.seconds(0.001),
                ReportedTimingUnit::Milliseconds,
                raw.prompt_ms.seconds(0.001),
            ),
            ServerTimingSource::TimeInfoCompletion | ServerTimingSource::UsageCompletion => (
                same_frame_tokens,
                raw.completion_time.seconds(1.0),
                ReportedTimingUnit::Seconds,
                raw.prompt_time.seconds(1.0),
            ),
            ServerTimingSource::XGroqUsageCompletion => (
                raw.completion_tokens.integer,
                raw.completion_time.seconds(1.0),
                ReportedTimingUnit::Seconds,
                raw.prompt_time.seconds(1.0),
            ),
        };
        let pair = tokens
            .zip(generation_ns)
            .filter(|(n, d)| *n > 0 && *d > 0 && !raw.duplicate);
        let Some((tokens, generation_ns)) = pair else {
            self.metrics = None;
            self.unavailable = ServerTimingUnavailable::Invalid;
            self.invalid_terminal = true;
            return;
        };
        if self
            .metrics
            .as_ref()
            .is_some_and(|old| old.tokens != tokens || old.generation_ns != generation_ns)
        {
            self.metrics = None;
            self.conflicted = true;
            self.unavailable = ServerTimingUnavailable::Conflicting;
            return;
        }
        self.metrics = Some(ServerGenerationMetrics {
            source,
            tokens,
            generation_ns,
            reported_unit: unit,
            prompt_ns,
            queue_ns: raw.queue_time.seconds(1.0),
            total_ns: raw.total_time.seconds(1.0),
        });
    }

    pub(crate) fn finish(&mut self) -> InferenceMetrics {
        let server = self.metrics.take();
        InferenceMetrics {
            client: None,
            server_unavailable: server.is_none().then_some(self.unavailable),
            server,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn all_supported_envelopes_have_matching_native_units() {
        for (source, raw, count) in [
            (
                ServerTimingSource::TimingsPredicted,
                json!({"predicted_n":100,"predicted_ms":500,"prompt_ms":100}),
                None,
            ),
            (
                ServerTimingSource::TimeInfoCompletion,
                json!({"completion_time":0.5,"queue_time":0.1}),
                Some(100),
            ),
            (
                ServerTimingSource::UsageCompletion,
                json!({"completion_tokens":100,"completion_time":0.5}),
                Some(100),
            ),
            (
                ServerTimingSource::XGroqUsageCompletion,
                json!({"completion_tokens":100,"completion_time":0.5}),
                None,
            ),
        ] {
            let mut state = ServerTiming::default();
            state.observe(source, &serde_json::from_value(raw).unwrap(), count, true);
            let sample = state.finish().server.unwrap();
            assert_eq!(sample.tokens_per_second(), Some(200.0));
            assert_eq!(sample.generation_ns, 500_000_000);
        }
    }

    #[test]
    fn advisory_invalid_data_never_becomes_a_rate_or_decode_failure() {
        for value in [
            json!(null),
            json!("secret"),
            json!([1, 2]),
            json!({}),
            json!({"predicted_n":100,"predicted_ms":0}),
            json!({"predicted_n":100,"predicted_ms":-1}),
            json!({"predicted_n":1.5,"predicted_ms":500}),
            json!({"predicted_n":100,"predicted_ms":"secret"}),
            json!({"predicted_n":100,"predicted_ms":1e100}),
        ] {
            let raw: RawMetrics = serde_json::from_value(value).unwrap();
            let mut state = ServerTiming::default();
            state.observe(ServerTimingSource::TimingsPredicted, &raw, None, true);
            let sample = state.finish();
            assert!(sample.server.is_none());
            assert_eq!(
                sample.server_unavailable,
                Some(ServerTimingUnavailable::Invalid)
            );
        }
    }

    #[test]
    fn duplicate_provisional_missing_and_conflicting_are_distinct() {
        let duplicate: RawMetrics =
            serde_json::from_str(r#"{"predicted_n":100,"predicted_ms":500,"predicted_ms":600}"#)
                .unwrap();
        let good: RawMetrics =
            serde_json::from_str(r#"{"predicted_n":100,"predicted_ms":500}"#).unwrap();
        let mut state = ServerTiming::default();
        assert_eq!(
            state.finish().server_unavailable,
            Some(ServerTimingUnavailable::NotReported)
        );
        state.observe(ServerTimingSource::TimingsPredicted, &good, None, false);
        assert_eq!(
            state.finish().server_unavailable,
            Some(ServerTimingUnavailable::Provisional)
        );
        state.observe(ServerTimingSource::TimingsPredicted, &duplicate, None, true);
        assert_eq!(
            state.finish().server_unavailable,
            Some(ServerTimingUnavailable::Invalid)
        );
        let mut state = ServerTiming::default();
        state.observe(ServerTimingSource::TimingsPredicted, &good, None, true);
        let different: RawMetrics = serde_json::from_str(r#"{"completion_time":1.0}"#).unwrap();
        state.observe(
            ServerTimingSource::TimeInfoCompletion,
            &different,
            Some(100),
            true,
        );
        assert_eq!(
            state.finish().server_unavailable,
            Some(ServerTimingUnavailable::Conflicting)
        );
    }
}
