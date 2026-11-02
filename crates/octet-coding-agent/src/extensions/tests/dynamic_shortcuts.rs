//! Extensions registering their own key bindings at runtime.
//!
//! Covers host-binding registration (first owner wins, reserved host bindings are
//! never taken over) and dispatch of a registered shortcut into the owning
//! process. Split from the terminal-grant tests because the ownership rule here
//! is about *key bindings* rather than terminal grants.

use super::*;

fn shortcut(key: &str, name: &str) -> ShortcutDefinition {
    ShortcutDefinition {
        key: key.to_owned(),
        name: name.to_owned(),
        description: format!("{name} action"),
    }
}

#[test]
fn shortcut_registration_keeps_host_bindings_and_first_owner() {
    let mut registered = Vec::new();
    let mut diagnostics = Vec::new();
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-one",
        &shortcut("ctrl+shift+p", "first"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-two",
        &shortcut("control+shift+p", "second"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-three",
        &shortcut("ctrl+d", "close"),
    );
    register_extension_shortcut(
        &mut registered,
        &mut diagnostics,
        "trusted-four",
        &shortcut("shift+p", "editor"),
    );

    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].invocation.extension, "trusted-one");
    assert_eq!(registered[0].invocation.name, "first");
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("conflicts")));
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("reserved")));
    assert!(diagnostics
        .iter()
        .any(|message| message.contains("must include ctrl, alt, or super")));
}

#[cfg(unix)]
#[tokio::test]
async fn shortcut_dispatch_runs_registered_action_in_background() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("shortcut-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[],"shortcuts":[{"key":"ctrl+shift+p","name":"open_panel","description":"Open the panel"}],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
IFS= read -r shortcut
case "$shortcut" in
  *'"method":"shortcut/execute"'*'"name":"open_panel"'*) ;;
  *) exit 41 ;;
esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"text":"shortcut output","notifications":[],"context":[]}}'
IFS= read -r shutdown
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{}}'
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"name = "shortcut-fixture"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "shortcut-fixture.sh"
[contributes]
shortcuts = [{ key = "ctrl+shift+p", name = "open_panel", description = "Open the panel" }]
"#,
    )
    .unwrap();
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(temp.path()),
    )
    .await
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process);
    let (shortcuts, diagnostics) = register_extension_shortcuts(&extensions.processes);
    assert!(diagnostics.is_empty());
    extensions.shortcuts = shortcuts;

    let event = Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('P'),
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
    ));
    let invocation = extensions
        .dispatch_shortcut_for_event(&event)
        .expect("registered shortcut is dispatched");
    assert_eq!(invocation.extension, "shortcut-fixture");
    assert_eq!(invocation.name, "open_panel");

    let messages = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = extensions.drain_background_updates();
            if !updates.shortcut_messages.is_empty() {
                return updates.shortcut_messages;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("shortcut result was not delivered");
    assert_eq!(messages, vec!["shortcut output"]);
    extensions.shutdown().await;
}

#[cfg(unix)]
async fn routed_dynamic_fixture() -> (tempfile::TempDir, ExecutableExtensions) {
    use std::os::unix::fs::PermissionsExt as _;
    let temp = tempfile::tempdir().unwrap();
    let script = temp.path().join("dynamic.py");
    std::fs::write(&script, r#"#!/usr/bin/env python3
import json,sys,os
pending=[]

def send(v): print(json.dumps(v),flush=True)
def reply(m,r): send({'jsonrpc':'2.0','id':m['id'],'result':r})
def control(m):
 if m.get('method')=='session/snapshot/prepare':
  d=m['params']['snapshot']; reply(m,{'accepted':True,'transfer_id':d['transfer_id'],'view_revision':d['view_revision'],'head':d['head'],'sha256':d['sha256']}); return True
 if m.get('method')=='shutdown': reply(m,{}); sys.exit(0)
 return False
def wait_child(ident):
 for line in sys.stdin:
  m=json.loads(line)
  if control(m): continue
  if m.get('id')==ident and ('result' in m or 'error' in m): return m
  if m.get('method') in ('shortcut/execute','command/execute'): pending.append(m)
init=json.loads(sys.stdin.readline()); offer=init['params']['protocol']
features=offer['required_features']+['shortcuts','session_entries','session_owner_routes_v1','session_snapshot_transport_v1','session_append_receipts_v1']
reply(init,{'api_version':'0.4','tools':[],'commands':[{'name':'prime','description':'Register shortcut'}],
 'shortcuts':[{'key':'ctrl+shift+p','name':'static','description':'Static action'}],
 'protocol':{'version':'0.4','features':features,'limits':{'max_concurrent_requests':4,'max_message_bytes':offer['limits']['max_message_bytes']},'session_snapshot_transport_v1':offer['session_snapshot_transport_v1']}})
while True:
 m=pending.pop(0) if pending else json.loads(sys.stdin.readline())
 method=m.get('method'); p=m.get('params',{})
 if control(m): continue
 if method=='command/execute':
  send({'jsonrpc':'2.0','id':'register','method':'shortcut/register','params':{'parent_request_id':m['id'],'id':'late','key':'ctrl+alt+f','description':'Late action'}})
  ack=wait_child('register'); assert ack['result']=={},ack; reply(m,{'text':'registered'})
 elif method=='shortcut/execute':
  with open(os.path.join(os.environ['OCTET_WORKSPACE'],'invocations'),'a') as f: f.write(str(m['id'])+'\n')
  if os.path.exists(os.path.join(os.environ['OCTET_WORKSPACE'],'hang')): continue
  ident='confirm-'+str(m['id'])
  send({'jsonrpc':'2.0','id':ident,'method':'confirmation/request','params':{'parent_request_id':m['id'],'prompt':'Approve background action?','destructive':True,'default':True}})
  ack=wait_child(ident)
  with open(os.path.join(os.environ['OCTET_WORKSPACE'],'confirmations'),'a') as f: f.write(json.dumps(ack)+'\n')
  reply(m,{'text':'unapproved output'})
 elif method=='shortcut/trigger':
  raise RuntimeError('routed dynamic shortcut must have a real request parent')
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"name = "routed-dynamic"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "dynamic.py"
[contributes]
commands = ["prime"]
shortcuts = [{key = "ctrl+shift+p", name = "static", description = "Static action"}]
confirmations = true
"#,
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(1);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join(EXTENSION_MANIFEST_FILENAME),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        config,
    )
    .await
    .unwrap();
    let session = Session::create(temp.path().join("session.jsonl")).unwrap();
    process
        .set_host_state_with_session(ExtensionHostState::default(), &session)
        .unwrap();
    let context = process.current_context_for_resource_owner(session.resource_owner_key());
    process
        .wait_for_session_snapshot_prepared(
            context.resource_owner.as_ref().unwrap(),
            Duration::from_secs(5),
            &CancellationToken::default(),
        )
        .await
        .unwrap();
    let mut events = process.subscribe();
    let mut extensions = ExecutableExtensions::default();
    extensions.processes.push(process.clone());
    extensions.receivers.push(process.subscribe());
    extensions.resource_owner = Some(session.resource_owner_key());
    let call = tokio::spawn({
        let process = process.clone();
        async move { process.execute_command("prime", vec![], context).await }
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ExtensionEvent::ShortcutRequested { request_id, generation, shortcut_id, key, description, .. } = events.recv().await.unwrap() {
                let outcome = extensions.register_dynamic_shortcut("routed-dynamic", process.clone(), generation, shortcut_id, &key, description);
                assert!(matches!(&outcome, ExtensionRequestOutcome::Ok(value) if value == &serde_json::json!({})));
                process.respond_to_extension_request(request_id, generation, outcome).await.unwrap();
                break;
            }
        }
    }).await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().text, "registered");
    (temp, extensions)
}

