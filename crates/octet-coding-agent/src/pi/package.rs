#![allow(missing_docs)]

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use fs2::FileExt;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{fingerprint_source, fingerprint_source_locks, PiPackageManager, SourceFingerprint};

const MAX_PACKAGE_JSON_BYTES: usize = 512 * 1024;
const MAX_PACKAGE_FILES: usize = 8192;
const MAX_PACKAGE_BYTES: usize = 128 * 1024 * 1024;
const MAX_PACKAGE_PATH_BYTES: usize = 4096;
const MAX_PACKAGE_DEPTH: usize = 64;
const MAX_PACKAGE_LOCK_BYTES: usize = 64 * 1024 * 1024;
const MIN_NODE_VERSION: &str = "22.19.0";
const PACKAGE_STORE_NAME: &str = "pi-packages";
const PACKAGE_STORE_LOCK: &str = ".package-store.lock";
const PACKAGE_RECORD: &str = "package-install.json";
const PACKAGE_RECORD_SCHEMA: u32 = 1;
const LOCK_NAMES: [&str; 5] = [
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageResources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) extensions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) skills: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) prompts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) themes: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageDependency {
    pub(super) name: String,
    pub(super) spec: String,
    pub(super) kind: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PackageIdentity {
    pub(super) input: String,
    pub(super) kind: String,
    pub(super) name: String,
    pub(super) version: String,
    pub(super) package_manager: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) node_requirement: Option<String>,
    pub(super) resolved_root: PathBuf,
    pub(super) dependency_root: PathBuf,
    pub(super) manifest_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dependency_lock_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) dependency_tree_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) integrity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) resolved_revision: Option<String>,
    pub(super) dependencies: Vec<PackageDependency>,
    pub(super) resources: PackageResources,
    pub(super) entrypoints: Vec<String>,
    pub(super) lifecycle_scripts: Vec<String>,
}

impl PackageIdentity {
    pub(super) fn summary(&self) -> String {
        let integrity = self.integrity.as_deref().unwrap_or("none");
        let lock = self.dependency_lock_sha256.as_deref().unwrap_or("none");
        let tree = self
            .dependency_tree_sha256
            .as_deref()
            .unwrap_or("not-installed");
        format!(
            "input={} kind={} package={}@{} manager={} integrity={} lock={} dependency_tree={} lifecycle={}",
            self.input,
            self.kind,
            self.name,
            self.version,
            self.package_manager,
            integrity,
            lock,
            tree,
            if self.lifecycle_scripts.is_empty() {
                "none"
            } else {
                "present"
            }
        )
    }

