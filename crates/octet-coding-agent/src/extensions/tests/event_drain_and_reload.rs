//! The event-drain budget and extension reload ordering.
//!
//! Covers the per-extension frame budget honoured under a slow consumer, a hung
//! renderer RPC that must never block the interactive boundary (the handler
//! here cancels immediately), and reloads whose lost requests resolve to stopped
//! or generation-mismatched queue entries while concurrent reloads still report
//! in stable process order.

#[cfg(unix)]
use super::support::*;
use super::*;

#[tokio::test]
async fn event_drain_obeys_the_per_extension_frame_budget() {
    let (sender, receiver) = broadcast::channel(128);
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(receiver);
    for index in 0..100 {
        sender
            .send(ExtensionEvent::Diagnostic {
                message: format!("diagnostic-{index}"),
            })
            .unwrap();
    }

    extensions.drain_events();

    assert_eq!(
        extensions.diagnostics.len(),
        EVENT_DRAIN_PER_RECEIVER_BUDGET
    );
    assert_eq!(
        extensions.receivers[0].len(),
        100 - EVENT_DRAIN_PER_RECEIVER_BUDGET
    );
}

#[cfg(unix)]
#[tokio::test]
async fn hung_renderer_rpc_never_blocks_the_interactive_boundary() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::{Duration, Instant};

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("hung-ui-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
id=$(printf '%s\n' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[{"name":"hang","description":"Never responds"}]}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"shutdown"'*)
      id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "hung-ui-fixture"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "hung-ui-fixture.sh"

[contributes]
commands = ["hang"]
ui = ["status"]
tool_renderers = ["slow"]
"#,
    )
    .unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join("extension.toml"),
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
    extensions.processes.push(process.clone());
    let started = Instant::now();
    assert!(extensions.request_tool_render(
        ToolCallId("call-1".into()),
        "slow",
        serde_json::json!({}),
        Some("output".into()),
        false,
    ));
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "scheduling extension work blocked the caller"
    );

    tokio::time::sleep(Duration::from_millis(700)).await;
    let updates = extensions.drain_background_updates();
    assert!(updates.rendered_tools.is_empty());
    assert!(extensions
        .diagnostics
        .iter()
        .any(|message| message.contains("renderer") && message.contains("exceeded")));

    let command_started = Instant::now();
    let error = extensions
        .execute_command_with_confirmation("hang", Vec::new(), &mut ImmediateCancellationHandler)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cancelled"));
    assert!(command_started.elapsed() < Duration::from_millis(100));

    tokio::time::timeout(Duration::from_secs(2), extensions.shutdown())
        .await
        .expect("extension shutdown exceeded its global bound");
    assert!(!process.is_running());
}

#[cfg(unix)]
#[tokio::test]
async fn reload_request_losses_use_stopped_or_generation_mismatched_queue_entries() {
    let temp = tempfile::tempdir().unwrap();
    let (mut extensions, _) = lifecycle_fixture(&temp).await;
    let process = extensions.processes[0].clone();
    let generation = process.health_snapshot().generation;
    let pending = |id, generation| PendingHostRequest {
        process: process.clone(),
        request_id: ExtensionRequestId::Number(id),
        generation,
        operation: HostRequestOperation::Composer(ExtensionComposerOperation::Get),
    };
    assert!(extensions.discard_stale_host_requests().is_empty());
    extensions
        .pending_host_requests
        .push_back(pending(1, generation));
    extensions
        .pending_host_requests
        .push_back(pending(2, generation + 1));
    assert_eq!(extensions.pending_host_request_count(), 2);
    assert_eq!(extensions.discard_stale_host_requests().len(), 1);
    assert_eq!(extensions.pending_host_request_count(), 1);
    assert!(extensions.discard_stale_host_requests().is_empty());
    process.shutdown().await;
    assert_eq!(extensions.discard_stale_host_requests().len(), 1);
    assert!(extensions.discard_stale_host_requests().is_empty());
    // A later identical occurrence is not suppressed by the earlier loss.
    extensions
        .pending_host_requests
        .push_back(pending(3, generation));
    assert_eq!(extensions.discard_stale_host_requests().len(), 1);
    extensions.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn reloads_run_concurrently_and_report_in_stable_process_order() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture_source = r#"#!/bin/sh
count_file="$OCTET_WORKSPACE/$OCTET_EXTENSION_NAME.count"
count=0
if [ -f "$count_file" ]; then IFS= read -r count < "$count_file"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
IFS= read -r initialize
if [ "$count" -gt 1 ]; then
  : > "$OCTET_WORKSPACE/$OCTET_EXTENSION_NAME.ready"
  attempts=0
  while [ ! -f "$OCTET_WORKSPACE/alpha.ready" ] || [ ! -f "$OCTET_WORKSPACE/beta.ready" ]; do
    attempts=$((attempts + 1))
    if [ "$attempts" -gt 500 ]; then exit 41; fi
    sleep 0.01
  done
fi
id=$(printf '%s\n' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[]}}\n' "$id"
while IFS= read -r request; do
  case "$request" in
    *'"method":"shutdown"'*)
      id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
      printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
      exit 0
      ;;
  esac
done
"#;

    let mut extensions = ExecutableExtensions::default();
    for name in ["alpha", "beta"] {
        let directory = temp.path().join(name);
        std::fs::create_dir_all(&directory).unwrap();
        let fixture = directory.join("probe.sh");
        std::fs::write(&fixture, fixture_source).unwrap();
        let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&fixture, permissions).unwrap();
        let manifest = ExtensionManifest::parse(&format!(
            r#"
name = {name:?}
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "probe.sh"
"#
        ))
        .unwrap();
        let mut runtime = ExtensionRuntimeConfig::new(temp.path());
        runtime.request_timeout = Duration::from_secs(2);
        let process = ExtensionProcess::start(
            DiscoveredExtension {
                manifest,
                manifest_path: directory.join("extension.toml"),
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
        extensions.receivers.push(process.subscribe());
        extensions.processes.push(process);
    }

    let messages = tokio::time::timeout(Duration::from_secs(3), extensions.reload())
        .await
        .expect("concurrent extension reload exceeded its bound");

    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages[0].starts_with("reloaded alpha"), "{messages:?}");
    assert!(messages[1].starts_with("reloaded beta"), "{messages:?}");
    extensions.shutdown().await;
}
