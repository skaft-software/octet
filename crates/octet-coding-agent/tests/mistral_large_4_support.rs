//! Mistral Large 4 (`mistral-large-4`, Mistral's "Le Chonk") must be selectable
//! on every provider octet already serves it from.
//!
//! Process-boundary coverage: the real binary builds its provider catalog from
//! the credential-scoped declarations, so a model missing from those catalogs is
//! exactly what a user sees when the route does not exist. The inventory column
//! layout is the documented `--list-models` table: provider, model, context,
//! max-out, thinking, images.
//!
//! Neither route needs the network: Mistral direct declares static discovery,
//! and OpenCode Zen's inventory is served from the same credential-scoped cache
//! record the host itself writes and reads.
#![cfg(unix)]

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest as _, Sha256};

/// The published Mistral Large 4 route: 512K context, 256K output, text+image
/// input and `reasoning_effort` none|high.
const MODEL: &str = "mistral-large-4";
const CONTEXT: &str = "524288";
const MAX_OUT: &str = "262144";
const OPENCODE_KEY: &str = "synthetic-opencode-key";
const OPENROUTER_KEY: &str = "synthetic-openrouter-key";
const OPENROUTER_MODEL: &str = "mistralai/mistral-large-4-0";

fn credential(home: &Path, provider: &str, key: &str) {
    let bytes = format!("{{\"version\":1,\"api_key\":\"{key}\"}}");
    octet_agent::secure_fs::write_private_atomic(
        &home.join(format!(".octet/credentials/api-keys/{provider}.json")),
        bytes.as_bytes(),
        8192,
    )
    .unwrap();
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Seed the credential-scoped inventory cache the host itself would have
/// written, so discovery resolves without contacting the provider.
fn seed_inventory_cache(home: &Path, provider: &str, url: &str, key: &str) {
    let directory = home.join(".octet/cache/model-inventories");
    fs::create_dir_all(&directory).unwrap();
    let record = serde_json::json!({
        "version": 1,
        "provider_id": provider,
        "inventory_url": url,
        "credential_fingerprint": digest(key.as_bytes()),
        "body": {"data": [{"id": MODEL}]},
        "checked_at": SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    });
    let path = directory.join(format!("{provider}-{}.json", digest(provider.as_bytes())));
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt as _;
    permissions.set_mode(0o600);
    fs::set_permissions(&path, permissions).unwrap();
}

fn list_models(home: &Path, workspace: &Path, search: &str) -> String {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_octet"))
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(
            "PATH",
            if cfg!(windows) {
                r"C:\Windows\System32"
            } else {
                "/usr/bin:/bin"
            },
        )
        .env("TERM", "dumb")
        .env("LANG", "C.UTF-8")
        .current_dir(workspace)
        .args(["--safe-mode", "--list-models", search])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(45);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("model enumeration did not finish within 45 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    stdout.seek(SeekFrom::Start(0)).unwrap();
    let mut listing = String::new();
    stdout.read_to_string(&mut listing).unwrap();
    let mut errors = String::new();
    stderr.read_to_string(&mut errors).unwrap();
    assert!(status.success(), "listing failed: {errors}");
    listing
}

/// The catalog row for the published route; octet lists the provider-scoped
/// catalog id, which is the provider id and the provider's own api name.
fn row(listing: &str, provider: &str, model: &str) -> Vec<String> {
    let id = format!("{provider}/{model}");
    listing
        .lines()
        .find(|line| {
            let mut fields = line.split('\t');
            fields.next() == Some(provider) && fields.next() == Some(id.as_str())
        })
        .unwrap_or_else(|| panic!("missing {id} row in:\n{listing}"))
        .split('\t')
        .map(str::to_owned)
        .collect()
}

fn assert_published_contract(fields: &[String], provider: &str, model: &str) {
    assert_eq!(fields[0], provider);
    assert_eq!(fields[1], format!("{provider}/{model}"));
    assert_eq!(fields[2], CONTEXT, "context window: {fields:?}");
    assert_eq!(fields[3], MAX_OUT, "max output tokens: {fields:?}");
    assert_eq!(fields[4], "true", "thinking must be advertised: {fields:?}");
    assert_eq!(
        fields[5], "true",
        "image input must be advertised: {fields:?}"
    );
}

#[test]
fn mistral_lists_mistral_large_4_for_its_direct_route() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(&home, "mistral", "synthetic-mistral-key");

    let listing = list_models(&home, &workspace, "mistral");
    assert_published_contract(&row(&listing, "mistral", MODEL), "mistral", MODEL);
}

#[test]
fn opencode_lists_mistral_large_4_from_its_credential_scoped_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(&home, "opencode", OPENCODE_KEY);
    seed_inventory_cache(
        &home,
        "opencode",
        "https://opencode.ai/zen/v1/models",
        OPENCODE_KEY,
    );

    let listing = list_models(&home, &workspace, "opencode");
    assert_published_contract(&row(&listing, "opencode", MODEL), "opencode", MODEL);
}

#[test]
fn openrouter_lists_mistral_large_4_from_its_credential_scoped_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(&home, "openrouter", OPENROUTER_KEY);
    seed_inventory_cache(
        &home,
        "openrouter",
        "https://openrouter.ai/api/v1/models",
        OPENROUTER_KEY,
    );
    // The live route uses a vendor-qualified id, unlike the direct Mistral and
    // Zen routes; exercise that exact published id through the real CLI.
    let path = home.join(format!(
        ".octet/cache/model-inventories/openrouter-{}.json",
        digest(b"openrouter")
    ));
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["body"]["data"][0] = serde_json::json!({
        "id": OPENROUTER_MODEL,
        "name": "Mistral: Mistral Large 4",
        "context_length": 524288,
        "architecture": {
            "input_modalities": ["text", "image"],
            "output_modalities": ["text"]
        },
        "pricing": {
            "prompt": "0.00000068",
            "completion": "0.00000209",
            "input_cache_read": "0.00000007"
        },
        "top_provider": {
            "context_length": 524288,
            "max_completion_tokens": 262144
        },
        "supported_parameters": [
            "include_reasoning", "reasoning", "reasoning_effort", "response_format",
            "structured_outputs", "temperature", "tool_choice", "tools"
        ],
        "reasoning": {
            "mandatory": false,
            "default_enabled": true,
            "supported_efforts": ["high", "none"],
            "default_effort": "high"
        }
    });
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    let listing = list_models(&home, &workspace, "openrouter");
    assert_published_contract(
        &row(&listing, "openrouter", OPENROUTER_MODEL),
        "openrouter",
        OPENROUTER_MODEL,
    );
}

#[test]
fn the_published_route_id_resolves_without_a_second_alias() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(&home, "mistral", "synthetic-mistral-key");

    let listing = list_models(&home, &workspace, MODEL);
    let rows = listing
        .lines()
        .filter(|line| line.split('\t').nth(1) == Some(format!("mistral/{MODEL}").as_str()))
        .count();
    assert_eq!(rows, 1, "one exact route for the published id: {listing}");
}
