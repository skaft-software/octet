//! Tests for the durable process fleet and its session bindings.
//!
//! These are manager tests rather than catalog or governance tests: every case
//! builds a real manager and drives activation, reload, replacement, or
//! shutdown through it, so the assertions are about fleet lifecycle and
//! fail-closed behaviour rather than about any single component in isolation.
//! They live in their own file because the manager's fixture scaffolding
//! (POSIX shell and Python extension launchers) is Unix-only and would
//! otherwise sit in the middle of the lifecycle code it exercises.

use super::*;
// Every fixture in this module launches a POSIX shell script, so the
// helpers and manifest types they build are Unix-only.
#[cfg(unix)]
use crate::extension_process::{
    ExtensionActivation, ExtensionEntrypoint, ExtensionManifest, ExtensionSource,
    ManifestContributions,
};
#[cfg(unix)]
use std::fs;
#[cfg(unix)]
use std::io::Write as _;
#[cfg(unix)]
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
fn write_script(path: &Path, source: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::write(path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
fn descriptor(
    root: &Path,
    name: &str,
    lifecycle: ExtensionLifecycleProfile,
) -> DiscoveredExtension {
    let directory = root.join(name);
    fs::create_dir_all(&directory).unwrap();
    let script = directory.join("runner.sh");
    write_script(
        &script,
        r#"#!/bin/sh
printf started >> "$OCTET_WORKSPACE/starts"
IFS= read -r initialize
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}'
while IFS= read -r line; do
  case "$line" in
*'"method":"shutdown"'*) printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{}}'; exit 0 ;;
  esac
done
"#,
    );
    let manifest_path = directory.join("extension.toml");
    let sharing = match lifecycle {
        ExtensionLifecycleProfile::WorkspaceService
        | ExtensionLifecycleProfile::Always
        | ExtensionLifecycleProfile::PiAggregate => ExtensionRuntimeSharing::Workspace,
        _ => ExtensionRuntimeSharing::Isolated,
    };
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
        runtime: crate::extension_process::ExtensionRuntimeSettings { lifecycle, sharing },
    };
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

#[cfg(unix)]
fn python_descriptor(root: &Path, name: &str) -> DiscoveredExtension {
    let mut selected = descriptor(root, name, ExtensionLifecycleProfile::WorkspaceService);
    let directory = selected.manifest_path.parent().unwrap();
    let package = directory.join("localpkg");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("__init__.py"), "").unwrap();
    fs::write(package.join("helper.py"), "START_TEXT = 'started'\n").unwrap();
    write_script(
        &directory.join("runner.py"),
        r#"#!/usr/bin/env python3
import os
import sys
sys.path.insert(0, os.environ['OCTET_EXTENSION_DIR'])
from localpkg.helper import START_TEXT
with open(os.path.join(os.environ['OCTET_WORKSPACE'], 'starts'), 'a') as output:
output.write(START_TEXT)
for line in sys.stdin:
if '"method":"initialize"' in line:
    print('{"jsonrpc":"2.0","id":1,"result":{"api_version":"0.2","tools":[],"commands":[],"protocol":{"version":"0.2","features":["request_cancellation","content_parts"],"limits":{"max_concurrent_requests":1}}}}', flush=True)
if '"method":"shutdown"' in line:
    print('{"jsonrpc":"2.0","id":2,"result":{}}', flush=True)
    break
"#,
    );
    selected.manifest.entrypoint.command = "runner.py".into();
    fs::write(
        &selected.manifest_path,
        toml::to_string(&selected.manifest).unwrap(),
    )
    .unwrap();
    selected
}

#[cfg(unix)]
#[tokio::test]
async fn imported_python_helper_edit_fences_start_and_retires_live_runtime() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = python_descriptor(temporary.path(), "workspace-service");
    let helper = selected
        .manifest_path
        .parent()
        .unwrap()
        .join("localpkg/helper.py");
    let initial = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    assert!(initial.get("workspace-service").unwrap().source_verified);
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    manager.replace_catalog(initial).await;
    fs::write(&helper, "START_TEXT = 'changed'\n").unwrap();
    let binding = manager.bind_session("session-a").unwrap();
    assert!(matches!(
        binding
            .activate(
                "workspace-service",
                ExtensionRuntimeConfig::new(temporary.path())
            )
            .await,
        Err(ExtensionRuntimeManagerError::StaleSource)
    ));
    assert!(!temporary.path().join("starts").exists());

    let updated = ExtensionRuntimeCatalog::from_descriptors([selected]);
    manager.replace_catalog(updated).await;
    binding
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(temporary.path().join("starts")).unwrap(),
        "changed"
    );
    fs::write(&helper, "START_TEXT = 'changed-again'\n").unwrap();
    assert!(matches!(
        manager.reload("workspace-service").await.as_slice(),
        [Err(ExtensionRuntimeManagerError::StaleSource)]
    ));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    binding.release().await;
    manager.shutdown().await;
}

