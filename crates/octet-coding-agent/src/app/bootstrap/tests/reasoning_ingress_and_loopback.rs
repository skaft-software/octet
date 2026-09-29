//! Reasoning ingress and loopback
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

#[test]
fn reasoning_ingress_distinguishes_absent_unknown_false_and_malformed() {
    use octet_ai::types::ReasoningMetadataSource as Source;
    assert_eq!(
        decode_reasoning_metadata(&serde_json::json!({}))
            .unwrap()
            .source,
        Source::Absent
    );
    assert_eq!(
        decode_reasoning_metadata(&serde_json::json!({"reasoning":null}))
            .unwrap()
            .source,
        Source::Unknown
    );
    let false_wins = serde_json::json!({"reasoning":false,"capabilities":{"reasoning":{"supported":true}},"supported_reasoning_levels":["high"]});
    assert_eq!(
        decode_reasoning_metadata(&false_wins).unwrap().supported,
        Some(false)
    );
    for values in [
        serde_json::json!(["low", "low"]),
        serde_json::json!(["low", "unknown"]),
        serde_json::json!([]),
    ] {
        assert!(decode_reasoning_metadata(
            &serde_json::json!({"supported_reasoning_levels":values})
        )
        .is_err());
    }
    assert!(decode_reasoning_metadata(&serde_json::json!({"capabilities":{"reasoning":{"supported":true,"values":["low"],"default":"high"}}})).is_err());
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "cerebras")
        .unwrap();
    assert!(discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiChat,
        "qwen-3.8-27b",
        &decode_reasoning_metadata(&false_wins).unwrap()
    )
    .is_none());
    assert!(sparse_route_reasoning(declaration, Protocol::OpenAiChat, "qwen3.8-27b").is_none());
    let oss = sparse_route_reasoning(declaration, Protocol::OpenAiChat, "gpt-oss-120b").unwrap();
    assert!(!oss.supports(&ReasoningConfig::Off));
    assert_eq!(
        oss.default_selection(),
        Some(ReasoningConfig::Effort(octet_ai::ReasoningEffort::Medium))
    );
}

#[test]
fn mistral_conversations_discovery_does_not_invent_reasoning_controls() {
    for provider in ["mistral", "openrouter"] {
        let declaration = BUILTIN_PROVIDER_DECLARATIONS
            .iter()
            .find(|d| d.id == provider)
            .unwrap();
        for value in [
            serde_json::json!({"reasoning": true}),
            serde_json::json!({"reasoning": {
                "supported": true, "values": ["low", "high"], "default": "high"
            }}),
        ] {
            assert!(discovered_reasoning_capability(
                declaration,
                Protocol::MistralConversations,
                "mistral-small-latest",
                &decode_reasoning_metadata(&value).unwrap(),
            )
            .is_none());
        }
    }
}

#[test]
fn exact_codex_cache_roundtrip_retains_choices_default_label_and_offline_holes() {
    let fixture = thinking_hotfix_fixture();
    let models = codex_models_from_response(&fixture["codex_exact"], None).unwrap();
    let cached: Vec<DiscoveredCodexModel> =
        serde_json::from_slice(&serde_json::to_vec(&models).unwrap()).unwrap();
    assert_eq!(models, cached);
    assert_eq!(cached[0].display_name.as_deref(), Some("GPT-6 Astra"));
    assert_eq!(cached[0].reasoning_options.values, ["low", "high", "ultra"]);
    assert_eq!(cached[0].reasoning_options.default.as_deref(), Some("low"));
    let offline = conservative_offline_codex_models(cached);
    assert_eq!(offline[0].reasoning_options.values, ["low", "high"]);
    assert_eq!(offline[0].max_effort, octet_ai::ReasoningEffort::High);
    assert!(!offline[0].responses_lite);
    assert_eq!(offline[0].agent_delegation, None);
}

