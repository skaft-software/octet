//! Coding-host qualification only: fake credentials, private temp files, loopback
//! wire fixtures. No SDK/CLI exports are required to exercise the owned adapter.
#![allow(dead_code)]

#[path = "../src/auth/copilot.rs"]
mod copilot;
#[expect(
    unused_macros,
    unused_imports,
    reason = "The standalone Copilot adapter fixture uses output functions, not the production stderr macro."
)]
#[path = "../src/output.rs"]
mod output;

mod providers {
    pub use octet_sdk::provider::{
        CopilotAvailabilityError, CopilotCredentialScheme, CopilotDeviceLogin,
        CopilotDeviceLoginStatus, CopilotDynamicHeader, CopilotEndpoint, CopilotHost, CopilotModel,
        CopilotProvider, CopilotSession,
    };
}

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use copilot::{CopilotCodingHost, CredentialStore};
use octet_agent::secure_fs;
use octet_ai::{Auth, EndpointId, ModelCatalog, ModelId, Protocol};
use providers::{CopilotAvailabilityError as Error, CopilotDeviceLoginStatus, CopilotHost};
use serde_json::{json, Value};
use wiremock::matchers::{body_string_contains, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OAUTH: &str = "github-oauth-PRIVATE-SENTINEL";
const INFERENCE: &str = "copilot-inference-PRIVATE-SENTINEL";
const DEVICE: &str = "device-state-PRIVATE-SENTINEL";

fn private_store() -> (tempfile::TempDir, CredentialStore, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    // macOS's /var and /tmp aliases must not bypass descriptor-walk validation.
    let path = temp
        .path()
        .canonicalize()
        .unwrap()
        .join("credentials/copilot.json");
    let store = CredentialStore::new(&path);
    (temp, store, path)
}

fn token_response(origin: &str, token: &str) -> Value {
    json!({
        "token": token,
        "expires_at": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + 600,
        "refresh_in": 600,
        "endpoints": {"api": origin}
    })
}

async fn exchange_mock(server: &MockServer, token: &str) {
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .and(header("authorization", format!("token {OAUTH}")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(token_response(&server.uri(), token)),
        )
        .mount(server)
        .await;
}

async fn device_mock(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/login/device/code"))
        .and(body_string_contains("client_id=Iv1.b507a08c87ecfe98"))
        .and(body_string_contains("scope=read%3Auser"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "device_code": DEVICE, "user_code": "TEST-ONLY",
            "verification_uri": "https://github.com/login/device",
            "expires_in": 60, "interval": 1
        })))
        .mount(server)
        .await;
}