#[cfg(unix)]
#[test]
fn inactive_python_source_is_not_scanned_until_it_can_activate() {
    use std::os::unix::fs::symlink;
    let temporary = tempfile::tempdir().unwrap();
    let mut selected = python_descriptor(temporary.path(), "python-service");
    let package = selected
        .manifest_path
        .parent()
        .unwrap()
        .join("linked-package");
    symlink(temporary.path(), &package).unwrap();
    selected.activation.enabled = false;
    let inactive = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    assert_eq!(inactive.digest_work().inactive, 1);
    assert_eq!(inactive.digest_work().files, 0);
    assert_eq!(inactive.digest_work().bytes, 0);
    assert!(inactive.diagnostics().is_empty());
    assert!(!inactive.get("python-service").unwrap().source_verified);

    selected.activation.enabled = true;
    selected.activation.trust = ExtensionTrust::Untrusted;
    let untrusted = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    assert_eq!(untrusted.digest_work().inactive, 1);
    assert_eq!(untrusted.digest_work().files, 0);
    assert!(untrusted.diagnostics().is_empty());

    selected.activation.trust = ExtensionTrust::Trusted;
    let enabled = ExtensionRuntimeCatalog::from_descriptors([selected]);
    assert!(!enabled.diagnostics().is_empty());
    assert!(!enabled.get("python-service").unwrap().source_verified);
    assert!(enabled.digest_work().files > 0);
}

#[cfg(unix)]
#[test]
fn python_package_digest_binds_vendored_modules_and_rejects_unverified_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let selected = python_descriptor(temporary.path(), "python-service");
    let directory = selected.manifest_path.parent().unwrap();
    let initial = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    let initial_digest = initial
        .get("python-service")
        .unwrap()
        .content_digest
        .clone();
    let vendor = directory.join("vendor/octet_extension");
    fs::create_dir_all(&vendor).unwrap();
    fs::write(vendor.join("__init__.py"), "").unwrap();
    let module = vendor.join("extension.py");
    fs::write(&module, "VALUE = 1\n").unwrap();
    let added = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    let added_digest = added.get("python-service").unwrap().content_digest.clone();
    assert_ne!(initial_digest, added_digest);
    fs::write(&module, "VALUE = 2\n").unwrap();
    let edited = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    assert_ne!(
        added_digest,
        edited.get("python-service").unwrap().content_digest
    );
    fs::remove_file(&module).unwrap();
    let removed = ExtensionRuntimeCatalog::from_descriptors([selected.clone()]);
    assert_ne!(
        added_digest,
        removed.get("python-service").unwrap().content_digest
    );
    use std::os::unix::fs::symlink;
    symlink(directory.join("localpkg/helper.py"), &module).unwrap();
    let unverified = ExtensionRuntimeCatalog::from_descriptors([selected]);
    assert!(!unverified.get("python-service").unwrap().source_verified);
    assert!(!unverified.diagnostics().is_empty());
}

