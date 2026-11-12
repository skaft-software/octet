//! A busy peer that cannot acknowledge cancellation of a side-effect-free
//! input query keeps running; an effectful request still escalates.
use super::tests::{trusted_descriptor, write_executable_script};
use super::*;
use tempfile::TempDir;

// Answers initialize, then never replies to anything and ignores
// `$/cancelRequest`: the shape of an adapter blocked in a long synchronous parse.
const SCRIPT: &str = r#"#!/usr/bin/env python3
import json, sys
init = json.loads(sys.stdin.readline())
p = init["params"]
protocol = {"version":"0.4", "features":sorted(set(p["protocol"]["required_features"]) | {"request_cancellation"}),
    "limits":{"max_concurrent_requests":4}}
print(json.dumps({"jsonrpc":"2.0","id":init["id"],"result":{"api_version":"0.4","tools":[],"protocol":protocol}}), flush=True)
for line in sys.stdin:
    message = json.loads(line)
    if message.get("method") == "shutdown":
        print(json.dumps({"jsonrpc":"2.0","id":message["id"],"result":{}}), flush=True)
        break
"#;

async fn busy_peer() -> (TempDir, ExtensionProcess) {
    let temp = TempDir::new().unwrap();
    write_executable_script(&temp.path().join("extension.py"), SCRIPT);
    let manifest = ExtensionManifest::parse(
        "name = \"busy-peer\"\nversion = \"0.1.0\"\napi_version = \"0.4\"\n[entrypoint]\ncommand = \"extension.py\"\n",
    )
    .unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.cancellation_grace = Duration::from_millis(150);
    let process = ExtensionProcess::start(trusted_descriptor(temp.path(), manifest), config)
        .await
        .unwrap();
    (temp, process)
}

async fn time_out(process: &ExtensionProcess, method: &str) {
    let connection = Arc::clone(&read_std_lock(&process.inner.connection));
    let result = connection
        .request(method, serde_json::json!({}), Duration::from_millis(100))
        .await;
    assert!(
        matches!(result, Err(ExtensionRuntimeError::Timeout { .. })),
        "{method}: {result:?}"
    );
    // Past the cancellation grace plus scheduling slack.
    tokio::time::sleep(Duration::from_millis(700)).await;
}

#[tokio::test]
async fn an_unacknowledged_input_intercept_cancellation_never_kills_a_busy_peer() {
    assert!(cancellation_never_escalates("ui/terminal-input/intercept"));
    let (_temp, process) = busy_peer().await;
    let generation = process.health_snapshot().generation;
    for _ in 0..3 {
        time_out(&process, "ui/terminal-input/intercept").await;
    }
    assert!(
        process.is_running(),
        "a busy peer must survive keystroke timeouts"
    );
    assert_eq!(process.health_snapshot().generation, generation);
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn an_unacknowledged_effectful_cancellation_still_terminates() {
    assert!(!cancellation_never_escalates("hook/run"));
    let (_temp, process) = busy_peer().await;
    time_out(&process, "hook/run").await;
    assert!(
        !process.is_running(),
        "an effectful request that ignores cancellation is still bounded"
    );
}
