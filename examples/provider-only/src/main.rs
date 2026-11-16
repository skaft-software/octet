//! Offline-by-default independent consumer. Every credential and response is synthetic.
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

use base64::Engine as _;
use octet_ai::provider_auth::codex::{local_subscription_claims, CredentialStore};
use octet_ai::providers::codex::{save_codex_model_cache, CodexCatalog, CodexDiscovery};
use octet_ai::providers::codex_context::CodexContextOverride;
use octet_ai::{AiClient, Model, ModelCatalog, ModelId, Request, StopReason};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const REFRESH_SENTINEL: &str = "synthetic-refresh-sentinel";

fn synthetic_access() -> String {
    let claims = json!({"https://api.openai.com/auth": {
        "chatgpt_account_id": "fixture-account", "chatgpt_plan_type": "plus"
    }});
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
    format!("h.{payload}.s")
}

struct Fixture {
    root: tempfile::TempDir,
    store: CredentialStore,
    model: Model,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("credentials/codex.json");
        // Fixture seeding only. Applications use shared login or an existing explicit store;
        // they never parse, copy, export or assemble real OAuth credentials.
        let credential = json!({"tokens": {
            "access_token": synthetic_access(), "refresh_token": REFRESH_SENTINEL,
            "account_id": "fixture-account"
        }, "expires_at": u64::MAX});
        octet_ai::secure_fs::write_private_atomic(&path, credential.to_string().as_bytes(), 4096)
            .unwrap();
        let store = CredentialStore::new(path);
        let inventory = CodexCatalog::from_inventory(&json!({"models": [{
            "slug": "gpt-6-luna", "input_modalities": ["text"],
            "context_window": 64000, "max_output_tokens": 4000,
            "supported_reasoning_levels": ["low", "high"], "default_reasoning_level": "low"
        }]}))
        .unwrap();
        save_codex_model_cache(
            &store,
            &CodexDiscovery {
                claims: local_subscription_claims(&store).unwrap().unwrap(),
                models: inventory.models().to_vec(),
            },
        )
        .unwrap();
        let snapshot = CodexCatalog::load_local(&store).unwrap().unwrap();
        let mut catalog = ModelCatalog::default();
        snapshot
            .bind_into(
                &mut catalog,
                store.clone(),
                CodexContextOverride::NONE,
                Duration::from_secs(1),
            )
            .unwrap();
        assert!(catalog.resolve(&ModelId("gpt-6-luna".into())).is_err());
        let model = catalog
            .resolve(&ModelId("codex/gpt-6-luna".into()))
            .unwrap();
        Self { root, store, model }
    }
    fn loopback_model(&self, server: &FakeProvider) -> Model {
        let mut model = self.model.clone();
        let endpoint = Arc::make_mut(&mut model.endpoint);
        // Redirect only this fixture binding to literal loopback; preserve the declared
        // auth, headers, Codex runtime/compression, capabilities and pricing unchanged.
        endpoint.base_url = server.url.parse().unwrap();
        endpoint.transport = octet_ai::EndpointTransport::Http;
        model
    }
}

struct Captured {
    headers: String,
    body: Value,
}
struct FakeProvider {
    url: String,
    count: Arc<AtomicUsize>,
    requests: tokio::sync::mpsc::Receiver<Captured>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl FakeProvider {
    async fn new(reply: Option<String>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let (sender, requests) = tokio::sync::mpsc::channel(4);
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut bytes = Vec::new();
                let (end, length) = loop {
                    let mut chunk = [0; 4096];
                    let size = stream.read(&mut chunk).await.unwrap();
                    if size == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&chunk[..size]);
                    assert!(bytes.len() <= 65536, "fixture request bound");
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")?
                                    .trim()
                                    .parse::<usize>()
                                    .ok()
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break (end, length);
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
                let body = &bytes[end + 4..end + 4 + length];
                let decoded = if headers.contains("content-encoding: zstd") {
                    use std::io::Read as _;
                    let mut result = Vec::new();
                    zstd::stream::read::Decoder::new(body)
                        .unwrap()
                        .take(65536)
                        .read_to_end(&mut result)
                        .unwrap();
                    result
                } else {
                    body.to_vec()
                };
                let body = serde_json::from_slice(&decoded).unwrap();
                if sender.send(Captured { headers, body }).await.is_err() {
                    return;
                }
                if let Some(reply) = &reply {
                    let _ = stream.write_all(reply.as_bytes()).await;
                } else {
                    std::future::pending::<()>().await;
                }
            }
        });
        Self {
            url,
            count,
            requests,
            task,
        }
    }
}

