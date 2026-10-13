use super::*;
use octet_agent::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionRuntimeConfig,
    ExtensionSource, ExtensionTrust,
};
use std::os::unix::fs::PermissionsExt;

const FIXTURE: &str = r#"#!/usr/bin/env python3
import json, os, sys, threading, time
lock = threading.Lock()
def send(value):
    with lock: print(json.dumps(dict(jsonrpc='2.0', **value)), flush=True)
def render(message):
    p = message['params']
    with lock:
        with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'renders.jsonl'), 'a') as out: out.write(json.dumps(p) + '\n')
    try:
        with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'mode')) as mode_file: mode = mode_file.read()
    except FileNotFoundError: mode = 'normal'
    if mode == 'crash': os._exit(1)
    if mode == 'late': time.sleep(.8)
    value = dict(registered=True, lines=['CUSTOM-' + str(p['width'])], markdown=None, render_shell=None)
    if p['render']['kind'] == 'markdown': value.update(lines=None, markdown='TRANSFORMED')
    send(dict(id=message['id'], result=value))
for line in sys.stdin:
    m = json.loads(line)
    if m.get('method') == 'initialize':
        send(dict(id=m['id'], result=dict(api_version='0.4', tools=[], commands=[], protocol=dict(version='0.4', features=['request_cancellation','content_parts','remote_ui','transcript_render_v1'], limits=dict(max_concurrent_requests=8)))))
    elif m.get('method') == 'transcript/render': threading.Thread(target=render, args=(m,), daemon=True).start()
    elif m.get('method') == 'shutdown':
        send(dict(id=m['id'], result={}))
        break
"#;
async fn fixture(temp: &tempfile::TempDir) -> ExecutableExtensions {
    let script = temp.path().join("renderer.py");
    std::fs::write(&script, FIXTURE).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest::parse(
        r#"
name = "renderer"
version = "0.4.0"
api_version = "0.4"
[entrypoint]
command = "renderer.py"
"#,
    )
    .unwrap();
    let wake = Arc::new(tokio::sync::Notify::new());
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.remote_ui = Some(wake.clone());
    runtime.transcript_render = true;
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
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
    extensions.remote_ui_wake = Some(wake);
    extensions.resource_owner = Some("renderer-session".into());
    extensions
}
fn append(shell: &mut InteractiveShell, id: &str) {
    shell.append_custom_transcript_message(
        &octet_agent::EntryId(id.into()),
        &octet_agent::session::CustomMessage {
            custom_type: "notice".into(),
            content: octet_agent::session::CustomMessageContent::Text("canonical fallback".into()),
            display: true,
            details: Some(serde_json::json!({"n":7})),
        },
        100,
    );
}
async fn settle(extensions: &mut ExecutableExtensions, shell: &mut InteractiveShell) {
    tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            extensions.drain_events_for_shell(shell);
            if extensions.transcript_renderers.jobs.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn transcript_jobs_are_bounded_and_timeouts_keep_default_without_retry_storms() {
    let temp = tempfile::tempdir().unwrap();
    let mut extensions = fixture(&temp).await;
    std::fs::write(temp.path().join("mode"), "late").unwrap();
    let mut shell = InteractiveShell::test_shell();
    for n in 0..9 {
        append(&mut shell, &format!("source-{n}"));
    }
    extensions.drain_events_for_shell(&mut shell);
    assert_eq!(extensions.transcript_renderers.jobs.len(), MAX_JOBS);
    settle(&mut extensions, &mut shell).await;
    let requests = std::fs::read_to_string(temp.path().join("renders.jsonl"))
        .unwrap()
        .lines()
        .count();
    assert_eq!(requests, 9);
    for _ in 0..10 {
        extensions.drain_events_for_shell(&mut shell);
    }
    assert!(extensions.transcript_renderers.jobs.is_empty());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("renders.jsonl"))
            .unwrap()
            .lines()
            .count(),
        requests
    );
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(!frame.contains("CUSTOM-"));
    assert!(frame.contains("canonical fallback"));
    extensions.shutdown().await;
}

#[tokio::test]
async fn transcript_jobs_fence_late_resize_and_owner_crash_and_private_namespaces() {
    let temp = tempfile::tempdir().unwrap();
    let mut extensions = fixture(&temp).await;
    std::fs::write(temp.path().join("mode"), "late").unwrap();
    let mut shell = InteractiveShell::test_shell();
    append(&mut shell, "source");
    shell.append_private_transcript_entry(
        "other-extension".into(),
        serde_json::json!({"id":"private-other","customType":"private","data":"SECRET"}),
    );
    extensions.drain_events_for_shell(&mut shell);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !temp.path().join("renders.jsonl").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    shell.set_size(60, 24);
    let width = shell
        .transcript_render_candidates()
        .into_iter()
        .find(|candidate| candidate.source_id == "source")
        .unwrap()
        .key
        .width;
    std::fs::write(temp.path().join("mode"), "normal").unwrap();
    settle(&mut extensions, &mut shell).await;
    tokio::time::sleep(Duration::from_millis(850)).await;
    extensions.drain_events_for_shell(&mut shell);
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(frame.contains(&format!("CUSTOM-{width}")));
    let requests = std::fs::read_to_string(temp.path().join("renders.jsonl")).unwrap();
    assert!(!requests.contains("SECRET"));
    assert!(!requests.contains("private-other"));
    std::fs::write(temp.path().join("mode"), "crash").unwrap();
    shell.set_verbose_tools(true);
    settle(&mut extensions, &mut shell).await;
    extensions.drain_events_for_shell(&mut shell);
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(!frame.contains("CUSTOM-"));
    assert!(frame.contains("canonical fallback"));
    extensions.shutdown().await;
}