#[tokio::test]
async fn sparse_cerebras_discovery_selection_stream_and_tool_continuation_loopback() {
    use octet_ai::{
        AssistantPart, Message, Request, ToolResult, ToolResultPart, UserMessage, UserPart,
    };
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(&thinking_hotfix_fixture()["cerebras_sparse"]),
        )
        .mount(&server)
        .await;
    // Synthetic separated reasoning + tool stream, not a production capture.
    let stream = concat!(
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{\"reasoning\":\"Inspect the result.\"}}]}\n\n",
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{}\"}}]}}]}\n\n",
        "data: {\"id\":\"synthetic\",\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n");
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream),
        )
        .mount(&server)
        .await;
    let url = format!("{}/models", server.uri());
    let body =
        tokio::task::spawn_blocking(move || get_models_json_blocking(&url, http::HeaderMap::new()))
            .await
            .unwrap()
            .unwrap();
    let discovered = api_models_from_response(&body).unwrap().remove(0);
    let declaration = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|d| d.id == "cerebras")
        .unwrap();
    let capability = discovered_reasoning_capability(
        declaration,
        Protocol::OpenAiChat,
        &discovered.id,
        &discovered.reasoning_metadata,
    )
    .unwrap();
    let mut catalog = ModelCatalog::default();
    let route = declaration.route_for_model(&discovered.id).unwrap();
    catalog
        .register_endpoint(Endpoint {
            id: EndpointId(route.endpoint_id.into()),
            base_url: url::Url::parse(&format!("{}/", server.uri())).unwrap(),
            auth: Auth::None,
            default_headers: Default::default(),
            transport: EndpointTransport::Http,
            runtime: Default::default(),
            timeout: Duration::from_secs(5),
        })
        .unwrap();
    let mut caps = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap()
        .spec
        .capabilities
        .clone();
    caps.reasoning = Some(capability);
    crate::providers::register_discovered_model(
        &mut catalog,
        declaration,
        &discovered.id,
        None,
        caps,
        ModelLimits {
            context_window: 32768,
            max_output_tokens: 4096,
        },
        None,
    )
    .unwrap();
    let model = catalog
        .resolve(&ModelId("cerebras/qwen-3.8-27b".into()))
        .unwrap();
    let selected = thinking_to_reasoning(crate::config::ThinkingLevel::On, &model).unwrap();
    assert_eq!(
        selected,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High)
    );
    let mut request = Request {
        system: Some("System contract".into()),
        messages: vec![Message::User(UserMessage {
            content: vec![UserPart::Text("Look up".into())],
        })],
        tools: vec![ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "lookup".into(),
            description: "lookup".into(),
            parameters: serde_json::json!({"type":"object"}),
        }],
        tool_choice: octet_ai::ToolChoice::Auto,
        max_output_tokens: Some(4096),
        temperature: None,
        stop: vec![],
        reasoning: selected,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: octet_ai::OutputFormat::Text,
        output_modalities: octet_ai::OutputModalities::Text,
        compatibility: octet_ai::CompatibilityMode::Strict,
        cache_retention: octet_ai::CacheRetention::None,
        session_id: None,
    };
    let client = AiClient::new();
    let response = client.complete(&model, request.clone()).await.unwrap();
    let call = response
        .message
        .content
        .iter()
        .find_map(|part| match part {
            AssistantPart::ToolCall(call) => Some(call.clone()),
            _ => None,
        })
        .unwrap();
    request.messages.push(Message::Assistant(response.message));
    request.messages.push(Message::User(UserMessage {
        content: vec![UserPart::ToolResult(ToolResult {
            tool_call_id: call.id,
            content: vec![ToolResultPart::Text("actual fixture result".into())],
            is_error: false,
            added_tool_names: None,
        })],
    }));
    request.reasoning = ReasoningConfig::Off;
    client.complete(&model, request).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let posts = requests
        .iter()
        .filter(|r| r.method.as_str() == "POST")
        .map(|r| r.body_json::<serde_json::Value>().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(posts.len(), 2);
    assert_eq!(posts[0]["model"], "qwen-3.8-27b");
    assert_eq!(posts[0]["messages"][0]["role"], "system");
    assert_eq!(posts[0]["reasoning_effort"], "high");
    assert_eq!(posts[1]["reasoning_effort"], "none");
    assert_eq!(posts[1]["messages"][2]["reasoning"], "Inspect the result.");
    assert!(posts[1]["messages"][2].get("reasoning_content").is_none());
    assert_eq!(posts[1]["messages"][3]["content"], "actual fixture result");
    for post in posts {
        for field in [
            "enable_thinking",
            "disable_reasoning",
            "preserve_thinking",
            "thinking_budget",
            "chat_template_kwargs",
            "thinking",
        ] {
            assert!(
                post.get(field).is_none(),
                "forbidden Cerebras field {field}"
            );
        }
    }
}

