//! Opt-in, isolated Anthropic cache keepalive. Synthetic input and generated
//! output never enter the conversation; only status and provider usage persist.

use std::time::Duration;

use futures_util::StreamExt;
use octet_ai::{
    AiClient, AssistantPart, CacheRetention, EndpointTransport, Message, Model, Protocol, Request,
    StopReason, StreamEvent,
};

use crate::session::{now_unix_millis, CacheWarmRecord, CacheWarmState, Session, SessionError};

/// Explicit keepalive trigger. An idle host must call the API to schedule it;
/// no speculative streaming-prefix mode is implemented.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CacheWarmMode {
    /// Disable warming (default; performs no I/O).
    #[default]
    Off,
    /// Warm a settled, idle assistant prefix after the four-minute threshold.
    Idle,
}

/// Outcome of one requested keepalive, without provider output or secrets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheWarmOutcome {
    /// No network request was made (off, unsupported, too early or over budget).
    Skipped,
    /// Provider reported a complete response and usage was committed.
    Completed,
    /// The hard deadline expired; provider usage may be unknown.
    TimedOut,
    /// Request or stream failed; provider usage may be unknown.
    Failed,
}

/// Conservative per-session guardrails for additional billable requests.
#[derive(Clone, Copy, Debug)]
pub struct CacheWarmPolicy {
    /// Hard wall-clock deadline around opening and consuming the entire stream.
    pub deadline: Duration,
    /// Maximum conservative reserved microdollars for one attempt.
    pub max_request_microdollars: u64,
    /// Maximum conservative reserved microdollars for all warm attempts in a session.
    pub max_session_microdollars: u64,
    /// Maximum number of attempts across all branches of a session.
    pub max_attempts: u64,
}

impl Default for CacheWarmPolicy {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(30),
            max_request_microdollars: 50_000,
            max_session_microdollars: 200_000,
            max_attempts: 6,
        }
    }
}

/// Latest successful prefix is eligible only near the 5-minute short TTL.
const WARM_AFTER_MS: u64 = 4 * 60 * 1_000;
const MIN_PROMPT_TOKENS: u64 = 4_096;

/// Qualified direct route only; Anthropic-compatible gateways and custom
/// transports must not inherit authority from a protocol or model name.
pub fn is_direct_anthropic(model: &Model) -> bool {
    model.spec.protocol == Protocol::AnthropicMessages
        && model.endpoint.transport == EndpointTransport::Http
        && model.endpoint.id.0 == "anthropic"
        && model.endpoint.base_url.scheme() == "https"
        && model.endpoint.base_url.host_str() == Some("api.anthropic.com")
        && model.endpoint.base_url.port().is_none()
        && model.endpoint.base_url.path() == "/v1/"
}

/// Check eligibility without performing provider I/O. `estimated_input` must
/// include the full prefix, schemas, system and synthetic suffix. Return the
/// conservative reservation for the caller's existing session ceiling checks.
pub(crate) fn reservation(
    model: &Model,
    session: &Session,
    retention: CacheRetention,
    mode: CacheWarmMode,
    policy: CacheWarmPolicy,
    estimated_input: u64,
    now: u64,
    worst_case_cost: Option<u64>,
) -> Option<u64> {
    if mode != CacheWarmMode::Idle
        || retention != CacheRetention::Short
        || !is_direct_anthropic(model)
        || policy.deadline.is_zero()
        || session.has_uncertain_usage()
        || session.has_unpriced_usage()
        || estimated_input < MIN_PROMPT_TOKENS
        || estimated_input.saturating_add(1) > model.spec.limits.context_window
        || !matches!(
            session.context().ok()?.last(),
            Some(Message::Assistant(assistant))
                if matches!(assistant.content.last(), Some(AssistantPart::Text(text)) if !text.is_empty())
        )
    {
        return None;
    }
    let latest = session.latest_active_assistant_usage()?;
    if latest.endpoint.as_ref()? != &model.endpoint.id
        || latest.model.as_ref()? != &model.spec.id
        || !matches!(
            latest.stop_reason.as_ref(),
            Some(StopReason::EndTurn | StopReason::StopSequence)
        )
    {
        return None;
    }
    let last_activity = session
        .cache_warm_records()
        .iter()
        .rev()
        .find(|record| record.state == CacheWarmState::Completed)
        .map_or(latest.completed_at_unix_ms?, |record| {
            record
                .at_unix_ms
                .max(latest.completed_at_unix_ms.unwrap_or(0))
        });
    if now.saturating_sub(last_activity) < WARM_AFTER_MS {
        return None;
    }
    let attempts = session
        .cache_warm_records()
        .iter()
        .filter(|record| record.state == CacheWarmState::Started)
        .count() as u64;
    if attempts >= policy.max_attempts {
        return None;
    }
    let reserved = worst_case_cost?;
    let used = session
        .usage_records()
        .iter()
        .filter(|record| matches!(&record.kind, crate::session::UsageRecordKind::CacheWarm))
        .try_fold(0u64, |sum, record| {
            sum.checked_add(record.cost_microdollars?)
        })?;
    (reserved <= policy.max_request_microdollars
        && used.checked_add(reserved)? <= policy.max_session_microdollars)
        .then_some(reserved)
}

