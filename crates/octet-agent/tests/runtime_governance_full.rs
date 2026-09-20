//! Public manager seams. The child is deliberately API 0.2, not an API 0.3
//! authoring example; these tests qualify fleet ownership independently of RPC.
#![cfg(unix)]

use std::fs;
use std::future::Future;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::time::Duration;

use futures_util::{future::join_all, poll};
use octet_agent::extension_process::{
    DiscoveredExtension, ExtensionActivation, ExtensionEntrypoint, ExtensionLifecycleProfile,
    ExtensionManifest, ExtensionRuntimeConfig, ExtensionRuntimeSettings, ExtensionRuntimeSharing,
    ExtensionSource, ExtensionTrust, ManifestContributions,
};
use octet_agent::extension_runtime::{
    ExtensionManagedRuntimeState, ExtensionResourceExhausted, ExtensionRuntimeBudget,
    ExtensionRuntimeCatalog, ExtensionRuntimeDomain, ExtensionRuntimeLease,
    ExtensionRuntimeManager, ExtensionRuntimeManagerError, ExtensionRuntimeResource,
    ExtensionRuntimeUsage,
};

const DEADLINE: Duration = Duration::from_secs(5);

fn descriptor(
    root: &Path,
    name: &str,
    lifecycle: ExtensionLifecycleProfile,
) -> DiscoveredExtension {
    let directory = root.join(name);
    fs::create_dir_all(&directory).unwrap();
    let script = directory.join("runner.sh");
    fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' "$$" >> "$OCTET_WORKSPACE/starts-$OCTET_EXTENSION_NAME"
IFS= read -r initialize || exit 1
while [ -f "$OCTET_WORKSPACE/hold-$OCTET_EXTENSION_NAME" ]; do sleep 0.01; done
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
while IFS= read -r line; do
  case "$line" in
    *'"method":"shutdown"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'; exit 0 ;;
  esac
done
"#,
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    let manifest = ExtensionManifest {
        name: name.into(),
        version: "0.1.0".into(),
        api_version: "0.2".into(),
        requires_octet: None,
        description: None,
        entrypoint: ExtensionEntrypoint {
            command: "runner.sh".into(),
            args: Vec::new(),
            env: Default::default(),
        },
        capabilities: Default::default(),
        contributes: ManifestContributions::default(),
        runtime: ExtensionRuntimeSettings {
            lifecycle,
            sharing: if lifecycle == ExtensionLifecycleProfile::WorkspaceService {
                ExtensionRuntimeSharing::Workspace
            } else {
                ExtensionRuntimeSharing::Isolated
            },
        },
    };
    let manifest_path = directory.join("extension.toml");
    fs::write(&manifest_path, toml::to_string(&manifest).unwrap()).unwrap();
    DiscoveredExtension {
        manifest,
        manifest_path,
        source: ExtensionSource::Explicit,
        activation: ExtensionActivation {
            enabled: true,
            trust: ExtensionTrust::Trusted,
        },
    }
}

fn config(root: &Path) -> ExtensionRuntimeConfig {
    let mut config = ExtensionRuntimeConfig::new(root);
    config.max_message_bytes = 16 * 1024;
    config.writer_queue_capacity = 1;
    config.max_pending_requests = 1;
    config.shutdown_timeout = Duration::from_millis(100);
    config
}

fn manager(root: &Path, budget: ExtensionRuntimeBudget) -> ExtensionRuntimeManager {
    ExtensionRuntimeManager::with_budget(ExtensionRuntimeDomain::ordinary(root).unwrap(), budget)
        .unwrap()
}

fn starts(root: &Path, name: &str) -> usize {
    fs::read_to_string(root.join(format!("starts-{name}")))
        .unwrap_or_default()
        .lines()
        .count()
}

