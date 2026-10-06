//! Real native owner dispatch and the reviewed adapter; no model/network calls.
use super::tests::Capture;
use super::*;
use crate::extension_process::session_leaf::transport;
use crate::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
use octet_ai::{CacheRetention, ModelCatalog, ModelId, ReasoningMode};

async fn adapter(temp: &tempfile::TempDir) -> ExtensionProcess {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
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
        .arg(root.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(&bundle)
        .arg(&factory)
        .env("HOME", temp.path())
        .current_dir(temp.path())
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "adapter capture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let manifest_path = bundle.join("extension.toml");
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path,
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap()
}

/// One real Agent run over `session` with the reviewed adapter, returning the
/// count the extension actually read through `ctx.sessionManager`.
async fn agent_reads_all(
    temp: &tempfile::TempDir,
    process: &ExtensionProcess,
    session: Session,
) -> usize {
    let mut host = ExtensionHost::new();
    process.register(&mut host);
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap();
    let reasoning = octet_ai::select_auxiliary_reasoning(&model).unwrap();
    let client = octet_ai::AiClient::new();
    let captures = Arc::new(StdMutex::new(Vec::new()));
    client.register_host_stream_transport(
        model.endpoint.id.clone(),
        Arc::new(Capture(captures.clone())),
    );
    let mut agent = Agent::new(AgentConfig {
        client,
        model,
        session,
        extensions: host,
        system: "canonical system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        max_turns: Some(1),
        reasoning,
        reasoning_mode: ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: Some("history-acceptance".into()),
    })
    .unwrap();
    let result = agent.complete("read every history entry").await;
    assert_eq!(
        result
            .expect("owner-bound dispatch must hydrate complete history")
            .text,
        "answer"
    );
    assert_eq!(captures.lock().unwrap().len(), 1);
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("history-read.json")).unwrap())
            .unwrap();
    report["count"].as_u64().unwrap() as usize
}

/// Append `count` 8,000-character user messages: each is far past the legacy
/// 16 KiB private-entry bound and 40 of them exceed the 512 KiB mirror.
fn append_history(session: &mut Session, count: usize) {
    for index in 0..count {
        session
            .append(crate::session::EntryValue::Message(
                octet_ai::Message::User(octet_ai::UserMessage {
                    content: vec![octet_ai::UserPart::Text(format!(
                        "{index:04}{}",
                        "x".repeat(7996)
                    ))],
                }),
            ))
            .unwrap();
    }
}

async fn complete_history(count: usize, publish: bool) {
    let temp = tempfile::tempdir().unwrap();
    // Each test process has a private manifest/env and private durable Session.
    let process = adapter(&temp).await;
    let mut session = Session::create(temp.path().join("history.jsonl")).unwrap();
    append_history(&mut session, count);
    if publish {
        process
            .set_host_state_with_session(ExtensionHostState::default(), &session)
            .expect("complete history publication must not be a whole-frame mirror");
    }
    assert_eq!(agent_reads_all(&temp, &process, session).await, count + 1);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn real_adapter_complete_history_40_messages() {
    complete_history(40, true).await;
}

#[tokio::test]
async fn real_adapter_complete_history_80_messages() {
    complete_history(80, false).await;
}

#[tokio::test]
async fn real_adapter_mid_transfer_cancel_leaves_no_stale_or_partial_view() {
    let temp = tempfile::tempdir().unwrap();
    let process = adapter(&temp).await;
    let mut session = Session::create(temp.path().join("history.jsonl")).unwrap();
    append_history(&mut session, 200);
    let namespace = process.descriptor().manifest.name.clone();
    let owner = process
        .current_context_for_resource_owner(session.resource_owner_key())
        .resource_owner
        .unwrap();
    let connection = read_std_lock(&process.inner.connection).clone();
    let profile = connection.session_profile().unwrap();
    // The real publication path invalidates first, then prepares the complete
    // counted document on the wire.
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .expect("counted publication is never a whole-frame mirror");
    let parent = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let found = {
                let pending = lock_std_mutex(&connection.pending);
                pending
                    .iter()
                    .find(|(_, request)| request.method == transport::PREPARE)
                    .map(|(id, _)| *id)
            };
            if let Some(id) = found {
                return id;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if lock_std_mutex(&connection.session_leaf.transport)
                .transfer_offset(parent)
                .is_some_and(|offset| offset > 0)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    connection.cancel_request(parent, "mid-transfer cancel");
    // Cancellation settles the staged handle: no transfer of that publication
    // stays readable, and nothing partial is retained for the owner.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if lock_std_mutex(&connection.session_leaf.transport).in_flight(parent) == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let reference = {
        let mut store = lock_std_mutex(&connection.session_leaf.transport);
        let revision = store.next_revision().unwrap();
        store
            .history(
                &session,
                &namespace,
                owner.clone(),
                &profile,
                None,
                revision,
            )
            .unwrap()
    };
    {
        let transport = lock_std_mutex(&connection.session_leaf.transport);
        // Exactly one host publication is retained (this reference), so the
        // cancelled transfer left no duplicated or partial reservation.
        assert_eq!(
            transport.retained().2,
            2,
            "one held publication plus the reference"
        );
        assert_eq!(
            transport.retained().0,
            reference.descriptor.bytes * 2,
            "no partial bytes are retained for the cancelled publication"
        );
        let held = transport.current.get(&owner).and_then(Option::as_ref);
        if let Some(held) = held {
            assert_eq!(held.descriptor.bytes, reference.descriptor.bytes);
            assert_eq!(held.descriptor.sha256, reference.descriptor.sha256);
            assert_eq!(held.descriptor.entry_count, session.entries().len());
        }
    }
    // The same complete document still publishes exactly on the next attempt.
    let reference = {
        let mut store = lock_std_mutex(&connection.session_leaf.transport);
        let revision = store.next_revision().unwrap();
        store
            .history(
                &session,
                &namespace,
                owner.clone(),
                &profile,
                None,
                revision,
            )
            .unwrap()
    };
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .expect("bounded failfast, never truncation");
    let current = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let published = lock_std_mutex(&connection.session_leaf.transport)
                .current
                .get(&owner)
                .cloned()
                .flatten();
            if let Some(view) = published {
                return view;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the complete document publishes after cancellation");
    assert_eq!(current.descriptor.bytes, reference.descriptor.bytes);
    assert_eq!(current.descriptor.sha256, reference.descriptor.sha256);
    assert_eq!(current.descriptor.entry_count, session.entries().len());
    assert_eq!(current.descriptor.branch_count, session.entries().len());
    drop(reference);
    // The cancelled transfer left the session complete and readable through
    // the real adapter: a later hook still reads every entry.
    let read = agent_reads_all(&temp, &process, session).await;
    assert_eq!(read, 201);
}
