//! Shared loopback DeepSeek fixture for the provider-startup acceptance tests.
//!
//! `GET /v1/models` is the only way to learn a discovery-only model id, so a
//! cold cache used to sit in front of the first turn (42/42 measured cells,
//! median 373 ms). It is also where the endpoint declares its reasoning default
//! (`effort.default_level`), which octet ignored: an unset preference resolved
//! to the first enabled level instead of the declared one. Both suites run the
//! real `<binary> --print` against this endpoint.

// Compiled once per integration-test binary, and each suite uses a subset.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// The environment credential the built-in DeepSeek declaration reads.
pub const KEY: &str = "synthetic-deepseek-credential";
pub const DISCOVERY_ONLY_MODEL: &str = "deepseek/deepseek-flash";
pub const ALWAYS_REGISTERED_MODEL: &str = "deepseek/deepseek-v4-pro";
pub const MARKER: &str = "provider-startup-ok";

/// How long the loopback inventory endpoint stalls. Longer than the 10 s
/// discovery timeout, so a launch that waits on it cannot finish inside the
/// test's deadline.
const INVENTORY_STALL: Duration = Duration::from_secs(30);

/// `/v1/models` in the shape the endpoint publishes: identifiers and the
/// declared effort contract, no display names and no limits.
pub fn deepseek_inventory(declared_default: &str) -> Value {
    json!({
        "object": "list",
        "data": [{
            "id": "deepseek-flash",
            "object": "model",
            "context_window": 1_048_576,
            "max_output_tokens": 393_216,
            "name": "DeepSeek-V4.1-Flash",
            "input_modalities": ["text", "image"],
            "effort": {
                "default_level": declared_default,
                "supported_levels": ["low", "high", "max"],
            },
        }],
    })
}

pub fn digest(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub fn inventory_cache_path(home: &Path) -> PathBuf {
    home.join(".octet/cache/model-inventories")
        .join(format!("deepseek-{}.json", digest("deepseek")))
}

/// Install the cached inventory record a previous refresh would have written.
pub fn seed_inventory(home: &Path, inventory_url: &str, declared_default: &str) {
    let path = inventory_cache_path(home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let record = json!({
        "version": 1,
        "provider_id": "deepseek",
        "inventory_url": inventory_url,
        "credential_fingerprint": digest(KEY),
        "body": deepseek_inventory(declared_default),
        "checked_at": 1,
    });
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

pub struct Fixture {
    root: tempfile::TempDir,
    pub server: MockServer,
}

/// A loopback DeepSeek endpoint plus the environment that points octet at it.
/// `stall_inventory` keeps `/v1/models` in flight for a whole discovery timeout.
pub async fn fixture(stall_inventory: bool, cache: Option<&str>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("home")).unwrap();
    std::fs::create_dir_all(root.path().join("workspace")).unwrap();
    let server = MockServer::start().await;
    let inventory_url = format!("{}/v1/models", server.uri());
    // The live shape carries the same declaration the cache would hold.
    let mut inventory = ResponseTemplate::new(200).set_body_json(deepseek_inventory("high"));
    if stall_inventory {
        inventory = inventory.set_delay(INVENTORY_STALL);
    }
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(inventory)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(|request: &Request| {
            let body: Value = request.body_json().unwrap();
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(format!(
                    "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
                    json!({"id":"fixture", "model": body["model"],
                        "choices":[{"delta":{"content": MARKER}}]}),
                    json!({"id":"fixture", "choices":[{"delta":{},"finish_reason":"stop"}]})
                ))
        })
        .mount(&server)
        .await;
    if let Some(declared_default) = cache {
        seed_inventory(
            root.path().join("home").as_path(),
            &inventory_url,
            declared_default,
        );
    }
    Fixture { root, server }
}

impl Fixture {
    /// Run the real print-mode CLI and return its wall time and the chat bodies
    /// the endpoint received.
    pub async fn run(&self, model: &str, args: &[&str]) -> (Duration, Vec<Value>) {
        let started = Instant::now();
        let output = tokio::time::timeout(
            Duration::from_secs(60),
            Command::new(env!("CARGO_BIN_EXE_octet"))
                .current_dir(self.root.path().join("workspace"))
                .env_clear()
                .env("HOME", self.root.path().join("home"))
                .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                .env("OCTET_SESSION_DIR", self.root.path().join("sessions"))
                .env("TERM", "dumb")
                .env("DEEPSEEK_API_KEY", KEY)
                .env(
                    "OCTET_DEEPSEEK_BASE_URL",
                    format!("{}/v1/", self.server.uri()),
                )
                .args([
                    "--no-context-files",
                    "--model",
                    model,
                    "--print",
                    "Reply with the marker.",
                ])
                .args(args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .expect("loopback provider task exceeded its deadline")
        .unwrap();
        let elapsed = started.elapsed();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(MARKER));
        let bodies = self.chat_bodies().await;
        assert!(!bodies.is_empty(), "the endpoint received no chat request");
        (elapsed, bodies)
    }

    pub async fn chat_bodies(&self) -> Vec<Value> {
        self.server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/v1/chat/completions")
            .map(|request| request.body_json::<Value>().unwrap())
            .collect()
    }
}
