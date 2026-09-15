//! PostMutation process-to-product queue qualification (retained API 0.2).
#![cfg(unix)]

use super::*;
use octet_agent::extension_process::ExtensionActivation;
use octet_agent::extension_runtime::{ExtensionRuntimeCatalog, ExtensionRuntimeDomain};

async fn fixture(root: &Path, mode: &str, managed: bool) -> ExecutableExtensions {
    let canonical_root = root.canonicalize().unwrap();
    let root = canonical_root.as_path();
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../octet-agent/tests/fixtures/extension_hooks.py")
        .canonicalize()
        .unwrap();
    let manifest = ExtensionManifest::parse(&format!(
        r#"name = "hook-fixture"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "python3"
args = [{script:?}, {log:?}, {mode:?}]
[contributes]
tools = ["decorate"]
hooks = ["post_mutation", "before_persistence"]
"#,
        log = root.join("hooks.jsonl"),
    ))
    .unwrap();
    let directory = root.join("hook-fixture");
    std::fs::create_dir_all(&directory).unwrap();
    let manifest_path = directory.join("extension.toml");
    std::fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    let descriptor = DiscoveredExtension {
        manifest,
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    };
    let mut extensions = ExecutableExtensions::default();
    extensions.resource_owner = Some("host-session-owner".into());
    let process = if managed {
        let manager = ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(root).unwrap());
        manager
            .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([descriptor]))
            .await;
        let binding = manager.bind_session("host-session-owner").unwrap();
        let lease = binding
            .activate("hook-fixture", ExtensionRuntimeConfig::new(root))
            .await
            .unwrap();
        let process = lease.process().clone();
        extensions.runtime_binding = Some(binding);
        extensions.runtime_manager = Some(manager);
        process
    } else {
        ExtensionProcess::start(descriptor, ExtensionRuntimeConfig::new(root))
            .await
            .unwrap()
    };
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process);
    extensions
}

fn mutation(id: &str, state: PostMutationState) -> PostMutationContext {
    PostMutationContext::new(
        id,
        PostMutationKind::Configuration,
        ["resource:settings".into()],
        7,
        state,
    )
    .unwrap()
}

#[tokio::test]
async fn post_mutation_sdk_round_trip_validates_subsets_and_deduplicates_across_reload() {
    for managed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut extensions = fixture(root.path(), "valid", managed).await;
        extensions.rescan_config = Some(super::tests::executable_extension_config(
            root.path(),
            root.path(),
            "hook-fixture",
        ));
        let committed = mutation("mutation:commit", PostMutationState::Committed);
        let requests = extensions.notify_post_mutation(committed.clone()).await;
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0],
            PostMutationRescan {
                extension: "hook-fixture".into(),
                mutation_id: "mutation:commit".into(),
                generation: 7,
                resource_ids: vec!["resource:settings".into()],
            }
        );
        assert_eq!(extensions.take_post_mutation_rescans(), requests);
        assert!(extensions.take_post_mutation_rescans().is_empty());
        assert!(extensions
            .notify_post_mutation(committed.clone())
            .await
            .is_empty());

        let messages = extensions.reload().await;
        assert!(
            messages
                .iter()
                .any(|line| line.starts_with("reloaded hook-fixture")),
            "{messages:?}"
        );
        assert!(
            messages
                .iter()
                .any(|line| line == "rescanned extension \"hook-fixture\" (generation 2)"),
            "manager and isolated reloads must both notify: {messages:?}"
        );
        assert!(
            extensions.take_post_mutation_rescans().is_empty(),
            "the reload product boundary must drain the rescan queue"
        );
        assert!(extensions.notify_post_mutation(committed).await.is_empty());

        let rollback = mutation("mutation:rollback", PostMutationState::RolledBack);
        assert_eq!(extensions.notify_post_mutation(rollback).await.len(), 1);
        extensions.shutdown().await;
        let records = std::fs::read_to_string(root.path().join("hooks.jsonl")).unwrap();
        let records = records
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            records.len(),
            3,
            "duplicate identities never reach the process"
        );
        assert_eq!(
            records[0]["payload"],
            serde_json::json!({
                "mutation_id": "mutation:commit", "kind": "configuration",
                "affected_resources": ["resource:settings"], "generation": 7, "state": "committed"
            })
        );
        assert_eq!(records[2]["payload"]["state"], "rolled_back");
        assert_eq!(
            records[0]["context"]["resource_owner"]["session_id"],
            "host-session-owner"
        );
        assert_eq!(
            records[2]["context"]["resource_owner"]["process_generation"],
            2
        );
    }
}

