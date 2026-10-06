//! Real native owner dispatch and the reviewed adapter; no model/network calls.
use super::tests::Capture;
use super::*;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
use octet_ai::{CacheRetention, ModelCatalog, ModelId, ReasoningConfig, ReasoningMode};

async fn adapter(temp: &tempfile::TempDir) -> ExtensionProcess {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize().unwrap();
    let factory = temp.path().join("history.mjs");
    std::fs::write(&factory, r#"
import { writeFileSync } from 'node:fs';
export default function(pi) {
  pi.on('context', (event, ctx) => {
    const entries = ctx.sessionManager.getEntries();
    const branch = ctx.sessionManager.getBranch();
    const messages = entries.filter(entry => entry.type === 'message');
    const expected = event.messages.length - 1;
    const check = (value, message) => { if (!value) throw Object.assign(new Error(message), { code: -32602 }); };
    check(messages.length === expected + 1, 'complete entries');
    check(branch.length === entries.length, 'complete branch');
    for (let i = 0; i < expected; i++) {
      const entry = messages[i];
      check(entry.message.content[0].text === String(i).padStart(4, '0') + 'x'.repeat(7996), 'exact message content');
      check(ctx.sessionManager.getEntry(entry.id).id === entry.id, 'entry lookup');
    }
    check(ctx.sessionManager.getLeafId() === branch.at(-1).id, 'exact head');
    writeFileSync('history-read.json', JSON.stringify({ count: messages.length, branch: branch.length }));
  });
}
"#).unwrap();
    let bundle = temp.path().join("octet-pi-compat");
    let output = tokio::process::Command::new("node")
        .arg(root.join("configure.mjs")).arg("--reviewed").arg("--output")
        .arg(&bundle).arg(&factory).env("HOME", temp.path())
        .current_dir(temp.path()).kill_on_drop(true).output().await.unwrap();
    assert!(output.status.success(), "adapter capture: {}", String::from_utf8_lossy(&output.stderr));
    let manifest_path = bundle.join("extension.toml");
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    ExtensionProcess::start(DiscoveredExtension {
        manifest, manifest_path, source: ExtensionSource::Explicit,
        activation: ExtensionActivation { enabled: true, trust: ExtensionTrust::Trusted },
    }, ExtensionRuntimeConfig::new(temp.path())).await.unwrap()
}

async fn complete_history(count: usize, publish: bool) {
    let temp = tempfile::tempdir().unwrap();
    // Each test process has a private manifest/env and private durable Session.
    let process = adapter(&temp).await;
    let mut session = Session::create(temp.path().join("history.jsonl")).unwrap();
    for index in 0..count {
        session.append(crate::session::EntryValue::Message(octet_ai::Message::User(octet_ai::UserMessage { content: vec![octet_ai::UserPart::Text(format!("{index:04}{}", "x".repeat(7996)))] }))).unwrap();
    }
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    if publish {
        process.set_host_state_with_session(ExtensionHostState::default(), &session)
            .expect("complete history publication must not be a whole-frame mirror");
    }
    let model = ModelCatalog::builtin().unwrap().resolve(&ModelId("gpt-4o-mini".into())).unwrap();
    let client = octet_ai::AiClient::new();
    let captures = Arc::new(StdMutex::new(Vec::new()));
    client.register_host_stream_transport(model.endpoint.id.clone(), Arc::new(Capture(captures.clone())));
    let mut agent = Agent::new(AgentConfig {
        client, model, session, extensions: host, system: "canonical system".into(),
        sandbox: SandboxConfig::new(temp.path()), effect_broker: EffectBroker::default(),
        max_turns: Some(1), reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard, cache_retention: CacheRetention::Short,
        session_id: Some("history-acceptance".into()),
    }).unwrap();
    let result = agent.complete("read every history entry").await;
    assert!(process.shutdown().await);
    assert_eq!(result.expect("owner-bound dispatch must hydrate complete history").text, "answer");
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(temp.path().join("history-read.json")).unwrap()).unwrap();
    assert_eq!(report["count"], count + 1);
    assert_eq!(captures.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn real_adapter_complete_history_40_messages() { complete_history(40, true).await; }

#[tokio::test]
async fn real_adapter_complete_history_80_messages() { complete_history(80, false).await; }