    fn has_runtime_dependencies(&self) -> bool {
        self.dependencies.iter().any(|dependency| {
            dependency.kind == "dependencies" || dependency.kind == "optionalDependencies"
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PackageInput {
    Local {
        path: PathBuf,
        display: String,
    },
    Npm {
        spec: String,
        name: String,
        display: String,
    },
    Git {
        repo: String,
        reference: Option<String>,
        display: String,
    },
}

impl PackageInput {
    fn display(&self) -> &str {
        match self {
            Self::Local { display, .. } | Self::Npm { display, .. } | Self::Git { display, .. } => {
                display
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRecord {
    schema_version: u32,
    identity: PackageIdentity,
    #[serde(default)]
    scripts_enabled: bool,
}

pub(super) struct PackageStoreLock {
    path: PathBuf,
    file: fs::File,
    identity: Option<octet_agent::secure_fs::PrivateLockIdentity>,
}

impl Drop for PackageStoreLock {
    fn drop(&mut self) {
        if let Some(identity) = self.identity.as_ref() {
            let _ = octet_agent::secure_fs::revalidate_private_lock_before_release(
                &self.path, &self.file, identity,
            );
        }
        let _ = self.file.unlock();
    }
}

pub(super) fn acquire_package_store_lock(
    extension_root: &Path,
) -> anyhow::Result<PackageStoreLock> {
    let store = package_store_root(extension_root)?;
    let path = store.join(PACKAGE_STORE_LOCK);
    let file = octet_agent::secure_fs::open_private_lock_file(&path).map_err(|error| {
        anyhow::anyhow!(
            "cannot open private Pi package-store lock {}: {error}",
            path.display()
        )
    })?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "cannot acquire private Pi package-store lock {}",
            path.display()
        )
    })?;
    let identity = match octet_agent::secure_fs::validate_private_lock_after_acquire(&path, &file) {
        Ok(identity) => identity,
        Err(error) => {
            let _ = file.unlock();
            return Err(anyhow::anyhow!(
                "private Pi package-store lock changed while acquiring {}: {error}",
                path.display()
            ));
        }
    };
    Ok(PackageStoreLock {
        path,
        file,
        identity: Some(identity),
    })
}

#[derive(Debug)]
struct PackageInspection {
    identity: PackageIdentity,
    needs_install: bool,
}

/// A resolved source and the optional private package transaction that supplies
/// its dependency tree. The transaction remains uncommitted until the
/// generated Pi link has been published successfully.
pub(super) struct PreparedSource {
    pub(super) requested: PathBuf,
    pub(super) source: PathBuf,
    pub(super) package: Option<PackageIdentity>,
    transaction: Option<PackageTransaction>,
    lifecycle_root: Option<PathBuf>,
    record_root: Option<PathBuf>,
}

struct PackageTransaction {
    root: PathBuf,
    committed: bool,
}

impl PackageTransaction {
    fn new(root: PathBuf) -> anyhow::Result<Self> {
        let parent = root
            .parent()
            .ok_or_else(|| anyhow::anyhow!("Pi package staging path has no parent"))?;
        octet_agent::secure_fs::create_private_directory_all(parent)?;
        fs::create_dir(&root).with_context(|| {
            format!("cannot create private Pi package store {}", root.display())
        })?;
        make_private_directory(&root)?;
        Ok(Self {
            root,
            committed: false,
        })
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for PackageTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub(super) fn resolve_unexecuted_sources(
    requested_sources: &[PathBuf],
    cwd: &Path,
    package_manager: PiPackageManager,
) -> anyhow::Result<Vec<PreparedSource>> {
    requested_sources
        .iter()
        .map(|requested| {
            let input = parse_input(requested)?;
            match input {
                PackageInput::Local { path, display } => {
                    let source = resolve_local_path(&path, cwd)?;
                    let package = inspect_package(&source, &display, package_manager)?;
                    Ok(PreparedSource {
                        requested: requested.clone(),
                        source,
                        package: package.map(|inspection| inspection.identity),
                        transaction: None,
                        lifecycle_root: None,
                        record_root: None,
                    })
                }
                PackageInput::Npm { .. } | PackageInput::Git { .. } => anyhow::bail!(
                    "Pi package input {} needs an explicit reviewed install; plan/preflight never download packages. Run `octet pi install --allow-network {}`",
                    input.display(),
                    input.display()
                ),
            }
        })
        .collect()
}

/// Resolve local inputs and execute only the dependency acquisition step needed
/// to make a fresh package usable. All manager invocations use `--ignore-scripts`;
/// lifecycle execution is a separate call made after the caller prints review
/// metadata and only when the caller supplied the explicit scripts opt-in.
pub(super) fn prepare_sources(
    requested_sources: &[PathBuf],
    cwd: &Path,
    extension_root: &Path,
    package_manager: PiPackageManager,
    allow_network: bool,
    allow_scripts: bool,
) -> anyhow::Result<Vec<PreparedSource>> {
    check_node_runtime()?;
    let store = package_store_root(extension_root)?;
    let mut prepared = Vec::with_capacity(requested_sources.len());

    for requested in requested_sources {
        let input = parse_input(requested)?;
        match input {
            PackageInput::Local { path, display } => {
                let source = resolve_local_path(&path, cwd)?;
                let Some(initial) = inspect_package(&source, &display, package_manager)? else {
                    prepared.push(PreparedSource {
                        requested: requested.clone(),
                        source,
                        package: None,
                        transaction: None,
                        lifecycle_root: None,
                        record_root: None,
                    });
                    continue;
                };

                let needs_scripts = allow_scripts
                    && (initial.needs_install || !initial.identity.lifecycle_scripts.is_empty());
                let source_fingerprint = if initial.needs_install || needs_scripts {
                    Some(fingerprint_source(&source).map_err(|_| {
                        anyhow::anyhow!(
                            "Pi package {} source cannot be snapshotted safely; review the package and retry",
                            display
                        )
                    })?)
                } else {
                    None
                };
                if !initial.needs_install && !needs_scripts {
                    prepared.push(PreparedSource {
                        requested: requested.clone(),
                        source,
                        package: Some(initial.identity),
                        transaction: None,
                        lifecycle_root: None,
                        record_root: None,
                    });
                    continue;
                }
                let source_fingerprint = source_fingerprint.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Pi package source fingerprint is unavailable")
                })?;
                let key = store_key(
                    &format!("local:{}:{}", display, source_fingerprint.digest),
                    package_manager,
                    allow_scripts,
                );
                let final_root = store.join(key);
                let package_root = final_root.join("package");
                if let Some(cached) = cached_package(
                    &final_root,
                    &package_root,
                    &display,
                    &display,
                    package_manager,
                    "local",
                    None,
                    allow_scripts,
                )? {
                    prepared.push(PreparedSource {
                        requested: requested.clone(),
                        source: cached.identity.resolved_root.clone(),
                        package: Some(cached.identity),
                        transaction: None,
                        lifecycle_root: None,
                        record_root: None,
                    });
                    continue;
                }
                if initial.needs_install && !allow_network {
                    anyhow::bail!(
                        "Pi package {} has missing runtime dependencies; rerun with --allow-network for the reviewed dependency step",
                        display
                    );
                }
                let transaction = PackageTransaction::new(final_root.clone())?;
                copy_package_tree(&source, &package_root)?;
                let lock_present = has_supported_lock(&package_root)?;
                run_package_manager(
                    &package_root,
                    package_manager,
                    None,
                    lock_present,
                    allow_network,
                    false,
                )?;
                let after = inspect_package_at(
                    &package_root,
                    &display,
                    package_manager,
                    &package_root,
                    "local",
                    None,
                    None,
                )?;
                if after.needs_install {
                    anyhow::bail!(
                        "Pi package {} dependency installation completed without a complete runtime dependency tree",
                        display
                    );
                }
                if fingerprint_source(&package_root)
                    .map_err(|_| anyhow::anyhow!("staged Pi package source cannot be verified"))?
                    != *source_fingerprint
                {
                    anyhow::bail!(
                        "Pi package {} changed while dependencies were being prepared; no link was published",
                        display
                    );
                }
                write_package_record(&final_root, &after.identity, allow_scripts)?;
                let lifecycle_root = needs_scripts.then(|| package_root.clone());
                prepared.push(PreparedSource {
                    requested: requested.clone(),
                    source: package_root,
                    package: Some(after.identity),
                    transaction: Some(transaction),
                    lifecycle_root,
                    record_root: Some(final_root),
                });
            }
            PackageInput::Npm {
                spec,
                name,
                display,
            } => {
                let key = store_key(&format!("npm:{spec}"), package_manager, allow_scripts);
                let final_root = store.join(key);
                let package_root = final_root.join("workspace/package");
                if let Some(cached) = cached_package(
                    &final_root,
                    &package_root,
                    &display,
                    &display,
                    package_manager,
                    "npm",
                    None,
                    allow_scripts,
                )? {
                    prepared.push(PreparedSource {
                        requested: requested.clone(),
                        source: cached.identity.resolved_root.clone(),
                        package: Some(cached.identity),
                        transaction: None,
                        lifecycle_root: None,
                        record_root: None,
                    });
                    continue;
                }
                if !allow_network {
                    anyhow::bail!(
                        "Pi npm package {} is not installed; rerun with --allow-network for the reviewed download and dependency step",
                        display
                    );
                }
                let transaction = PackageTransaction::new(final_root.clone())?;
                let workspace = final_root.join("workspace");
                make_private_directory(&workspace)?;
                write_npm_workspace_manifest(&workspace, &name, &spec)?;
                run_package_manager(
                    &workspace,
                    package_manager,
                    Some(&spec),
                    false,
                    allow_network,
                    false,
                )?;
                let package_root = workspace.join("package");
                materialize_npm_package(&workspace, &name, &package_root, &display)?;
                mirror_workspace_locks(&workspace, &package_root)?;
                let inspection = inspect_package_at(
                    &package_root,
                    &display,
                    package_manager,
                    &workspace,
                    "npm",
                    None,
                    Some(&spec),
                )?;
                if inspection.needs_install {
                    anyhow::bail!(
                        "Pi npm package {} did not produce a complete runtime dependency tree",
                        display
                    );
                }
                write_package_record(&final_root, &inspection.identity, allow_scripts)?;
                let lifecycle_root = allow_scripts.then_some(workspace);
                prepared.push(PreparedSource {
                    requested: requested.clone(),
                    source: package_root,
                    package: Some(inspection.identity),
                    transaction: Some(transaction),
                    lifecycle_root,
                    record_root: Some(final_root),
                });
            }
            PackageInput::Git {
                repo,
                reference,
                display,
            } => {
                let identity_input = git_identity_input(&repo, reference.as_deref());
                let exact_reference = reference
                    .as_deref()
                    .filter(|value| is_exact_git_revision(value))
                    .map(str::to_ascii_lowercase);
                if let Some(revision) = exact_reference.as_deref() {
                    let key = store_key(
                        &format!("git:{repo}@{revision}"),
                        package_manager,
                        allow_scripts,
                    );
                    let final_root = store.join(key);
                    let package_root = final_root.join("workspace/package");
                    if let Some(cached) = cached_package(
                        &final_root,
                        &package_root,
                        &display,
                        &identity_input,
                        package_manager,
                        "git",
                        Some(revision),
                        allow_scripts,
                    )? {
                        prepared.push(PreparedSource {
                            requested: requested.clone(),
                            source: cached.identity.resolved_root.clone(),
                            package: Some(cached.identity),
                            transaction: None,
                            lifecycle_root: None,
                            record_root: None,
                        });
                        continue;
                    }
                }
                if !allow_network {
                    anyhow::bail!(
                        "Pi git package {} is not installed; rerun with --allow-network for the reviewed clone and dependency step",
                        display
                    );
                }
                let stage_root = store.join(format!(".pi-package-transaction-{}", unique_suffix()));
                let mut transaction = PackageTransaction::new(stage_root.clone())?;
                let stage_workspace = stage_root.join("workspace");
                let stage_package_root = stage_workspace.join("package");
                make_private_directory(&stage_workspace)?;
                clone_git_package(&repo, reference.as_deref(), &stage_package_root, &display)?;
                let has_manifest = fs::symlink_metadata(stage_package_root.join("package.json"))
                    .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
                    .unwrap_or(false);
                if has_manifest {
                    let lock_present = has_supported_lock(&stage_package_root)?;
                    run_package_manager(
                        &stage_package_root,
                        package_manager,
                        None,
                        lock_present,
                        allow_network,
                        false,
                    )?;
                }
                let revision = git_revision(&stage_package_root)?;
                let staged_inspection = inspect_package_at(
                    &stage_package_root,
                    &identity_input,
                    package_manager,
                    &stage_package_root,
                    "git",
                    Some(revision.clone()),
                    None,
                )?;
                if staged_inspection.needs_install {
                    anyhow::bail!(
                        "Pi git package {} did not produce a complete runtime dependency tree",
                        display
                    );
                }
                let key = store_key(
                    &format!("git:{repo}@{revision}"),
                    package_manager,
                    allow_scripts,
                );
                let final_root = store.join(key);
                let final_package_root = final_root.join("workspace/package");
                if final_root.exists() {
                    if let Some(cached) = cached_package(
                        &final_root,
                        &final_package_root,
                        &display,
                        &identity_input,
                        package_manager,
                        "git",
                        Some(&revision),
                        allow_scripts,
                    )? {
                        drop(transaction);
                        prepared.push(PreparedSource {
                            requested: requested.clone(),
                            source: cached.identity.resolved_root.clone(),
                            package: Some(cached.identity),
                            transaction: None,
                            lifecycle_root: None,
                            record_root: None,
                        });
                        continue;
                    }
                    anyhow::bail!(
                        "Pi git package store entry {} exists but failed identity validation; refusing to replace it",
                        final_root.display()
                    );
                }
                fs::rename(&stage_root, &final_root).with_context(|| {
                    format!(
                        "cannot publish private Pi git package staging tree {}",
                        final_root.display()
                    )
                })?;
                transaction.root = final_root.clone();
                let package_root = final_package_root;
                let inspection = inspect_package_at(
                    &package_root,
                    &identity_input,
                    package_manager,
                    &package_root,
                    "git",
                    Some(revision),
                    None,
                )?;
                write_package_record(&final_root, &inspection.identity, allow_scripts)?;
                let lifecycle_root = allow_scripts.then_some(package_root.clone());
                prepared.push(PreparedSource {
                    requested: requested.clone(),
                    source: package_root,
                    package: Some(inspection.identity),
                    transaction: Some(transaction),
                    lifecycle_root,
                    record_root: Some(final_root),
                });
            }
        }
    }
    Ok(prepared)
}

pub(super) fn run_lifecycle(
    prepared: &mut [PreparedSource],
    package_manager: PiPackageManager,
    allow_network: bool,
    allow_scripts: bool,
) -> anyhow::Result<()> {
    if !allow_scripts {
        return Ok(());
    }
    if !allow_network {
        anyhow::bail!("Pi lifecycle execution requires explicit network approval");
    }
    for source in prepared.iter_mut() {
        let Some(root) = source.lifecycle_root.as_ref() else {
            continue;
        };
        let lock_present = has_supported_lock(root)?;
        run_package_manager(
            root,
            package_manager,
            None,
            lock_present,
            allow_network,
            true,
        )?;
        if let Some(identity) = source.package.as_mut() {
            if identity.kind == "npm" {
                materialize_npm_package(
                    &identity.dependency_root,
                    &identity.name,
                    &identity.resolved_root,
                    &identity.input,
                )?;
                mirror_workspace_locks(&identity.dependency_root, &identity.resolved_root)?;
            }
            let resolved_revision = if identity.kind == "git" {
                let revision = git_revision(&identity.resolved_root)?;
                if identity.resolved_revision.as_deref() != Some(revision.as_str()) {
                    anyhow::bail!(
                        "Pi git package {} changed its checked-out revision during lifecycle execution",
                        identity.input
                    );
                }
                Some(revision)
            } else {
                identity.resolved_revision.clone()
            };
            let refreshed = inspect_package_at(
                &identity.resolved_root,
                &identity.input,
                package_manager,
                &identity.dependency_root,
                &identity.kind,
                resolved_revision,
                identity.input.strip_prefix("npm:"),
            )?;
            if refreshed.needs_install {
                anyhow::bail!(
                    "Pi lifecycle execution left {} without a complete runtime dependency tree",
                    identity.input
                );
            }
            *identity = refreshed.identity;
            if let Some(record_root) = source.record_root.as_ref() {
                write_package_record(record_root, identity, true)?;
            }
        }
    }
    Ok(())
}

pub(super) fn commit_sources(prepared: &mut [PreparedSource]) {
    for source in prepared {
        if let Some(transaction) = source.transaction.as_mut() {
            transaction.commit();
        }
    }
}

pub(super) fn has_package_execution(prepared: &[PreparedSource]) -> bool {
    prepared.iter().any(|source| source.package.is_some())
}

pub(super) fn review_lines(
    prepared: &[PreparedSource],
    package_manager: PiPackageManager,
    allow_network: bool,
    allow_scripts: bool,
) -> Vec<String> {
    let mut lines = Vec::new();
    for source in prepared {
        if let Some(identity) = &source.package {
            lines.push(format!(
                "  {} · resolved={} · network={} · lifecycle={}",
                identity.summary(),
                identity.resolved_root.display(),
                if allow_network {
                    "explicit"
                } else {
                    "not-requested"
                },
                if allow_scripts {
                    "explicit"
                } else {
                    "denied (--ignore-scripts)"
                },
            ));
        }
    }
    if !lines.is_empty() {
        lines.insert(
            0,
            format!(
                "Pi package execution review (manager={}, Node >= {}):",
                package_manager.name(),
                MIN_NODE_VERSION
            ),
        );
    }
    lines
}

pub(super) fn identity_bytes(identity: &PackageIdentity) -> anyhow::Result<Vec<u8>> {
    Ok(serde_json::to_vec(identity)?)
}

pub(super) fn verify_identity(identity: &PackageIdentity, source: &Path) -> anyhow::Result<()> {
    validate_identity_shape(identity)?;
    if identity.resolved_root != source {
        anyhow::bail!("resolved Pi package root changed")
    }
    let manager = PiPackageManager::parse(&identity.package_manager)
        .ok_or_else(|| anyhow::anyhow!("Pi package manager identity is unsupported"))?;
    let current = inspect_package_at(
        source,
        &identity.input,
        manager,
        &identity.dependency_root,
        &identity.kind,
        identity.resolved_revision.clone(),
        identity.input.strip_prefix("npm:"),
    )?;
    let expected = identity;
    let actual = current.identity;
    if actual.input != expected.input
        || actual.kind != expected.kind
        || actual.resolved_root != expected.resolved_root
        || actual.dependency_root != expected.dependency_root
        || actual.name != expected.name
        || actual.version != expected.version
        || actual.package_manager != expected.package_manager
        || actual.node_requirement != expected.node_requirement
        || actual.manifest_sha256 != expected.manifest_sha256
        || actual.dependency_lock_sha256 != expected.dependency_lock_sha256
        || actual.dependency_tree_sha256 != expected.dependency_tree_sha256
        || actual.integrity != expected.integrity
        || actual.resolved_revision != expected.resolved_revision
        || actual.dependencies != expected.dependencies
        || actual.resources != expected.resources
        || actual.entrypoints != expected.entrypoints
        || actual.lifecycle_scripts != expected.lifecycle_scripts
    {
        anyhow::bail!("Pi package metadata or dependency identity changed")
    }
    if current.needs_install {
        anyhow::bail!("Pi package runtime dependencies are not completely installed")
    }
    Ok(())
}

pub(super) fn validate_identity_shape(identity: &PackageIdentity) -> anyhow::Result<()> {
    if !matches!(identity.kind.as_str(), "local" | "npm" | "git") {
        anyhow::bail!("Pi package identity has an unsupported input kind");
    }
    if identity.kind == "git" {
        let Some(revision) = identity.resolved_revision.as_deref() else {
            anyhow::bail!("Pi git package identity has no resolved revision");
        };
        if !is_exact_git_revision(revision) {
            anyhow::bail!("Pi git package identity has an invalid resolved revision");
        }
    } else if identity.resolved_revision.is_some() {
        anyhow::bail!("non-git Pi package identity has a resolved revision");
    }
    if identity.input.starts_with("npm:") != (identity.kind == "npm") {
        anyhow::bail!("Pi package identity input and kind do not agree");
    }
    if identity.input.starts_with("git:") && identity.kind != "git" {
        anyhow::bail!("Pi package identity input and kind do not agree");
    }
    if !identity.input.starts_with("git:")
        && (identity.kind == "git"
            || identity.input.starts_with("http://")
            || identity.input.starts_with("https://"))
    {
        anyhow::bail!("Pi git package identity input is invalid");
    }
    if identity.input.is_empty()
        || identity.input.len() > MAX_PACKAGE_PATH_BYTES
        || identity.input.contains('\0')
        || identity.name.is_empty()
        || identity.version.is_empty()
        || identity.resolved_root.as_os_str().is_empty()
        || identity.dependency_root.as_os_str().is_empty()
        || !identity.resolved_root.is_absolute()
        || !identity.dependency_root.is_absolute()
    {
        anyhow::bail!("Pi package identity contains an invalid bounded path or name");
    }
    if PiPackageManager::parse(&identity.package_manager).is_none() {
        anyhow::bail!("Pi package identity names an unsupported package manager");
    }
    for digest in [
        Some(identity.manifest_sha256.as_str()),
        identity.dependency_lock_sha256.as_deref(),
        identity.dependency_tree_sha256.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            anyhow::bail!("Pi package identity contains an invalid digest");
        }
    }
    if let Some(integrity) = &identity.integrity {
        if integrity.len() > MAX_PACKAGE_PATH_BYTES || integrity.contains('\0') {
            anyhow::bail!("Pi package integrity metadata is invalid");
        }
    }
    Ok(())
}

fn parse_input(requested: &Path) -> anyhow::Result<PackageInput> {
    let display = requested
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Pi package input is not valid UTF-8"))?
        .to_owned();
    if let Some(spec) = display.strip_prefix("npm:") {
        let spec = spec.trim().to_owned();
        let name = npm_name(&spec)?;
        return Ok(PackageInput::Npm {
            spec,
            name,
            display,
        });
    }
    if display.starts_with("git:")
        || display.starts_with("https://")
        || display.starts_with("http://")
        || display.starts_with("ssh://")
        || display.starts_with("git://")
    {
        let (repo, reference) = parse_git_input(&display)?;
        return Ok(PackageInput::Git {
            repo,
            reference,
            display,
        });
    }
    Ok(PackageInput::Local {
        path: requested.to_owned(),
        display,
    })
}

fn npm_name(spec: &str) -> anyhow::Result<String> {
    if spec.is_empty()
        || spec.len() > 512
        || spec
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        anyhow::bail!("npm Pi package spec must be a bounded, whitespace-free package spec")
    }
    let name_end = if spec.starts_with('@') {
        let slash = spec.find('/').ok_or_else(|| {
            anyhow::anyhow!("scoped npm Pi package spec must contain a package name")
        })?;
        spec[slash + 1..]
            .find('@')
            .map(|offset| slash + 1 + offset)
            .unwrap_or(spec.len())
    } else {
        spec.find('@').unwrap_or(spec.len())
    };
    let name = &spec[..name_end];
    let valid = if let Some(rest) = name.strip_prefix('@') {
        let mut parts = rest.split('/');
        let scope = parts.next().unwrap_or_default();
        let package = parts.next().unwrap_or_default();
        !scope.is_empty()
            && !package.is_empty()
            && parts.next().is_none()
            && scope.bytes().all(valid_npm_name_byte)
            && package.bytes().all(valid_npm_name_byte)
    } else {
        !name.is_empty() && name.bytes().all(valid_npm_name_byte)
    };
    if !valid || name.starts_with('-') {
        anyhow::bail!("unsupported npm Pi package name {name:?}")
    }
    Ok(name.to_owned())
}

fn valid_npm_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/' | b'@')
}

fn git_identity_input(repo: &str, reference: Option<&str>) -> String {
    match reference {
        Some(reference) => format!("git:{repo}@{reference}"),
        None => format!("git:{repo}"),
    }
}

fn parse_git_input(input: &str) -> anyhow::Result<(String, Option<String>)> {
    let raw = input.strip_prefix("git:").unwrap_or(input);
    if raw.is_empty()
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte == 0)
    {
        anyhow::bail!("git Pi package input must be a bounded, whitespace-free URL or shorthand")
    }
    let (repo_without_ref, reference) = if raw.starts_with("git@") {
        match raw.rfind('@') {
            Some(index) if index > raw.find(':').unwrap_or(0) + 1 => {
                (raw[..index].to_owned(), Some(raw[index + 1..].to_owned()))
            }
            _ => (raw.to_owned(), None),
        }
    } else {
        match raw.rfind('@') {
            Some(index) if index > raw.find('/').unwrap_or(0) && index + 1 < raw.len() => {
                (raw[..index].to_owned(), Some(raw[index + 1..].to_owned()))
            }
            _ => (raw.to_owned(), None),
        }
    };
    if repo_without_ref.contains('?')
        || repo_without_ref.contains('#')
        || repo_without_ref.contains("..")
        || repo_without_ref.ends_with('/')
    {
        anyhow::bail!("git Pi package repository has an unsafe path")
    }
    if let Some(reference) = &reference {
        if reference.is_empty()
            || reference.starts_with('-')
            || reference.contains('/') && reference.split('/').any(|part| part == "..")
        {
            anyhow::bail!("git Pi package reference is unsafe")
        }
    }
    let repo = if repo_without_ref.starts_with("https://")
        || repo_without_ref.starts_with("http://")
        || repo_without_ref.starts_with("ssh://")
        || repo_without_ref.starts_with("git://")
        || repo_without_ref.starts_with("git@")
    {
        repo_without_ref
    } else {
        let shorthand = repo_without_ref.trim_end_matches(".git");
        let mut parts = shorthand.splitn(2, '/');
        let host = parts.next().unwrap_or_default();
        let path = parts.next().unwrap_or_default();
        if host.is_empty() || path.is_empty() {
            anyhow::bail!("git Pi package input must be a host/repository shorthand or URL")
        }
        format!("https://{host}/{path}.git")
    };
    if !repo.starts_with("https://") {
        anyhow::bail!("Pi git package input must use HTTPS; SSH credentials and insecure git transports are not accepted");
    }
    Ok((repo, reference))
}

fn resolve_local_path(path: &Path, cwd: &Path) -> anyhow::Result<PathBuf> {
    let selected = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let metadata = fs::symlink_metadata(&selected)
        .with_context(|| format!("cannot inspect Pi package input {}", path.display()))?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        anyhow::bail!(
            "Pi package input must be a regular non-symlink file or directory: {}",
            selected.display()
        );
    }
    let canonical = selected
        .canonicalize()
        .with_context(|| format!("cannot resolve Pi package input {}", selected.display()))?;
    if canonical != selected {
        anyhow::bail!(
            "Pi package input must use a canonical non-symlink path: {}",
            selected.display()
        )
    }
    Ok(canonical)
}