#[tokio::test]
async fn post_mutation_invalid_foreign_empty_and_late_rescans_never_enter_queue() {
    for mode in ["outside", "malformed", "empty", "timeout"] {
        let root = tempfile::tempdir().unwrap();
        let mut extensions = fixture(root.path(), mode, false).await;
        let started = tokio::time::Instant::now();
        assert!(extensions
            .notify_post_mutation(mutation("mutation:reject", PostMutationState::Committed))
            .await
            .is_empty());
        assert!(
            started.elapsed() < Duration::from_millis(900),
            "bounded dispatch for {mode}"
        );
        assert!(extensions.take_post_mutation_rescans().is_empty());
        if mode != "empty" {
            assert!(!extensions.diagnostics.is_empty());
        }
        extensions.shutdown().await;
    }
}

#[tokio::test]
async fn post_mutation_rescan_consumes_current_resources_and_rejects_changed_or_stale_sources() {
    let root = tempfile::tempdir().unwrap();
    let mut extensions = fixture(root.path(), "valid", true).await;
    let config =
        super::tests::executable_extension_config(root.path(), root.path(), "hook-fixture");
    extensions.rescan_config = Some(config.clone());
    // The reload is a product mutation boundary: it notifies PostMutation for
    // its own resource and drains the admitted request in the same call.
    let reload_messages = extensions.reload().await;
    assert!(
        reload_messages
            .iter()
            .any(|line| line == "rescanned extension \"hook-fixture\" (generation 2)"),
        "{reload_messages:?}"
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    assert!(extensions
        .rescan_post_mutation_resources(&config)
        .is_empty());

    // The response was valid when queued, but its resource generation is old
    // by the time the product reaches its consumption boundary.
    let current_generation = extensions.processes[0].health_snapshot().generation;
    let stale = PostMutationContext::new(
        "mutation:stale",
        PostMutationKind::Resource,
        [opaque_extension_resource_id("hook-fixture")],
        current_generation.saturating_sub(1).max(1),
        PostMutationState::Committed,
    )
    .unwrap();
    assert_eq!(extensions.notify_post_mutation(stale).await.len(), 1);
    let messages = extensions.rescan_post_mutation_resources(&config);
    assert!(
        messages.iter().any(|line| line.contains("stale")),
        "{messages:?}"
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    assert!(
        extensions.processes[0].is_running(),
        "a stale rescan never re-enters or replaces a process"
    );

    // A changed source is reported and never activated implicitly.
    let manifest = root.path().join("hook-fixture/extension.toml");
    std::fs::write(&manifest, "invalid manifest =").unwrap();
    let broken = PostMutationContext::new(
        "mutation:broken",
        PostMutationKind::Resource,
        [opaque_extension_resource_id("hook-fixture")],
        extensions.processes[0].health_snapshot().generation,
        PostMutationState::Committed,
    )
    .unwrap();
    assert_eq!(extensions.notify_post_mutation(broken).await.len(), 1);
    let messages = extensions.rescan_post_mutation_resources(&config);
    assert!(
        !messages
            .iter()
            .any(|line| line.starts_with("rescanned extension")),
        "{messages:?}"
    );
    assert!(
        messages.iter().any(|line| line.contains("error:")),
        "{messages:?}"
    );
    assert!(
        extensions.processes[0].is_running(),
        "a read-only rescan never executes or stops code"
    );
    extensions.shutdown().await;
}

#[tokio::test]
async fn post_mutation_queue_and_dedup_window_stay_bounded_and_rescans_coalesce() {
    let root = tempfile::tempdir().unwrap();
    let mut extensions = fixture(root.path(), "valid", false).await;
    for index in 0..300 {
        let mutation = PostMutationContext::new(
            format!("mutation:n{index}"),
            PostMutationKind::Resource,
            [opaque_extension_resource_id("hook-fixture")],
            1,
            PostMutationState::Committed,
        )
        .unwrap();
        assert_eq!(extensions.notify_post_mutation(mutation).await.len(), 1);
    }
    assert_eq!(
        extensions.seen_post_mutation_ids.len(),
        MAX_SEEN_POST_MUTATION_IDS
    );
    assert_eq!(
        extensions.pending_post_mutation_rescans.len(),
        MAX_PENDING_POST_MUTATION_RESCANS
    );
    assert_eq!(
        extensions
            .pending_post_mutation_rescans
            .front()
            .unwrap()
            .mutation_id,
        "mutation:n44"
    );
    let config =
        super::tests::executable_extension_config(root.path(), root.path(), "hook-fixture");
    let messages = extensions.rescan_post_mutation_resources(&config);
    assert_eq!(
        messages,
        ["rescanned extension \"hook-fixture\" (generation 1)"]
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    extensions.shutdown().await;
}

#[tokio::test]
async fn reload_drains_the_rescan_queue_at_the_product_mutation_boundary() {
    for managed in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut extensions = fixture(root.path(), "valid", managed).await;
        extensions.rescan_config = Some(super::tests::executable_extension_config(
            root.path(),
            root.path(),
            "hook-fixture",
        ));

        let messages = extensions.reload().await;
        assert!(
            messages
                .iter()
                .any(|line| line.starts_with("reloaded hook-fixture")),
            "{messages:?}"
        );
        // The reload notified PostMutation for its own generation and then
        // drained the bounded queue through the same trusted discovery path.
        assert!(
            messages
                .iter()
                .any(|line| line == "rescanned extension \"hook-fixture\" (generation 2)"),
            "{messages:?}"
        );
        assert!(
            extensions.take_post_mutation_rescans().is_empty(),
            "the product boundary must drain the rescan queue"
        );
        // A rescan never starts or replaces a process by itself.
        assert_eq!(extensions.processes.len(), 1);
        assert_eq!(extensions.processes[0].health_snapshot().generation, 2);
        extensions.shutdown().await;
    }
}

#[tokio::test]
async fn product_drain_drops_out_of_scope_and_stale_rescans_with_bounded_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let mut extensions = fixture(root.path(), "valid", false).await;
    extensions.rescan_config = Some(super::tests::executable_extension_config(
        root.path(),
        root.path(),
        "hook-fixture",
    ));

    // A configuration mutation names a non-extension resource: the request is
    // admitted, then dropped at re-resolution because no running owner matches.
    let before = extensions.processes[0].health_snapshot().generation;
    let admitted = extensions
        .notify_post_mutation(mutation("mutation:settings", PostMutationState::Committed))
        .await;
    assert_eq!(admitted.len(), 1);
    assert_eq!(
        extensions.drain_post_mutation_rescans(),
        ["warning: discarded stale or unavailable post_mutation resource rescan"]
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    assert_eq!(extensions.processes.len(), 1);
    assert_eq!(extensions.processes[0].health_snapshot().generation, before);

    // A stopped process is never re-entered, even for its own resource id.
    let own = PostMutationContext::new(
        "mutation:own",
        PostMutationKind::Resource,
        [opaque_extension_resource_id("hook-fixture")],
        before,
        PostMutationState::Committed,
    )
    .unwrap();
    assert_eq!(extensions.notify_post_mutation(own).await.len(), 1);
    extensions.shutdown().await;
    assert_eq!(
        extensions.drain_post_mutation_rescans(),
        ["warning: discarded stale or unavailable post_mutation resource rescan"]
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    assert!(extensions.processes.is_empty());
}

#[tokio::test]
async fn product_drain_fails_closed_without_a_bound_discovery_configuration() {
    let root = tempfile::tempdir().unwrap();
    let mut extensions = fixture(root.path(), "valid", false).await;
    assert!(extensions.rescan_config.is_none());
    assert_eq!(
        extensions
            .notify_post_mutation(mutation("mutation:unbound", PostMutationState::Committed))
            .await
            .len(),
        1
    );
    assert_eq!(
        extensions.drain_post_mutation_rescans(),
        ["warning: discarded 1 post_mutation rescan request(s); no discovery configuration is bound"]
    );
    assert!(extensions.take_post_mutation_rescans().is_empty());
    // An empty queue is not a diagnostic.
    assert!(extensions.drain_post_mutation_rescans().is_empty());
    extensions.shutdown().await;
}