async fn poll_mock(server: &MockServer, response: Value) {
    Mock::given(method("POST"))
        .and(path("/login/oauth/access_token"))
        .and(body_string_contains(format!("device_code={DEVICE}")))
        .and(body_string_contains(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(server)
        .await;
}

fn model(id: &str, route: &str) -> Value {
    json!({
        "id": id, "name": id, "model_picker_enabled": true,
        "policy": {"state": "enabled"}, "supported_endpoints": [route],
        "capabilities": {
            "type": "chat", "supports": {"tool_calls": true, "parallel_tool_calls": true},
            "limits": {"max_context_window_tokens": 32000, "max_output_tokens": 4096}
        }
    })
}

fn assert_no_advertisement(catalog: &ModelCatalog) {
    assert_eq!(catalog.models().count(), 0);
    for id in ["github-copilot-chat", "github-copilot-responses"] {
        assert!(!catalog.has_endpoint(&EndpointId(id.into())));
    }
}

#[tokio::test]
async fn copilot_offline_and_missing_credentials_are_no_io_no_advertisement() {
    let mut catalog = ModelCatalog::default();
    // Deliberately invalid store: offline must return before filesystem access.
    copilot::register_available_models_with_store(
        &mut catalog,
        &CredentialStore::new("relative-invalid-store"),
        true,
    )
    .await
    .unwrap();
    copilot::register_available_models(&mut catalog, true)
        .await
        .unwrap();
    octet_sdk::provider::CopilotProvider::register_available_models(&mut catalog, true)
        .await
        .unwrap();
    let (_temp, store, _) = private_store();
    copilot::register_available_models_with_store(&mut catalog, &store, false)
        .await
        .unwrap();
    let server = MockServer::start().await;
    let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
    host.register_available_models(&mut catalog, false)
        .await
        .unwrap();
    assert_eq!(host.availability().await.unwrap_err(), Error::LoginRequired);
    assert_eq!(host.exchange().await.unwrap_err(), Error::LoginRequired);
    assert_no_advertisement(&catalog);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copilot_blocking_catalog_bridge_keeps_the_live_host_after_runtime_exit() {
    let (_temp, store, _) = private_store();
    store.save(OAUTH).unwrap();
    let server = MockServer::start().await;
    exchange_mock(&server, INFERENCE).await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            model("runtime-bridge", "/responses")
        ]})))
        .mount(&server)
        .await;
    let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
    let mut catalog = ModelCatalog::default();
    copilot::register_available_models_blocking(&mut catalog, true).unwrap();
    copilot::register_available_models_with_host_blocking(&mut catalog, host.clone(), true)
        .unwrap();
    assert_no_advertisement(&catalog);
    assert!(server.received_requests().await.unwrap().is_empty());

    copilot::register_available_models_with_host_blocking(&mut catalog, host.clone(), false)
        .unwrap();
    let resolved = catalog
        .resolve(&ModelId("github-copilot/runtime-bridge".into()))
        .unwrap();
    assert_eq!(resolved.spec.protocol, Protocol::OpenAiResponses);
    let Auth::Dynamic(resolver) = &resolved.endpoint.auth else {
        panic!("the scoped runtime must not replace the host resolver with a static token");
    };
    assert!(resolver.resolve().await.is_ok());
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
    // Refresh uses the retained client on this runtime after the discovery
    // thread's current-thread runtime has been dropped.
    assert!(host.refresh().await.is_ok());
    assert!(resolver.resolve().await.is_ok());
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[cfg(unix)]
#[tokio::test]
async fn copilot_cli_logout_aliases_use_only_the_isolated_selected_store() {
    for alias in ["copilot", "github-copilot"] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let path = home.join(".octet/credentials/copilot.json");
        CredentialStore::new(&path).save(OAUTH).unwrap();
        let sibling = path.with_file_name("codex.json");
        secure_fs::write_private_atomic(&sibling, b"unrelated-provider", 16384).unwrap();
        let tmp = home.join("tmp");
        let cache = home.join("cache");
        std::fs::create_dir(&tmp).unwrap();
        std::fs::create_dir(&cache).unwrap();
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_octet"))
                .args(["--logout", alias])
                .env_clear()
                .env("HOME", &home)
                .env("TMPDIR", &tmp)
                .env("XDG_CACHE_HOME", &cache)
                .current_dir(&home)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("CLI logout must settle")
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"Signed out of GitHub Copilot.\n");
        assert!(!path.exists());
        assert_eq!(std::fs::read(sibling).unwrap(), b"unrelated-provider");
        assert!(!String::from_utf8_lossy(&output.stderr).contains(OAUTH));
    }
}