#[test]
fn dropping_a_temporary_binding_does_not_release_its_owner() {
    let temporary = tempfile::tempdir().unwrap();
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    let binding = manager.bind_session("session-a").unwrap();
    drop(binding.clone());
    assert!(!binding.released.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn static_lazy_catalog_never_starts_eligible_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptors = (0..100)
        .map(|index| {
            descriptor(
                temporary.path(),
                &format!("lazy-{index}"),
                ExtensionLifecycleProfile::LazyResident,
            )
        })
        .collect::<Vec<_>>();
    let catalog = ExtensionRuntimeCatalog::from_descriptors(descriptors);
    let domain = ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap();
    let manager = ExtensionRuntimeManager::new(domain);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(manager.replace_catalog(catalog));
    assert_eq!(manager.statuses().len(), 100);
    assert!(manager
        .statuses()
        .iter()
        .all(|status| status.state == ExtensionManagedRuntimeState::Eligible));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    assert!(!temporary.path().join("starts").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_wakes_startup_waiters_without_launching_a_child() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptor = descriptor(
        temporary.path(),
        "lazy-runtime",
        ExtensionLifecycleProfile::LazyResident,
    );
    let manager = ExtensionRuntimeManager::with_budget(
        ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap(),
        ExtensionRuntimeBudget {
            max_processes: 1,
            max_file_descriptors: ESTIMATED_PROCESS_FDS,
            max_buffered_bytes: usize::MAX,
            max_concurrent_startups: 1,
            startup_timeout: Duration::from_secs(5),
            max_reloads_per_window: 1,
            max_restarts_per_window: 1,
            restart_window: Duration::from_secs(1),
            restart_backoff: Duration::from_millis(1),
        },
    )
    .unwrap();
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([descriptor]))
        .await;
    let permit = Arc::clone(&manager.inner.startup_slots)
        .acquire_owned()
        .await
        .unwrap();
    let binding = manager.bind_session("session-a").unwrap();
    let workspace = temporary.path().to_owned();
    let mut pending = tokio::task::JoinSet::new();
    let mut ready = Vec::new();
    // Spawn real waiters and confirm that each has registered its executor
    // waker before shutdown; observing Starting alone only proves that one
    // of the eight callers has reached the startup reservation.
    for _ in 0..8 {
        let binding = binding.clone();
        let workspace = workspace.clone();
        let (signal, received) = tokio::sync::oneshot::channel();
        ready.push(received);
        pending.spawn(async move {
            let mut activation =
                Box::pin(binding.activate("lazy-runtime", ExtensionRuntimeConfig::new(workspace)));
            let mut signal = Some(signal);
            std::future::poll_fn(|cx| {
                let polled = std::future::Future::poll(activation.as_mut(), cx);
                if let Some(signal) = signal.take() {
                    let readiness = match &polled {
                        std::task::Poll::Pending => Ok(()),
                        std::task::Poll::Ready(Err(error)) => {
                            Err(format!("activation completed before shutdown: {error:?}"))
                        }
                        std::task::Poll::Ready(Ok(_)) => {
                            Err("activation launched a child before shutdown".to_owned())
                        }
                    };
                    let _ = signal.send(readiness);
                }
                polled
            })
            .await
        });
    }
    let readiness = tokio::time::timeout(Duration::from_secs(1), join_all(ready))
        .await
        .expect("all activations must be polled before shutdown");
    for received in readiness {
        received
            .expect("activation dropped its readiness signal")
            .expect("activation must remain pending while the startup slot is held");
    }
    assert_eq!(lock(&manager.inner.state).starting.len(), 1);
    assert!(manager
        .statuses()
        .iter()
        .any(|status| status.state == ExtensionManagedRuntimeState::Starting));

    manager.shutdown().await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while let Some(result) = pending.join_next().await {
            match result.unwrap() {
                Err(ExtensionRuntimeManagerError::ManagerClosed) => {}
                Err(error) => panic!("shutdown returned {error:?} instead of ManagerClosed"),
                Ok(_) => panic!("shutdown admitted a runtime"),
            }
        }
    })
    .await
    .expect("shutdown must wake both the startup owner and coalesced activations");
    assert!(matches!(
        binding
            .activate(
                "lazy-runtime",
                ExtensionRuntimeConfig::new(temporary.path())
            )
            .await,
        Err(ExtensionRuntimeManagerError::ManagerClosed)
    ));
    assert!(matches!(
        manager.bind_session("session-b"),
        Err(ExtensionRuntimeManagerError::ManagerClosed)
    ));
    drop(permit);
    assert!(!temporary.path().join("starts").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn compatible_binding_reuses_content_bound_workspace_service() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptor = descriptor(
        temporary.path(),
        "workspace-service",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let catalog = ExtensionRuntimeCatalog::from_descriptors([descriptor]);
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    manager.replace_catalog(catalog).await;
    let first = manager.bind_session("session-a").unwrap();
    let first_process = first
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap()
        .process()
        .extension_instance_id()
        .to_owned();
    first.release().await;
    let second = manager.bind_session("session-b").unwrap();
    let second_process = second
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap()
        .process()
        .extension_instance_id()
        .to_owned();
    assert_eq!(first_process, second_process);
    assert_eq!(
        fs::read_to_string(temporary.path().join("starts")).unwrap(),
        "started"
    );
    second.release().await;
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn shared_runtime_rejects_active_session_lifecycle_service() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptor = descriptor(
        temporary.path(),
        "workspace-service",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let catalog = ExtensionRuntimeCatalog::from_descriptors([descriptor]);
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    manager.replace_catalog(catalog).await;
    let binding = manager.bind_session("session-a").unwrap();
    let (service, _receiver) =
        crate::extension_process::ExtensionSessionLifecycleService::channel(1).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temporary.path());
    config.session_lifecycle = Some(service);

    assert!(matches!(
        binding.activate("workspace-service", config).await,
        Err(ExtensionRuntimeManagerError::SharedServiceUnsupported)
    ));
    assert!(!temporary.path().join("starts").exists());
    binding.release().await;
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn source_change_is_retired_fail_closed_and_releases_governance() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptor = descriptor(
        temporary.path(),
        "workspace-service",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let script = descriptor.manifest_path.parent().unwrap().join("runner.sh");
    let catalog = ExtensionRuntimeCatalog::from_descriptors([descriptor]);
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    manager.replace_catalog(catalog).await;
    let binding = manager.bind_session("session-a").unwrap();
    binding
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(script)
        .unwrap()
        .write_all(b"\n# changed\n")
        .unwrap();
    let result = manager.reload("workspace-service").await;
    assert!(matches!(
        result.as_slice(),
        [Err(ExtensionRuntimeManagerError::StaleSource)]
    ));
    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    assert!(manager.statuses().iter().any(|status| {
        status.state == ExtensionManagedRuntimeState::StaleSource
            && status.failure == Some(ExtensionRuntimeFailure::StaleSource)
    }));
    binding.release().await;
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn catalog_replacement_retires_old_content_without_overwriting_new_status() {
    let temporary = tempfile::tempdir().unwrap();
    let descriptor = descriptor(
        temporary.path(),
        "workspace-service",
        ExtensionLifecycleProfile::WorkspaceService,
    );
    let replacement = descriptor.clone();
    let script = descriptor.manifest_path.parent().unwrap().join("runner.sh");
    let manager =
        ExtensionRuntimeManager::new(ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap());
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([descriptor]))
        .await;
    let first = manager.bind_session("session-a").unwrap();
    first
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap();

    fs::OpenOptions::new()
        .append(true)
        .open(script)
        .unwrap()
        .write_all(b"\n# replacement\n")
        .unwrap();
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([replacement]))
        .await;

    assert_eq!(manager.usage(), ExtensionRuntimeUsage::default());
    assert!(matches!(
        manager.statuses().as_slice(),
        [ExtensionRuntimeStatus {
            state: ExtensionManagedRuntimeState::Eligible,
            resource_exhausted: None,
            failure: None,
            ..
        }]
    ));

    let second = manager.bind_session("session-b").unwrap();
    second
        .activate(
            "workspace-service",
            ExtensionRuntimeConfig::new(temporary.path()),
        )
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(temporary.path().join("starts")).unwrap(),
        "startedstarted"
    );
    first.release().await;
    second.release().await;
    manager.shutdown().await;
}

