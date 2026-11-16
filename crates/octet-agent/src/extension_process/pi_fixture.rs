//! Raw-agent fixtures capture actual factory registrations, not App-generated
//! bridges. The latter reserve resource discovery and require the complete App
//! consumer; offering that feature just to pass initialization would be wrong.
use super::*;

pub(crate) async fn capture(workspace: &Path, factories: &[PathBuf]) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let captured = tokio::time::timeout(
        Duration::from_secs(20),
        Command::new("node")
            .arg(root.join("runner.mjs"))
            .arg("--inspect")
            .args(factories)
            .env("HOME", workspace)
            .env("OCTET_PI_AGENT_DIR", workspace.join(".pi/agent"))
            .env("OCTET_PI_COMPAT_CACHE", workspace.join(".cache"))
            .current_dir(workspace)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("Pi registration capture timed out")
    .expect("raw-agent acceptance requires local Node and adapter dependencies");
    assert!(
        captured.status.success(),
        "Pi capture failed: {}",
        String::from_utf8_lossy(&captured.stderr)
    );
    assert!(captured.stdout.len() <= 4 * 1024 * 1024);
    let frame: serde_json::Value = serde_json::from_slice(&captured.stdout).unwrap();
    let metadata = &frame["result"];
    assert!(metadata.is_object());
    let names = |kind: &str| -> Vec<String> {
        metadata[kind]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let bundle = workspace.join("octet-pi-compat");
    std::fs::create_dir_all(&bundle).unwrap();
    let config_path = bundle.join("bridge.json");
    let hashes: BTreeMap<_, _> = factories
        .iter()
        .map(|path| {
            (
                path.to_string_lossy().into_owned(),
                format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
            )
        })
        .collect();
    std::fs::write(
        &config_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "extensions": factories,
            "entrypoint_sha256": hashes,
            "registrations": metadata,
            "subscribed_hooks": metadata["hooks"],
            "pi_agent_dir": workspace.join(".pi/agent"),
        }))
        .unwrap(),
    )
    .unwrap();
    let mut manifest = ExtensionManifest::parse(
        r#"
name = "octet-pi-compat"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "node"
[capabilities]
filesystem = "unrestricted"
process = true
network = true
system_prompt = true
[contributes]
notifications = true
confirmations = true
providers = true
"#,
    )
    .unwrap();
    manifest.entrypoint.args = vec![
        root.join("runner.mjs").to_string_lossy().into_owned(),
        "--config".into(),
        config_path.to_string_lossy().into_owned(),
    ];
    manifest
        .entrypoint
        .env
        .insert("HOME".into(), workspace.to_string_lossy().into_owned());
    manifest.entrypoint.env.insert(
        "OCTET_PI_COMPAT_CACHE".into(),
        workspace.join(".cache").to_string_lossy().into_owned(),
    );
    manifest.contributes.tools = names("tools");
    manifest.contributes.commands = names("commands");
    manifest.contributes.hooks = serde_json::from_value(metadata["hooks"].clone()).unwrap();
    manifest.contributes.flags = serde_json::from_value(metadata["flags"].clone()).unwrap();
    manifest.contributes.shortcuts = serde_json::from_value(metadata["shortcuts"].clone()).unwrap();
    manifest.contributes.tool_renderers =
        serde_json::from_value(metadata["tool_renderers"].clone()).unwrap();
    manifest.validate().unwrap();
    let manifest_path = bundle.join("extension.toml");
    std::fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    manifest_path
}

#[tokio::test]
async fn raw_factory_resource_registration_still_requires_the_complete_consumer() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("resources.mjs");
    std::fs::write(
        &factory,
        "export default pi => pi.on('resources_discover', () => { throw new Error('discovery must not run during capture or initialize'); });",
    )
    .unwrap();
    let manifest_path = capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    assert!(manifest
        .contributes
        .hooks
        .contains(&ExtensionHook::ResourcesDiscover));
    let config = ExtensionRuntimeConfig::new(temp.path());
    assert!(!config.resource_paths);
    let error = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        config,
    )
    .await
    .err()
    .expect("a real resource registration cannot start without the App consumer");
    assert!(
        matches!(error, ExtensionRuntimeError::Remote { code: -32601, message, .. }
        if message == "unsupported_feature resource_paths_v1: feature was not negotiated")
    );
}

/// The host already provides some tool names; a mirror that also registers one
/// of them keeps running and simply does not contribute that tool.
#[tokio::test]
async fn withheld_tool_names_are_dropped_while_the_rest_of_the_bridge_runs() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("two-tools.mjs");
    std::fs::write(
        &factory,
        r#"export default pi => {
  for (const name of ['web_search', 'bridge_only']) {
    pi.registerTool({ name, label: name, description: name, parameters: { type: 'object', properties: {}, additionalProperties: false },
      async execute() { return { content: [{ type: 'text', text: name }] }; } });
  }
};"#,
    )
    .unwrap();
    let manifest_path = capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    config.withheld_tools = BTreeSet::from(["web_search".to_owned()]);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        config,
    )
    .await
    .expect("a mirror with one clashing tool still starts");
    let names = process
        .tool_definitions()
        .into_iter()
        .map(|tool| tool.name)
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["bridge_only".to_owned()]);
    assert!(process.shutdown().await);
}