async fn wait_for_start(
    root: &Path,
    name: &str,
    start: impl Future<Output = Result<ExtensionRuntimeLease, ExtensionRuntimeManagerError>>,
) {
    tokio::time::timeout(DEADLINE, async {
        // Continue driving activation across pre-spawn awaits while the child
        // keeps its initialization response behind the explicit hold file.
        tokio::select! {
            () = async {
                while starts(root, name) == 0 {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            } => {}
            result = start => match result {
                Ok(_) => panic!("fixture completed initialization while its startup gate was held"),
                Err(error) => panic!("fixture failed before reaching its startup gate: {error}"),
            },
        }
    })
    .await
    .expect("fixture must reach its explicit startup gate");
}

#[tokio::test]
async fn one_hundred_lazy_entries_stay_visible_without_spawning() {
    let root = tempfile::tempdir().unwrap();
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_file_descriptors: 256,
            ..Default::default()
        },
    );
    assert!(manager.statuses().is_empty());
    let entries = (0..100)
        .map(|index| {
            descriptor(
                root.path(),
                &format!("lazy-{index}"),
                ExtensionLifecycleProfile::LazyResident,
            )
        })
        .collect::<Vec<_>>();
    let names = entries
        .iter()
        .map(|entry| entry.manifest.name.clone())
        .collect::<Vec<_>>();
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
        .await;
    let binding = manager.bind_session("catalog-only").unwrap();
    assert!(binding
        .activate_eager(names.clone(), |_| panic!("lazy entry was eager"))
        .await
        .is_empty());
    assert_eq!(manager.statuses().len(), 100);
    assert!(manager
        .statuses()
        .iter()
        .all(|status| status.state == ExtensionManagedRuntimeState::Eligible));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    assert!(names.iter().all(|name| starts(root.path(), name) == 0));
    binding.release().await;
    manager.shutdown().await;
}

#[tokio::test]
async fn concurrent_shared_leases_keep_one_durable_owner() {
    let root = tempfile::tempdir().unwrap();
    let entry = descriptor(
        root.path(),
        "shared",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let manager = manager(root.path(), ExtensionRuntimeBudget::default());
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([entry]))
        .await;
    let bindings = (0..8)
        .map(|index| manager.bind_session(format!("session-{index}")).unwrap())
        .collect::<Vec<_>>();
    let leases = tokio::time::timeout(
        DEADLINE,
        join_all(
            bindings
                .iter()
                .map(|binding| binding.activate("shared", config(root.path()))),
        ),
    )
    .await
    .unwrap()
    .into_iter()
    .map(Result::unwrap)
    .collect::<Vec<_>>();
    assert_eq!(leases.iter().filter(|lease| !lease.shared()).count(), 1);
    assert!(leases
        .iter()
        .all(|lease| lease.process().extension_instance_id()
            == leases[0].process().extension_instance_id()));
    assert_eq!(starts(root.path(), "shared"), 1);
    assert_eq!(manager.statuses()[0].bindings, 8);
    for binding in bindings {
        binding.release().await;
    }
    assert_eq!(manager.statuses()[0].bindings, 0);
    assert!(leases[0].process().is_running());
    let rebuilt = manager.bind_session("rebuilt").unwrap();
    assert!(rebuilt
        .activate("shared", config(root.path()))
        .await
        .unwrap()
        .shared());
    rebuilt.release().await;
    manager.shutdown().await;
    assert!(!leases[0].process().is_running());
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
}