#[cfg(unix)]
fn routed_dynamic_key() -> Event {
    Event::Key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('f'),
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT,
    ))
}

#[cfg(unix)]
#[tokio::test]
async fn routed_dynamic_shortcuts_use_bounded_headless_requests_and_deny_confirmation() {
    let (temp, mut extensions) = routed_dynamic_fixture().await;
    let process = extensions.processes[0].clone();
    let generation = process.health_snapshot().generation;
    for (id, key) in [
        ("static", "ctrl+alt+e"),
        ("late", "ctrl+alt+e"),
        ("other", "ctrl+alt+f"),
        ("reserved", "ctrl+alt+d"),
    ] {
        assert!(matches!(
            extensions.register_dynamic_shortcut(
                "routed-dynamic",
                process.clone(),
                generation,
                id.into(),
                key,
                "Refused action".into()
            ),
            ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, _)
        ));
    }
    assert_eq!(extensions.dynamic_shortcuts.len(), 1);
    for _ in 0..SHORTCUT_TASK_CONCURRENCY {
        assert!(extensions
            .dispatch_shortcut_for_event(&routed_dynamic_key())
            .is_some());
    }
    assert_eq!(extensions.shortcut_tasks.len(), SHORTCUT_TASK_CONCURRENCY);
    assert!(extensions
        .dispatch_shortcut_for_event(&routed_dynamic_key())
        .is_none());
    tokio::time::timeout(Duration::from_secs(5), async {
        while extensions
            .shortcut_tasks
            .iter()
            .any(|task| !task.is_finished())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let updates = extensions.drain_background_updates();
    assert_eq!(updates.shortcut_messages.len(), SHORTCUT_TASK_CONCURRENCY);
    assert!(updates.shortcut_messages.iter().all(|message| message
        .contains("requires an interactive confirmation")
        && !message.contains("unapproved output")));
    let confirmations = std::fs::read_to_string(temp.path().join("confirmations")).unwrap();
    assert_eq!(confirmations.lines().count(), SHORTCUT_TASK_CONCURRENCY);
    for line in confirmations.lines() {
        let ack: Value = serde_json::from_str(line).unwrap();
        assert_eq!(ack["result"]["confirmed"], false);
    }
    assert_eq!(
        std::fs::read_to_string(temp.path().join("invocations"))
            .unwrap()
            .lines()
            .count(),
        SHORTCUT_TASK_CONCURRENCY
    );
    assert!(Session::open(temp.path().join("session.jsonl"))
        .unwrap()
        .entries()
        .is_empty());
    extensions.dynamic_shortcuts[0].generation -= 1;
    assert!(extensions
        .dispatch_shortcut_for_event(&routed_dynamic_key())
        .is_none());
    assert!(extensions.dynamic_shortcuts.is_empty());
    extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn routed_dynamic_shortcut_expiry_discards_authority_without_session_effects() {
    let (temp, mut extensions) = routed_dynamic_fixture().await;
    std::fs::write(temp.path().join("hang"), "").unwrap();
    assert!(extensions
        .dispatch_shortcut_for_event(&routed_dynamic_key())
        .is_some());
    tokio::time::timeout(Duration::from_secs(5), async {
        while extensions
            .shortcut_tasks
            .iter()
            .any(|task| !task.is_finished())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let updates = extensions.drain_background_updates();
    assert_eq!(updates.shortcut_messages.len(), 1);
    assert!(updates.shortcut_messages[0].contains("failed"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("invocations"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    assert!(Session::open(temp.path().join("session.jsonl"))
        .unwrap()
        .entries()
        .is_empty());
    extensions.shutdown().await;
}
