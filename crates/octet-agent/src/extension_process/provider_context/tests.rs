use super::super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
use octet_ai::{
    AssistantMessage, AssistantPart, CacheRetention, Cost, ModelCatalog, ModelId, ReasoningConfig,
    ReasoningMode, Request, Response,
};
use tempfile::TempDir;

#[test]
fn activation_refusal_mapping_is_finite_and_redacts_native_error_payloads() {
    assert_eq!(
        binding_activation_refusal(ExtensionRuntimeError::Protocol(
            "private session leaf activation already bound".into()
        )),
        "session_append_lease_busy"
    );
    for message in [
        "secret-token private-session /private/path",
        "session snapshot unavailable",
    ] {
        assert_eq!(
            binding_activation_refusal(ExtensionRuntimeError::Protocol(message.into())),
            "session_append_authority_unavailable"
        );
        assert_eq!(
            snapshot_activation_refusal(ExtensionRuntimeError::Protocol(message.into())),
            "session_snapshot_unavailable"
        );
        assert_eq!(
            binding_activation_refusal(ExtensionRuntimeError::Closed(message.into())),
            "session_append_process_retired"
        );
        assert_eq!(
            snapshot_activation_refusal(ExtensionRuntimeError::Closed(message.into())),
            "session_append_process_retired"
        );
    }
    assert_eq!(
        snapshot_activation_refusal(ExtensionRuntimeError::MessageTooLarge { limit: 1 }),
        "session_snapshot_too_large"
    );
    assert_eq!(
        snapshot_activation_refusal(ExtensionRuntimeError::Protocol(
            "session snapshot exceeds entry bound; no entries truncated".into()
        )),
        "session_snapshot_too_large"
    );
    assert_eq!(
        snapshot_activation_refusal(ExtensionRuntimeError::Protocol(
            "session snapshot owner changed".into()
        )),
        "session_append_authority_unavailable"
    );
    assert_eq!(
        snapshot_activation_refusal(ExtensionRuntimeError::Remote {
            code: -32002,
            message: "secret-token".into(),
            data: Some(serde_json::json!({"session":"private"}))
        }),
        "session_snapshot_unavailable"
    );
}

