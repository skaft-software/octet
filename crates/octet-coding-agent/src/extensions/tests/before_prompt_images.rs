//! Pi's `before_agent_start` user event carries the run's attached images
//! (`BeforeAgentStartUserEvent.images`), so the native before-prompt payload has
//! to name the same media for the peer that negotiated `before_prompt_state_v1`.
//! A peer that never negotiated that payload keeps the legacy prompt-only shape:
//! it cannot read the member, and the host must not send what it cannot honor.

use super::*;

#[cfg(unix)]
const STATE_FIXTURE: &str = r#"#!/bin/sh
log=$1
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
read_request() { IFS= read -r line || exit 91; printf '%s\n' "$line" >> "$log"; }
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.4","tools":[],"commands":[],"protocol":{"version":"0.4","features":["request_cancellation","content_parts","before_prompt_state_v1"],"limits":{"max_concurrent_requests":1}}}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#;

#[cfg(unix)]
const LEGACY_FIXTURE: &str = r#"#!/bin/sh
log=$1
request_id() { sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'; }
read_request() { IFS= read -r line || exit 91; printf '%s\n' "$line" >> "$log"; }
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{"disposition":{"action":"continue"},"context":[],"notifications":[]}}\n' "$id"
read_request
id=$(printf '%s' "$line" | request_id)
printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
"#;

/// The one image every case attaches: Pi's `ImageContent.data` is the same
/// standard base64 the native `Media` payload names.
#[cfg(unix)]
fn attached_image() -> octet_ai::Media {
    octet_ai::Media::image_bytes(bytes::Bytes::from_static(b"x"), mime::IMAGE_PNG)
}

#[cfg(unix)]
fn attached_image_payload() -> serde_json::Value {
    serde_json::json!([
        {"Image": {"source": {"Inline": "eA=="}, "media_type": "image/png", "detail": null}}
    ])
}

/// Start one fixture extension and return the fleet plus its wire log.
#[cfg(unix)]
async fn fixture_fleet(
    temp: &tempfile::TempDir,
    name: &str,
    script: &str,
    manifest: &str,
) -> (ExecutableExtensions, PathBuf) {
    use std::os::unix::fs::PermissionsExt as _;

    let log_path = temp.path().join(format!("{name}-wire.jsonl"));
    let fixture = temp.path().join(format!("{name}.sh"));
    std::fs::write(&fixture, script).unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let mut manifest = ExtensionManifest::parse(manifest).unwrap();
    manifest.entrypoint.args = vec![log_path.to_string_lossy().into_owned()];
    let manifest_path = temp.path().join(EXTENSION_MANIFEST_FILENAME);
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(2);
    runtime.shutdown_timeout = Duration::from_secs(2);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process);
    (extensions, log_path)
}

/// Every `hook/run` frame the fixture logged, in wire order.
#[cfg(unix)]
fn hook_payloads(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["method"] == "hook/run")
        .map(|frame| frame["params"]["payload"].clone())
        .collect()
}

#[cfg(unix)]
#[tokio::test]
async fn before_prompt_state_payload_carries_the_run_images() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log_path) = fixture_fleet(
        &temp,
        "state-images",
        STATE_FIXTURE,
        r#"
name = "state-images"
version = "0.1.0"
api_version = "0.4"

[entrypoint]
command = "state-images.sh"

[capabilities]
system_prompt = true

[contributes]
hooks = ["before_prompt"]
"#,
    )
    .await;
    extensions
        .compose_prompt_with_images("system", "what is this".into(), vec![attached_image()])
        .await
        .unwrap();
    // A text-only run attaches nothing, and Pi omits the optional member.
    extensions
        .compose_prompt("system", "and this".into())
        .await
        .unwrap();
    extensions.shutdown().await;

    assert_eq!(
        hook_payloads(&log_path),
        vec![
            serde_json::json!({
                "prompt": "what is this",
                "system_prompt": "system",
                "images": attached_image_payload(),
            }),
            serde_json::json!({
                "prompt": "and this",
                "system_prompt": "system",
                "images": serde_json::Value::Null,
            }),
        ]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn legacy_before_prompt_payload_stays_prompt_only_for_a_media_run() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, log_path) = fixture_fleet(
        &temp,
        "legacy-images",
        LEGACY_FIXTURE,
        r#"
name = "legacy-images"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "legacy-images.sh"

[contributes]
hooks = ["before_prompt"]
"#,
    )
    .await;
    extensions
        .compose_prompt_with_images("system", "what is this".into(), vec![attached_image()])
        .await
        .unwrap();
    extensions.shutdown().await;

    assert_eq!(
        hook_payloads(&log_path),
        vec![serde_json::json!({"prompt": "what is this"})]
    );
}