#[tokio::test]
async fn aggregate_budgets_fail_visibly_without_launching_the_rejected_child() {
    for resource in [
        ExtensionRuntimeResource::Processes,
        ExtensionRuntimeResource::FileDescriptors,
        ExtensionRuntimeResource::BufferedBytes,
    ] {
        let root = tempfile::tempdir().unwrap();
        let entries = ["first", "second"]
            .map(|name| descriptor(root.path(), name, ExtensionLifecycleProfile::LazyResident));
        let mut budget = ExtensionRuntimeBudget::default();
        match resource {
            ExtensionRuntimeResource::Processes => budget.max_processes = 1,
            ExtensionRuntimeResource::FileDescriptors => budget.max_file_descriptors = 4,
            ExtensionRuntimeResource::BufferedBytes => budget.max_buffered_bytes = 4 * 16 * 1024,
            _ => unreachable!(),
        }
        let manager = manager(root.path(), budget);
        manager
            .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
            .await;
        let binding = manager.bind_session("secret-session-owner").unwrap();
        binding
            .activate("first", config(root.path()))
            .await
            .unwrap();
        let before = manager.usage();
        let error = binding
            .activate("second", config(root.path()))
            .await
            .err()
            .expect("second child must exceed the selected budget");
        let ExtensionRuntimeManagerError::ResourceExhausted(exhausted) = error else {
            panic!("expected typed exhaustion")
        };
        assert_eq!(exhausted.resource, resource);
        assert_eq!(exhausted.api_error_name(), "resource_exhausted");
        assert_eq!(ExtensionResourceExhausted::JSON_RPC_CODE, -32012);
        let diagnostic = serde_json::to_string(&exhausted).unwrap();
        assert!(!diagnostic.contains("secret-session-owner"));
        assert!(!diagnostic.contains(root.path().to_str().unwrap()));
        assert_eq!(manager.usage(), before);
        assert_eq!(starts(root.path(), "second"), 0);
        assert_eq!(manager.statuses().len(), 2);
        binding.release().await;
        assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
        manager.shutdown().await;
    }
}

#[tokio::test]
async fn dead_one_shot_charge_is_reclaimed_before_the_next_admission() {
    let root = tempfile::tempdir().unwrap();
    let entry = descriptor(root.path(), "once", ExtensionLifecycleProfile::OneShot);
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_processes: 1,
            ..Default::default()
        },
    );
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([entry]))
        .await;
    let binding = manager.bind_session("one-shot-owner").unwrap();
    let first = binding.activate("once", config(root.path())).await.unwrap();
    assert!(first.is_one_shot());
    assert!(first.process().shutdown().await);
    // Deliberately do not call usage/statuses: admission must reconcile itself.
    let second = binding.activate("once", config(root.path())).await.unwrap();
    assert_ne!(
        first.process().extension_instance_id(),
        second.process().extension_instance_id()
    );
    binding.settle_one_shots().await;
    assert!(!second.process().is_running());
    assert!(binding.processes().is_empty());
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    binding.release().await;
    manager.shutdown().await;
}

#[tokio::test]
async fn release_wakes_coalesced_and_startup_slot_waiters() {
    let root = tempfile::tempdir().unwrap();
    let entries = ["blocker", "queued"]
        .map(|name| descriptor(root.path(), name, ExtensionLifecycleProfile::LazyResident));
    fs::write(root.path().join("hold-blocker"), b"").unwrap();
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_concurrent_startups: 1,
            ..Default::default()
        },
    );
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
        .await;
    let blocker = manager.bind_session("blocker").unwrap();
    let queued = manager.bind_session("queued").unwrap();
    let mut starting = Box::pin(blocker.activate("blocker", config(root.path())));
    assert!(poll!(starting.as_mut()).is_pending());
    wait_for_start(root.path(), "blocker", starting.as_mut()).await;
    let mut owner = Box::pin(queued.activate("queued", config(root.path())));
    let mut waiter = Box::pin(queued.activate("queued", config(root.path())));
    assert!(poll!(owner.as_mut()).is_pending());
    assert!(poll!(waiter.as_mut()).is_pending());
    assert_eq!(manager.usage().processes, 2);
    queued.release().await;
    assert!(matches!(
        tokio::time::timeout(DEADLINE, owner).await.unwrap(),
        Err(ExtensionRuntimeManagerError::BindingClosed)
    ));
    assert!(matches!(
        tokio::time::timeout(DEADLINE, waiter).await.unwrap(),
        Err(ExtensionRuntimeManagerError::BindingClosed)
    ));
    assert_eq!(manager.usage().processes, 1);
    assert_eq!(starts(root.path(), "queued"), 0);
    blocker.release().await;
    assert!(matches!(
        tokio::time::timeout(DEADLINE, starting).await.unwrap(),
        Err(ExtensionRuntimeManagerError::BindingClosed)
    ));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    manager.shutdown().await;
}

