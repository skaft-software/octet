//! Authoritative idle Pi model controls, without an App/extension rebuild.
use super::*;
use crate::app::App;
use octet_agent::extension_process::ExtensionModelControl;

pub(crate) fn apply_idle_model_control(
    app: &mut App,
    operation: &ExtensionModelControl,
) -> Result<Value, String> {
    // Dynamic declarations are acknowledged by the same native registry before
    // this request. Reconcile them before resolving the selected route.
    app.synchronize_extension_provider_catalog();
    let (model, level) = match operation {
        ExtensionModelControl::Model { provider, id } => {
            let model = resolve_model(&app.catalog, provider, id);
            let Some(model) = model else {
                return Ok(serde_json::json!({
                    "selected": false,
                    "context_usage": context_usage(app.agent.session(), &app.model),
                }));
            };
            let level = crate::app::level_from_reasoning(&app.reasoning, &app.model)
                .map_err(|error| error.to_string())?;
            (model, level)
        }
        ExtensionModelControl::Thinking { level } => (
            app.model.clone(),
            crate::config::ThinkingLevel::parse(level).map_err(|error| error.to_string())?,
        ),
    };
    let reasoning = crate::app::thinking_to_reasoning_with_subagents(
        level,
        &model,
        app.executable_extensions.has_agent_session_service(),
    )
    .map_err(|error| error.to_string())?;
    let portable = pi_thinking_level(&model, &reasoning).ok_or_else(|| {
        "unsupported_feature: selected reasoning has no Pi thinking level".to_owned()
    })?;
    let view = app
        .executable_extensions
        .provider_runtime
        .pi_model_view(&model)
        .ok_or_else(|| "unsupported_feature: selected model has no bounded Pi view".to_owned())?;
    app.agent
        .select_model_at_idle(
            model.clone(),
            reasoning.clone(),
            crate::app::reasoning_label(&reasoning),
        )
        .map_err(|error| error.to_string())?;
    app.model = model;
    app.reasoning = reasoning;
    // Config is the invocation's requested default, not a project configuration
    // write. The durable Config entry and live Agent own this selection.
    Ok(serde_json::json!({
        "selected": true,
        "model_view": view,
        "reasoning": portable,
        "context_usage": context_usage(app.agent.session(), &app.model),
    }))
}

/// Resolve only configured native routes; Pi names never create endpoints.
pub(crate) fn resolve_model(catalog: &ModelCatalog, provider: &str, id: &str) -> Option<Model> {
    catalog.models().find_map(|spec| {
        let model = catalog.resolve(&spec.id).ok()?;
        (model.spec.api_name == id
            && pi_provider_id(&model) == provider
            && model.endpoint.auth.is_configured())
        .then_some(model)
    })
}

/// Measured context facts for the committed selection, never a token estimate.
/// New content, compaction, a different route, or absent provider counters make
/// the current context unmeasured. In particular, a model switch must not reuse
/// the previous model's tokenizer measurement.
pub(crate) fn context_usage(session: &Session, model: &Model) -> Option<Value> {
    let tokens = observed_context_tokens(session, &model.endpoint.id, &model.spec.id)?;
    let window = model.spec.limits.context_window;
    if window == 0 || window > 9_007_199_254_740_991 {
        return None;
    }
    Some(serde_json::json!({
        "tokens": tokens,
        "contextWindow": window,
        "percent": tokens as f64 / window as f64 * 100.0,
    }))
}