fn inspect_package(
    source: &Path,
    input: &str,
    package_manager: PiPackageManager,
) -> anyhow::Result<Option<PackageInspection>> {
    if !source.is_dir() || !source.join("package.json").exists() {
        return Ok(None);
    }
    Ok(Some(inspect_package_at(
        source,
        input,
        package_manager,
        source,
        "local",
        None,
        None,
    )?))
}

fn inspect_package_at(
    source: &Path,
    input: &str,
    package_manager: PiPackageManager,
    dependency_root: &Path,
    kind: &str,
    resolved_revision: Option<String>,
    npm_spec: Option<&str>,
) -> anyhow::Result<PackageInspection> {
    ensure_package_root(source, input)?;
    ensure_package_root(dependency_root, input)?;
    let resolved_revision = if kind == "git" {
        let actual = git_revision(source)?;
        if let Some(expected) = resolved_revision.as_deref() {
            if !expected.eq_ignore_ascii_case(&actual) {
                anyhow::bail!(
                    "Pi git package checked-out revision does not match its recorded identity"
                );
            }
        }
        Some(actual)
    } else {
        resolved_revision
    };
    let manifest_path = source.join("package.json");
    let bytes =
        octet_agent::secure_fs::read_regular_file_bounded(&manifest_path, MAX_PACKAGE_JSON_BYTES)
            .map_err(|_| {
            anyhow::anyhow!(
                "Pi package manifest cannot be read safely: {}",
                manifest_path.display()
            )
        })?;
    let manifest_sha256 = digest_bytes(&bytes);
    let manifest: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("invalid Pi package manifest {}", manifest_path.display()))?;
    let object = manifest
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Pi package manifest must be a JSON object"))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Pi package manifest has no non-empty name"))?;
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("Pi package manifest has no non-empty version"))?;

    let node_requirement = object
        .get("engines")
        .and_then(Value::as_object)
        .and_then(|engines| engines.get("node"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(requirement) = &node_requirement {
        let requirement = VersionReq::parse(requirement).with_context(|| {
            format!("Pi package declares an invalid Node engine {requirement:?}")
        })?;
        if !requirement.matches(&Version::parse(MIN_NODE_VERSION)?) {
            anyhow::bail!(
                "Pi package {name}@{version} requires Node {requirement}, but the pinned Pi contract is Node >= {MIN_NODE_VERSION}"
            );
        }
    }

    let manager = select_package_manager(object, dependency_root, package_manager)?;
    let resources = parse_resources(object, source)?;
    let entrypoints = package_entrypoints(&resources, source)?;
    if entrypoints.is_empty() {
        anyhow::bail!(
            "Pi package {name}@{version} has no supported extension entrypoint; declare pi.extensions or provide a conventional extensions/index entry"
        );
    }
    let dependencies = parse_dependencies(object)?;
    let lifecycle_scripts = parse_lifecycle_scripts(object)?;
    let dependency_lock_sha256 = dependency_lock_digest(dependency_root)?;
    let dependency_tree_sha256 = dependency_tree_digest(dependency_root)?;
    let needs_install = has_missing_runtime_dependencies(dependency_root, &dependencies)?;
    let integrity = npm_integrity(dependency_root, name, npm_spec);

    Ok(PackageInspection {
        identity: PackageIdentity {
            input: input.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            version: version.to_owned(),
            package_manager: manager.name().to_owned(),
            node_requirement,
            resolved_root: source.to_owned(),
            dependency_root: dependency_root.to_owned(),
            manifest_sha256,
            dependency_lock_sha256,
            dependency_tree_sha256,
            integrity,
            resolved_revision,
            dependencies,
            resources,
            entrypoints,
            lifecycle_scripts,
        },
        needs_install,
    })
}

fn ensure_package_root(path: &Path, input: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("Pi package {input} does not resolve to a readable root"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("Pi package {input} root is not a regular non-symlink directory")
    }
    let canonical = path.canonicalize()?;
    if canonical != path {
        anyhow::bail!("Pi package {input} root is not canonical")
    }
    Ok(())
}

fn select_package_manager(
    object: &serde_json::Map<String, Value>,
    root: &Path,
    requested: PiPackageManager,
) -> anyhow::Result<PiPackageManager> {
    if let Some(package_manager) = object.get("packageManager").and_then(Value::as_str) {
        let name = package_manager.split('@').next().unwrap_or_default();
        let declared = PiPackageManager::parse(name).ok_or_else(|| {
            anyhow::anyhow!("Pi package declares unsupported packageManager {package_manager:?}")
        })?;
        if declared != requested {
            anyhow::bail!(
                "Pi package requires package manager {}, but this reviewed install selected {}",
                declared.name(),
                requested.name(),
            );
        }
    }
    let locks = supported_lock_paths(root)?;
    if locks.len() > 1 {
        anyhow::bail!(
            "Pi package has multiple package-manager lockfiles; retain exactly one reviewed lock"
        )
    }
    if let Some((lock, _)) = locks.first() {
        let declared = match lock.as_str() {
            "package-lock.json" | "npm-shrinkwrap.json" => PiPackageManager::Npm,
            "pnpm-lock.yaml" => PiPackageManager::Pnpm,
            "yarn.lock" => PiPackageManager::Yarn,
            "bun.lockb" => PiPackageManager::Bun,
            _ => unreachable!("supported lock names are exhaustive"),
        };
        if declared != requested {
            anyhow::bail!(
                "Pi package lockfile {lock} requires package manager {}, not {}",
                declared.name(),
                requested.name(),
            )
        }
    }
    Ok(requested)
}

fn parse_resources(
    object: &serde_json::Map<String, Value>,
    root: &Path,
) -> anyhow::Result<PackageResources> {
    let pi = object.get("pi").and_then(Value::as_object);
    let resources = PackageResources {
        extensions: resource_array(pi, "extensions")?,
        skills: resource_array(pi, "skills")?,
        prompts: resource_array(pi, "prompts")?,
        themes: resource_array(pi, "themes")?,
    };
    for entries in [
        resources.extensions.as_ref(),
        resources.skills.as_ref(),
        resources.prompts.as_ref(),
        resources.themes.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        for entry in entries {
            validate_resource_pattern(entry, root)?;
        }
    }
    Ok(resources)
}

fn resource_array(
    pi: Option<&serde_json::Map<String, Value>>,
    name: &str,
) -> anyhow::Result<Option<Vec<String>>> {
    let Some(value) = pi.and_then(|pi| pi.get(name)) else {
        return Ok(None);
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("Pi package pi.{name} must be an array of strings"))?;
    let mut result = Vec::with_capacity(values.len());
    for value in values {
        result.push(
            value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Pi package pi.{name} must contain only strings"))?
                .to_owned(),
        );
    }
    Ok(Some(result))
}

fn validate_resource_pattern(pattern: &str, root: &Path) -> anyhow::Result<()> {
    let pattern = pattern
        .strip_prefix('!')
        .or_else(|| pattern.strip_prefix('+'))
        .or_else(|| pattern.strip_prefix('-'))
        .unwrap_or(pattern);
    if pattern.is_empty() || pattern.len() > MAX_PACKAGE_PATH_BYTES || pattern.contains('\0') {
        anyhow::bail!("Pi package resource pattern is empty or oversized")
    }
    let path = Path::new(pattern);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        anyhow::bail!("Pi package resource pattern escapes its package root: {pattern}")
    }
    if !pattern.contains('*') && !pattern.contains('?') {
        let candidate = root.join(pattern);
        if !candidate.exists() {
            anyhow::bail!("Pi package resource entry does not exist: {pattern}")
        }
        let metadata = fs::symlink_metadata(&candidate)?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Pi package resource entry is a symbolic link: {pattern}")
        }
    }
    Ok(())
}