/// One fully bounded attempt. The caller must preflight reservation and supply
/// an owned, synthetic request; no retry or autonomous scheduling occurs here.
/// A started record precedes dispatch so a crash is usage-uncertain on replay.
pub(crate) async fn dispatch(
    client: &AiClient,
    model: &Model,
    session: &mut Session,
    request: Request,
    deadline: Duration,
) -> Result<CacheWarmOutcome, SessionError> {
    let attempt = session
        .cache_warm_records()
        .iter()
        .filter(|record| record.state == CacheWarmState::Started)
        .count() as u64
        + 1;
    let mut status = CacheWarmRecord {
        attempt,
        endpoint: model.endpoint.id.clone(),
        model: model.spec.id.clone(),
        state: CacheWarmState::Started,
        at_unix_ms: now_unix_millis(),
    };
    session.record_cache_warm_status(status.clone())?;
    let call = async {
        let mut stream = client.stream(model, request).await?;
        while let Some(event) = stream.next().await {
            if let StreamEvent::Finished(response) = event? {
                return Ok::<_, octet_ai::AiError>(response);
            }
        }
        Err(octet_ai::AiError::Canceled)
    };
    let result = tokio::time::timeout(deadline, call).await;
    let outcome = match result {
        Ok(Ok(response)) => {
            session.record_cache_warm_usage(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                response.usage,
                response.cost,
            )?;
            status.state = CacheWarmState::Completed;
            CacheWarmOutcome::Completed
        }
        other => {
            // An opening failure does not prove non-acceptance. In particular
            // deadline expiration can race response headers or a provider body.
            session.record_usage_uncertainty(
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                "cache_warm",
            )?;
            status.state = if other.is_err() {
                CacheWarmState::TimedOut
            } else {
                CacheWarmState::Failed
            };
            if status.state == CacheWarmState::TimedOut {
                CacheWarmOutcome::TimedOut
            } else {
                CacheWarmOutcome::Failed
            }
        }
    };
    status.at_unix_ms = now_unix_millis();
    session.record_cache_warm_status(status)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{EntryValue, UsageRecordKind};
    use octet_ai::{
        AiError, AssistantMessage, AssistantPart, CompatibilityMode, Cost, Diagnostic,
        HostStreamModel, HostStreamTransport, ModelCatalog, ModelId, OutputFormat,
        OutputModalities, ReasoningConfig, ReasoningMode, Response, ResponseStream, ToolChoice,
        Usage, UserMessage, UserPart,
    };
    use std::sync::Arc;

    fn model() -> Model {
        ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("claude-sonnet-4-6".into()))
            .unwrap()
    }

    fn settled_session(path: &std::path::Path, model: &Model) -> Session {
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("original user text".into())],
            })))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("original answer".into())],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })))
            .unwrap();
        session
            .record_assistant_usage_with_stop_reason(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 5000,
                    cache_write_tokens: 5000,
                    total_tokens: 10_000,
                    ..Usage::default()
                },
                Some(Cost::default()),
                StopReason::EndTurn,
            )
            .unwrap();
        session
    }

    fn request(session: &Session) -> Request {
        let mut messages = session.context().unwrap();
        messages.push(Message::User(UserMessage {
            content: vec![UserPart::Text("Reply with a single period.".into())],
        }));
        Request {
            system: Some("system".into()),
            messages,
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output_tokens: Some(1),
            temperature: None,
            stop: Vec::new(),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: OutputFormat::Text,
            output_modalities: OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: CacheRetention::Short,
            session_id: None,
        }
    }

    #[test]
    fn only_direct_idle_settled_priced_prefix_near_expiry_qualifies() {
        let model = model();
        assert!(is_direct_anthropic(&model));
        let mut gateway = model.clone();
        Arc::make_mut(&mut gateway.endpoint).base_url =
            url::Url::parse("https://gateway.example/v1/").unwrap();
        assert!(!is_direct_anthropic(&gateway));
        let dir = tempfile::tempdir().unwrap();
        let session = settled_session(&dir.path().join("session.jsonl"), &model);
        let now = session
            .latest_active_assistant_usage()
            .unwrap()
            .completed_at_unix_ms
            .unwrap();
        let eligible = |mode, retention, input, cost, at| {
            reservation(
                &model,
                &session,
                retention,
                mode,
                CacheWarmPolicy::default(),
                input,
                at,
                cost,
            )
        };
        assert_eq!(
            eligible(
                CacheWarmMode::Idle,
                CacheRetention::Short,
                9000,
                Some(100),
                now + WARM_AFTER_MS
            ),
            Some(100)
        );
        assert_eq!(
            eligible(
                CacheWarmMode::Off,
                CacheRetention::Short,
                9000,
                Some(100),
                now + WARM_AFTER_MS
            ),
            None
        );
        assert_eq!(
            eligible(
                CacheWarmMode::Idle,
                CacheRetention::Long,
                9000,
                Some(100),
                now + WARM_AFTER_MS
            ),
            None
        );
        assert_eq!(
            eligible(
                CacheWarmMode::Idle,
                CacheRetention::Short,
                100,
                Some(100),
                now + WARM_AFTER_MS
            ),
            None
        );
        assert_eq!(
            eligible(
                CacheWarmMode::Idle,
                CacheRetention::Short,
                9000,
                Some(50_001),
                now + WARM_AFTER_MS
            ),
            None
        );
        assert_eq!(
            eligible(
                CacheWarmMode::Idle,
                CacheRetention::Short,
                9000,
                Some(100),
                now + WARM_AFTER_MS - 1
            ),
            None
        );
    }

    #[test]
    fn reasoning_only_assistant_tail_cannot_start_a_billable_warm() {
        let model = model();
        let dir = tempfile::tempdir().unwrap();
        let mut session = settled_session(&dir.path().join("session.jsonl"), &model);
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Reasoning(octet_ai::ReasoningPart {
                    text: Some("thinking only".into()),
                    state: None,
                })],
                model: model.spec.id.clone(),
                protocol: model.spec.protocol,
            })))
            .unwrap();
        session
            .record_assistant_usage_with_stop_reason(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    input_tokens: 5000,
                    ..Usage::default()
                },
                Some(Cost::default()),
                StopReason::EndTurn,
            )
            .unwrap();
        let now = session
            .latest_active_assistant_usage()
            .unwrap()
            .completed_at_unix_ms
            .unwrap()
            + WARM_AFTER_MS;
        assert_eq!(
            reservation(
                &model,
                &session,
                CacheRetention::Short,
                CacheWarmMode::Idle,
                CacheWarmPolicy::default(),
                9000,
                now,
                Some(100),
            ),
            None
        );
    }

    struct FinishedTransport;
    #[async_trait::async_trait]
    impl HostStreamTransport for FinishedTransport {
        async fn stream(
            &self,
            model: HostStreamModel,
            request: Request,
            _: Vec<Diagnostic>,
        ) -> Result<ResponseStream, AiError> {
            assert!(matches!(request.messages.last(), Some(Message::User(_))));
            assert_eq!(request.max_output_tokens, Some(1));
            assert_eq!(request.tool_choice, ToolChoice::Auto);
            let response = Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text(".".into())],
                    model: model.id,
                    protocol: model.protocol,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    cache_read_tokens: 4990,
                    cache_write_tokens: 10,
                    output_tokens: 1,
                    total_tokens: 5011,
                    ..Usage::default()
                },
                cost: Some(Cost {
                    total: 7,
                    ..Cost::default()
                }),
                response_id: None,
                responses_output: None,
                deferred: None,
                diagnostics: Vec::new(),
            };
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::Started { response_id: None }),
                Ok(StreamEvent::Finished(response)),
            ])))
        }
    }

    #[tokio::test]
    async fn successful_warm_has_durable_usage_and_status_but_no_conversation_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let model = model();
        let mut session = settled_session(&path, &model);
        let context = session.context().unwrap();
        let head = session.head();
        let client = AiClient::new();
        client
            .register_host_stream_transport(model.endpoint.id.clone(), Arc::new(FinishedTransport));
        let warm_request = request(&session);
        let outcome = dispatch(
            &client,
            &model,
            &mut session,
            warm_request,
            Duration::from_secs(1),
        )
        .await;
        assert_eq!(outcome.unwrap(), CacheWarmOutcome::Completed);
        assert_eq!(session.head(), head);
        assert_eq!(
            serde_json::to_value(session.context().unwrap()).unwrap(),
            serde_json::to_value(&context).unwrap()
        );
        assert_eq!(session.cache_warm_records().len(), 2);
        assert_eq!(
            session.cache_warm_records()[1].state,
            CacheWarmState::Completed
        );
        assert!(matches!(
            &session.usage_records()[1].kind,
            UsageRecordKind::CacheWarm
        ));
        assert_eq!(session.total_cost_microdollars(), 7);
        assert_eq!(
            crate::cache::analyze_session_cache_stats(&session).assistant_turns,
            1
        );
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.context().unwrap()).unwrap(),
            serde_json::to_value(&context).unwrap()
        );
        assert!(!reopened.has_uncertain_usage());
        assert_eq!(reopened.cache_warm_records().len(), 2);
        assert_eq!(reopened.usage_records()[1].cost_microdollars, Some(7));
    }

    struct PendingTransport;
    #[async_trait::async_trait]
    impl HostStreamTransport for PendingTransport {
        async fn stream(
            &self,
            _: HostStreamModel,
            _: Request,
            _: Vec<Diagnostic>,
        ) -> Result<ResponseStream, AiError> {
            Ok(Box::pin(futures_util::stream::pending()))
        }
    }

    #[tokio::test]
    async fn deadline_records_uncertainty_separate_from_zero_usage() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("timed-out.jsonl");
        let model = model();
        let mut session = settled_session(&path, &model);
        let request = request(&session);
        let client = AiClient::new();
        client
            .register_host_stream_transport(model.endpoint.id.clone(), Arc::new(PendingTransport));
        let outcome = dispatch(
            &client,
            &model,
            &mut session,
            request,
            Duration::from_millis(5),
        )
        .await
        .unwrap();
        assert_eq!(outcome, CacheWarmOutcome::TimedOut);
        assert_eq!(
            session.cache_warm_records()[1].state,
            CacheWarmState::TimedOut
        );
        assert_eq!(session.usage_records().len(), 1);
        assert_eq!(session.usage_uncertainty_records().len(), 1);
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.cache_warm_records().len(), 2);
    }

    #[test]
    fn unfinished_attempt_is_uncertain_after_crash_and_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.jsonl");
        let model = model();
        let mut session = settled_session(&path, &model);
        let old_head = session.head().unwrap();
        session
            .record_cache_warm_status(CacheWarmRecord {
                attempt: 1,
                endpoint: model.endpoint.id.clone(),
                model: model.spec.id.clone(),
                state: CacheWarmState::Started,
                at_unix_ms: now_unix_millis(),
            })
            .unwrap();
        assert!(session.has_uncertain_usage());
        session.checkout_root().unwrap();
        session.checkout(old_head).unwrap();
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.cache_warm_records().len(), 1);
    }
}