fn request() -> Request {
    Request {
        system: Some("Fixture only.".into()),
        messages: vec![octet_ai::Message::User(octet_ai::UserMessage {
            content: vec![octet_ai::UserPart::Text("Return one word.".into())],
        })],
        tools: vec![octet_ai::ToolDef {
            name: "fixture_noop".into(),
            description: "Never executed by the provider layer.".into(),
            parameters: json!({"type":"object","properties":{}}),
            constrained_sampling: None,
            async_execution: false,
        }],
        tool_choice: octet_ai::ToolChoice::Auto,
        max_output_tokens: Some(32),
        temperature: None,
        stop: vec![],
        reasoning: octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        responses: None,
        output_format: octet_ai::OutputFormat::Text,
        output_modalities: octet_ai::OutputModalities::Text,
        compatibility: octet_ai::CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: Some("fixture-session".into()),
    }
}
fn client() -> AiClient {
    AiClient::try_with_proxy_environment(Default::default())
        .unwrap()
        .with_initial_stream_timeout(Duration::from_secs(1))
        .with_stream_timeouts(Duration::from_secs(1), Duration::from_secs(2))
}
fn http(status: &str, headers: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
        body.len()
    )
}
fn response(reason: Option<&str>) -> String {
    let mut events = vec![
        json!({"type":"response.created","response":{"id":"resp_fixture"}}),
        json!({"type":"response.content_part.added","output_index":0,"content_index":0,"part":{"type":"output_text"}}),
        json!({"type":"response.output_text.delta","output_index":0,"content_index":0,"delta":"done"}),
        json!({"type":"response.output_text.done","output_index":0,"content_index":0}),
    ];
    if let Some(reason) = reason {
        let kind = if reason == "completed" {
            "response.completed"
        } else {
            "response.incomplete"
        };
        events.push(json!({"type":kind,"response":{"incomplete_details":{"reason":reason},"usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}}}));
    }
    let body: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    http("200 OK", "Content-Type: text/event-stream\r\n", &body)
}

async fn demonstration() {
    let fixture = Fixture::new();
    let mut server = FakeProvider::new(Some(response(Some("completed")))).await;
    let model = fixture.loopback_model(&server);
    assert_eq!(model.endpoint.id.0, "openai-codex");
    assert_eq!(
        model.endpoint.runtime.responses_profile,
        octet_ai::ResponsesRuntimeProfile::Codex
    );
    assert!(model.spec.pricing.is_none());
    assert!(!model.spec.capabilities.structured_output);
    assert_eq!(model.spec.limits.context_window, 64000);
    // Codex deliberately omits a wire output cap. Do not claim the request's 32-token
    // preference is an enforceable hard budget; only the fixture response is bounded.
    assert_eq!(octet_ai::effective_output_token_cap(&model, Some(32)), None);
    let result = client().complete(&model, request()).await.unwrap();
    assert_eq!(result.stop_reason, StopReason::EndTurn);
    assert_eq!(result.usage.output_tokens, 1);
    let captured = server.requests.recv().await.unwrap();
    assert!(captured.headers.starts_with("post /responses "));
    assert!(captured.headers.contains("authorization: bearer h."));
    assert!(captured
        .headers
        .contains("chatgpt-account-id: fixture-account"));
    assert!(captured.headers.contains("originator: octet"));
    assert_eq!(captured.body["model"], "gpt-6-luna");
    assert_eq!(captured.body["parallel_tool_calls"], true);
    assert_eq!(captured.body["reasoning"]["effort"], "low");
    assert!(captured.body.get("max_output_tokens").is_none());
    assert_eq!(server.count.load(Ordering::SeqCst), 1);
    let public = format!(
        "{:?} {:?} {}",
        fixture.store,
        fixture.model,
        serde_json::to_string(&*model.spec).unwrap()
    );
    assert!(!public.contains(REFRESH_SENTINEL));
    assert!(!public.contains("Bearer"));
    fixture.store.delete_async().await.unwrap();
    assert!(!fixture.store.is_signed_in().unwrap());
    assert!(!fixture
        .root
        .path()
        .join("credentials/codex-models.json")
        .exists());
}

#[tokio::main]
async fn main() {
    demonstration().await;
    println!("PASS: one loopback Responses inference through shared Codex storage and routing");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn independent_consumer_completes_one_shared_codex_request() {
        demonstration().await;
    }

    #[tokio::test]
    async fn expired_credentials_and_mismatched_cache_are_local_only() {
        let fixture = Fixture::new();
        let server = FakeProvider::new(None).await;
        let path = fixture.root.path().join("credentials/codex.json");
        let mut seed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        seed["expires_at"] = json!(0);
        octet_ai::secure_fs::write_private_atomic(&path, seed.to_string().as_bytes(), 4096)
            .unwrap();
        let before = std::fs::read(&path).unwrap();
        let snapshot = CodexCatalog::load_local(&fixture.store).unwrap().unwrap();
        assert_eq!(
            snapshot.source(),
            octet_ai::providers::codex::InventorySource::FreshCache
        );
        assert_eq!(snapshot.models().len(), 1);
        let mut local = ModelCatalog::default();
        snapshot
            .bind_into(
                &mut local,
                fixture.store.clone(),
                CodexContextOverride::NONE,
                Duration::from_secs(1),
            )
            .unwrap();
        assert!(local.resolve(&ModelId("codex/gpt-6.1-sol".into())).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let cache_path = fixture.root.path().join("credentials/codex-models.json");
        let mut cache: Value =
            serde_json::from_slice(&std::fs::read(&cache_path).unwrap()).unwrap();
        for (field, value) in [
            ("account_id", json!("other-account")),
            ("plan", json!("other-plan")),
            ("version", json!(0)),
        ] {
            let mut mismatched = cache.clone();
            mismatched[field] = value;
            octet_ai::secure_fs::write_private_atomic(
                &cache_path,
                mismatched.to_string().as_bytes(),
                65536,
            )
            .unwrap();
            let loaded = CodexCatalog::load_local(&fixture.store).unwrap().unwrap();
            assert_eq!(
                loaded.source(),
                octet_ai::providers::codex::InventorySource::ConservativeFallback
            );
            assert!(!loaded.is_account_inventory());
        }
        cache["models"] = json!([]);
        octet_ai::secure_fs::write_private_atomic(&cache_path, cache.to_string().as_bytes(), 65536)
            .unwrap();
        assert_eq!(
            CodexCatalog::load_local(&fixture.store)
                .unwrap()
                .unwrap()
                .source(),
            octet_ai::providers::codex::InventorySource::ConservativeFallback
        );
        assert_eq!(server.count.load(Ordering::SeqCst), 0);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn terminal_outcomes_and_ambiguous_disconnect_never_replay() {
        let fixture = Fixture::new();
        for (terminal, expected) in [
            (Some("max_output_tokens"), Some(StopReason::MaxTokens)),
            (Some("content_filter"), Some(StopReason::Refusal)),
            (None, None),
        ] {
            let server = FakeProvider::new(Some(response(terminal))).await;
            let result = client()
                .complete(&fixture.loopback_model(&server), request())
                .await;
            match expected {
                Some(reason) => assert_eq!(result.unwrap().stop_reason, reason),
                None => assert!(result.is_err()),
            }
            assert_eq!(server.count.load(Ordering::SeqCst), 1);
        }
        let server = FakeProvider::new(Some(
            "HTTP/1.1 200 OK\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n{".into(),
        ))
        .await;
        let result = client()
            .complete(&fixture.loopback_model(&server), request())
            .await;
        assert!(result.is_err());
        assert_eq!(server.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn timeout_and_cancellation_retain_possible_dispatch_without_replay() {
        let fixture = Fixture::new();
        let mut server = FakeProvider::new(None).await;
        let model = fixture.loopback_model(&server);
        let tracked = client().track_request_dispatch();
        let waiter_client = tracked.clone();
        let waiter = tokio::spawn(async move { waiter_client.complete(&model, request()).await });
        server.requests.recv().await.unwrap();
        waiter.abort();
        assert!(waiter.await.err().unwrap().is_cancelled());
        assert!(tracked.request_may_have_been_sent());
        assert_eq!(server.count.load(Ordering::SeqCst), 1);

        let server = FakeProvider::new(None).await;
        let mut model = fixture.loopback_model(&server);
        Arc::make_mut(&mut model.endpoint).timeout = Duration::from_millis(50);
        let tracked = client().track_request_dispatch();
        assert!(tracked.complete(&model, request()).await.is_err());
        assert!(tracked.request_may_have_been_sent());
        assert_eq!(server.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn malformed_destination_and_redirect_fail_without_secret_disclosure() {
        let fixture = Fixture::new();
        let server =
            FakeProvider::new(Some(http("400 Bad Request", "", &synthetic_access()))).await;
        let model = fixture.loopback_model(&server);
        let error = client().complete(&model, request()).await.unwrap_err();
        // A provider can echo the access credential it received, never the refresh token.
        assert!(!format!("{error:?} {error}").contains(&synthetic_access()));
        assert!(!format!("{error:?} {error}").contains(REFRESH_SENTINEL));
        assert_eq!(server.count.load(Ordering::SeqCst), 1);

        let destination = FakeProvider::new(Some(response(Some("completed")))).await;
        let redirect = FakeProvider::new(Some(http(
            "302 Found",
            &format!("Location: {}\r\n", destination.url),
            "",
        )))
        .await;
        assert!(client()
            .complete(&fixture.loopback_model(&redirect), request())
            .await
            .is_err());
        assert_eq!(redirect.count.load(Ordering::SeqCst), 1);
        assert_eq!(destination.count.load(Ordering::SeqCst), 0);
        let mut invalid = fixture.loopback_model(&destination);
        Arc::make_mut(&mut invalid.endpoint)
            .base_url
            .set_username(REFRESH_SENTINEL)
            .unwrap();
        let error = client().complete(&invalid, request()).await.unwrap_err();
        assert!(!format!("{error:?} {error}").contains(REFRESH_SENTINEL));
        assert_eq!(destination.count.load(Ordering::SeqCst), 0);
    }
}