fn package_entrypoints(resources: &PackageResources, root: &Path) -> anyhow::Result<Vec<String>> {
    if let Some(entries) = &resources.extensions {
        let positive = entries
            .iter()
            .filter(|entry| {
                !entry.starts_with('!') && !entry.starts_with('+') && !entry.starts_with('-')
            })
            .cloned()
            .collect::<Vec<_>>();
        if positive.is_empty() {
            anyhow::bail!("Pi package pi.extensions does not select an extension entrypoint")
        }
        return Ok(positive);
    }
    let mut entries = Vec::new();
    for candidate in ["index.ts", "index.js"] {
        if root.join(candidate).is_file() {
            entries.push(candidate.to_owned());
        }
    }
    if entries.is_empty() && root.join("extensions").is_dir() {
        entries.push("extensions".to_owned());
    }
    Ok(entries)
}

fn parse_dependencies(
    object: &serde_json::Map<String, Value>,
) -> anyhow::Result<Vec<PackageDependency>> {
    let mut dependencies = Vec::new();
    for (kind, required) in [("dependencies", true), ("optionalDependencies", false)] {
        let Some(value) = object.get(kind) else {
            continue;
        };
        let values = value
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("Pi package {kind} must be an object"))?;
        for (name, spec) in values {
            let spec = spec.as_str().ok_or_else(|| {
                anyhow::anyhow!("Pi package dependency {name} must have a string spec")
            })?;
            if name.is_empty() || name.contains('\0') || spec.contains('\0') {
                anyhow::bail!("Pi package dependency metadata contains an invalid name or spec")
            }
            dependencies.push(PackageDependency {
                name: name.clone(),
                spec: spec.to_owned(),
                kind: if required {
                    "dependencies".to_owned()
                } else {
                    "optionalDependencies".to_owned()
                },
            });
        }
    }
    dependencies.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.spec.cmp(&right.spec))
    });
    Ok(dependencies)
}