#[tokio::test]
async fn copilot_headless_login_persists_only_oauth_and_logout_is_exact() {
    let (_temp, store, path) = private_store();
    let server = MockServer::start().await;
    device_mock(&server).await;
    poll_mock(
        &server,
        json!({"access_token": OAUTH, "token_type": "bearer"}),
    )
    .await;
    let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
    copilot::login_with_host(&host, true).await.unwrap();
    assert!(store.is_configured().unwrap());
    let bytes = secure_fs::read_private_file_bounded(&path, 16384).unwrap();
    let persisted: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(persisted, json!({"version": 1, "github_token": OAUTH}));
    let rendered = format!("{host:?} {store:?}");
    for secret in [OAUTH, INFERENCE, DEVICE] {
        assert!(!rendered.contains(secret));
    }
    let sibling = path.with_file_name("codex.json");
    secure_fs::write_private_atomic(&sibling, b"unrelated-provider", 16384).unwrap();
    copilot::logout(&store).await.unwrap();
    assert!(!path.exists());
    assert_eq!(std::fs::read(sibling).unwrap(), b"unrelated-provider");
    copilot::logout(&store).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[test]
fn copilot_store_rejects_malformed_oversized_and_invalid_credentials_without_echoing() {
    let (_temp, store, path) = private_store();
    for token in ["", "a\nb", "a\rb", "a b"] {
        assert!(store.save(token).is_err());
    }
    assert!(!path.exists());
    assert!(store.save(&"x".repeat(4097)).is_err());
    for value in [
        json!({"version": 2, "github_token": OAUTH}),
        json!({"version": 1, "github_token": OAUTH, "endpoint": "https://attacker.test"}),
        json!({"version": 1, "github_token": format!("{OAUTH}\n")}),
    ] {
        secure_fs::write_private_atomic(&path, &serde_json::to_vec(&value).unwrap(), 16384)
            .unwrap();
        let error = store.is_configured().unwrap_err();
        assert!(!format!("{error:#} {error:?}").contains(OAUTH));
        store.delete().unwrap();
    }
    secure_fs::write_private_atomic(&path, &vec![b'x'; 16385], 16385).unwrap();
    assert!(store.is_configured().is_err());
    assert!(store.delete().is_err()); // Never read/delete an unbounded target.
}

#[cfg(unix)]
#[test]
fn copilot_private_store_refuses_links_and_insecure_modes() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (_temp, store, path) = private_store();
    store.save(OAUTH).unwrap();
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let target = path.with_file_name("target.json");
    std::fs::rename(&path, &target).unwrap();
    symlink(&target, &path).unwrap();
    assert!(store.is_configured().is_err());
    assert!(store.save("replacement").is_err());
    assert!(store.delete().is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&target, &path).unwrap();
    assert!(store.is_configured().is_err());
    assert!(store.delete().is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&target, &path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.is_configured().is_err());
    assert!(store.delete().is_err());
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    store.delete().unwrap();
}

