use super::*;
use crate::protocol::harness;
use crate::{Message, UserMessage, UserPart};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(base: &str) -> Model {
    let mut model = harness::model(Protocol::GoogleGenerativeAi, None);
    Arc::make_mut(&mut model.spec).capabilities.reasoning = None;
    Arc::make_mut(&mut model.spec)
        .capabilities
        .output_modalities = crate::ModalitySet::none();
    let endpoint = Arc::make_mut(&mut model.endpoint);
    endpoint.base_url = url::Url::parse(base).unwrap();
    endpoint.id.0 = "gemini".into();
    model
}
fn request(texts: &[&str]) -> Request {
    let mut request = crate::test_fixtures::base_request();
    request.system = Some("stable instructions".into());
    request.messages = texts
        .iter()
        .map(|text| {
            Message::User(UserMessage {
                content: vec![UserPart::Text((*text).into())],
            })
        })
        .collect();
    request
}
async fn create_mock(server: &MockServer, name: &str) {
    Mock::given(method("POST"))
        .and(path("/v1beta/cachedContents"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name":name})))
        .mount(server)
        .await;
}
fn session(server: &MockServer, max_entries: usize, max_operations: usize) -> GoogleCacheSession {
    GoogleCacheSession::local(
        model(&format!("{}/v1beta/", server.uri())),
        GoogleCacheLimits {
            max_entries,
            max_operations,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn construction_is_local_validated_and_direct_route_only() {
    let good = model("https://generativelanguage.googleapis.com/v1beta/");
    GoogleCacheSession::new(good.clone(), Default::default())
        .unwrap_or_else(|error| panic!("{error:?}"));
    for base in [
        "https://gateway.example/v1beta/",
        "https://generativelanguage.googleapis.com/v1/",
        "https://generativelanguage.googleapis.com/v1beta/?key=secret",
    ] {
        let mut bad = good.clone();
        Arc::make_mut(&mut bad.endpoint).base_url = url::Url::parse(base).unwrap();
        assert!(GoogleCacheSession::new(bad, Default::default()).is_err());
    }
    for limits in [
        GoogleCacheLimits {
            max_entries: 0,
            ..Default::default()
        },
        GoogleCacheLimits {
            max_entries: 9,
            ..Default::default()
        },
        GoogleCacheLimits {
            max_operations: 65,
            ..Default::default()
        },
        GoogleCacheLimits {
            max_ttl: Duration::from_secs(3601),
            ..Default::default()
        },
        GoogleCacheLimits {
            operation_timeout: Duration::from_secs(31),
            ..Default::default()
        },
    ] {
        assert!(GoogleCacheSession::new(good.clone(), limits).is_err());
    }
}

#[tokio::test]
async fn create_refresh_delete_and_projection_use_native_wire_and_disable_stale_handles() {
    let server = MockServer::start().await;
    let mut session = session(&server, 2, 8);
    assert!(server.received_requests().await.unwrap().is_empty());
    create_mock(&server, "cachedContents/owned_1").await;
    Mock::given(method("PATCH"))
        .and(path("/v1beta/cachedContents/owned_1"))
        .and(query_param("updateMask", "ttl"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"name":"cachedContents/owned_1"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1beta/cachedContents/owned_1"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let prefix = request(&["prefix"]);
    let cache = session
        .create(&prefix, Duration::from_secs(60))
        .await
        .unwrap();
    let full: Value = serde_json::from_slice(
        &super::super::build_request(&session.model, &request(&["prefix", "next"]))
            .unwrap()
            .body,
    )
    .unwrap();
    let context = HookModelContext::from_model(&session.model);
    let projected = cache.on_payload(full.clone(), &context).unwrap().unwrap();
    assert_eq!(projected["cachedContent"], "cachedContents/owned_1");
    assert_eq!(projected["contents"].as_array().unwrap().len(), 1);
    assert!(projected.get("systemInstruction").is_none());
    let mut edited = full.clone();
    edited["systemInstruction"] = json!({"parts":[{"text":"changed"}]});
    assert!(cache.on_payload(edited, &context).unwrap().is_none());
    session
        .refresh(&cache, Duration::from_secs(120))
        .await
        .unwrap();
    session.delete(&cache).await.unwrap();
    assert!(cache.on_payload(full, &context).unwrap().is_none());
    session.close().await.unwrap();
    let calls = server.received_requests().await.unwrap();
    assert_eq!(calls.len(), 3);
    let create: Value = serde_json::from_slice(&calls[0].body).unwrap();
    assert_eq!(create["model"], "models/fixture-api-name");
    assert_eq!(create["ttl"], "60s");
    assert!(create.get("generationConfig").is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&calls[1].body).unwrap(),
        json!({"ttl":"120s"})
    );
}

#[tokio::test]
async fn entry_ttl_and_operation_limits_reserve_session_end_cleanup() {
    let server = MockServer::start().await;
    create_mock(&server, "cachedContents/one").await;
    Mock::given(method("PATCH"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .mount(&server)
        .await;
    let mut session = session(&server, 1, 3);
    assert!(session
        .create(&request(&["prefix"]), Duration::from_secs(301))
        .await
        .is_err());
    let cache = session
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(session
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .is_err());
    session
        .refresh(&cache, Duration::from_secs(60))
        .await
        .unwrap();
    assert!(session
        .refresh(&cache, Duration::from_secs(60))
        .await
        .is_err());
    session.close().await.unwrap();
    session.close().await.unwrap();
    assert!(session
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn failed_or_cancelled_creates_keep_capacity_and_report_uncertain_cleanup() {
    for cancel in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(if cancel { 200 } else { 503 })
                    .set_delay(if cancel {
                        Duration::from_secs(2)
                    } else {
                        Duration::ZERO
                    })
                    .set_body_json(json!({"name":"cachedContents/late"})),
            )
            .mount(&server)
            .await;
        let mut session = session(&server, 1, 4);
        if cancel {
            assert!(tokio::time::timeout(
                Duration::from_millis(50),
                session.create(&request(&["prefix"]), Duration::from_secs(60))
            )
            .await
            .is_err());
        } else {
            assert!(session
                .create(&request(&["prefix"]), Duration::from_secs(60))
                .await
                .is_err());
        }
        assert!(session
            .create(&request(&["prefix"]), Duration::from_secs(60))
            .await
            .is_err());
        assert!(
            session.close().await.is_err(),
            "unknown resources need explicit TTL-fallback reporting"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn close_attempts_every_entry_even_when_one_delete_fails() {
    let server = MockServer::start().await;
    let mut session = session(&server, 2, 6);
    create_mock(&server, "cachedContents/first").await;
    let first = session
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    server.reset().await;
    create_mock(&server, "cachedContents/second").await;
    let second = session
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    Mock::given(method("DELETE"))
        .and(path("/v1beta/cachedContents/second"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1beta/cachedContents/first"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    assert!(session.close().await.is_err());
    assert!(!first.active.load(std::sync::atomic::Ordering::Acquire));
    assert!(!second.active.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    session.close().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn response_size_names_and_resource_ownership_are_bounded() {
    for body in [
        json!({"name":"https://evil.example/cachedContents/id"}),
        json!({"name":"cachedContents/../bad"}),
        json!({"padding":"x".repeat(17 * 1024)}),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let mut session = session(&server, 1, 4);
        assert!(session
            .create(&request(&["prefix"]), Duration::from_secs(60))
            .await
            .is_err());
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    let server = MockServer::start().await;
    create_mock(&server, "cachedContents/owned").await;
    let mut owner = session(&server, 1, 4);
    let cache = owner
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    let mut stranger = session(&server, 1, 4);
    assert!(stranger.delete(&cache).await.is_err());
    assert!(stranger
        .refresh(&cache, Duration::from_secs(60))
        .await
        .is_err());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    drop(owner);
    assert!(!cache.active.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn operation_deadline_and_failed_refresh_disable_handles_without_retries() {
    let server = MockServer::start().await;
    create_mock(&server, "cachedContents/timeout").await;
    Mock::given(method("PATCH"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(2))
                .set_body_json(json!({})),
        )
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let mut owner = session(&server, 1, 4);
    owner.limits.operation_timeout = Duration::from_secs(1);
    let cache = owner
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(owner
        .refresh(&cache, Duration::from_secs(60))
        .await
        .is_err());
    assert!(!cache.active.load(std::sync::atomic::Ordering::Acquire));
    owner.close().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn local_prefix_limits_expiry_and_changed_history_fall_back_without_network() {
    let server = MockServer::start().await;
    create_mock(&server, "cachedContents/prefix").await;
    let mut owner = session(&server, 1, 6);
    for ttl in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(301),
    ] {
        assert!(owner.create(&request(&["prefix"]), ttl).await.is_err());
    }
    Arc::make_mut(&mut owner.model.spec).limits.context_window = 4_000_000;
    let large = "x".repeat(2 * 1024 * 1024);
    assert!(owner
        .create(&request(&[&large]), Duration::from_secs(60))
        .await
        .is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
    let cache = owner
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    let context = HookModelContext::from_model(&owner.model);
    let encode = |request: &Request| {
        serde_json::from_slice::<Value>(
            &super::super::build_request(&owner.model, request)
                .unwrap()
                .body,
        )
        .unwrap()
    };
    assert!(cache
        .on_payload(encode(&request(&["prefix"])), &context)
        .unwrap()
        .is_none());
    assert!(cache
        .on_payload(encode(&request(&["edited prefix", "next"])), &context)
        .unwrap()
        .is_none());
    let mut next = request(&["prefix"]);
    next.messages
        .push(Message::Assistant(crate::AssistantMessage {
            content: vec![crate::AssistantPart::Text("answer".into())],
            model: owner.model.spec.id.clone(),
            protocol: Protocol::GoogleGenerativeAi,
        }));
    next.messages.push(request(&["next"]).messages.remove(0));
    let full = encode(&next);
    assert_eq!(
        cache.on_payload(full.clone(), &context).unwrap().unwrap()["contents"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    *cache.deadline.lock().unwrap() = Instant::now();
    assert!(cache.on_payload(full, &context).unwrap().is_none());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn failed_explicit_delete_preserves_a_reserved_close_attempt() {
    let server = MockServer::start().await;
    create_mock(&server, "cachedContents/close").await;
    Mock::given(method("DELETE"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let mut owner = session(&server, 1, 3);
    let cache = owner
        .create(&request(&["prefix"]), Duration::from_secs(60))
        .await
        .unwrap();
    assert!(owner.delete(&cache).await.is_err());
    assert!(owner.delete(&cache).await.is_err()); // reserve cannot be spent twice
    assert!(owner.close().await.is_err());
    owner.close().await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}