#[tokio::test]
async fn removed_and_reselected_catalog_cannot_commit_an_old_start_reservation() {
    let root = tempfile::tempdir().unwrap();
    let entry = descriptor(root.path(), "slow", ExtensionLifecycleProfile::LazyResident);
    let catalog = ExtensionRuntimeCatalog::from_descriptors([entry]);
    fs::write(root.path().join("hold-slow"), b"").unwrap();
    let manager = manager(root.path(), ExtensionRuntimeBudget::default());
    manager.replace_catalog(catalog.clone()).await;
    let binding = manager.bind_session("owner").unwrap();
    let mut old = Box::pin(binding.activate("slow", config(root.path())));
    assert!(poll!(old.as_mut()).is_pending());
    wait_for_start(root.path(), "slow", old.as_mut()).await;
    manager
        .replace_catalog(ExtensionRuntimeCatalog::default())
        .await;
    manager.replace_catalog(catalog).await;
    let mut current = Box::pin(binding.activate("slow", config(root.path())));
    assert!(poll!(current.as_mut()).is_pending());
    assert_eq!(manager.usage().processes, 2);
    assert!(matches!(
        tokio::time::timeout(DEADLINE, old).await.unwrap(),
        Err(ExtensionRuntimeManagerError::StaleSource)
    ));
    // Dropping the old owner must not erase the newer reservation or its charge.
    assert_eq!(manager.usage().processes, 1);
    fs::remove_file(root.path().join("hold-slow")).unwrap();
    let lease = tokio::time::timeout(DEADLINE, current)
        .await
        .unwrap()
        .unwrap();
    assert!(lease.process().is_running());
    assert_eq!(binding.processes().len(), 1);
    assert_eq!(manager.usage().processes, 1);
    binding.release().await;
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    manager.shutdown().await;
}

#[tokio::test]
async fn canceled_queued_reload_restores_the_old_lease_and_releases_transient_usage() {
    let root = tempfile::tempdir().unwrap();
    let entries = ["resident", "blocker"]
        .map(|name| descriptor(root.path(), name, ExtensionLifecycleProfile::LazyResident));
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_concurrent_startups: 1,
            ..Default::default()
        },
    );
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
        .await;
    let binding = manager.bind_session("owner").unwrap();
    let old = binding
        .activate("resident", config(root.path()))
        .await
        .unwrap();
    fs::write(root.path().join("hold-blocker"), b"").unwrap();
    let mut blocker = Box::pin(binding.activate("blocker", config(root.path())));
    assert!(poll!(blocker.as_mut()).is_pending());
    wait_for_start(root.path(), "blocker", blocker.as_mut()).await;
    let mut reload = Box::pin(manager.reload("resident"));
    assert!(poll!(reload.as_mut()).is_pending());
    assert_eq!(manager.usage().processes, 3);
    drop(reload);
    assert_eq!(manager.usage().processes, 2);
    assert_eq!(
        manager
            .statuses()
            .into_iter()
            .find(|status| status.provenance.extension == "resident")
            .unwrap()
            .state,
        ExtensionManagedRuntimeState::Ready
    );
    let attached =
        tokio::time::timeout(DEADLINE, binding.activate("resident", config(root.path())))
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        old.process().extension_instance_id(),
        attached.process().extension_instance_id()
    );
    assert_eq!(starts(root.path(), "resident"), 1);
    drop(blocker);
    binding.release().await;
    manager.shutdown().await;
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
}

