//! Real command-parent idle barriers must settle while the command is pending.
//! The mutation probes use the same idle consumer as ordinary frontend commands.
#![cfg(unix)]

use super::support::*;
use super::*;

const IDLE_COMMAND_PROBE: &str = r#"import json
import sys

pending = None
receipts = []

def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)

def reverse(method, request_id, **params):
    send({'jsonrpc': '2.0', 'id': request_id + ':' + str(pending['id']), 'method': method, 'params': {
        'parent_request_id': pending['id'],
        'resource_owner': pending['params']['context']['resource_owner'], **params}})

def finish(value, context=False):
    send({'jsonrpc': '2.0', 'id': pending['id'], 'result': {
        'text': json.dumps(value), 'notifications': [], 'context': ([{
            'label': 'origin-command', 'content': 'original owner contribution',
            'placement': 'prompt_suffix'}] if context else [])}})

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        offer = request['params']['protocol']
        features = ['session_control_v1', 'composer']
        assert all(feature in offer['optional_features'] for feature in features)
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': {
            'api_version': '0.4', 'tools': [],
            'commands': [{'name': 'idle-proof', 'description': 'Await real idle receipts'}],
            'protocol': {'version': '0.4',
                'features': offer['required_features'] + features,
                'limits': {'max_concurrent_requests': 1}}}})
    elif method == 'command/execute':
        assert pending is None
        pending = request
        receipts = []
        mode = request['params']['arguments']
        if mode == ['mutations']:
            mutations = {}
            reverse('session/create', 'created')
        elif mode == ['reload']:
            reverse('session/reload', 'reloaded')
        elif mode == ['missing']:
            reverse('session/switch', 'missing', session_id='does-not-exist')
        else:
            reverse('session/wait_for_idle', 'idle-first')
    elif method is None and str(request.get('id')).startswith('idle-first:'):
        if pending['params']['arguments'] == ['inactive']:
            assert request['error']['code'] == -32603, request
            finish({'unavailable': True})
            pending = None
        else:
            assert 'result' in request, request
            receipts.append(request['result']['session_id'])
            reverse('composer/set', 'composer-proof', text='idle barrier acknowledged')
    elif method is None and str(request.get('id')).startswith('composer-proof:'):
        assert 'result' in request, request
        reverse('session/wait_for_idle', 'idle-second')
    elif method is None and str(request.get('id')).startswith('idle-second:'):
        assert 'result' in request, request
        receipts.append(request['result']['session_id'])
        finish({'sessions': receipts})
        pending = None
    elif method is None and str(request.get('id')).startswith('created:'):
        mutations['created'] = request['result']['session_id']
        reverse('session/fork', 'forked')
    elif method is None and str(request.get('id')).startswith('forked:'):
        mutations['forked'] = request['result']['session_id']
        reverse('session/wait_for_idle', 'before-switch')
    elif method is None and str(request.get('id')).startswith('before-switch:'):
        mutations['idle'] = request['result']['session_id']
        reverse('session/switch', 'switched', session_id=mutations['created'])
    elif method is None and str(request.get('id')).startswith('switched:'):
        mutations['switched'] = request['result']['session_id']
        reverse('composer/set', 'stale-owner', text='must not reach replacement')
    elif method is None and str(request.get('id')).startswith('stale-owner:'):
        assert request['error']['code'] == -32002, request
        mutations['stale_rejected'] = True
        finish(mutations, context=True)
        pending = None
    elif method is None and str(request.get('id')).startswith('reloaded:'):
        finish(request['result'], context=True)
        pending = None
    elif method is None and str(request.get('id')).startswith('missing:'):
        assert request['error']['code'] == -32603, request
        finish({'missing': True}, context=True)
        pending = None
    elif method == 'shutdown':
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': {}})
        break
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_wait_for_idle_makes_real_progress_without_settling_its_parent() {
    let directory = tempfile::tempdir().unwrap();
    let extension_root = directory.path().join("extensions");
    let extension = extension_root.join("idle-command-probe");
    std::fs::create_dir_all(&extension).unwrap();
    let script = extension.join("probe.py");
    std::fs::write(&script, IDLE_COMMAND_PROBE).unwrap();
    std::fs::write(
        extension.join("extension.toml"),
        format!(
            r#"name = "idle-command-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script}]
[contributes]
commands = ["idle-proof"]
"#,
            script = serde_json::to_string(&script).unwrap(),
        ),
    )
    .unwrap();
    let mut config = terminal_theme_test_config(directory.path().to_owned());
    config.session_dir = directory.path().join("sessions");
    config.extension_paths = vec![extension_root];
    config.enabled_extensions = vec!["idle-command-probe".into()];
    config.invocation_trusted_extensions = vec!["idle-command-probe".into()];
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let session = Session::create(directory.path().join("idle-session.jsonl")).unwrap();
    let session_id = terminal_goal_session_id(&session).unwrap();
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = crate::session_store::SessionStore::new(&config.session_dir, directory.path());
    let mut host = octet_agent::ExtensionHost::new();
    let mut extensions = crate::extensions::ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    assert!(
        extensions
            .summaries()
            .iter()
            .any(|summary| summary.name == "idle-command-probe" && summary.running),
        "{}",
        extensions.inspect_text()
    );
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    // Startup has no driver. Activate only after the negative request settles,
    // then prove the same live process can await two real barriers in one command.
    for active in [false, true] {
        if active {
            extensions.activate_session_lifecycle_driver();
        }
        let dialogs = extensions.lifecycle_snapshot();
        let mut confirmations = InteractiveExtensionConfirmations {
            shell: &mut shell,
            input: &mut input,
            dialogs: &dialogs,
        };
        let arguments = if active {
            vec![]
        } else {
            vec!["inactive".into()]
        };
        let output = tokio::time::timeout(
            Duration::from_secs(3),
            extensions.execute_command_with_confirmation(
                "idle-proof",
                arguments,
                &mut confirmations,
            ),
        )
        .await;
        // Reap the process before reporting a timeout or command failure.
        if !matches!(&output, Ok(Ok(Some(_)))) {
            extensions.shutdown().await;
        }
        let output = output
            .expect("command awaited an idle barrier that its frontend did not consume")
            .unwrap()
            .expect("real command remains registered");
        let receipt: serde_json::Value = serde_json::from_str(&output).unwrap();
        if active {
            assert_eq!(
                receipt,
                serde_json::json!({"sessions": [&session_id, &session_id]})
            );
            assert_eq!(
                shell.extension_editor_snapshot().text,
                "idle barrier acknowledged"
            );
        } else {
            assert_eq!(receipt, serde_json::json!({"unavailable": true}));
            assert_eq!(shell.extension_editor_snapshot().text, "");
        }
        assert!(extensions.next_session_lifecycle_request().is_none());
    }
    extensions.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_awaited_mutations_commit_before_reply_and_fence_old_contributions() {
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let extension_root = directory.path().join("extensions");
    let extension = extension_root.join("idle-command-probe");
    std::fs::create_dir_all(&extension).unwrap();
    let script = extension.join("probe.py");
    std::fs::write(&script, IDLE_COMMAND_PROBE).unwrap();
    std::fs::write(
        extension.join("extension.toml"),
        format!(
            r#"name = "idle-command-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{}]
[contributes]
commands = ["idle-proof"]
"#,
            serde_json::to_string(&script).unwrap()
        ),
    )
    .unwrap();
    app.config.extension_paths = vec![extension_root];
    app.config.enabled_extensions = vec!["idle-command-probe".into()];
    app.config.invocation_trusted_extensions = vec!["idle-command-probe".into()];
    app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = true;
    let original_entry = app
        .agent
        .session_mut()
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    let original_path = app.agent.session().path().to_owned();
    let original_id = terminal_goal_session_id(app.agent.session()).unwrap();
    let mut host = octet_agent::ExtensionHost::new();
    app.executable_extensions = crate::extensions::ExecutableExtensions::discover_and_start(
        &app.config,
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
        &mut host,
    );
    assert!(
        app.executable_extensions
            .summaries()
            .iter()
            .any(|summary| { summary.name == "idle-command-probe" && summary.running }),
        "{}",
        app.executable_extensions.inspect_text()
    );
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    let mut switched_id = None;
    for mode in ["missing", "mutations", "reload"] {
        let dialogs = app.executable_extensions.lifecycle_snapshot();
        let result = {
            let mut frontend = InteractiveExtensionConfirmations {
                shell: &mut shell,
                input: &mut input,
                dialogs: &dialogs,
            };
            tokio::time::timeout(
                Duration::from_secs(5),
                run_interactive_extension_command(
                    &mut app,
                    &mut frontend,
                    Some("idle-command-probe"),
                    "idle-proof",
                    vec![mode.into()],
                    false,
                    0,
                ),
            )
            .await
        };
        if !matches!(&result, Ok(Ok(Some(_)))) {
            app.executable_extensions.shutdown().await;
        }
        let output = result
            .expect("command must not await its own idle owner")
            .unwrap()
            .expect("registered command");
        let receipt: serde_json::Value =
            serde_json::from_str(output.lines().next().unwrap()).unwrap();
        match mode {
            "missing" => {
                assert_eq!(receipt, serde_json::json!({"missing": true}));
                assert_eq!(app.agent.session().path(), original_path);
            }
            "mutations" => {
                let created = receipt["created"].as_str().unwrap();
                let forked = receipt["forked"].as_str().unwrap();
                assert_eq!(receipt["idle"], original_id);
                assert_eq!(receipt["switched"], created);
                assert_eq!(receipt["stale_rejected"], true);
                assert_ne!(created, forked);
                assert_eq!(
                    terminal_goal_session_id(app.agent.session()).unwrap(),
                    created
                );
                assert_eq!(app.goal_session_id, created);
                assert!(Session::open_read_only(app.sessions.path_by_id(created).unwrap()).is_ok());
                let fork =
                    Session::open_read_only(app.sessions.path_by_id(forked).unwrap()).unwrap();
                assert!(
                    fork.entry(&original_entry).is_some(),
                    "fork copied the actual durable head"
                );
                assert!(Session::open_read_only(&original_path)
                    .unwrap()
                    .entry(&original_entry)
                    .is_some());
                assert_ne!(
                    shell.extension_editor_snapshot().text,
                    "must not reach replacement"
                );
                switched_id = Some(created.to_owned());
            }
            "reload" => {
                assert_eq!(receipt["session_id"].as_str(), switched_id.as_deref());
                assert_eq!(
                    terminal_goal_session_id(app.agent.session()).unwrap(),
                    switched_id.clone().unwrap()
                );
            }
            _ => unreachable!(),
        }
        let composed = app
            .executable_extensions
            .compose_prompt("system", "prompt".into())
            .await
            .unwrap();
        // Failed replacement leaves origin authority intact; switch and even a
        // same-path reload must discard the old command's returned context.
        assert_eq!(
            composed.pending_context_count,
            usize::from(mode == "missing")
        );
        app.executable_extensions
            .commit_prompt_context(composed.pending_context_count);
        assert!(app
            .executable_extensions
            .next_session_lifecycle_request()
            .is_none());
    }
    app.executable_extensions.shutdown().await;
}