struct RefusedActivation(&'static str);
#[async_trait::async_trait]
impl ProviderContextHook for RefusedActivation {
    fn begin_session_wait(
        &self,
        _: &Session,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn ProviderContextSessionWait>>, String> {
        Err(self.0.into())
    }
    async fn project_context(
        &self,
        _: &Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        panic!("refused activation must not dispatch a hook")
    }
}

#[tokio::test]
async fn activation_refusal_categories_reach_agent_and_persistence_without_hook_secrets() {
    for code in [
        "session_append_authority_unavailable",
        "session_append_lease_busy",
        "session_append_process_retired",
        "session_snapshot_too_large",
        "session_snapshot_unavailable",
        "secret-token /private/path private-session",
        "session_snapshot_too_large: secret-token",
    ] {
        let temp = TempDir::new().unwrap();
        let mut host = ExtensionHost::new();
        host.provider_context_hook(RefusedActivation(code));
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let client = octet_ai::AiClient::new();
        let captures = Arc::new(StdMutex::new(Vec::new()));
        client.register_host_stream_transport(
            model.endpoint.id.clone(),
            Arc::new(Capture(captures.clone())),
        );
        let mut agent = Agent::new(AgentConfig {
            client,
            model,
            session: Session::create(temp.path().join("session.jsonl")).unwrap(),
            extensions: host,
            system: "canonical system".into(),
            sandbox: SandboxConfig::new(temp.path()),
            effect_broker: EffectBroker::default(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: Some("synthetic-affinity".into()),
        })
        .unwrap();
        let expected = if code.contains("secret-token") {
            "session append service refused activation"
        } else {
            code
        };
        let error = agent.complete("canonical task").await.unwrap_err();
        assert!(
            matches!(error, crate::AgentError::ProviderContextPreparation(reason) if reason == expected)
        );
        assert!(captures.lock().unwrap().is_empty());
        let persisted = std::fs::read_to_string(agent.session().path()).unwrap();
        // Failed preparation persists only the prompt and synthetic closure,
        // not the failure detail. Public attribution lives in the Agent error.
        assert_eq!(agent.session().entries().len(), 2);
        assert!(
            agent.session().entries()[1]
                .metadata
                .as_ref()
                .unwrap()
                .local_synthetic_assistant
        );
        for secret in ["secret-token", "/private/path", "private-session"] {
            assert!(!persisted.contains(secret));
            assert!(!error.to_string().contains(secret));
        }
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn activation_refusal_distinguishes_capability_and_retired_process_from_busy() {
    let temp = TempDir::new().unwrap();
    let process = process(&temp, false).await;
    let session = synthetic_history(&temp, 0);
    let context = preparation(&session);
    let connection = read_std_lock(&process.inner.connection).clone();
    write_std_lock(&connection.protocol)
        .features
        .remove(EXTENSION_FEATURE_SESSION_ENTRIES);
    assert_eq!(
        process
            .begin_session_wait(&session, &context)
            .err()
            .unwrap(),
        "session_append_authority_unavailable"
    );
    write_std_lock(&connection.protocol)
        .features
        .insert(EXTENSION_FEATURE_SESSION_ENTRIES.into());
    let execution = process.current_context_for_resource_owner(session.resource_owner_key());
    let binding = SessionLeafBinding {
        activation_epoch: 1,
        owner: execution.resource_owner.unwrap(),
        namespace: "context-process".into(),
        operation_id: "retired-activation".into(),
    };
    let (consumer, producer, grant) = SessionLeafConsumer::new(&session, binding).unwrap();
    let lease = process
        .bind_session_leaf(producer, consumer.revoker(), grant)
        .unwrap();
    assert!(process.begin_drain());
    assert_eq!(
        process
            .begin_session_wait(&session, &context)
            .err()
            .unwrap(),
        "session_append_process_retired"
    );
    assert!(matches!(
        lease.with_session_snapshot(&session),
        Err(ExtensionRuntimeError::Closed(_))
    ));
    assert!(process.shutdown().await);
    assert_eq!(
        process
            .begin_session_wait(&session, &context)
            .err()
            .unwrap(),
        "session_append_process_retired"
    );
    assert!(session.entries().is_empty());
    assert!(!temp.path().join("hook-calls").exists());
}

#[test]
fn pipeline_header_patch_preserves_repeated_values_and_explicit_deletions() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-remove", reqwest::header::HeaderValue::from_static("old"));
    headers.insert("x-stays", reqwest::header::HeaderValue::from_static("kept"));
    let reply: ProviderPipelineReply = serde_json::from_value(serde_json::json!({
        "provider_headers":{"x-remove":null,"x-multi":["first","second"],"x-new":"inserted"}
    }))
    .unwrap();
    apply_pipeline_headers(&mut headers, reply.provider_headers.unwrap()).unwrap();
    assert!(!headers.contains_key("x-remove"));
    assert_eq!(headers["x-stays"], "kept");
    assert_eq!(headers["x-new"], "inserted");
    assert_eq!(
        headers
            .get_all("x-multi")
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect::<Vec<_>>(),
        ["first", "second"]
    );
    assert!(headers["x-new"].is_sensitive());
    let projected = serde_json::to_value(pipeline_header_projection(&headers).unwrap()).unwrap();
    assert_eq!(projected["x-new"], "inserted");
    assert_eq!(projected["x-multi"], serde_json::json!(["first", "second"]));
}

#[test]
fn pipeline_rejects_ambiguous_or_unrepresentable_header_patches_privately() {
    for patch in [
        serde_json::json!({"X-Name":"a", "x-name":"b"}),
        serde_json::json!({"x-name":[]}),
        serde_json::json!({"x-name":"secret\r\ninjected"}),
        serde_json::json!({"invalid name":"private-value"}),
    ] {
        let mut headers = reqwest::header::HeaderMap::new();
        let patch = serde_json::from_value(patch).unwrap();
        let error = apply_pipeline_headers(&mut headers, patch).unwrap_err();
        assert!(!error.to_string().contains("secret"));
        assert!(!error.to_string().contains("private-value"));
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-binary",
        reqwest::header::HeaderValue::from_bytes(&[0x80]).unwrap(),
    );
    assert!(pipeline_header_projection(&headers).is_err());
    assert!(serde_json::from_value::<ProviderPipelineReply>(
        serde_json::json!({"provider_context":{}})
    )
    .is_err());
}

fn manifest() -> ExtensionManifest {
    ExtensionManifest::parse("name = \"context-process\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"extension.py\"\n[contributes]\nhooks = [\"provider_context\"]\n").unwrap()
}
#[test]
fn preparation_hook_requires_api_04_and_negotiated_existing_session_entries() {
    let declared = manifest();
    for version in ["0.1", "0.2", "0.3"] {
        let mut old = declared.clone();
        old.api_version = version.into();
        assert!(old.validate().is_err());
    }
    for selected in [false, true] {
        let mut features = API_0_2_REQUIRED_FEATURES.to_vec();
        if selected {
            features.push(EXTENSION_FEATURE_SESSION_ENTRIES);
        }
        let response: InitializeResponse = serde_json::from_value(serde_json::json!({
            "api_version":"0.4", "protocol":{"version":"0.4", "features": features,
                "limits":{"max_concurrent_requests":1}},
        }))
        .unwrap();
        assert_eq!(
            negotiate_contributions_with_host_services(
                &declared,
                response,
                DEFAULT_PENDING_REQUESTS,
                OfferedHostServices::default()
            )
            .is_ok(),
            selected
        );
    }
}

#[test]
fn context_projection_accepts_tool_omissions_but_refuses_host_owned_request_fields() {
    let projection = serde_json::json!({"messages": [], "system": null, "tools": []});
    let parsed: ProviderContextProjection = serde_json::from_value(projection.clone()).unwrap();
    assert!(parsed.tools.unwrap().is_empty());
    for field in ["max_output_tokens", "tool_choice", "session_id"] {
        let mut invalid = projection.clone();
        invalid[field] = serde_json::json!(1);
        assert!(serde_json::from_value::<ProviderContextProjection>(invalid).is_err());
    }
}

pub(super) struct Capture(pub(super) Arc<StdMutex<Vec<Request>>>);
#[async_trait::async_trait]
impl HostStreamTransport for Capture {
    async fn stream(
        &self,
        model: HostStreamModel,
        request: Request,
        _: Vec<Diagnostic>,
    ) -> Result<ResponseStream, AiError> {
        self.0.lock().unwrap().push(request);
        Ok(Box::pin(futures_util::stream::iter([
            Ok(StreamEvent::Started { response_id: None }),
            Ok(StreamEvent::TextStart { index: 0 }),
            Ok(StreamEvent::TextDelta {
                index: 0,
                delta: "answer".into(),
            }),
            Ok(StreamEvent::TextEnd { index: 0 }),
            Ok(StreamEvent::Finished(Response {
                message: AssistantMessage {
                    model: model.id,
                    protocol: model.protocol,
                    content: vec![AssistantPart::Text("answer".into())],
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: Some(Cost::default()),
                response_id: None,
                responses_output: None,
                deferred: None,
                inference: None,
                diagnostics: Vec::new(),
            })),
        ])))
    }
}

#[cfg(any(unix, windows))]
async fn process(temp: &TempDir, invalid: bool) -> ExtensionProcess {
    let source = r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path

def receive(): return json.loads(sys.stdin.readline())
def send(value): print(json.dumps(value, separators=(",",":")), flush=True)
init = receive()
protocol = init["params"]["protocol"]
assert init["params"]["contributes"]["hooks"] == ["provider_context"]
send({"jsonrpc":"2.0","id":init["id"],"result":{
    "api_version":"0.4","tools":[],"commands":[],
    "protocol":{"version":"0.4","features":protocol["required_features"]+["session_entries"],
        "limits":{"max_concurrent_requests":1}}}})
while True:
    request = receive()
    if request["method"] == "shutdown":
        send({"jsonrpc":"2.0","id":request["id"],"result":{}})
        break
    assert request["method"] == "hook/run", request
    p = request["params"]
    assert p["hook"] == "provider_context"
    grant = p["session_leaf"]
    assert grant["owner"] == p["context"]["resource_owner"]
    assert grant["owner"]["session_id"] == p["payload"]["preparation"]["resource_owner"]
    assert grant["expected_head"] == p["payload"]["preparation"]["head"]
    assert p["payload"]["request"]["system"] == "canonical system"
    host = p["context"]["host"]
    assert host["session_id"] == p["payload"]["preparation"]["session_id"]
    assert host["session_leaf_id"] == grant["expected_head"]
    assert Path(host["session_file"]).is_file()
    assert len(host["session_entries"]) == 1
    assert host["session_branch"] == host["session_entries"]
    assert host["session_entries"][0]["id"] == grant["expected_head"]
    calls = Path("hook-calls")
    calls.write_text(str(int(calls.read_text())+1 if calls.exists() else 1))
    send({"jsonrpc":"2.0","id":100,"method":"session/append_entry","params":{
        "parent_request_id":request["id"],"entry_type":"checkpoint",
        "data":{"revision":1,"text":"private\ncheckpoint"},
        "session_leaf":{"grant_id":grant["grant_id"],"activation_epoch":grant["activation_epoch"],
            "operation_id":grant["operation_id"]}}})
    reply = receive()
    assert reply["id"] == 100 and "result" in reply, reply
    result = reply["result"]
    assert result["entry_id"] == result["head"] == result["successor"]["expected_head"]
    assert result["successor"]["grant_id"] != grant["grant_id"]
    assert result["successor"]["owner"] == grant["owner"]
    projection = {"messages":[{"User":{"content":[{"Text":"effective leaf context"}]}}],"system":None}
    # Tool omissions are supported; output caps remain exclusively host-owned.
    if $INVALID: projection["max_output_tokens"] = 1
    send({"jsonrpc":"2.0","id":request["id"],"result":{"provider_context":projection}})
"#.replace("$INVALID", if invalid { "True" } else { "False" });
    write_executable_script(&temp.path().join("extension.py"), &source);
    ExtensionProcess::start(
        trusted_descriptor(temp.path(), manifest()),
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap()
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn real_run_process_hook_waits_for_original_writer_commit_once_and_refuses_bad_projection_or_cap(
) {
    for case in ["valid", "invalid", "finite"] {
        let temp = TempDir::new().unwrap();
        let process = process(&temp, case == "invalid").await;
        let mut host = ExtensionHost::new();
        process.register(&mut host);
        assert_eq!(host.provider_context_hooks.len(), 1);
        let model = ModelCatalog::builtin()
            .unwrap()
            .resolve(&ModelId("gpt-4o-mini".into()))
            .unwrap();
        let client = octet_ai::AiClient::new();
        let captures = Arc::new(StdMutex::new(Vec::new()));
        client.register_host_stream_transport(
            model.endpoint.id.clone(),
            Arc::new(Capture(captures.clone())),
        );
        let mut agent = Agent::new(AgentConfig {
            client,
            model,
            session: Session::create(temp.path().join("session.jsonl")).unwrap(),
            extensions: host,
            system: "canonical system".into(),
            sandbox: SandboxConfig::new(temp.path()),
            effect_broker: EffectBroker::default(),
            max_turns: Some(1),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: ReasoningMode::Standard,
            cache_retention: CacheRetention::Short,
            session_id: Some("host-affinity".into()),
        })
        .unwrap();
        if case == "finite" {
            agent.set_max_session_cost_microdollars(Some(1_000_000));
        }
        let result =
            tokio::time::timeout(Duration::from_secs(10), agent.complete("canonical task"))
                .await
                .unwrap();
        match case {
            "valid" => assert_eq!(result.unwrap().text, "answer"),
            "invalid" => assert!(
                matches!(
                    result,
                    Err(crate::AgentError::ProviderContextPreparation(_))
                ),
                "unexpected result for {case}: {result:?}"
            ),
            _ => assert!(matches!(
                result,
                Err(crate::AgentError::InputLimitUnavailable)
            )),
        }
        assert_eq!(
            std::fs::read_to_string(temp.path().join("hook-calls")).unwrap(),
            "1"
        );
        assert_eq!(captures.lock().unwrap().len(), usize::from(case == "valid"));
        if case == "valid" {
            let request = &captures.lock().unwrap()[0];
            assert_eq!(request.system, None);
            assert!(serde_json::to_string(&request.messages)
                .unwrap()
                .contains("effective leaf context"));
            assert!(!serde_json::to_string(&request.messages)
                .unwrap()
                .contains("private"));
        }
        let private = &agent.session().entries()[1];
        let checkpoint = agent
            .session()
            .extension_entry(&private.id, "context-process")
            .unwrap();
        assert_eq!(checkpoint.data["text"], "private\ncheckpoint");
        assert!(private
            .metadata
            .as_ref()
            .unwrap()
            .public_extension_metadata()
            .is_empty());
        let reopened = Session::open_read_only(agent.session().path()).unwrap();
        assert_eq!(
            reopened
                .extension_entry(&private.id, "context-process")
                .unwrap()
                .data,
            checkpoint.data
        );
        assert!(agent.session().usage_uncertainty_records().is_empty());
        assert!(process.shutdown().await);
    }
}

// Diagnostic reproductions, not large-history acceptance: these assert the
// current refusal and distinguish it from append authority and owner changes.
#[cfg(any(unix, windows))]
fn synthetic_history(temp: &TempDir, count: usize) -> Session {
    let mut session = Session::create(temp.path().join("history.jsonl")).unwrap();
    for index in 0..count {
        session
            .append_extension_entry(
                "context-process",
                Some(1),
                "synthetic-history",
                serde_json::json!({"index":index,"text":"x".repeat(8_000)}),
            )
            .unwrap();
    }
    session
}

#[cfg(any(unix, windows))]
fn preparation(session: &Session) -> ProviderContextProjectionContext {
    ProviderContextProjectionContext {
        resource_owner: session.resource_owner_key(),
        session_id: "synthetic-affinity".into(),
        head: session.head(),
        tool_generation: 0,
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn append_activation_repro_distinguishes_mirror_limit_from_private_snapshot_limit() {
    for count in [0, 40, 80] {
        let temp = TempDir::new().unwrap();
        let process = process(&temp, false).await;
        let session = synthetic_history(&temp, count);
        let before = std::fs::read(session.path()).unwrap();
        let head = session.head();
        let context = preparation(&session);
        let execution = process.current_context_for_resource_owner(session.resource_owner_key());
        let owner = execution.resource_owner.unwrap();
        let connection = read_std_lock(&process.inner.connection).clone();
        assert_eq!(connection.max_message_bytes(), 1024 * 1024);

        // The retained mirror is bounded at 512 KiB, whereas an invocation's
        // authoritative snapshot uses the negotiated 1 MiB wire bound.
        let mirror = process.set_host_state_with_session(
            ExtensionHostState {
                session_id: Some(context.session_id.clone()),
                ..Default::default()
            },
            &session,
        );
        assert_eq!(mirror.is_ok(), count == 0);
        if let Err(error) = mirror {
            assert!(error
                .to_string()
                .contains("session snapshot exceeds wire bound"));
        }
        let binding = SessionLeafBinding {
            activation_epoch: 1,
            owner: owner.clone(),
            namespace: "context-process".into(),
            operation_id: "synthetic-activation".into(),
        };
        let (consumer, producer, grant) =
            SessionLeafConsumer::new(&session, binding.clone()).unwrap();
        // Prove native append authority binds even for the oversized history.
        let lease = process
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .unwrap();
        let snapshot = lease.with_session_snapshot(&session);
        if count == 80 {
            let error = snapshot.err().expect("whole snapshot exceeds wire bound");
            assert!(error
                .to_string()
                .contains("session snapshot exceeds wire bound; no entries truncated"));
        } else {
            drop(snapshot.unwrap());
        }
        drop(consumer);

        let wait = process.begin_session_wait(&session, &context);
        if count == 80 {
            assert_eq!(wait.err().unwrap(), "session_snapshot_too_large");
        } else {
            assert!(wait.unwrap().is_some());
        }
        let (consumer, producer, grant) = SessionLeafConsumer::new(&session, binding).unwrap();
        drop(
            process
                .bind_session_leaf(producer, consumer.revoker(), grant)
                .unwrap(),
        );
        drop(consumer);
        assert_eq!(
            process
                .current_context_for_resource_owner(session.resource_owner_key())
                .resource_owner,
            Some(owner),
            "snapshot rejection must not change process or session identity"
        );
        assert_eq!(session.head(), head);
        assert_eq!(std::fs::read(session.path()).unwrap(), before);
        assert!(!temp.path().join("hook-calls").exists());
        assert!(process.is_running());
        assert!(process.shutdown().await);
    }
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn append_activation_repro_large_history_fails_before_hook_and_provider_without_data_loss() {
    let temp = TempDir::new().unwrap();
    let process = process(&temp, false).await;
    let session = synthetic_history(&temp, 80);
    let before = std::fs::read(session.path()).unwrap();
    let original_entries = serde_json::to_value(session.entries()).unwrap();
    let owner = session.resource_owner_key();
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let client = octet_ai::AiClient::new();
    let captures = Arc::new(StdMutex::new(Vec::new()));
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        extensions: host,
        system: "canonical system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("synthetic-affinity".into()),
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), agent.complete("canonical task"))
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(crate::AgentError::ProviderContextPreparation(
            "session_snapshot_too_large"
        ))
    ));
    assert!(captures.lock().unwrap().is_empty(), "no inference occurred");
    assert!(
        !temp.path().join("hook-calls").exists(),
        "hook was never dispatched"
    );
    assert_eq!(agent.session().resource_owner_key(), owner);
    assert!(std::fs::read(agent.session().path())
        .unwrap()
        .starts_with(&before));
    let reopened = Session::open_read_only(agent.session().path()).unwrap();
    assert_eq!(
        reopened.entries().len(),
        82,
        "prompt plus local failure marker"
    );
    assert!(matches!(
        &reopened.entries()[80].value,
        crate::session::EntryValue::Message(octet_ai::Message::User(user))
            if matches!(user.content.as_slice(), [octet_ai::UserPart::Text(text)] if text == "canonical task")
    ));
    assert!(
        reopened.entries()[81]
            .metadata
            .as_ref()
            .unwrap()
            .local_synthetic_assistant
    );
    assert_eq!(
        serde_json::to_value(&reopened.entries()[..80]).unwrap(),
        original_entries
    );
    assert!(agent.session().usage_uncertainty_records().is_empty());
    assert!(process.is_running());
    assert!(process.shutdown().await);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn append_activation_repro_keeps_foreign_owner_and_duplicate_lease_refusals() {
    let temp = TempDir::new().unwrap();
    let process = process(&temp, false).await;
    let session = synthetic_history(&temp, 0);
    let before = std::fs::read(session.path()).unwrap();
    let context = preparation(&session);
    let mut foreign = context.clone();
    foreign.resource_owner = "different-native-owner".into();
    assert_eq!(
        process
            .begin_session_wait(&session, &foreign)
            .err()
            .unwrap(),
        "session_append_authority_unavailable"
    );
    let lease = process
        .begin_session_wait(&session, &context)
        .unwrap()
        .unwrap();
    assert_eq!(
        process
            .begin_session_wait(&session, &context)
            .err()
            .unwrap(),
        "session_append_lease_busy",
        "busy activation remains distinct from authority and snapshot refusals"
    );
    drop(lease);
    drop(
        process
            .begin_session_wait(&session, &context)
            .unwrap()
            .unwrap(),
    );
    assert_eq!(std::fs::read(session.path()).unwrap(), before);
    assert!(!temp.path().join("hook-calls").exists());
    assert!(process.shutdown().await);
}

#[cfg(any(unix, windows))]
#[tokio::test]
async fn append_activation_repro_scoped_child_conflicts_with_parent_even_without_large_history() {
    let temp = TempDir::new().unwrap();
    let process = process(&temp, false).await;
    let parent = Session::create(temp.path().join("parent.jsonl")).unwrap();
    let parent_before = std::fs::read(parent.path()).unwrap();
    let mut host = ExtensionHost::new();
    host.tool(crate::tools::ReadTool);
    process.register(&mut host);
    // This is the same host-scope constructor used by build_child_agent; it
    // inherits the exact process-backed hook, not a new process/append mailbox.
    let (child_host, _) = host
        .scoped_tool_snapshot(&BTreeSet::from(["read".into()]))
        .unwrap();
    assert!(Arc::ptr_eq(
        &host.provider_context_hooks[0],
        &child_host.provider_context_hooks[0]
    ));
    let parent_wait = host.provider_context_hooks[0]
        .begin_session_wait(&parent, &preparation(&parent))
        .unwrap()
        .unwrap();
    let child = Session::create(temp.path().join("child.jsonl")).unwrap();
    assert_ne!(child.resource_owner_key(), parent.resource_owner_key());
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let client = octet_ai::AiClient::new();
    let captures = Arc::new(StdMutex::new(Vec::new()));
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session: child,
        extensions: child_host,
        system: "canonical system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("synthetic-child-affinity".into()),
    })
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), agent.complete("canonical task"))
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(crate::AgentError::ProviderContextPreparation(
            "session_append_lease_busy"
        ))
    ));
    assert!(captures.lock().unwrap().is_empty());
    assert!(!temp.path().join("hook-calls").exists());
    assert_eq!(std::fs::read(parent.path()).unwrap(), parent_before);
    assert!(std::fs::metadata(agent.session().path()).unwrap().len() < 512 * 1024);
    drop(parent_wait);
    // Dropping the parent's real lease makes the child's exact same history
    // bindable. No cap, ownership check or hook registration changed.
    drop(
        process
            .begin_session_wait(agent.session(), &preparation(agent.session()))
            .unwrap()
            .unwrap(),
    );
    assert!(process.shutdown().await);
}