#[tokio::test]
async fn copilot_denied_expired_and_cancelled_flows_never_commit_credentials() {
    for (error, expected) in [
        ("access_denied", CopilotDeviceLoginStatus::Denied),
        ("expired_token", CopilotDeviceLoginStatus::Expired),
    ] {
        let (_temp, store, path) = private_store();
        let server = MockServer::start().await;
        device_mock(&server).await;
        poll_mock(&server, json!({"error": error})).await;
        let host = CopilotCodingHost::with_mock_server(store, &server.uri());
        let display = host.begin_device_login().await.unwrap();
        assert!(!format!("{display:?}").contains(DEVICE));
        assert_eq!(host.poll_device_login().await.unwrap(), expected);
        assert_eq!(
            host.poll_device_login().await.unwrap_err(),
            Error::LoginRequired
        );
        assert!(!path.exists());
    }
    let (_temp, store, path) = private_store();
    let server = MockServer::start().await;
    device_mock(&server).await;
    poll_mock(
        &server,
        json!({"access_token": OAUTH, "token_type": "bearer"}),
    )
    .await;
    let host = CopilotCodingHost::with_mock_server(store, &server.uri());
    host.begin_device_login().await.unwrap();
    // Cancellation before the first allowed poll leaves no detached saver.
    assert!(
        tokio::time::timeout(Duration::from_millis(100), host.poll_device_login())
            .await
            .is_err()
    );
    drop(host);
    assert!(!path.exists());
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn copilot_pending_and_slow_down_respect_intervals_and_login_does_not_overwrite() {
    let (_temp, store, path) = private_store();
    let server = MockServer::start().await;
    device_mock(&server).await;
    poll_mock(&server, json!({"error": "authorization_pending"})).await;
    let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
    host.begin_device_login().await.unwrap();
    assert_eq!(
        host.poll_device_login().await.unwrap(),
        CopilotDeviceLoginStatus::Pending
    );
    assert!(!path.exists());
    server.reset().await;
    poll_mock(&server, json!({"error": "slow_down"})).await;
    assert_eq!(
        host.poll_device_login().await.unwrap(),
        CopilotDeviceLoginStatus::Pending
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(100), host.poll_device_login())
            .await
            .is_err()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    assert!(!path.exists());
    server.reset().await;
    device_mock(&server).await;
    poll_mock(
        &server,
        json!({"access_token": OAUTH, "token_type": "bearer"}),
    )
    .await;
    host.begin_device_login().await.unwrap();
    store.save("other-login").unwrap();
    assert_eq!(
        host.poll_device_login().await.unwrap_err(),
        Error::TokenExchangeUnavailable
    );
    let bytes = secure_fs::read_private_file_bounded(&path, 16384).unwrap();
    assert!(!String::from_utf8(bytes).unwrap().contains(OAUTH));
}

#[tokio::test]
async fn copilot_device_presentation_rejects_redirects_secrets_and_invalid_controls() {
    let sink = MockServer::start().await;
    let (_temp, store, credential_path) = private_store();
    let valid = json!({
        "device_code": DEVICE, "user_code": "TEST-ONLY",
        "verification_uri": "https://github.com/login/device", "expires_in": 60, "interval": 1
    });
    let mut bad_uri = valid.clone();
    bad_uri["verification_uri"] = json!(sink.uri());
    let mut secret_code = valid.clone();
    secret_code["user_code"] = json!(DEVICE);
    let mut control_code = valid.clone();
    control_code["user_code"] = json!("CODE\u{1b}[2J");
    let mut zero_interval = valid;
    zero_interval["interval"] = json!(0);
    for response in [
        ResponseTemplate::new(302).insert_header("location", sink.uri()),
        ResponseTemplate::new(200).set_body_json(bad_uri),
        ResponseTemplate::new(200).set_body_json(secret_code),
        ResponseTemplate::new(200).set_body_json(control_code),
        ResponseTemplate::new(200).set_body_json(zero_interval),
        ResponseTemplate::new(200).set_body_string(DEVICE.repeat(4000)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/device/code"))
            .respond_with(response)
            .mount(&server)
            .await;
        let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
        let error = host.begin_device_login().await.unwrap_err();
        assert_eq!(error, Error::InvalidDeviceLogin);
        assert!(!format!("{error:?} {error}").contains(DEVICE));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert!(!credential_path.exists());
    }
    assert!(sink.received_requests().await.unwrap().is_empty());
}

#[test]
fn copilot_inference_authority_is_not_a_token_supplied_proxy() {
    for origin in [
        "api.githubcopilot.com",
        "api.individual.githubcopilot.com",
        "api.business.githubcopilot.com",
        "api.enterprise.githubcopilot.com",
    ] {
        assert!(copilot::validate_inference_endpoint(&format!("https://{origin}/")).is_ok());
    }
    for origin in [
        "http://api.githubcopilot.com/",
        "https://api.githubcopilot.com.evil.test/",
        "https://githubcopilot.com/",
        "https://api.githubcopilot.com:444/",
        "https://token@api.githubcopilot.com/",
        "https://api.githubcopilot.com/?token=secret",
        "https://api.githubcopilot.com/#secret",
        "https://api.githubcopilot.com/other/",
        "https://127.0.0.1/",
        "https://api.example.test/",
    ] {
        assert_eq!(
            copilot::validate_inference_endpoint(origin).unwrap_err(),
            Error::InvalidEndpoint
        );
    }
}

#[tokio::test]
async fn copilot_registration_preserves_explicit_protocols_and_never_catalogs_secrets() {
    let (_temp, store, credential_path) = private_store();
    store.save(OAUTH).unwrap();
    let before = std::fs::read(&credential_path).unwrap();
    let server = MockServer::start().await;
    exchange_mock(&server, INFERENCE).await;
    let mut disabled = model("disabled", "/chat/completions");
    disabled["policy"]["state"] = json!("disabled");
    let mut unknown_reasoning = model("unknown-reasoning", "/responses");
    unknown_reasoning["capabilities"]["supports"]["reasoning"] = json!(true);
    let mut no_tools = model("no-tools", "/responses");
    no_tools["capabilities"]["supports"]["tool_calls"] = json!(false);
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", format!("Bearer {INFERENCE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            model("gpt-looking-chat", "/chat/completions"),
            model("claude-looking-responses", "/responses"),
            model("anthropic-only", "/v1/messages"), disabled, unknown_reasoning, no_tools
        ]})))
        .mount(&server)
        .await;
    let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
    let mut catalog = ModelCatalog::default();
    host.register_available_models(&mut catalog, false)
        .await
        .unwrap();
    assert_eq!(catalog.models().count(), 2);
    assert!(catalog
        .resolve(&ModelId("github-copilot/no-tools".into()))
        .is_err());
    for (id, protocol, endpoint_id) in [
        (
            "gpt-looking-chat",
            Protocol::OpenAiChat,
            "github-copilot-chat",
        ),
        (
            "claude-looking-responses",
            Protocol::OpenAiResponses,
            "github-copilot-responses",
        ),
    ] {
        let resolved = catalog
            .resolve(&ModelId(format!("github-copilot/{id}")))
            .unwrap();
        assert_eq!(resolved.spec.protocol, protocol);
        assert_eq!(resolved.endpoint.id.0, endpoint_id);
        assert!(matches!(&resolved.endpoint.auth, Auth::Dynamic(_)));
        let metadata = serde_json::to_string(&*resolved.spec).unwrap();
        let diagnostics = format!("{:?}", resolved.endpoint.auth);
        for secret in [OAUTH, INFERENCE, DEVICE] {
            assert!(!metadata.contains(secret));
            assert!(!diagnostics.contains(secret));
        }
    }
    assert_eq!(std::fs::read(credential_path).unwrap(), before);
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3); // Endpoint binding, resolver exchange, inventory.
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/models")
            .count(),
        1
    );
}