/// A prompt hook must not queue behind a session start that is still running:
/// `wait_until_idle` is the host's way to wait for it, bounded.
#[tokio::test]
async fn wait_until_idle_covers_a_session_start_still_in_flight() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("slow-start.mjs");
    std::fs::write(
        &factory,
        "export default pi => pi.on('session_start', async () => { await new Promise(resolve => setTimeout(resolve, 1500)); });",
    )
    .unwrap();
    let manifest_path = capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        config,
    )
    .await
    .expect("the slow-start mirror starts");
    assert!(
        process.wait_until_idle(Duration::from_millis(50)).await,
        "an idle process never makes a prompt wait"
    );
    let starting = {
        let process = process.clone();
        tokio::spawn(async move { process.start_session_hook_binding("owner-1").await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !process.wait_until_idle(Duration::from_millis(200)).await,
        "the start is still running, so a short wait gives up"
    );
    let begun = Instant::now();
    assert!(process.wait_until_idle(Duration::from_secs(10)).await);
    assert!(
        begun.elapsed() >= Duration::from_millis(500),
        "the wait covered the rest of the start ({:?})",
        begun.elapsed()
    );
    starting
        .await
        .unwrap()
        .expect("the session start settled successfully");
    assert!(process.shutdown().await);
}

/// Native writer/subscribed child measurement, without a provider or App UI.
#[tokio::test]
async fn subscribed_native_stream_observation_work_is_delta_sized() {
    use super::tests::trusted_descriptor;
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("stream-work.mjs");
    std::fs::write(&factory, r#"import { appendFileSync } from 'node:fs';
export default pi => {
  const stringify = JSON.stringify;
  let serialized = 0, updates = 0;
  JSON.stringify = function(value, ...rest) {
    const text = stringify(value, ...rest);
    if (value?.role === 'assistant' || value?.message?.role === 'assistant') serialized += Buffer.byteLength(text);
    return text;
  };
  pi.on('session_start', () => {});
  pi.on('agent_start', () => { serialized = 0; updates = 0; });
  pi.on('message_update', event => { if (event.assistantMessageEvent.type === 'text_delta') updates++; });
  pi.on('agent_end', () => appendFileSync(`${process.cwd()}/work.jsonl`, stringify({ serialized, updates }) + '\n'));
};"#).unwrap();
    let manifest_path = capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    let process = ExtensionProcess::start(
        trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    let session = crate::Session::create(temp.path().join("session.jsonl")).unwrap();
    let key = session.resource_owner_key();
    process
        .set_host_state_with_session(process.current_context().host, &session)
        .unwrap();
    process
        .start_session_hook_binding(key.clone())
        .await
        .unwrap();
    let owner = process
        .current_context_for_resource_owner(key)
        .resource_owner
        .unwrap();
    let connection = read_std_lock(&process.inner.connection).clone();
    let mut work = Vec::new();
    for (trial, count) in [10, 100, 1000].into_iter().enumerate() {
        assert!(connection
            .queue_notification("turn/started", serde_json::json!({"resource_owner":owner})));
        assert!(connection.queue_notification(
            "message/started",
            serde_json::json!({"resource_owner":owner})
        ));
        for _ in 0..count {
            assert!(connection.queue_notification(
                "message/updated",
                serde_json::json!({"resource_owner":owner,"delta":"x".repeat(64)})
            ));
            // Exercise every delta without overrunning the bounded writer. This
            // pacing is not a throughput measurement; only work is counted.
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert!(connection
            .queue_notification("turn/settled", serde_json::json!({"resource_owner":owner})));
        let records = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let text =
                    std::fs::read_to_string(temp.path().join("work.jsonl")).unwrap_or_default();
                let rows = text.lines().collect::<Vec<_>>();
                if rows.len() > trial {
                    break rows
                        .into_iter()
                        .map(|row| serde_json::from_str::<serde_json::Value>(row).unwrap())
                        .collect::<Vec<_>>();
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let measured = &records[trial];
        assert_eq!(measured["updates"], count);
        let bytes = measured["serialized"].as_u64().unwrap();
        eprintln!(
            "native subscribed child: {count} text deltas serialized {bytes} observation bytes"
        );
        work.push((count, bytes));
    }
    assert!(process.shutdown().await);
    for (count, bytes) in work {
        assert!(
            bytes < 2048 + count as u64 * 256,
            "{count} deltas serialized {bytes} bytes"
        );
    }
}