#[tokio::test]
async fn custom_binary_profiles_discover_cache_and_send_distinct_controls() {
    use octet_ai::{CompatibilityMode, Message, Request, UserMessage, UserPart};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    for fixture_name in ["local_binary", "local_template"] {
        let directory = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(&thinking_hotfix_fixture()[fixture_name]),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"id\":\"fixture\",\"choices\":[{\"delta\":{\"content\":\"answer\"}}]}\n\n",
                    "data: {\"id\":\"fixture\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")))
            .expect(2).mount(&server).await;
        let provider: crate::auth::custom::CustomProvider = serde_json::from_value(serde_json::json!({
            "base_url": format!("{}/v1/", server.uri()), "auth": {"kind": "none"}, "auto_discover": true
        })).unwrap();
        let store = crate::auth::custom::CredentialStore::new(directory.path().join("custom.json"));
        // Exercise the production registration/discovery/cache path, then a
        // fresh offline catalog. Only the loopback inventory may be requested.
        let (online, model) = tokio::task::spawn_blocking(move || {
            let mut online = ModelCatalog::default();
            register_custom_openai_provider(
                &mut online,
                &store,
                "fixture",
                &provider,
                false,
                false,
            )
            .unwrap();
            let mut offline = ModelCatalog::default();
            register_custom_openai_provider(
                &mut offline,
                &store,
                "fixture",
                &provider,
                false,
                true,
            )
            .unwrap();
            let id = ModelId("custom/fixture/qwen-3.8-27b".into());
            (online.resolve(&id).unwrap(), offline.resolve(&id).unwrap())
        })
        .await
        .unwrap();
        assert_eq!(
            online.spec.capabilities.reasoning,
            model.spec.capabilities.reasoning
        );
        assert_eq!(online.spec.display_name, model.spec.display_name);
        if fixture_name == "local_template" {
            assert_eq!(model.spec.display_name.as_deref(), Some("Lab model"));
        }
        let mut request = Request {
            system: Some("Fixture system instruction".into()),
            messages: vec![Message::User(UserMessage {
                content: vec![UserPart::Text("Fixture input".into())],
            })],
            tools: vec![],
            tool_choice: octet_ai::ToolChoice::Auto,
            max_output_tokens: Some(4096),
            temperature: None,
            stop: vec![],
            reasoning: thinking_to_reasoning(crate::config::ThinkingLevel::On, &model).unwrap(),
            reasoning_mode: ReasoningMode::Standard,
            responses: None,
            output_format: octet_ai::OutputFormat::Text,
            output_modalities: octet_ai::OutputModalities::Text,
            compatibility: CompatibilityMode::Strict,
            cache_retention: octet_ai::CacheRetention::None,
            session_id: None,
        };
        assert_eq!(request.reasoning, ReasoningConfig::On);
        let client = AiClient::new();
        client.complete(&model, request.clone()).await.unwrap();
        request.reasoning =
            thinking_to_reasoning(crate::config::ThinkingLevel::Off, &model).unwrap();
        assert_eq!(request.reasoning, ReasoningConfig::Off);
        client.complete(&model, request.clone()).await.unwrap();
        for compatibility in [CompatibilityMode::Strict, CompatibilityMode::Lossy] {
            request.compatibility = compatibility;
            request.reasoning = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
            assert!(matches!(
                client.complete(&model, request.clone()).await,
                Err(octet_ai::AiError::Unsupported(
                    octet_ai::UnsupportedError::Reasoning
                ))
            ));
        }
        let requests = server.received_requests().await.unwrap();
        let posts = requests
            .iter()
            .filter(|r| r.method.as_str() == "POST")
            .map(|r| r.body_json::<serde_json::Value>().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            posts.len(),
            2,
            "unsupported explicit selections must not reach HTTP"
        );
        for (index, post) in posts.iter().enumerate() {
            assert_eq!(post["model"], "qwen-3.8-27b");
            assert_eq!(post["messages"][0]["role"], "system");
            if fixture_name == "local_template" {
                assert_eq!(
                    post["chat_template_kwargs"],
                    serde_json::json!({"enable_thinking":index == 0,"preserve_thinking":true})
                );
                assert!(post.get("reasoning_effort").is_none());
            } else {
                assert!(post.get("chat_template_kwargs").is_none());
                if index == 0 {
                    assert!(post.get("reasoning_effort").is_none());
                } else {
                    assert_eq!(post["reasoning_effort"], "none");
                }
            }
            for field in [
                "enable_thinking",
                "thinking",
                "reasoning",
                "disable_reasoning",
                "preserve_thinking",
                "thinking_budget",
            ] {
                assert!(
                    post.get(field).is_none(),
                    "unexpected profile field {field}"
                );
            }
        }
        server.verify().await;
    }
}