fn parse_lifecycle_scripts(object: &serde_json::Map<String, Value>) -> anyhow::Result<Vec<String>> {
    let Some(value) = object.get("scripts") else {
        return Ok(Vec::new());
    };
    let scripts = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Pi package scripts must be an object"))?;
    let mut names = scripts
        .keys()
        .filter(|name| {
            matches!(
                name.as_str(),
                "preinstall"
                    | "install"
                    | "postinstall"
                    | "prepare"
                    | "prepublish"
                    | "postpublish"
                    | "prepublishOnly"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    names.sort();
    Ok(names)
}

fn supported_lock_paths(root: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut paths = Vec::new();
    for name in LOCK_NAMES {
        let path = root.join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    anyhow::bail!(
                        "Pi package lockfile must be a regular non-symlink file: {}",
                        path.display()
                    )
                }
                if metadata.len() > MAX_PACKAGE_LOCK_BYTES as u64 {
                    anyhow::bail!(
                        "Pi package lockfile exceeds its bounded size: {}",
                        path.display()
                    )
                }
                paths.push((name.to_owned(), path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(paths)
}

fn has_supported_lock(root: &Path) -> anyhow::Result<bool> {
    Ok(!supported_lock_paths(root)?.is_empty())
}

fn dependency_lock_digest(root: &Path) -> anyhow::Result<Option<String>> {
    let paths = supported_lock_paths(root)?;
    if paths.is_empty() {
        return Ok(None);
    }
    let mut hasher = Sha256::new();
    hasher.update(b"octet-pi-package-dependency-locks\0");
    for (name, path) in paths {
        hash_framed(&mut hasher, name.as_bytes());
        let bytes =
            octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_PACKAGE_LOCK_BYTES)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Pi package lockfile cannot be read safely: {}",
                        path.display()
                    )
                })?;
        hash_framed(&mut hasher, &bytes);
    }
    Ok(Some(digest_hex(&hasher.finalize())))
}

fn dependency_tree_digest(root: &Path) -> anyhow::Result<Option<String>> {
    let node_modules = root.join("node_modules");
    match fs::symlink_metadata(&node_modules) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!("Pi package node_modules must be a regular non-symlink directory")
            }
            Ok(Some(hash_tree(&node_modules)?))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_missing_runtime_dependencies(
    root: &Path,
    dependencies: &[PackageDependency],
) -> anyhow::Result<bool> {
    let node_modules = root.join("node_modules");
    for dependency in dependencies
        .iter()
        .filter(|dependency| dependency.kind == "dependencies")
    {
        let path = dependency_path(&node_modules, &dependency.name)?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_dir() => {}
            Ok(_) => return Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn dependency_path(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
    if name.is_empty() || name.contains('\0') {
        anyhow::bail!("Pi package dependency name is invalid")
    }
    let path = root.join(name);
    let canonical_root = root.to_path_buf();
    if !path.starts_with(&canonical_root) {
        anyhow::bail!("Pi package dependency path escapes node_modules")
    }
    Ok(path)
}

fn hash_tree(root: &Path) -> anyhow::Result<String> {
    let mut entries = Vec::<(String, bool, PathBuf)>::new();
    let mut stack = vec![(root.to_owned(), String::new(), 0usize)];
    let mut bytes = 0usize;
    while let Some((directory, relative, depth)) = stack.pop() {
        if depth > MAX_PACKAGE_DEPTH {
            anyhow::bail!("Pi package dependency tree exceeds its depth limit")
        }
        let mut children = fs::read_dir(&directory)
            .with_context(|| {
                format!(
                    "cannot inspect Pi package dependency tree {}",
                    directory.display()
                )
            })?
            .collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let path = child.path();
            let filename = child.file_name();
            let name = filename
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Pi package dependency path is not UTF-8"))?;
            let child_relative = if relative.is_empty() {
                name.to_owned()
            } else {
                format!("{relative}/{name}")
            };
            if child_relative.len() > MAX_PACKAGE_PATH_BYTES {
                anyhow::bail!("Pi package dependency path exceeds its bound")
            }
            if entries.len() >= MAX_PACKAGE_FILES {
                anyhow::bail!("Pi package dependency tree exceeds its file limit")
            }
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "Pi package dependency tree contains a symbolic link: {}",
                    path.display()
                )
            }
            if metadata.is_dir() {
                entries.push((child_relative.clone(), true, path.clone()));
                stack.push((path, child_relative, depth + 1));
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len() as usize)
                    .ok_or_else(|| anyhow::anyhow!("Pi package dependency tree size overflow"))?;
                if bytes > MAX_PACKAGE_BYTES {
                    anyhow::bail!("Pi package dependency tree exceeds its byte limit")
                }
                entries.push((child_relative, false, path));
            } else {
                anyhow::bail!("Pi package dependency tree contains a non-regular entry")
            }
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut hasher = Sha256::new();
    hasher.update(b"octet-pi-package-dependency-tree\0");
    for (relative, directory, path) in entries {
        hasher.update([if directory { b'd' } else { b'f' }]);
        hash_framed(&mut hasher, relative.as_bytes());
        if !directory {
            let bytes = octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_PACKAGE_BYTES)
                .map_err(|_| {
                    anyhow::anyhow!(
                        "Pi package dependency file cannot be read safely: {}",
                        path.display()
                    )
                })?;
            hash_framed(&mut hasher, &bytes);
        }
    }
    Ok(digest_hex(&hasher.finalize()))
}