fn observed_context_tokens(
    session: &Session,
    endpoint: &octet_ai::EndpointId,
    model: &octet_ai::ModelId,
) -> Option<u64> {
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id)?;
        match &entry.value {
            octet_agent::EntryValue::Message(octet_ai::Message::Assistant(_)) => {
                let record = session.usage_records().iter().rev().find(|record| {
                    matches!(&record.kind, octet_agent::UsageRecordKind::AssistantTurn { assistant } if assistant == id)
                        && record.endpoint.as_ref() == Some(endpoint)
                        && record.model.as_ref() == Some(model)
                })?;
                let usage = &record.usage;
                let tokens = if usage.total_tokens > 0 {
                    usage.total_tokens
                } else {
                    usage
                        .input_tokens
                        .checked_add(usage.cache_read_tokens)?
                        .checked_add(usage.cache_write_tokens)?
                        .checked_add(usage.output_tokens)?
                };
                // Native zero cannot distinguish absent counters from an
                // explicitly measured zero-token response.
                return (tokens > 0 && tokens <= 9_007_199_254_740_991).then_some(tokens);
            }
            octet_agent::EntryValue::Message(_)
            | octet_agent::EntryValue::Compaction { .. }
            | octet_agent::EntryValue::ResponsesCompaction { .. }
            | octet_agent::EntryValue::SkillActivated { .. }
            | octet_agent::EntryValue::SkillResourceRead { .. }
            | octet_agent::EntryValue::SkillDeactivated { .. } => return None,
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use octet_ai::{AssistantMessage, Message, Usage, UserMessage, UserPart};

    #[test]
    fn measured_context_is_branch_route_and_message_bound_not_an_estimate() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let endpoint = octet_ai::EndpointId("fixture".into());
        let model = octet_ai::ModelId("fixture-model".into());
        let observed = |session: &Session| observed_context_tokens(session, &endpoint, &model);
        assert_eq!(observed(&session), None);
        let assistant = session
            .append(octet_agent::EntryValue::Message(Message::Assistant(
                AssistantMessage {
                    content: vec![],
                    model: model.clone(),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        assert_eq!(observed(&session), None);
        session
            .record_assistant_usage(
                assistant.clone(),
                endpoint.clone(),
                model.clone(),
                Usage {
                    input_tokens: 80,
                    cache_read_tokens: 10,
                    cache_write_tokens: 20,
                    cache_write_1h_tokens: 5,
                    output_tokens: 4,
                    reasoning_tokens: 2,
                    total_tokens: 114,
                },
                None,
            )
            .unwrap();
        assert_eq!(observed(&session), Some(114));
        assert_eq!(
            observed_context_tokens(&session, &octet_ai::EndpointId("foreign".into()), &model),
            None
        );
        assert_eq!(
            observed_context_tokens(
                &session,
                &endpoint,
                &octet_ai::ModelId("other-model".into())
            ),
            None
        );
        session
            .append(octet_agent::EntryValue::Message(Message::User(
                UserMessage {
                    content: vec![UserPart::Text("unmeasured new content".into())],
                },
            )))
            .unwrap();
        assert_eq!(observed(&session), None);
        session.checkout(assistant.clone()).unwrap();
        assert_eq!(observed(&session), Some(114));
        session.compact("unmeasured summary", assistant).unwrap();
        assert_eq!(observed(&session), None);
    }

    #[test]
    fn host_refresh_preserves_native_palette_but_recomputes_context_measurement() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let sessions = SessionStore::new(directory.path(), directory.path());
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
            .unwrap();
        let mut extensions = ExecutableExtensions::default();
        let palette = serde_json::json!({"name":"actual-native-palette"});
        extensions.host_state.lock().unwrap().theme = Some(palette.clone());
        extensions.refresh_host_state(&session, &model, &ReasoningConfig::Off, &sessions);
        assert_eq!(
            extensions.host_state.lock().unwrap().context_usage,
            Some(Value::Null)
        );
        let assistant = session
            .append(octet_agent::EntryValue::Message(Message::Assistant(
                AssistantMessage {
                    content: vec![],
                    model: model.spec.id.clone(),
                    protocol: model.spec.protocol,
                },
            )))
            .unwrap();
        session
            .record_assistant_usage(
                assistant,
                model.endpoint.id.clone(),
                model.spec.id.clone(),
                Usage {
                    total_tokens: 120,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        extensions.refresh_host_state(&session, &model, &ReasoningConfig::Off, &sessions);
        let measured = extensions.host_state.lock().unwrap().clone();
        assert_eq!(measured.theme, Some(palette.clone()));
        assert_eq!(measured.context_usage, context_usage(&session, &model));
        assert_eq!(measured.context_usage.as_ref().unwrap()["tokens"], 120);
        session
            .append(octet_agent::EntryValue::Message(Message::User(
                UserMessage {
                    content: vec![UserPart::Text("not yet provider-tokenized".into())],
                },
            )))
            .unwrap();
        extensions.refresh_host_state(&session, &model, &ReasoningConfig::Off, &sessions);
        let changed = extensions.host_state.lock().unwrap().clone();
        assert_eq!(changed.theme, Some(palette));
        assert_eq!(
            changed.context_usage,
            Some(Value::Null),
            "never keep a stale usage snapshot"
        );
        let encoded = serde_json::to_value(changed).unwrap();
        assert!(encoded.as_object().unwrap().contains_key("context_usage"));
        assert!(encoded["context_usage"].is_null());
    }

    #[test]
    fn absent_zero_counters_remain_unknown_and_bucket_subsets_are_not_double_counted() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let endpoint = octet_ai::EndpointId("fixture".into());
        let model = octet_ai::ModelId("fixture-model".into());
        for (usage, expected) in [
            (Usage::default(), None),
            (
                Usage {
                    input_tokens: 10,
                    cache_read_tokens: 2,
                    cache_write_tokens: 3,
                    cache_write_1h_tokens: 1,
                    output_tokens: 5,
                    reasoning_tokens: 2,
                    total_tokens: 0,
                },
                Some(20),
            ),
        ] {
            let assistant = session
                .append(octet_agent::EntryValue::Message(Message::Assistant(
                    AssistantMessage {
                        content: vec![],
                        model: model.clone(),
                        protocol: octet_ai::Protocol::OpenAiChat,
                    },
                )))
                .unwrap();
            session
                .record_assistant_usage(assistant, endpoint.clone(), model.clone(), usage, None)
                .unwrap();
            assert_eq!(
                observed_context_tokens(&session, &endpoint, &model),
                expected
            );
        }
    }
}