#[tokio::test]
async fn custom_catalog_discovery_bounds_batches_and_isolates_endpoints_and_offline_cache() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = tempfile::tempdir().unwrap();
    let store = crate::auth::custom::CredentialStore::new(directory.path().join("custom.json"));
    let mut registry: crate::auth::custom::CustomRegistry =
        serde_json::from_value(serde_json::json!({"version": 1, "providers": {}})).unwrap();
    let (started_tx, mut started_rx) = tokio::sync::mpsc::channel(5);
    let mut releases = Vec::new();
    let mut servers = Vec::new();
    for provider in ["alpha", "beta", "delta", "epsilon", "gamma"] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        registry.providers.insert(
            provider.into(),
            serde_json::from_value(serde_json::json!({
                "base_url": format!("http://{address}/v1/"), "api_key": format!("{provider}-fixture-token"),
                "auto_discover": true,
                "models": [{"api_name": provider, "context_window": 32000}]
            }))
            .unwrap(),
        );
        let (release, wait) = tokio::sync::oneshot::channel();
        releases.push(release);
        let started = started_tx.clone();
        servers.push(tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut bytes = [0; 1024];
                let read = socket.read(&mut bytes).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&bytes[..read]);
                if request.windows(4).any(|part| part == b"\r\n\r\n") { break; }
                assert!(request.len() < 16 * 1024);
            }
            assert!(request.starts_with(b"GET /v1/models "));
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.contains(&format!("authorization: bearer {provider}-fixture-token\r\n")));
            started.send(provider).await.unwrap();
            let _ = wait.await;
            let body = serde_json::json!({"data": [
                {"id": provider, "context_window": 64000},
                {"id": format!("{provider}-discovered"), "context_window": 48000}
            ]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }));
    }
    registry.providers.insert(
        "z-broken".into(),
        serde_json::from_value(serde_json::json!({
            "base_url": "not a URL", "auth": {"kind": "none"}, "auto_discover": true
        }))
        .unwrap(),
    );
    store.save_registry(&registry).unwrap();
    let original = std::fs::read(store.path()).unwrap();
    let worker_store = store.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let mut catalog = ModelCatalog::default();
        register_custom_openai_endpoints_from_store(&mut catalog, &worker_store, false).unwrap();
        catalog
    });
    // Hold the first batch: all four jobs must overlap, but the fifth must
    // wait. Deadlines are deadlock guards, not startup performance thresholds.
    let overlap = tokio::time::timeout(Duration::from_secs(2), async {
        let mut admitted = Vec::new();
        for _ in 0..4 {
            admitted.push(started_rx.recv().await.unwrap());
        }
        admitted.sort_unstable();
        admitted
    })
    .await;
    let premature_fifth = tokio::time::timeout(Duration::from_millis(100), started_rx.recv()).await;
    for release in releases {
        let _ = release.send(());
    }
    let online = worker.await.unwrap();
    for server in servers {
        server.await.unwrap();
    }
    assert_eq!(overlap.unwrap(), ["alpha", "beta", "delta", "epsilon"]);
    assert!(
        premature_fifth.is_err(),
        "fifth registration escaped the batch bound"
    );
    assert_eq!(started_rx.recv().await, Some("gamma"));
    let mut offline = ModelCatalog::default();
    register_custom_openai_endpoints_from_store(&mut offline, &store, true).unwrap();
    for provider in ["alpha", "beta", "delta", "epsilon", "gamma"] {
        let id = ModelId(format!("custom/{provider}/{provider}"));
        let discovered = ModelId(format!("custom/{provider}/{provider}-discovered"));
        assert!(online.resolve(&discovered).is_ok());
        assert!(offline.resolve(&discovered).is_ok());
        for other in ["alpha", "beta", "delta", "epsilon", "gamma"] {
            if other != provider {
                let foreign = ModelId(format!("custom/{provider}/{other}-discovered"));
                assert!(online.resolve(&foreign).is_err());
                assert!(offline.resolve(&foreign).is_err());
            }
        }
        let online = online.resolve(&id).unwrap();
        let offline = offline.resolve(&id).unwrap();
        // Discovery enabled: the live 64k assertion wins over the 32k registry
        // pin online, and the offline run reuses the same normalized cache.
        assert_eq!(online.spec.limits.context_window, 64000);
        assert_eq!(
            online.spec.limits.context_window,
            offline.spec.limits.context_window
        );
        assert_eq!(online.endpoint.id, offline.endpoint.id);
        assert_eq!(
            online.endpoint.base_url.as_str(),
            registry.providers[provider].credential.base_url
        );
        assert_eq!(online.endpoint.base_url, offline.endpoint.base_url);
    }
    assert_eq!(std::fs::read(store.path()).unwrap(), original);
}