fn npm_integrity(root: &Path, name: &str, npm_spec: Option<&str>) -> Option<String> {
    if !matches!(npm_spec, Some(_)) {
        return None;
    }
    let path = root.join("package-lock.json");
    let bytes =
        octet_agent::secure_fs::read_regular_file_bounded(&path, MAX_PACKAGE_LOCK_BYTES).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let entry = value
        .get("packages")
        .and_then(Value::as_object)
        .and_then(|packages| packages.get(&format!("node_modules/{name}")))?;
    entry
        .get("integrity")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn cached_package(
    store_root: &Path,
    source: &Path,
    display: &str,
    input: &str,
    package_manager: PiPackageManager,
    kind: &str,
    expected_revision: Option<&str>,
    scripts_enabled: bool,
) -> anyhow::Result<Option<PackageInspection>> {
    let record_path = store_root.join(PACKAGE_RECORD);
    let record_bytes = match octet_agent::secure_fs::read_regular_file_bounded(
        &record_path,
        MAX_PACKAGE_JSON_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(None)
        }
        Err(error) => {
            return Err(anyhow::anyhow!(
                "cached Pi package {display} has an unreadable identity record: {error}"
            ))
        }
    };
    let record: PackageRecord = serde_json::from_slice(&record_bytes)
        .with_context(|| format!("cached Pi package {display} has an invalid identity record"))?;
    if record.schema_version != PACKAGE_RECORD_SCHEMA || record.scripts_enabled != scripts_enabled {
        return Ok(None);
    }
    let identity = record.identity;
    validate_identity_shape(&identity)?;
    if identity.input != input
        || identity.kind != kind
        || identity.package_manager != package_manager.name()
        || identity.resolved_root != source
    {
        return Ok(None);
    }
    if let Some(expected_revision) = expected_revision {
        if identity.resolved_revision.as_deref() != Some(expected_revision) {
            return Ok(None);
        }
    } else if kind == "git" {
        return Ok(None);
    }
    ensure_package_root(store_root, display)?;
    let store_root = store_root.canonicalize()?;
    let source = source.canonicalize()?;
    let dependency_root = identity.dependency_root.canonicalize()?;
    if !source.starts_with(&store_root) || !dependency_root.starts_with(&store_root) {
        anyhow::bail!(
            "cached Pi package {} resolves outside its private store",
            display
        );
    }
    let inspection = inspect_package_at(
        &source,
        input,
        package_manager,
        &dependency_root,
        kind,
        identity.resolved_revision.clone(),
        input.strip_prefix("npm:"),
    )?;
    if inspection.needs_install || inspection.identity != identity {
        return Ok(None);
    }
    Ok(Some(inspection))
}

