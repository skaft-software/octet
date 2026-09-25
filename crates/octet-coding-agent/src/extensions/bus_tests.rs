//! Real product discovery binds two ordinary processes to one private bus.
#![cfg(unix)]
use super::*;
use serde_json::json;

/// Thread one bounded tool call through the fixture probe of `process`.
async fn probe(
    process: &ExtensionProcess,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    let name = format!("probe_{}", process.descriptor().manifest.name);
    process
        .call_tool(
            &name,
            json!({"method": method, "params": params}),
            process.current_context(),
        )
        .await
        .unwrap()
        .structured_content
        .unwrap()
}

/// Polls one peer's SDK ledger until `active` is an acknowledged subscription.
///
/// Every observation is returned so a failure reports the whole sequence (and
/// the elapsed time) instead of one sample taken at the wrong moment.
async fn wait_for_subscription(
    process: &ExtensionProcess,
    active: &str,
    budget: Duration,
) -> (serde_json::Value, Vec<serde_json::Value>, Duration) {
    let started = Instant::now();
    let mut observed = Vec::new();
    loop {
        let state = probe(
            process,
            "sdk/state",
            json!({"revision": 3, "active": active}),
        )
        .await;
        let subscribed = state["subscribed"]
            .as_array()
            .is_some_and(|topics| topics.iter().any(|topic| topic == active));
        observed.push(state.clone());
        let elapsed = started.elapsed();
        if subscribed || elapsed >= budget {
            return (state, observed, elapsed);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Polls one publisher's SDK ledger until `topic` is declared again.
async fn wait_for_declaration(
    process: &ExtensionProcess,
    topic: &str,
    budget: Duration,
) -> (serde_json::Value, Vec<serde_json::Value>, Duration) {
    let started = Instant::now();
    let mut observed = Vec::new();
    loop {
        let state = probe(process, "sdk/state", json!({"revision": 3})).await;
        let declared = state["declared"]
            .as_array()
            .is_some_and(|topics| topics.iter().any(|entry| entry == topic));
        observed.push(state.clone());
        let elapsed = started.elapsed();
        if declared || elapsed >= budget {
            return (state, observed, elapsed);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn product_discovery_connects_isolated_peers_without_model_or_durable_events() {
    let root = tempfile::tempdir().unwrap();
    let extension_root = root.path().join("extensions");
    let fixture = include_str!("../../../octet-agent/tests/support/extension_bus_peer.py");
    for name in ["alpha", "beta"] {
        let directory = extension_root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("peer.py"),
            fixture
                .replace("__EXTENSION_NAME__", name)
                .replace(
                    "__SDK_PATH__",
                    &format!("{}/../../sdk/python", env!("CARGO_MANIFEST_DIR")),
                )
                .replace(
                    "\"name\": \"probe\"",
                    &format!("\"name\": \"probe_{name}\""),
                ),
        )
        .unwrap();
        // The host runs entrypoint arguments from the workspace, not the
        // extension directory. Keep the script path bound to this fixture.
        let script_argument = serde_json::to_string(&directory.join("peer.py")).unwrap();
        std::fs::write(
            directory.join(EXTENSION_MANIFEST_FILENAME),
            format!(
                r#"name = "{name}"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = [{script_argument}]
[contributes]
tools = ["probe_{name}"]
"#
            ),
        )
        .unwrap();
    }
    let mut config =
        super::tests::executable_extension_config(root.path(), &extension_root, "alpha");
    config.enabled_extensions.push("beta".into());
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let session_path = root.path().join("session.jsonl");
    let session = Session::create(&session_path).unwrap();
    let before = std::fs::read(&session_path).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.path());
    let mut host = ExtensionHost::new();
    let mut extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    assert_eq!(
        extensions.processes.len(),
        2,
        "{}\n{}",
        extensions.status_summary(),
        extensions
            .diagnostics
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let alpha = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "alpha")
        .unwrap()
        .clone();
    let beta = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "beta")
        .unwrap()
        .clone();
    for process in [&alpha, &beta] {
        assert!(process.negotiated_features().contains("event_bus"));
    }
    for (process, method, params) in [
        (
            &alpha,
            "bus/declare",
            json!({"topic":"bus.alpha.status","fields":[{"name":"summary","kind":"string","required":true,"max_bytes":128,"values":[]}]}),
        ),
        (&beta, "bus/subscribe", json!({"topic":"bus.alpha.status"})),
        (
            &alpha,
            "bus/publish",
            json!({"topic":"bus.alpha.status","payload":{"summary":"inert-data"}}),
        ),
    ] {
        let name = format!("probe_{}", process.descriptor().manifest.name);
        let output = process
            .call_tool(
                &name,
                json!({"method":method,"params":params}),
                process.current_context(),
            )
            .await
            .unwrap();
        assert!(output.structured_content.unwrap().get("result").is_some());
    }
    let output = beta
        .call_tool(
            "probe_beta",
            json!({"method":"events","count":1}),
            beta.current_context(),
        )
        .await
        .unwrap();
    assert_eq!(
        output.structured_content.unwrap()[0]["payload"],
        json!({"summary":"inert-data"})
    );
    assert!(
        extensions.drain_events().is_empty(),
        "bus events are not general host observations"
    );
    assert_eq!(
        std::fs::read(&session_path).unwrap(),
        before,
        "bus events must not be persisted"
    );
    extensions.shutdown().await;
}

/// The product switch A→B→A keeps both SDK participants alive on one private
/// bus: each client completes its own bounded rebinding, delivery resumes on
/// the replacement binding, and an incoming request that was admitted under
/// the previous binding is refused as an ingress fence.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_session_switch_a_b_a_survives_both_peers_and_fences_stale_requests() {
    let root = tempfile::tempdir().unwrap();
    let extension_root = root.path().join("extensions");
    let fixture = include_str!("../../../octet-agent/tests/support/extension_bus_peer.py");
    for name in ["alpha", "beta"] {
        let directory = extension_root.join(name);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("peer.py"),
            fixture
                .replace("__EXTENSION_NAME__", name)
                .replace(
                    "__SDK_PATH__",
                    &format!("{}/../../sdk/python", env!("CARGO_MANIFEST_DIR")),
                )
                .replace(
                    "\"name\": \"probe\"",
                    &format!("\"name\": \"probe_{name}\""),
                ),
        )
        .unwrap();
        let script_argument = serde_json::to_string(&directory.join("peer.py")).unwrap();
        std::fs::write(
            directory.join(EXTENSION_MANIFEST_FILENAME),
            format!(
                r#"name = "{name}"
version = "0.1.0"
api_version = "0.3"
[entrypoint]
command = "python3"
args = [{script_argument}]
[contributes]
tools = ["probe_{name}"]
"#
            ),
        )
        .unwrap();
    }
    let mut config =
        super::tests::executable_extension_config(root.path(), &extension_root, "alpha");
    config.enabled_extensions.push("beta".into());
    config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    config.sandbox.allow_process = true;
    config.sandbox.allow_shell = true;
    let session_a_path = root.path().join("session-a.jsonl");
    let session = Session::create(&session_a_path).unwrap();
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-4o-mini".into()))
        .unwrap();
    let sessions = SessionStore::new(&config.session_dir, root.path());
    let mut host = ExtensionHost::new();
    let mut extensions = ExecutableExtensions::discover_and_start(
        &config,
        &session,
        &model,
        &ReasoningConfig::Off,
        &sessions,
        &mut host,
    );
    drop(session);
    assert_eq!(
        extensions.processes.len(),
        2,
        "{}\n{}",
        extensions.status_summary(),
        extensions
            .diagnostics
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n")
    );
    let alpha = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "alpha")
        .unwrap()
        .clone();
    let beta = extensions
        .processes
        .iter()
        .find(|process| process.descriptor().manifest.name == "beta")
        .unwrap()
        .clone();
    let session_a_before = std::fs::read(&session_a_path).unwrap();

    // Session A: both SDK clients observe the first host binding before they
    // build any operation, then reconcile declaration, subscription and an
    // ordinary delivery.
    for process in [&alpha, &beta] {
        assert!(process.negotiated_features().contains("event_bus"));
        assert!(process.is_running());
        let state = probe(process, "sdk/state", json!({"revision": 1})).await;
        assert_eq!(state["binding_revision"], 1, "{state}");
        assert!(state["rebind_error"].is_null(), "{state}");
    }
    let declared = probe(&alpha, "sdk/declare", json!({})).await;
    let bound_a = declared["result"]["binding_id"]
        .as_str()
        .expect("host-issued binding")
        .to_owned();
    let subscribed = probe(&beta, "sdk/subscribe", json!({})).await;
    assert!(
        subscribed["result"]["subscribed"]
            .as_array()
            .unwrap()
            .iter()
            .any(|topic| topic == "bus.alpha.status"),
        "{subscribed}"
    );
    let published = probe(
        &alpha,
        "sdk/publish",
        json!({"payload": {"summary": "session-a"}}),
    )
    .await;
    assert!(published["result"]["sequence"].is_u64(), "{published}");
    let delivered = probe(&beta, "events", json!({"count": 1})).await;
    assert_eq!(delivered.as_array().unwrap().len(), 1);
    assert_eq!(delivered[0]["payload"], json!({"summary": "session-a"}));
    assert_eq!(delivered[0]["binding_id"], bound_a.as_str());

    // This declaration was authorized under the session-A binding and is
    // released only after two real product session transitions.
    let captured = probe(
        &alpha,
        "capture",
        json!({
            "method": "bus/declare",
            "params": {
                "topic": "bus.alpha.after",
                "fields": [
                    {"name": "summary", "kind": "string", "required": true, "max_bytes": 128, "values": []}
                ]
            }
        }),
    )
    .await;
    assert_eq!(captured["binding_id"], bound_a.as_str());

    // A→B→A through the real product boundary. Both participants must survive
    // both switches; a session transition is not a process replacement. Capture
    // the pre-switch identity: a live handle alone does not prove survival,
    // because a supervisor replacement keeps the handle and the transport but
    // changes the generation and the extension instance.
    let alpha_generation = alpha.health_snapshot().generation;
    let beta_generation = beta.health_snapshot().generation;
    let alpha_instance = alpha.extension_instance_id().to_owned();
    let beta_instance = beta.extension_instance_id().to_owned();
    let session_b_path = root.path().join("session-b.jsonl");
    let session_b = Session::create(&session_b_path).unwrap();
    let session_b_before = std::fs::read(&session_b_path).unwrap();
    extensions.transition_active_session(&session_b, &model, &ReasoningConfig::Off, &sessions);
    drop(session_b);
    let reopened_a = Session::open(&session_a_path).unwrap();
    extensions.transition_active_session(&reopened_a, &model, &ReasoningConfig::Off, &sessions);
    drop(reopened_a);
    assert_eq!(extensions.processes.len(), 2);
    // Surface everything the host recorded while the switch ran: a child
    // crash/restart leaves its stderr traceback and a health transition here,
    // not in the SDK ledger, so the failure message must carry both.
    let mut diagnostic_lines = extensions.drain_events();
    diagnostic_lines.extend(extensions.diagnostics.iter().cloned());
    let diagnostics = diagnostic_lines.join("\n");
    for (name, process, generation, instance) in [
        ("alpha", &alpha, alpha_generation, alpha_instance.as_str()),
        ("beta", &beta, beta_generation, beta_instance.as_str()),
    ] {
        assert!(process.is_running(), "{name} stopped across the switch");
        assert_eq!(
            process.health_snapshot().generation,
            generation,
            "{name} was replaced across the session switch (generation {generation} -> {}); a replacement is not a surviving peer\n{}\n{diagnostics}",
            process.health_snapshot().generation,
            extensions.status_summary(),
        );
        assert_eq!(
            process.extension_instance_id(),
            instance,
            "{name} was recreated across the session switch; a replacement is not a surviving peer\n{}\n{diagnostics}",
            extensions.status_summary(),
        );
    }

    // The subscriber's active acknowledgement can only exist once the
    // publisher re-declared under the current host binding, so requiring both
    // also proves the SDK's bounded rebinding finished for both peers. The
    // budget bounds the wait; it does not weaken the assertion, and the final
    // state must show both peers on the same host-issued incarnation.
    let (beta_state, beta_observed, beta_elapsed) =
        wait_for_subscription(&beta, "bus.alpha.status", Duration::from_secs(12)).await;
    let beta_log = beta_observed
        .iter()
        .map(|state| state.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        beta_state["subscribed"]
            .as_array()
            .is_some_and(|topics| topics.iter().any(|topic| topic == "bus.alpha.status")),
        "beta did not re-establish its subscription across the session switch after {beta_elapsed:?} / {} observation(s):\n{beta_log}\n{}\n{diagnostics}",
        beta_observed.len(),
        extensions.status_summary(),
    );
    assert!(beta_state["rebind_error"].is_null(), "{beta_state}");

    let (alpha_state, alpha_observed, alpha_elapsed) =
        wait_for_declaration(&alpha, "bus.alpha.status", Duration::from_secs(12)).await;
    let alpha_log = alpha_observed
        .iter()
        .map(|state| state.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        alpha_state["declared"]
            .as_array()
            .is_some_and(|topics| topics.iter().any(|entry| entry == "bus.alpha.status")),
        "alpha did not re-declare across the session switch after {alpha_elapsed:?} / {} observation(s):\n{alpha_log}\n{}\n{diagnostics}",
        alpha_observed.len(),
        extensions.status_summary(),
    );
    assert!(alpha_state["rebind_error"].is_null(), "{alpha_state}");
    // Both survivors must observe the same current host incarnation: a peer
    // left on a retired binding is not a surviving peer.
    assert_eq!(
        beta_state["binding_id"], alpha_state["binding_id"],
        "the surviving peers observe different bus incarnations:\nbeta  {beta_state}\nalpha {alpha_state}\n{diagnostics}"
    );
    let bound_after = alpha_state["binding_id"].as_str().unwrap().to_owned();
    assert_ne!(bound_after, bound_a, "each bus incarnation is host-fresh");

    // The held request crosses the switch with its original binding. The host
    // refuses it before any mutation, so the topic it named still does not
    // exist and a bounded interest stays explicitly pending.
    let fenced = probe(&alpha, "release", json!({})).await;
    assert_eq!(fenced["error"]["code"], -32011, "{fenced}");
    let pending = probe(&beta, "bus/subscribe", json!({"topic": "bus.alpha.after"})).await;
    assert_eq!(pending["result"]["state"], "pending", "{pending}");

    // A publication carrying the retired binding fans out to nobody and does
    // not consume the publisher's replacement sequence.
    let stale = probe(
        &alpha,
        "bus/publish",
        json!({
            "binding_id": bound_a,
            "topic": "bus.alpha.status",
            "payload": {"summary": "fenced"}
        }),
    )
    .await;
    assert_eq!(stale["error"]["code"], -32011, "{stale}");
    let unchanged = probe(&beta, "events", json!({"count": 1})).await;
    assert_eq!(unchanged.as_array().unwrap().len(), 1, "{unchanged}");

    // Delivery resumes on the replacement binding without reloading either
    // process; the sequence continues where the surviving publisher stopped.
    let published = probe(
        &alpha,
        "sdk/publish",
        json!({"payload": {"summary": "session-a-again"}}),
    )
    .await;
    assert_eq!(published["result"]["sequence"], 2, "{published}");
    let delivered = probe(&beta, "events", json!({"count": 2})).await;
    assert_eq!(delivered.as_array().unwrap().len(), 2);
    assert_eq!(
        delivered[1]["payload"],
        json!({"summary": "session-a-again"})
    );
    assert_eq!(delivered[1]["binding_id"], bound_after.as_str());
    assert_eq!(delivered[1]["process_generation"], 1);

    assert_eq!(
        std::fs::read(&session_a_path).unwrap(),
        session_a_before,
        "bus traffic and session transitions must not be persisted"
    );
    assert_eq!(
        std::fs::read(&session_b_path).unwrap(),
        session_b_before,
        "the switched-to session observes no bus writes"
    );
    extensions.shutdown().await;
}
