//! The real MCP bridge speaking to a live stdio server.
//!
//! One test, kept on its own because it is the only place the host is exercised
//! against a real bridge process: the bridge must obey the host's access policy
//! and must not replay calls it has already answered.

use super::*;

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_policy_real_bridge_obeys_host_access_without_replaying_calls() {
    let temp = tempfile::tempdir().unwrap();
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-mcp")
        .canonicalize()
        .unwrap();
    let server_path = temp.path().join("server.py");
    // Instrument a conforming server, not a mock of policy/evaluate: this
    // exercises Rust host -> bundled Python bridge -> upstream MCP stdio.
    let server = std::fs::read_to_string(package.join("fixtures/real_mcp_server.py"))
        .unwrap()
        .replace(
            "        name = params.get(\"name\")",
            "        with open('calls.jsonl', 'a') as log:\n            log.write(json.dumps(params) + '\\n')\n        name = params.get(\"name\")",
        );
    std::fs::write(&server_path, server).unwrap();
    let config_path = temp.path().join("mcp.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "servers": {"ableton": {
                "transport": "stdio", "command": "python3",
                "args": [server_path], "cwd": temp.path(), "enabled": true
            }}
        }))
        .unwrap(),
    )
    .unwrap();
    let mut manifest =
        ExtensionManifest::parse(&std::fs::read_to_string(package.join("extension.toml")).unwrap())
            .unwrap();
    manifest.entrypoint.args = vec![
        "--config".into(),
        config_path.to_string_lossy().into_owned(),
    ];
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: package.join("extension.toml"),
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
    let mut host = ExtensionHost::new();
    host.load(&process);
    host.finalize_tool_surface();
    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    extensions.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    extensions.start_policy_supervisors();
    let tools = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let tools = process.tool_definitions();
            if tools.len() == 3 {
                break tools;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if tools.is_err() {
        let status = process
            .execute_command("mcp", vec!["status".into()], process.current_context())
            .await;
        panic!(
            "MCP catalog should register: {status:?}; {:?}",
            extensions.drain_events()
        );
    }
    let tools = tools.unwrap();
    let unknown = tools
        .iter()
        .find(|tool| tool.name.contains("unknown_effect"))
        .unwrap();
    let echo = tools
        .iter()
        .find(|tool| tool.name.contains("fixture_echo"))
        .unwrap();
    let owner = || process.current_context_for_resource_owner("mcp-test");
    let count_calls = || {
        std::fs::read_to_string(temp.path().join("calls.jsonl"))
            .unwrap_or_default()
            .lines()
            .count()
    };

    // Inspect the live host-correlated policy request before giving the
    // supervisor ownership. No changed target may borrow this call.
    for task in extensions.policy_supervisors.drain(..) {
        task.abort();
    }
    let mut events = process.subscribe();
    let probe = process.call_tool(&unknown.name, serde_json::json!({}), owner());
    tokio::pin!(probe);
    let (request_id, generation, parent, intent) = loop {
        tokio::select! {
            result = &mut probe => panic!("call settled before policy: {result:?}"),
            event = events.recv() => if let ExtensionEvent::PolicyEvaluationRequested {
                request_id, generation, parent_request_id, intent,
            } = event.unwrap() {
                break (request_id, generation, parent_request_id, intent);
            }
        }
    };
    let decide = |generation, parent, intent: &octet_agent::ExtensionActionIntent| {
        mcp_policy_response(
            octet_agent::EffectPolicy::UnsafeHost,
            &process,
            generation,
            parent,
            intent,
        )
        .decision
    };
    assert_eq!(
        decide(generation, parent, &intent),
        ExtensionPolicyDecision::Allow
    );
    assert_eq!(
        decide(generation + 1, parent, &intent),
        ExtensionPolicyDecision::Deny
    );
    assert_eq!(
        decide(generation, parent + 1, &intent),
        ExtensionPolicyDecision::Deny
    );
    for (key, value) in [
        ("server", serde_json::json!("other")),
        ("tool", serde_json::json!("mcp_ableton_other_tool")),
        ("arguments", serde_json::json!({"changed": true})),
    ] {
        let mut changed = intent.clone();
        changed.target[key] = value;
        assert_eq!(
            decide(generation, parent, &changed),
            ExtensionPolicyDecision::Deny
        );
    }
    let mut changed = intent.clone();
    changed.operation = "other.operation".into();
    assert_eq!(
        decide(generation, parent, &changed),
        ExtensionPolicyDecision::Deny
    );
    changed = intent.clone();
    changed.kind = "read_only".into();
    assert_eq!(
        decide(generation, parent, &changed),
        ExtensionPolicyDecision::Deny
    );
    changed = intent.clone();
    changed.adapter_hints.destructive = Some(true);
    assert_eq!(
        decide(generation, parent, &changed),
        ExtensionPolicyDecision::Allow,
        "full access authorizes mutations, not just guessed reads"
    );
    for policy in [
        octet_agent::EffectPolicy::Controlled,
        octet_agent::EffectPolicy::ControlledBashApproval,
    ] {
        assert_eq!(
            mcp_policy_response(policy, &process, generation, parent, &intent).decision,
            ExtensionPolicyDecision::Deny
        );
    }
    process
        .respond_to_policy_evaluation(
            request_id,
            generation,
            ExtensionPolicyEvaluationResponse {
                decision: ExtensionPolicyDecision::Deny,
                approval_token: None,
            },
        )
        .await
        .unwrap();
    assert!(probe.await.unwrap().is_error);
    assert_eq!(count_calls(), 0);
    assert_eq!(
        decide(generation, parent, &intent),
        ExtensionPolicyDecision::Deny,
        "settled parent cannot authorize another operation"
    );
    extensions.start_policy_supervisors();

    let result = process
        .call_tool(&unknown.name, serde_json::json!({}), owner())
        .await
        .unwrap();
    assert!(!result.is_error, "{}", result.content);
    assert_eq!(
        count_calls(),
        1,
        "authorized missing-annotation call executes once"
    );
    assert!(!extensions
        .drain_events()
        .iter()
        .any(|line| line.contains("policy intent denied")));

    // Changing the supervisor's policy models denial independently of the
    // startup floor (controlled mode never starts this process in product).
    extensions.effect_policy = octet_agent::EffectPolicy::Controlled;
    extensions.start_policy_supervisors();
    let denied = process
        .call_tool(&unknown.name, serde_json::json!({}), owner())
        .await
        .unwrap();
    assert!(denied.is_error);
    assert_eq!(count_calls(), 1, "denied call never reaches MCP server");
    let read = process
        .call_tool(&echo.name, serde_json::json!({"value": "hello"}), owner())
        .await
        .unwrap();
    assert!(!read.is_error, "{}", read.content);
    assert_eq!(count_calls(), 2, "read-only calls remain usable");

    extensions.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    extensions.start_policy_supervisors();
    let ownerless = process
        .call_tool(
            &unknown.name,
            serde_json::json!({}),
            process.current_context(),
        )
        .await
        .unwrap();
    assert!(
        ownerless.is_error,
        "ownerless policy calls must fail closed"
    );
    assert_eq!(count_calls(), 2);
    extensions.shutdown().await;
}