fn package_store_root(extension_root: &Path) -> anyhow::Result<PathBuf> {
    let parent = extension_root
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Pi extension root has no package-store parent"))?;
    let store = parent.join(PACKAGE_STORE_NAME);
    octet_agent::secure_fs::create_private_directory_all(&store)?;
    Ok(store.canonicalize()?)
}

fn store_key(input: &str, package_manager: PiPackageManager, scripts_enabled: bool) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"octet-pi-package-store\0");
    hash_framed(&mut hasher, package_manager.name().as_bytes());
    hash_framed(
        &mut hasher,
        if scripts_enabled {
            b"scripts"
        } else {
            b"no-scripts"
        },
    );
    hash_framed(&mut hasher, input.as_bytes());
    format!("pkg-{}", digest_hex(&hasher.finalize())[..24].to_owned())
}

fn write_package_record(
    root: &Path,
    identity: &PackageIdentity,
    scripts_enabled: bool,
) -> anyhow::Result<()> {
    let record = serde_json::json!({
        "schema_version": PACKAGE_RECORD_SCHEMA,
        "identity": identity,
        "scripts_enabled": scripts_enabled,
    });
    let mut bytes = serde_json::to_vec_pretty(&record)?;
    bytes.push(b'\n');
    octet_agent::secure_fs::write_private_atomic(
        &root.join(PACKAGE_RECORD),
        &bytes,
        MAX_PACKAGE_JSON_BYTES,
    )
    .map_err(|_| anyhow::anyhow!("cannot durably record Pi package identity"))
}