#[tokio::test]
async fn canceled_start_owner_hands_its_reservation_to_a_shared_waiter() {
    let root = tempfile::tempdir().unwrap();
    let entries = [
        descriptor(
            root.path(),
            "blocker",
            ExtensionLifecycleProfile::LazyResident,
        ),
        descriptor(
            root.path(),
            "shared",
            ExtensionLifecycleProfile::WorkspaceService,
        ),
    ];
    fs::write(root.path().join("hold-blocker"), b"").unwrap();
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_processes: 2,
            max_concurrent_startups: 1,
            ..Default::default()
        },
    );
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
        .await;
    let blocker = manager.bind_session("blocker").unwrap();
    let first = manager.bind_session("first").unwrap();
    let second = manager.bind_session("second").unwrap();
    // Model a pre-spawn yield, such as Linux's bounded ETXTBSY retry. A single
    // poll is not sufficient to drive activation to the child's startup gate.
    let mut starting = Box::pin(async {
        tokio::task::yield_now().await;
        blocker.activate("blocker", config(root.path())).await
    });
    assert!(poll!(starting.as_mut()).is_pending());
    wait_for_start(root.path(), "blocker", starting.as_mut()).await;

    let mut owner = Box::pin(first.activate("shared", config(root.path())));
    let mut waiter = Box::pin(second.activate("shared", config(root.path())));
    let mut canceled_waiter = Box::pin(second.activate("shared", config(root.path())));
    assert!(poll!(owner.as_mut()).is_pending());
    assert!(poll!(waiter.as_mut()).is_pending());
    assert!(poll!(canceled_waiter.as_mut()).is_pending());
    let reserved = manager.usage();
    assert_eq!(reserved.processes, 2);
    drop(canceled_waiter);
    assert_eq!(manager.usage(), reserved);

    // Only the startup owner releases a reservation. The surviving waiter
    // must be able to reserve again, even while the startup slot stays busy.
    drop(owner);
    assert_eq!(manager.usage().processes, 1);
    first.release().await;
    assert!(poll!(waiter.as_mut()).is_pending());
    assert_eq!(manager.usage(), reserved);
    assert_eq!(starts(root.path(), "shared"), 0);
    drop(starting);
    assert_eq!(manager.usage().processes, 1);
    let lease = tokio::time::timeout(DEADLINE, waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(!lease.shared());
    assert_eq!(starts(root.path(), "shared"), 1);
    assert_eq!(
        manager
            .statuses()
            .into_iter()
            .find(|status| status.provenance.extension == "shared")
            .unwrap()
            .bindings,
        1
    );
    assert!(matches!(
        first.activate("shared", config(root.path())).await,
        Err(ExtensionRuntimeManagerError::BindingClosed)
    ));

    // Neither a temporary binding clone nor the lease is the durable owner.
    drop(second.clone());
    let process = lease.process().clone();
    drop(lease);
    assert_eq!(second.processes().len(), 1);
    second.release().await;
    assert!(process.is_running());
    blocker.release().await;
    manager.shutdown().await;
    assert!(!process.is_running());
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
}

