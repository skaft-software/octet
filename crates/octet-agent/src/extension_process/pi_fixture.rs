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