#[tokio::test]
async fn copilot_bad_inventory_is_atomic_and_does_not_fall_back_to_static_models() {
    let mut secret_name = model("safe-id", "/responses");
    secret_name["name"] = json!(format!("label-{INFERENCE}"));
    let mut invalid_limits = model("invalid-limits", "/responses");
    invalid_limits["capabilities"]["limits"]["max_output_tokens"] = json!(0);
    let mut invalid_flags = model("invalid-flags", "/responses");
    invalid_flags["capabilities"]["supports"]["tool_calls"] = json!("true");
    for inventory in [
        json!({"data": []}),
        json!({"data": [model("unsupported", "/v1/messages")]}),
        json!({"data": [model("duplicate", "/responses"), model("duplicate", "/responses")]}),
        json!({"data": vec![model("bounded", "/chat/completions"); 129]}),
        json!({"data": [model("valid", "/responses"), null]}),
        json!({"data": [model("valid", "/responses"), {"id": "incomplete"}]}),
        json!({"data": [model("valid", "/responses"), model(OAUTH, "/responses")]}),
        json!({"data": [model("valid", "/responses"), model(INFERENCE, "/responses")]}),
        json!({"data": [model("valid", "/responses"), secret_name]}),
        json!({"data": [model("valid", "/responses"), invalid_limits]}),
        json!({"data": [model("valid", "/responses"), invalid_flags]}),
    ] {
        let (_temp, store, _) = private_store();
        store.save(OAUTH).unwrap();
        let server = MockServer::start().await;
        exchange_mock(&server, INFERENCE).await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(inventory))
            .mount(&server)
            .await;
        let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
        let mut catalog = ModelCatalog::default();
        let error = host
            .register_available_models(&mut catalog, false)
            .await
            .unwrap_err();
        for secret in [OAUTH, INFERENCE] {
            assert!(!format!("{error:#} {error:?}").contains(secret));
        }
        assert_no_advertisement(&catalog);
    }
}