#[tokio::test]
async fn canceled_candidate_reload_restores_waiting_leases_without_switching_generation() {
    let root = tempfile::tempdir().unwrap();
    let entry = descriptor(
        root.path(),
        "shared",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let manager = manager(root.path(), ExtensionRuntimeBudget::default());
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([entry]))
        .await;
    let binding = manager.bind_session("owner").unwrap();
    let waiting = manager.bind_session("waiting").unwrap();
    let released = manager.bind_session("released").unwrap();
    let old = binding
        .activate("shared", config(root.path()))
        .await
        .unwrap();
    let generation = old.process().health_snapshot().generation;
    let baseline = manager.usage();
    fs::write(root.path().join("hold-shared"), b"").unwrap();
    let mut reload = Box::pin(manager.reload("shared"));
    assert!(poll!(reload.as_mut()).is_pending());
    tokio::time::timeout(DEADLINE, async {
        while starts(root.path(), "shared") < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("replacement must reach its handshake gate");
    assert_eq!(manager.usage().processes, 2);
    assert_eq!(
        manager.statuses()[0].state,
        ExtensionManagedRuntimeState::Starting
    );

    let mut waiter = Box::pin(waiting.activate("shared", config(root.path())));
    let mut released_waiter = Box::pin(released.activate("shared", config(root.path())));
    assert!(poll!(waiter.as_mut()).is_pending());
    assert!(poll!(released_waiter.as_mut()).is_pending());
    released.release().await;
    assert!(matches!(
        tokio::time::timeout(DEADLINE, released_waiter)
            .await
            .unwrap(),
        Err(ExtensionRuntimeManagerError::BindingClosed)
    ));
    assert_eq!(manager.usage().processes, 2);

    // The reload future owns the candidate charge and the lifecycle gate;
    // cancellation must restore Ready before a waiting lease can attach.
    drop(reload);
    assert_eq!(manager.usage(), baseline);
    assert_eq!(
        manager.statuses()[0].state,
        ExtensionManagedRuntimeState::Ready
    );
    let attached = tokio::time::timeout(DEADLINE, waiter)
        .await
        .unwrap()
        .unwrap();
    assert!(attached.shared());
    assert_eq!(
        attached.process().extension_instance_id(),
        old.process().extension_instance_id()
    );
    assert_eq!(attached.process().health_snapshot().generation, generation);
    assert_eq!(manager.statuses()[0].bindings, 2);
    assert_eq!(starts(root.path(), "shared"), 2);

    // Cancellation must not poison a later explicit reload or its charge.
    fs::remove_file(root.path().join("hold-shared")).unwrap();
    let reports = tokio::time::timeout(DEADLINE, manager.reload("shared"))
        .await
        .unwrap();
    assert!(matches!(reports.as_slice(), [Ok(_)]));
    assert!(old.process().health_snapshot().generation > generation);
    assert_eq!(manager.usage(), baseline);
    assert_eq!(
        manager.statuses()[0].state,
        ExtensionManagedRuntimeState::Ready
    );
    assert_eq!(starts(root.path(), "shared"), 3);
    waiting.release().await;
    binding.release().await;
    manager.shutdown().await;
    assert!(!old.process().is_running());
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
}

#[tokio::test]
async fn successful_reload_clears_prior_exhaustion_without_losing_the_charge() {
    let root = tempfile::tempdir().unwrap();
    let entries = ["resident", "other"]
        .map(|name| descriptor(root.path(), name, ExtensionLifecycleProfile::LazyResident));
    let manager = manager(
        root.path(),
        ExtensionRuntimeBudget {
            max_processes: 2,
            ..Default::default()
        },
    );
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors(entries))
        .await;
    let binding = manager.bind_session("resident-owner").unwrap();
    let other = manager.bind_session("other-owner").unwrap();
    binding
        .activate("resident", config(root.path()))
        .await
        .unwrap();
    other.activate("other", config(root.path())).await.unwrap();
    assert!(matches!(
        manager.reload("resident").await.as_slice(),
        [Err(ExtensionRuntimeManagerError::ResourceExhausted(_))]
    ));
    other.release().await;
    assert!(matches!(
        tokio::time::timeout(DEADLINE, manager.reload("resident"))
            .await
            .unwrap()
            .as_slice(),
        [Ok(_)]
    ));
    let status = manager
        .statuses()
        .into_iter()
        .find(|status| status.provenance.extension == "resident")
        .unwrap();
    assert_eq!(status.state, ExtensionManagedRuntimeState::Ready);
    assert!(status.resource_exhausted.is_none());
    assert!(status.failure.is_none());
    assert_eq!(manager.usage().processes, 1);
    binding.release().await;
    manager.shutdown().await;
}

#[tokio::test]
async fn shutdown_wakes_an_inflight_handshake_and_is_terminal() {
    let root = tempfile::tempdir().unwrap();
    let entry = descriptor(root.path(), "slow", ExtensionLifecycleProfile::LazyResident);
    fs::write(root.path().join("hold-slow"), b"").unwrap();
    let manager = manager(root.path(), ExtensionRuntimeBudget::default());
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([entry]))
        .await;
    let binding = manager.bind_session("owner").unwrap();
    let mut start = Box::pin(binding.activate("slow", config(root.path())));
    assert!(poll!(start.as_mut()).is_pending());
    wait_for_start(root.path(), "slow", start.as_mut()).await;
    manager.shutdown().await;
    assert!(matches!(
        tokio::time::timeout(DEADLINE, start).await.unwrap(),
        Err(ExtensionRuntimeManagerError::ManagerClosed)
    ));
    assert!(matches!(
        manager.bind_session("after-shutdown"),
        Err(ExtensionRuntimeManagerError::ManagerClosed)
    ));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    binding.release().await;
}