/// Matched startup-catalog measurement with scheduled loopback inventory delay.
/// This does not measure inference or terminal paint.
#[tokio::test]
#[ignore = "manual matched timing experiment"]

async fn custom_catalog_startup_benchmark() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(100))
                .set_body_json(serde_json::json!({"data": [{"id": "fixture"}]})),
        )
        .mount(&server)
        .await;
    for trial in 0..9 {
        for candidate in if trial % 2 == 0 {
            [false, true]
        } else {
            [true, false]
        } {
            let directory = tempfile::tempdir().unwrap();
            let store =
                crate::auth::custom::CredentialStore::new(directory.path().join("custom.json"));
            let registry: crate::auth::custom::CustomRegistry = serde_json::from_value(serde_json::json!({
                "version": 1, "providers": {
                    "alpha": {"base_url": format!("{}/v1/", server.uri()), "auth": {"kind": "none"}, "auto_discover": true},
                    "beta": {"base_url": format!("{}/v1/", server.uri()), "auth": {"kind": "none"}, "auto_discover": true}
                }
            })).unwrap();
            store.save_registry(&registry).unwrap();
            tokio::task::spawn_blocking(move || {
                for scenario in ["cold", "cached", "offline"] {
                    let start = std::time::Instant::now();
                    let mut catalog = ModelCatalog::default();
                    if candidate {
                        register_custom_openai_endpoints_from_store(&mut catalog, &store, scenario == "offline").unwrap();
                    } else {
                        // Original serial orchestration, same registration/cache code.
                        let registry = store.load_registry().unwrap().unwrap();
                        for (id, provider) in registry.providers {
                            let mut provider_catalog = ModelCatalog::default();
                            register_custom_openai_provider(&mut provider_catalog, &store, &id, &provider, false, scenario == "offline").unwrap();
                            merge_provider_catalog(&mut catalog, provider_catalog).unwrap();
                        }
                    }
                    let elapsed = start.elapsed().as_nanos();
                    for provider in ["alpha", "beta"] {
                        assert!(catalog.resolve(&ModelId(format!("custom/{provider}/fixture"))).is_ok());
                    }
                    println!("custom_catalog_startup scenario={scenario} trial={trial} candidate={candidate} elapsed_ns={elapsed}");
                }
            }).await.unwrap();
        }
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 36);
}