#[tokio::test]
async fn copilot_inventory_rejects_redirects_and_oversized_or_malformed_bodies() {
    let sink = MockServer::start().await;
    for response in [
        ResponseTemplate::new(302).insert_header("location", sink.uri()),
        ResponseTemplate::new(200).set_body_string(INFERENCE.repeat(40000)),
        ResponseTemplate::new(200).set_body_string(format!("invalid-json-{OAUTH}")),
        ResponseTemplate::new(401).set_body_string(INFERENCE),
    ] {
        let (_temp, store, _) = private_store();
        store.save(OAUTH).unwrap();
        let server = MockServer::start().await;
        exchange_mock(&server, INFERENCE).await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(response)
            .mount(&server)
            .await;
        let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
        let mut catalog = ModelCatalog::default();
        let error = host
            .register_available_models(&mut catalog, false)
            .await
            .unwrap_err();
        for secret in [OAUTH, INFERENCE] {
            assert!(!format!("{error:#} {error:?}").contains(secret));
        }
        assert_no_advertisement(&catalog);
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
    }
    assert!(sink.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn copilot_fresh_catalog_credentials_reject_logout_account_change_and_invalid_store() {
    for mutation in ["logout", "account-change", "invalid-store"] {
        let (_temp, store, credential_path) = private_store();
        store.save(OAUTH).unwrap();
        let server = MockServer::start().await;
        exchange_mock(&server, INFERENCE).await;
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
                model("fresh-session", "/chat/completions")
            ]})))
            .mount(&server)
            .await;
        let host = Arc::new(CopilotCodingHost::with_mock_server(
            store.clone(),
            &server.uri(),
        ));
        let mut catalog = ModelCatalog::default();
        host.register_available_models(&mut catalog, false)
            .await
            .unwrap();
        let resolved = catalog
            .resolve(&ModelId("github-copilot/fresh-session".into()))
            .unwrap();
        let Auth::Dynamic(resolver) = &resolved.endpoint.auth else {
            panic!("Copilot must retain the live host resolver");
        };
        assert!(resolver.resolve().await.is_ok());
        assert_eq!(server.received_requests().await.unwrap().len(), 3);
        match mutation {
            "logout" => copilot::logout(&store).await.unwrap(),
            "account-change" => store.save("different-account-oauth").unwrap(),
            "invalid-store" => secure_fs::write_private_atomic(
                &credential_path,
                b"invalid-private-credential",
                16384,
            )
            .unwrap(),
            _ => unreachable!(),
        }
        for _ in 0..2 {
            let error = resolver
                .resolve()
                .await
                .err()
                .expect("fresh cache must not outlive its login");
            let diagnostic = format!("{error:?} {error}");
            for secret in [OAUTH, INFERENCE, "different-account-oauth"] {
                assert!(!diagnostic.contains(secret));
            }
        }
        assert_eq!(
            server.received_requests().await.unwrap().len(),
            3,
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn copilot_refresh_reexchanges_without_persisting_and_rejects_replaced_credentials() {
    let (_temp, store, path) = private_store();
    store.save(OAUTH).unwrap();
    let before = std::fs::read(&path).unwrap();
    let server = MockServer::start().await;
    exchange_mock(&server, INFERENCE).await;
    let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
    host.exchange().await.unwrap();
    server.reset().await;
    exchange_mock(&server, "replacement-inference").await;
    let refreshed = host.refresh().await.unwrap();
    assert!(!format!("{refreshed:?}").contains("replacement-inference"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    store.save("replacement-oauth").unwrap();
    assert_eq!(
        host.refresh().await.unwrap_err(),
        Error::TokenRefreshUnavailable
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    store.delete().unwrap();
    assert_eq!(host.exchange().await.unwrap_err(), Error::LoginRequired);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn copilot_exchange_cannot_install_after_concurrent_logout() {
    let (_temp, store, credential_path) = private_store();
    store.save(OAUTH).unwrap();
    let server = MockServer::start().await;
    let arrived = Arc::new(tokio::sync::Notify::new());
    let notify = arrived.clone();
    let response = token_response(&server.uri(), INFERENCE);
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(move |_request: &wiremock::Request| {
            notify.notify_one();
            ResponseTemplate::new(200)
                .set_body_json(response.clone())
                .set_delay(Duration::from_millis(100))
        })
        .mount(&server)
        .await;
    let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
    let (exchange, ()) = tokio::join!(host.exchange(), async {
        tokio::time::timeout(Duration::from_secs(5), arrived.notified())
            .await
            .expect("exchange reached the mock server");
        store.delete().unwrap();
    });
    assert_eq!(exchange.unwrap_err(), Error::LoginRequired);
    assert!(!credential_path.exists());
    assert_eq!(host.exchange().await.unwrap_err(), Error::LoginRequired);
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn copilot_refresh_cannot_rebind_an_existing_origin() {
    let (_temp, store, _) = private_store();
    store.save(OAUTH).unwrap();
    let server = MockServer::start().await;
    exchange_mock(&server, INFERENCE).await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": [
            model("pinned-origin", "/chat/completions")
        ]})))
        .mount(&server)
        .await;
    let host = Arc::new(CopilotCodingHost::with_mock_server(store, &server.uri()));
    let mut catalog = ModelCatalog::default();
    host.register_available_models(&mut catalog, false)
        .await
        .unwrap();
    let resolved = catalog
        .resolve(&ModelId("github-copilot/pinned-origin".into()))
        .unwrap();
    let Auth::Dynamic(resolver) = &resolved.endpoint.auth else {
        panic!("Copilot must retain the live host resolver");
    };
    assert!(resolver.resolve().await.is_ok());
    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/copilot_internal/v2/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(token_response(
            "https://api.enterprise.githubcopilot.com/",
            "new-origin-token",
        )))
        .mount(&server)
        .await;
    assert_eq!(
        host.refresh().await.unwrap_err(),
        Error::TokenRefreshUnavailable
    );
    assert_eq!(
        host.availability().await.unwrap_err(),
        Error::InvalidEndpoint
    );
    assert_eq!(
        host.discover_models().await.unwrap_err(),
        Error::InvalidEndpoint
    );
    assert!(resolver.resolve().await.is_err());
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/copilot_internal/v2/token");
    // Even a later response restoring the old origin cannot revive this host.
    server.reset().await;
    exchange_mock(&server, INFERENCE).await;
    assert_eq!(host.exchange().await.unwrap_err(), Error::InvalidEndpoint);
    assert!(resolver.resolve().await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn copilot_exchange_rejects_redirects_bad_destinations_expired_and_oversized_bodies() {
    let sink = MockServer::start().await;
    let server = MockServer::start().await;
    let (_temp, store, _) = private_store();
    store.save(OAUTH).unwrap();
    let mut expired = token_response(&server.uri(), INFERENCE);
    expired["expires_at"] = json!(1);
    let responses = vec![
        ResponseTemplate::new(302).insert_header("location", sink.uri()),
        ResponseTemplate::new(200).set_body_json(token_response(&sink.uri(), INFERENCE)),
        ResponseTemplate::new(200)
            .set_body_json(token_response("https://api.evil.test/", INFERENCE)),
        ResponseTemplate::new(200).set_body_json(expired),
        ResponseTemplate::new(200).set_body_json(token_response(&server.uri(), OAUTH)),
        ResponseTemplate::new(200).set_body_string(INFERENCE.repeat(4000)),
        ResponseTemplate::new(401).set_body_string(OAUTH),
        ResponseTemplate::new(200).set_body_string(format!("invalid-json-{OAUTH}")),
    ];
    for response in responses {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/copilot_internal/v2/token"))
            .respond_with(response)
            .mount(&server)
            .await;
        let host = CopilotCodingHost::with_mock_server(store.clone(), &server.uri());
        let error = host.exchange().await.unwrap_err();
        for secret in [OAUTH, INFERENCE] {
            assert!(!format!("{error:?} {error}").contains(secret));
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
    assert!(sink.received_requests().await.unwrap().is_empty());
}
