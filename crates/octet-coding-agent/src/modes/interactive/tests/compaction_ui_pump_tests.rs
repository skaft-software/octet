//! Real process-hook reverse RPCs must drain before compaction can settle.
#![cfg(unix)]

use super::support::*;
use super::*;

const COMPACTION_UI_PROBE: &str = r#"import json
import sys

pending = None
hooks = ['session_before_compact', 'session_compact']

def send(value):
    print(json.dumps(value, separators=(',', ':')), flush=True)

for line in sys.stdin:
    request = json.loads(line)
    method = request.get('method')
    if method == 'initialize':
        offer = request['params']['protocol']
        for feature in ['session_entries', 'composer']:
            assert feature in offer['optional_features']
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': {
            'api_version': '0.4', 'tools': [], 'commands': [],
            'protocol': {'version': '0.4',
                'features': offer['required_features'] + ['session_entries', 'composer'],
                'limits': {'max_concurrent_requests': 1}}}})
    elif method == 'hook/run':
        assert pending is None
        assert request['params']['hook'] in hooks
        pending = request
        send({'jsonrpc': '2.0', 'id': 'composer:' + str(request['id']),
            'method': 'composer/set', 'params': {
                'parent_request_id': request['id'],
                'resource_owner': request['params']['context']['resource_owner'],
                'text': request['params']['hook'] + ': serviced'}})
    elif pending is not None and request.get('id') == 'composer:' + str(pending['id']):
        assert 'result' in request, request
        hook = pending['params']['hook']
        with open(sys.argv[1], 'a', encoding='utf-8') as log:
            log.write(hook + '\n')
        decision = {'action': 'continue'}
        if hook == 'session_before_compact':
            decision = {'action': 'replace_compaction', 'replacement': {
                'summary': 'real process hook handoff',
                'first_kept': pending['params']['payload']['first_kept']}}
        send({'jsonrpc': '2.0', 'id': pending['id'], 'result': {
            'disposition': {'action': 'continue'}, 'session_operation': decision}})
        pending = None
    elif method == 'shutdown':
        send({'jsonrpc': '2.0', 'id': request['id'], 'result': {}})
        break
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn manual_and_requested_compaction_pump_real_before_and_after_hook_composer_requests() {
    for requested in [false, true] {
        let (directory, mut app) = crate::compaction::tests::app_for_estimate();
        let extension_root = directory.path().join("extensions");
        let extension = extension_root.join("compaction-ui-probe");
        std::fs::create_dir_all(&extension).unwrap();
        let script = extension.join("probe.py");
        let log = directory.path().join("receipts.txt");
        std::fs::write(&script, COMPACTION_UI_PROBE).unwrap();
        std::fs::write(
            extension.join("extension.toml"),
            format!(
                r#"name = "compaction-ui-probe"
version = "0.1.0"
api_version = "0.4"
[entrypoint]
command = "python3"
args = [{script}, {log}]
[contributes]
hooks = ["session_before_compact", "session_compact"]
"#,
                script = serde_json::to_string(&script).unwrap(),
                log = serde_json::to_string(&log).unwrap(),
            ),
        )
        .unwrap();
        app.config.extension_paths = vec![extension_root];
        app.config.enabled_extensions = vec!["compaction-ui-probe".into()];
        app.config.invocation_trusted_extensions = vec!["compaction-ui-probe".into()];
        app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
        app.config.sandbox.allow_process = true;
        app.config.sandbox.allow_shell = true;
        app.config.compaction.keep_recent_tokens = 1;
        let session = Session::create(directory.path().join("pump-session.jsonl")).unwrap();
        let mut host = octet_agent::ExtensionHost::new();
        app.executable_extensions = crate::extensions::ExecutableExtensions::discover_and_start(
            &app.config,
            &session,
            &app.model,
            &app.reasoning,
            &app.sessions,
            &mut host,
        );
        assert!(
            app.executable_extensions.summaries().iter().any(|summary| {
                summary.name == "compaction-ui-probe" && summary.running
            }),
            "{}",
            app.executable_extensions.inspect_text()
        );
        app.agent = octet_agent::Agent::new(octet_agent::AgentConfig {
            client: app.client.clone(),
            model: app.model.clone(),
            session,
            system: "system".into(),
            sandbox: octet_agent::SandboxConfig::new(directory.path()),
            effect_broker: octet_agent::EffectBroker::default(),
            extensions: host,
            max_turns: None,
            reasoning: app.reasoning.clone(),
            reasoning_mode: app.reasoning_mode,
            cache_retention: app.config.cache_retention,
            session_id: None,
        })
        .unwrap();
        seed_compaction_session(&mut app.agent);
        app.agent
            .set_compaction_token_mode(AgentCompactionMode::Local, 0.8, 1)
            .unwrap();
        let mut shell = InteractiveShell::test_shell();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        // Without the frontend pump the first reverse call has no consumer;
        // neither process hook may be mistaken for an immediate fake callback.
        tokio::time::timeout(Duration::from_secs(3), async {
            if requested {
                let receipt = compact_extension_session(
                    &mut app,
                    &mut shell,
                    &mut input,
                    None,
                    octet_agent::CancellationToken::default(),
                )
                .await
                .unwrap();
                assert_eq!(receipt.summary, "real process hook handoff");
            } else {
                compact_interactively(&mut app, &mut shell, &mut input, false, None).await;
            }
        })
        .await
        .expect("compaction hooks must settle after actual reverse-RPC responses");
        assert_eq!(
            shell.extension_editor_snapshot().text,
            "session_compact: serviced"
        );
        assert_eq!(
            std::fs::read_to_string(log).unwrap(),
            "session_before_compact\nsession_compact\n"
        );
        let checkpoints = app.agent.session().entries().iter().filter(|entry| {
            matches!(&entry.value, EntryValue::Compaction { summary, .. }
                if summary == "real process hook handoff")
        });
        assert_eq!(checkpoints.count(), 1);
        assert!(app.agent.session().usage_records().is_empty());
        app.executable_extensions.shutdown().await;
    }
}