#[cfg(unix)]
#[tokio::test]
async fn resource_exhaustion_is_typed_and_does_not_erase_other_entries() {
    let temporary = tempfile::tempdir().unwrap();
    let first = descriptor(
        temporary.path(),
        "first",
        ExtensionLifecycleProfile::LazyResident,
    );
    let second = descriptor(
        temporary.path(),
        "second",
        ExtensionLifecycleProfile::LazyResident,
    );
    let manager = ExtensionRuntimeManager::with_budget(
        ExtensionRuntimeDomain::ordinary(temporary.path()).unwrap(),
        ExtensionRuntimeBudget {
            max_processes: 1,
            max_file_descriptors: 8,
            max_buffered_bytes: usize::MAX,
            max_concurrent_startups: 1,
            startup_timeout: Duration::from_secs(2),
            max_reloads_per_window: 2,
            max_restarts_per_window: 2,
            restart_window: Duration::from_secs(2),
            restart_backoff: Duration::from_millis(1),
        },
    )
    .unwrap();
    manager
        .replace_catalog(ExtensionRuntimeCatalog::from_descriptors([first, second]))
        .await;
    let binding = manager.bind_session("session-a").unwrap();
    binding
        .activate("first", ExtensionRuntimeConfig::new(temporary.path()))
        .await
        .unwrap();
    let error = match binding
        .activate("second", ExtensionRuntimeConfig::new(temporary.path()))
        .await
    {
        Err(error) => error,
        Ok(_) => panic!("second process should exceed the process budget"),
    };
    assert!(matches!(
        error,
        ExtensionRuntimeManagerError::ResourceExhausted(ExtensionResourceExhausted {
            resource: ExtensionRuntimeResource::Processes,
            ..
        })
    ));
    assert_eq!(manager.statuses().len(), 2);
    binding.release().await;
    manager.shutdown().await;
}