fn materialize_npm_package(
    workspace: &Path,
    name: &str,
    package_root: &Path,
    display: &str,
) -> anyhow::Result<()> {
    let package_source = workspace.join("node_modules").join(name);
    let source_metadata = fs::symlink_metadata(&package_source)
        .with_context(|| format!("npm Pi package {display} was not materialized"))?;
    if !source_metadata.is_dir() && !source_metadata.file_type().is_symlink() {
        anyhow::bail!("npm Pi package {display} did not resolve to a directory");
    }
    let workspace_canonical = workspace.canonicalize()?;
    let source_canonical = package_source.canonicalize().with_context(|| {
        format!("npm Pi package {display} resolved to an unreadable package directory")
    })?;
    if !source_canonical.starts_with(&workspace_canonical) {
        anyhow::bail!("npm Pi package {display} resolved outside its private workspace");
    }
    let source_metadata = fs::symlink_metadata(&source_canonical)?;
    if !source_metadata.is_dir() || source_metadata.file_type().is_symlink() {
        anyhow::bail!("npm Pi package {display} did not resolve to a regular package directory");
    }
    match fs::symlink_metadata(package_root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            anyhow::bail!("npm Pi package {display} has an unsafe materialized destination")
        }
        Ok(_) => fs::remove_dir_all(package_root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    copy_package_tree(&source_canonical, package_root)
}

fn is_exact_git_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

fn write_npm_workspace_manifest(root: &Path, name: &str, spec: &str) -> anyhow::Result<()> {
    let manifest = serde_json::json!({
        "name": "octet-pi-package-stage",
        "private": true,
        "version": "0.0.0",
        "dependencies": {name: spec},
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    octet_agent::secure_fs::write_private_atomic(
        &root.join("package.json"),
        &bytes,
        MAX_PACKAGE_JSON_BYTES,
    )
    .map_err(|_| anyhow::anyhow!("cannot create private Pi package-manager project"))
}

fn mirror_workspace_locks(workspace: &Path, package_root: &Path) -> anyhow::Result<()> {
    for name in LOCK_NAMES {
        let source = workspace.join(name);
        if !source.is_file() {
            continue;
        }
        let destination = package_root.join(name);
        let bytes =
            octet_agent::secure_fs::read_regular_file_bounded(&source, MAX_PACKAGE_LOCK_BYTES)
                .map_err(|_| {
                    anyhow::anyhow!("generated Pi package lockfile cannot be read safely")
                })?;
        octet_agent::secure_fs::write_private_atomic(&destination, &bytes, MAX_PACKAGE_LOCK_BYTES)
            .map_err(|_| {
                anyhow::anyhow!("cannot bind generated Pi package lockfile to its source root")
            })?;
    }
    Ok(())
}

fn copy_package_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("Pi package source must be a regular non-symlink directory")
    }
    make_private_directory(destination)?;
    let mut stack = vec![(source.to_owned(), destination.to_owned(), 0usize)];
    let mut files = 0usize;
    let mut bytes = 0usize;
    while let Some((from, to, depth)) = stack.pop() {
        if depth > MAX_PACKAGE_DEPTH {
            anyhow::bail!("Pi package source exceeds its directory depth limit")
        }
        let mut entries = fs::read_dir(&from)
            .with_context(|| format!("cannot read Pi package source {}", from.display()))?
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry
                .file_name()
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("Pi package source path is not UTF-8"))?
                .to_owned();
            if matches!(name.as_str(), "node_modules" | "target" | ".git" | ".pnpm") {
                continue;
            }
            let from_path = entry.path();
            let to_path = to.join(&name);
            let metadata = fs::symlink_metadata(&from_path)?;
            if metadata.file_type().is_symlink() {
                anyhow::bail!(
                    "Pi package source contains a symbolic link: {}",
                    from_path.display()
                )
            }
            if metadata.is_dir() {
                make_private_directory(&to_path)?;
                stack.push((from_path, to_path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                anyhow::bail!("Pi package source contains a non-regular entry")
            }
            files += 1;
            bytes = bytes
                .checked_add(metadata.len() as usize)
                .ok_or_else(|| anyhow::anyhow!("Pi package source size overflow"))?;
            if files > MAX_PACKAGE_FILES || bytes > MAX_PACKAGE_BYTES {
                anyhow::bail!("Pi package source exceeds its bounded copy size")
            }
            let contents =
                octet_agent::secure_fs::read_regular_file_bounded(&from_path, MAX_PACKAGE_BYTES)
                    .map_err(|_| {
                        anyhow::anyhow!(
                            "Pi package source file cannot be read safely: {}",
                            from_path.display()
                        )
                    })?;
            octet_agent::secure_fs::write_private_atomic(&to_path, &contents, MAX_PACKAGE_BYTES)
                .map_err(|_| {
                    anyhow::anyhow!("cannot copy Pi package source file {}", to_path.display())
                })?;
        }
    }
    Ok(())
}

fn clone_git_package(
    repo: &str,
    reference: Option<&str>,
    destination: &Path,
    display: &str,
) -> anyhow::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Pi git staging path has no parent"))?;
    let mut clone = Command::new("git");
    clone
        .arg("clone")
        .arg("--no-tags")
        .arg(repo)
        .arg(destination)
        .current_dir(parent)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    sanitize_command(&mut clone);
    run_child(&mut clone, &format!("clone Pi git package {display}"))?;
    if let Some(reference) = reference {
        let mut checkout = Command::new("git");
        checkout
            .arg("-C")
            .arg(destination)
            .arg("checkout")
            .arg("--detach")
            .arg(reference)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        sanitize_command(&mut checkout);
        run_child(&mut checkout, &format!("checkout Pi git package {display}"))?;
    }
    Ok(())
}

fn git_revision(root: &Path) -> anyhow::Result<String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .arg("rev-parse")
        .arg("HEAD")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_command(&mut command);
    let output = command
        .output()
        .context("cannot inspect the Pi git package revision")?;
    if !output.status.success() {
        anyhow::bail!("Pi git package revision could not be verified")
    }
    let revision = String::from_utf8(output.stdout)?
        .trim()
        .to_ascii_lowercase();
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        anyhow::bail!("Pi git package returned an invalid resolved revision")
    }
    Ok(revision)
}

fn run_package_manager(
    root: &Path,
    package_manager: PiPackageManager,
    spec: Option<&str>,
    lock_present: bool,
    allow_network: bool,
    scripts: bool,
) -> anyhow::Result<()> {
    let args = package_manager.install_args(spec, lock_present, allow_network, scripts);
    let mut command = Command::new(package_manager.executable());
    command
        .args(args)
        .current_dir(root)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    sanitize_command(&mut command);
    let cache_root = root.parent().unwrap_or(root).join(".npm-cache");
    command.env("npm_config_cache", cache_root);
    run_child(
        &mut command,
        &format!(
            "run reviewed Pi {} dependency operation",
            package_manager.name()
        ),
    )
}

struct ChildGuard {
    child: Child,
    finished: bool,
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn run_child(command: &mut Command, label: &str) -> anyhow::Result<()> {
    let child = command.spawn().with_context(|| format!("cannot {label}"))?;
    let mut child = ChildGuard {
        child,
        finished: false,
    };
    loop {
        if let Some(status) = child.child.try_wait()? {
            child.finished = true;
            if status.success() {
                return Ok(());
            }
            anyhow::bail!("{label} failed with {status}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn check_node_runtime() -> anyhow::Result<()> {
    let mut command = Command::new("node");
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    sanitize_command(&mut command);
    let output = command
        .output()
        .context("Node >= 22.19.0 is required for Pi package installation")?;
    if !output.status.success() {
        anyhow::bail!("Node >= 22.19.0 is required for Pi package installation")
    }
    let version_output = String::from_utf8(output.stdout)?;
    let version = version_output.trim().trim_start_matches('v');
    let version = Version::parse(version)
        .with_context(|| format!("Node returned an invalid version {version:?}"))?;
    if version < Version::parse(MIN_NODE_VERSION)? {
        anyhow::bail!("Node {version} is too old for the pinned Pi package contract; require >= {MIN_NODE_VERSION}")
    }
    Ok(())
}

fn sanitize_command(command: &mut Command) {
    let path = env::var_os("PATH");
    #[cfg(windows)]
    let system_root = env::var_os("SystemRoot");
    command.env_clear();
    if let Some(path) = path {
        command.env("PATH", path);
    }
    #[cfg(windows)]
    if let Some(system_root) = system_root {
        command.env("SystemRoot", system_root);
    }
    command.env("CI", "1");
    command.env("GIT_TERMINAL_PROMPT", "0");
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("npm_config_audit", "false");
    command.env("npm_config_fund", "false");
    #[cfg(unix)]
    {
        command.env("npm_config_userconfig", "/dev/null");
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
    }
    #[cfg(windows)]
    {
        command.env("npm_config_userconfig", "NUL");
        command.env("GIT_CONFIG_GLOBAL", "NUL");
    }
}

fn make_private_directory(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        octet_agent::secure_fs::create_private_directory_all(parent)?;
    }
    if !path.exists() {
        fs::create_dir(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("Pi package private staging path is not a regular directory")
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn supported_lock_name(name: &str) -> bool {
    LOCK_NAMES.contains(&name)
}

fn digest_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    digest_hex(&hasher.finalize())
}

fn hash_framed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}
