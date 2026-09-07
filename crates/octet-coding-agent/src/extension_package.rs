#![allow(missing_docs)]

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, Write};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use clap::Subcommand;
use flate2::read::GzDecoder;
use fs2::FileExt;
use futures_util::StreamExt;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(super) const PACKAGE_ID: &str = "octet-serve";
const PACKAGE_MANIFEST: &str = "package.toml";
const INSTALL_RECORD: &str = "install.json";
const ENTRYPOINT: &str = "bin/octet-serve-runtime";
pub(super) const RELEASE_REPOSITORY: &str = "https://github.com/skaft-software/octet";
const DOWNLOAD_MAX_ATTEMPTS: usize = 3;
const DOWNLOAD_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const DOWNLOAD_MAX_BACKOFF: Duration = Duration::from_secs(1);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_CHECKSUM_BYTES: usize = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub(super) const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_ENTRYPOINT_BYTES: u64 = 384 * 1024 * 1024;
const MAX_EXPANDED_ARCHIVE_BYTES: u64 = MAX_ENTRYPOINT_BYTES + MAX_MANIFEST_BYTES;

#[derive(Clone, Debug, Subcommand)]
pub enum ExtensionCommand {
    /// Install an official extension package or a local release archive.
    Install {
        /// Official extension package name.
        #[arg(
            value_name = "NAME",
            required_unless_present = "path",
            conflicts_with = "path"
        )]
        name: Option<String>,
        /// Install a local release archive instead of downloading one.
        #[arg(long, value_name = "ARCHIVE")]
        path: Option<PathBuf>,
    },
    /// List installed extension packages.
    List,
    /// Install the matching official release or a local replacement atomically.
    Update {
        /// Official extension package name.
        #[arg(
            value_name = "NAME",
            required_unless_present = "path",
            conflicts_with = "path"
        )]
        name: Option<String>,
        /// Update from a local release archive instead of downloading one.
        #[arg(long, value_name = "ARCHIVE")]
        path: Option<PathBuf>,
    },
    /// Remove an installed package without deleting external data.
    Remove { name: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema_version: u32,
    id: String,
    version: String,
    requires_octet: String,
    target: String,
    entrypoint: PackageEntrypoint,
    capabilities: PackageCapabilities,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageEntrypoint {
    path: String,
    args: Vec<String>,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageCapabilities {
    network: String,
    process: bool,
    filesystem: String,
}

#[derive(Debug, Serialize)]
struct InstallRecord<'a> {
    schema_version: u32,
    id: &'a str,
    version: &'a str,
    target: &'a str,
    source: &'a str,
    archive_sha256: &'a str,
    entrypoint_sha256: &'a str,
    installed_by_octet: &'a str,
}

pub(super) struct PackageLock(File);

impl Drop for PackageLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub async fn run(command: ExtensionCommand) -> anyhow::Result<()> {
    match command {
        ExtensionCommand::Install { name, path } => {
            let root = extensions_root()?;
            if let Some(path) = path {
                let path = resolve_local_archive_path(&path)?;
                match classify_local_archive(&path)? {
                    LocalArchiveKind::Application => {
                        let manifest = install_local(&root, &path, false)?;
                        crate::output::stdout_line(format!(
                            "Installed {} {} for {}.",
                            manifest.id, manifest.version, manifest.target
                        ));
                    }
                    LocalArchiveKind::ExecutableBundle => {
                        let manifest = crate::extension_bundle::install_local(&root, &path, false)?;
                        print_bundle_installed("Installed", &manifest);
                    }
                }
            } else {
                let name = name.expect("clap requires a name unless --path is present");
                if name == PACKAGE_ID {
                    let manifest = install_official(&root, false).await?;
                    crate::output::stdout_line(format!(
                        "Installed {} {} for {}.",
                        manifest.id, manifest.version, manifest.target
                    ));
                } else {
                    let manifest =
                        crate::extension_bundle::install_official(&root, &name, false).await?;
                    print_bundle_installed("Installed", &manifest);
                }
            }
            Ok(())
        }
        ExtensionCommand::List => list_all_installed(&extensions_root()?),
        ExtensionCommand::Update { name, path } => {
            let root = extensions_root()?;
            if let Some(path) = path {
                let path = resolve_local_archive_path(&path)?;
                match classify_local_archive(&path)? {
                    LocalArchiveKind::Application => {
                        ensure_package_directory(&root).with_context(|| {
                            format!(
                                "{PACKAGE_ID} is not installed; run 'octet extension install --path {}'",
                                path.display()
                            )
                        })?;
                        let manifest = install_local(&root, &path, true)?;
                        crate::output::stdout_line(format!(
                            "Updated {} to {} for {}.",
                            manifest.id, manifest.version, manifest.target
                        ));
                    }
                    LocalArchiveKind::ExecutableBundle => {
                        let manifest = crate::extension_bundle::install_local(&root, &path, true)?;
                        print_bundle_installed("Updated", &manifest);
                    }
                }
            } else {
                let name = name.expect("clap requires a name unless --path is present");
                if name == PACKAGE_ID {
                    ensure_package_directory(&root).with_context(|| {
                        format!(
                            "{PACKAGE_ID} is not installed; run 'octet extension install {PACKAGE_ID}'"
                        )
                    })?;
                    let manifest = install_official(&root, true).await?;
                    crate::output::stdout_line(format!(
                        "Updated {} to {} for {}.",
                        manifest.id, manifest.version, manifest.target
                    ));
                } else {
                    if !crate::extension_bundle::is_official_bundle(&name) {
                        anyhow::bail!(
                            "{name:?} has no official update source; use 'octet extension update --path ARCHIVE'"
                        );
                    }
                    crate::extension_bundle::ensure_installed(&root, &name).with_context(|| {
                        format!("{name} is not installed; run 'octet extension install {name}'")
                    })?;
                    let manifest =
                        crate::extension_bundle::install_official(&root, &name, true).await?;
                    print_bundle_installed("Updated", &manifest);
                }
            }
            Ok(())
        }
        ExtensionCommand::Remove { name } => {
            let root = extensions_root()?;
            if name == PACKAGE_ID {
                remove_installed(&root)?;
                crate::output::stdout_line(format!(
                    "Removed {PACKAGE_ID}. Serve sessions and other user data were preserved."
                ));
            } else {
                crate::extension_bundle::remove_installed(&root, &name)?;
                crate::output::stdout_line(format!(
                    "Removed {name}. Configuration and other data outside the bundle were preserved."
                ));
            }
            Ok(())
        }
    }
}

fn print_bundle_installed(
    action: &str,
    manifest: &crate::extension_bundle::InstalledBundleManifest,
) {
    crate::output::stdout_line(format!(
        "{action} {} {} (API {}, requires octet {}).",
        manifest.id, manifest.version, manifest.api_version, manifest.requires_octet
    ));
    crate::output::stdout_line(
        "Installation does not enable extensions. Full access trusts enabled extensions by default; safe mode keeps them stopped.",
    );
}

#[allow(dead_code)]
pub fn run_serve(no_open: bool, port: u16, web_root: Option<PathBuf>) -> anyhow::Result<()> {
    let root = extensions_root()?;
    let manifest = load_installed(&root).with_context(|| {
        format!("octet Serve is not installed; run 'octet extension install {PACKAGE_ID}'")
    })?;
    let package_dir = root.join(PACKAGE_ID);
    let entrypoint = package_dir.join(&manifest.entrypoint.path);
    let (_entrypoint_snapshot, staged_entrypoint) =
        stage_validated_entrypoint(&entrypoint, &manifest.entrypoint.sha256)?;

    let mut command = Command::new(&staged_entrypoint);
    // This exact-version, first-party runtime replaces the launcher process. It
    // must receive the same user-controlled configuration and provider
    // credentials as a directly launched octet binary; the sanitized environment
    // is reserved for model-controlled tool and executable-extension children.
    command.args(&manifest.entrypoint.args);
    if no_open {
        command.arg("--no-open");
    }
    command.arg("--port").arg(port.to_string());
    if let Some(web_root) = web_root {
        command.arg("--web-root").arg(web_root);
    }
    command
        .env("OCTET_EXTENSION_PACKAGE_DIR", &package_dir)
        .env("OCTET_EXTENSION_PACKAGE_VERSION", &manifest.version);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        let error = command.exec();
        Err(anyhow::Error::new(error).context(format!(
            "cannot launch octet Serve at {}",
            entrypoint.display()
        )))
    }

    #[cfg(not(unix))]
    {
        let status = command
            .status()
            .with_context(|| format!("cannot launch octet Serve at {}", entrypoint.display()))?;
        if status.success() {
            Ok(())
        } else {
            anyhow::bail!("octet Serve exited with {status}")
        }
    }
}

fn validate_supported_name(name: &str) -> anyhow::Result<()> {
    if name == PACKAGE_ID {
        Ok(())
    } else {
        anyhow::bail!(
            "unsupported application extension {name:?}; this release supports only {PACKAGE_ID:?}"
        )
    }
}

pub(super) fn extensions_root() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| anyhow::anyhow!("cannot manage extensions: user home is unavailable"))?;
    let home = home
        .canonicalize()
        .with_context(|| format!("cannot resolve user home {}", home.display()))?;
    Ok(home.join(".octet").join("extensions"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalArchiveKind {
    Application,
    ExecutableBundle,
}

fn resolve_local_archive_path(path: &Path) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot resolve the current directory")?;
    resolve_local_archive_path_from(path, &cwd)
}

fn resolve_local_archive_path_from(path: &Path, cwd: &Path) -> anyhow::Result<PathBuf> {
    let unresolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    unresolved
        .canonicalize()
        .with_context(|| format!("cannot resolve package archive {}", path.display()))
}

fn classify_local_archive(path: &Path) -> anyhow::Result<LocalArchiveKind> {
    const MAX_CLASSIFICATION_ENTRIES: usize = 4096;

    let file = open_archive_snapshot(path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = tar::Archive::new(decoder);
    let mut root = None::<String>;
    let mut application_manifest = false;
    let mut bundle_manifest = false;
    let mut entries = 0usize;
    let mut expanded_bytes = 0u64;

    for entry in archive
        .entries()
        .context("cannot read local extension archive")?
    {
        entries = entries.saturating_add(1);
        if entries > MAX_CLASSIFICATION_ENTRIES {
            anyhow::bail!(
                "local extension archive exceeds the {MAX_CLASSIFICATION_ENTRIES}-entry limit"
            );
        }
        let entry = entry.context("cannot read local extension archive entry")?;
        let entry_type = entry.header().entry_type();
        if !entry_type.is_dir() && !entry_type.is_file() {
            anyhow::bail!("local extension archive contains a link or special entry");
        }
        let size = entry.header().size()?;
        expanded_bytes = expanded_bytes
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("local extension archive size overflow"))?;
        if expanded_bytes > MAX_ARCHIVE_BYTES {
            anyhow::bail!(
                "local extension archive expands beyond the {MAX_ARCHIVE_BYTES}-byte classification limit"
            );
        }

        let path = entry
            .path()
            .context("local extension archive contains an invalid path")?
            .into_owned();
        let components = path.components().collect::<Vec<_>>();
        if components.is_empty()
            || components.len() > 64
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            anyhow::bail!(
                "local extension archive path is not portable: {}",
                path.display()
            );
        }
        let archive_root = match components[0] {
            Component::Normal(value) => value
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("local extension archive root is not UTF-8"))?
                .to_owned(),
            _ => unreachable!("non-normal components were rejected"),
        };
        match &root {
            Some(expected) if expected != &archive_root => {
                anyhow::bail!("local extension archive contains multiple root directories")
            }
            None => root = Some(archive_root),
            _ => {}
        }
        if components.len() != 2 || !entry_type.is_file() {
            continue;
        }
        let Component::Normal(name) = components[1] else {
            unreachable!("non-normal components were rejected")
        };
        if name == PACKAGE_MANIFEST {
            if application_manifest {
                anyhow::bail!("local extension archive contains duplicate {PACKAGE_MANIFEST}");
            }
            application_manifest = true;
        } else if name == crate::extension_bundle::BUNDLE_MANIFEST {
            if bundle_manifest {
                anyhow::bail!(
                    "local extension archive contains duplicate {}",
                    crate::extension_bundle::BUNDLE_MANIFEST
                );
            }
            bundle_manifest = true;
        }
    }

    match (application_manifest, bundle_manifest) {
        (true, false) => Ok(LocalArchiveKind::Application),
        (false, true) => Ok(LocalArchiveKind::ExecutableBundle),
        (true, true) => anyhow::bail!(
            "local extension archive cannot contain both {PACKAGE_MANIFEST} and {}",
            crate::extension_bundle::BUNDLE_MANIFEST
        ),
        (false, false) => anyhow::bail!(
            "local extension archive must contain either {PACKAGE_MANIFEST} or {}",
            crate::extension_bundle::BUNDLE_MANIFEST
        ),
    }
}

pub(super) fn acquire_lock(root: &Path) -> anyhow::Result<PackageLock> {
    fs::create_dir_all(root)
        .with_context(|| format!("cannot create extension directory {}", root.display()))?;
    let path = root.join(".package.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open extension package lock {}", path.display()))?;
    file.try_lock_exclusive().with_context(|| {
        format!(
            "another extension install, update, or removal is already running ({})",
            path.display()
        )
    })?;
    Ok(PackageLock(file))
}

async fn install_official(root: &Path, replace: bool) -> anyhow::Result<PackageManifest> {
    let version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let target = target_triple()?;
    let asset = format!("{PACKAGE_ID}-{version}-{target}.tar.gz");
    let tag = format!("v{version}");
    let release = format!("{RELEASE_REPOSITORY}/releases/download/{tag}");
    let checksums_url = format!("{release}/SHA256SUMS");
    let archive_url = format!("{release}/{asset}");

    let checksums = download_bytes(&checksums_url, MAX_CHECKSUM_BYTES).await?;
    let checksums =
        String::from_utf8(checksums).context("official release SHA256SUMS is not valid UTF-8")?;
    let expected = checksum_for_asset(&checksums, &asset)?;

    let temporary = tempfile::Builder::new()
        .prefix("octet-serve-download-")
        .tempdir()
        .context("cannot create temporary extension download directory")?;
    let archive = temporary.path().join(&asset);
    let actual = download_file(&archive_url, &archive, MAX_ARCHIVE_BYTES).await?;
    if actual != expected {
        anyhow::bail!("checksum mismatch for {asset}: expected {expected}, downloaded {actual}");
    }

    install_archive(root, &archive, &archive_url, &actual, replace)
}

fn install_local(root: &Path, archive: &Path, replace: bool) -> anyhow::Result<PackageManifest> {
    let archive = archive
        .canonicalize()
        .with_context(|| format!("cannot resolve package archive {}", archive.display()))?;
    let metadata = fs::metadata(&archive)
        .with_context(|| format!("cannot inspect package archive {}", archive.display()))?;
    if !metadata.is_file() {
        anyhow::bail!(
            "package archive is not a regular file: {}",
            archive.display()
        );
    }
    let digest = sha256_file_bounded(&archive, MAX_ARCHIVE_BYTES)?;
    let source = archive.to_string_lossy();
    install_archive(root, &archive, &source, &digest, replace)
}

fn install_archive(
    root: &Path,
    archive: &Path,
    source: &str,
    archive_sha256: &str,
    replace: bool,
) -> anyhow::Result<PackageManifest> {
    let mut archive_file = open_archive_snapshot(archive)?;
    let bound_digest = sha256_open_file_bounded(&mut archive_file, MAX_ARCHIVE_BYTES)?;
    if bound_digest != archive_sha256 {
        anyhow::bail!(
            "package archive changed before extraction: expected {archive_sha256}, found {bound_digest}"
        );
    }
    archive_file.rewind()?;
    let _lock = acquire_lock(root)?;
    let destination = root.join(PACKAGE_ID);
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                anyhow::bail!(
                    "extension destination is not a regular directory: {}",
                    destination.display()
                );
            }
            if !replace {
                anyhow::bail!(
                    "{PACKAGE_ID} is already installed; run 'octet extension update {PACKAGE_ID}'"
                );
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("cannot inspect extension destination"),
    }

    let staging = tempfile::Builder::new()
        .prefix(".octet-serve-install-")
        .tempdir_in(root)
        .context("cannot create extension staging directory")?;
    extract_archive_reader(&mut archive_file, staging.path())?;
    let manifest = load_manifest(&staging.path().join(PACKAGE_MANIFEST))?;
    validate_manifest(&manifest)?;
    let entrypoint = staging.path().join(&manifest.entrypoint.path);
    validate_entrypoint(&entrypoint, &manifest.entrypoint.sha256)?;
    write_install_record(staging.path(), &manifest, source, archive_sha256)?;

    publish_staging(root, staging.path(), &destination, replace, PACKAGE_ID)?;
    Ok(manifest)
}

pub(super) fn publish_staging(
    root: &Path,
    staging: &Path,
    destination: &Path,
    replace: bool,
    _package_id: &str,
) -> anyhow::Result<()> {
    if !replace {
        fs::rename(staging, destination).with_context(|| {
            format!(
                "cannot publish extension from {} to {}",
                staging.display(),
                destination.display()
            )
        })?;
        sync_directory(root);
        return Ok(());
    }

    atomic_exchange_directories(staging, destination).with_context(|| {
        format!(
            "cannot atomically publish extension update from {} to {}; previous install remains active",
            staging.display(),
            destination.display()
        )
    })?;
    sync_directory(root);
    if let Err(error) = fs::remove_dir_all(staging) {
        crate::output::stderr_line(format!(
            "warning: extension updated, but previous package cleanup failed at {}: {error}",
            staging.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn atomic_exchange_directories(left: &Path, right: &Path) -> io::Result<()> {
    let left = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let right = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both C strings live through the call and point to NUL-terminated paths.
    let result = unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(target_os = "linux")]
fn atomic_exchange_directories(left: &Path, right: &Path) -> io::Result<()> {
    let left = CString::new(left.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    let right = CString::new(right.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
    // SAFETY: both C strings live through the call and renameat2 reads only those paths.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn atomic_exchange_directories(_left: &Path, _right: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic directory exchange is unavailable on this platform",
    ))
}

pub(super) fn open_archive_snapshot(path: &Path) -> anyhow::Result<File> {
    let file = octet_agent::secure_fs::open_regular_file_for_read(path)
        .with_context(|| format!("cannot open package archive {}", path.display()))?;
    let metadata = file.metadata()?;
    if metadata.len() > MAX_ARCHIVE_BYTES {
        anyhow::bail!(
            "package archive {} exceeds the {MAX_ARCHIVE_BYTES}-byte limit",
            path.display()
        );
    }
    Ok(file)
}

pub(super) fn sha256_open_file_bounded(file: &mut File, maximum: u64) -> anyhow::Result<String> {
    file.rewind()?;
    let mut hasher = Sha256::new();
    let copied = io::copy(&mut Read::by_ref(file).take(maximum + 1), &mut hasher)?;
    if copied > maximum {
        anyhow::bail!("package archive exceeds the {maximum}-byte limit");
    }
    Ok(digest_hex(&hasher.finalize()))
}

#[cfg(test)]
fn extract_archive(path: &Path, destination: &Path) -> anyhow::Result<()> {
    let mut file = open_archive_snapshot(path)?;
    extract_archive_reader(&mut file, destination)
}

fn extract_archive_reader<R: Read>(reader: R, destination: &Path) -> anyhow::Result<()> {
    let decoder = GzDecoder::new(BufReader::new(reader));
    let mut archive = tar::Archive::new(decoder);
    let bin = destination.join("bin");
    fs::create_dir(&bin).context("cannot create package bin directory")?;

    let mut found_root = false;
    let mut found_bin = false;
    let mut found_manifest = false;
    let mut found_entrypoint = false;
    let mut expanded_bytes = 0u64;
    for entry in archive.entries().context("cannot read package archive")? {
        let mut entry = entry.context("cannot read package archive entry")?;
        let path = entry
            .path()
            .context("package archive contains an invalid path")?
            .into_owned();
        match archive_member(&path)? {
            ArchiveMember::RootDirectory => {
                if found_root {
                    anyhow::bail!("package archive contains duplicate {PACKAGE_ID} directory");
                }
                if !entry.header().entry_type().is_dir() {
                    anyhow::bail!(
                        "package archive entry {} must be a directory",
                        path.display()
                    );
                }
                found_root = true;
            }
            ArchiveMember::BinDirectory => {
                if found_bin {
                    anyhow::bail!("package archive contains duplicate {PACKAGE_ID}/bin directory");
                }
                if !entry.header().entry_type().is_dir() {
                    anyhow::bail!(
                        "package archive entry {} must be a directory",
                        path.display()
                    );
                }
                found_bin = true;
            }
            ArchiveMember::Manifest => {
                if found_manifest {
                    anyhow::bail!("package archive contains duplicate {PACKAGE_MANIFEST}");
                }
                require_regular_entry(&entry, &path)?;
                account_expanded_entry(&entry, &mut expanded_bytes)?;
                copy_archive_entry(
                    &mut entry,
                    &destination.join(PACKAGE_MANIFEST),
                    MAX_MANIFEST_BYTES,
                )?;
                found_manifest = true;
            }
            ArchiveMember::Entrypoint => {
                if found_entrypoint {
                    anyhow::bail!("package archive contains duplicate {ENTRYPOINT}");
                }
                require_regular_entry(&entry, &path)?;
                account_expanded_entry(&entry, &mut expanded_bytes)?;
                copy_archive_entry(
                    &mut entry,
                    &destination.join(ENTRYPOINT),
                    MAX_ENTRYPOINT_BYTES,
                )?;
                found_entrypoint = true;
            }
        }
    }
    if !found_manifest || !found_entrypoint {
        anyhow::bail!(
            "package archive must contain {PACKAGE_ID}/{PACKAGE_MANIFEST} and {PACKAGE_ID}/{ENTRYPOINT}"
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(
            destination.join(ENTRYPOINT),
            fs::Permissions::from_mode(0o755),
        )?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum ArchiveMember {
    RootDirectory,
    BinDirectory,
    Manifest,
    Entrypoint,
}

fn archive_member(path: &Path) -> anyhow::Result<ArchiveMember> {
    let components = path.components().collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        anyhow::bail!("package archive path is not portable: {}", path.display());
    }
    let names = components
        .iter()
        .map(|component| match component {
            Component::Normal(name) => name.to_string_lossy(),
            _ => unreachable!("non-normal components were rejected"),
        })
        .collect::<Vec<_>>();
    match names.as_slice() {
        [root] if root == PACKAGE_ID => Ok(ArchiveMember::RootDirectory),
        [root, bin] if root == PACKAGE_ID && bin == "bin" => Ok(ArchiveMember::BinDirectory),
        [root, manifest] if root == PACKAGE_ID && manifest == PACKAGE_MANIFEST => {
            Ok(ArchiveMember::Manifest)
        }
        [root, bin, executable]
            if root == PACKAGE_ID && bin == "bin" && executable == "octet-serve-runtime" =>
        {
            Ok(ArchiveMember::Entrypoint)
        }
        _ => anyhow::bail!("unexpected package archive entry: {}", path.display()),
    }
}

fn require_regular_entry<R: Read>(entry: &tar::Entry<'_, R>, path: &Path) -> anyhow::Result<()> {
    if !entry.header().entry_type().is_file() {
        anyhow::bail!(
            "package archive entry {} must be a regular file",
            path.display()
        );
    }
    Ok(())
}

fn account_expanded_entry<R: Read>(
    entry: &tar::Entry<'_, R>,
    expanded_bytes: &mut u64,
) -> anyhow::Result<()> {
    let size = entry.header().size()?;
    *expanded_bytes = expanded_bytes
        .checked_add(size)
        .ok_or_else(|| anyhow::anyhow!("package archive expanded size overflow"))?;
    if *expanded_bytes > MAX_EXPANDED_ARCHIVE_BYTES {
        anyhow::bail!("package archive expands beyond the {MAX_EXPANDED_ARCHIVE_BYTES}-byte limit");
    }
    Ok(())
}

fn copy_archive_entry<R: Read>(
    entry: &mut tar::Entry<'_, R>,
    destination: &Path,
    maximum: u64,
) -> anyhow::Result<()> {
    let size = entry.header().size()?;
    if size > maximum {
        anyhow::bail!(
            "package archive entry {} exceeds the {maximum}-byte limit",
            destination.display()
        );
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .with_context(|| format!("cannot create package file {}", destination.display()))?;
    let copied = io::copy(entry, &mut output)?;
    if copied != size {
        anyhow::bail!(
            "package archive entry {} ended after {copied} of {size} bytes",
            destination.display()
        );
    }
    output.sync_all()?;
    Ok(())
}

fn ensure_package_directory(root: &Path) -> anyhow::Result<PathBuf> {
    let package = root.join(PACKAGE_ID);
    let metadata = fs::symlink_metadata(&package)
        .with_context(|| format!("cannot inspect installed extension {}", package.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!(
            "installed extension is not a regular directory: {}",
            package.display()
        );
    }
    Ok(package)
}

fn load_installed(root: &Path) -> anyhow::Result<PackageManifest> {
    let package = ensure_package_directory(root)?;
    let manifest = load_manifest(&package.join(PACKAGE_MANIFEST))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

/// The version of the installed octet Serve extension, if any, read without
/// validating it against this binary. A fresh update can leave the
/// extension stale, and validation would fail in exactly that case.
pub(crate) fn installed_version() -> Option<Version> {
    let root = extensions_root().ok()?;
    let package = ensure_package_directory(&root).ok()?;
    let manifest = load_manifest(&package.join(PACKAGE_MANIFEST)).ok()?;
    Version::parse(&manifest.version).ok()
}

/// Managed executable bundles that can be refreshed from the official catalog.
pub(crate) fn installed_official_bundle_ids() -> Vec<String> {
    let Ok(root) = extensions_root() else {
        return Vec::new();
    };
    crate::extension_bundle::list_installed(&root)
        .unwrap_or_default()
        .into_iter()
        .filter(|bundle| crate::extension_bundle::is_official_bundle(&bundle.id))
        .map(|bundle| bundle.id)
        .collect()
}

fn load_manifest(path: &Path) -> anyhow::Result<PackageManifest> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect package manifest {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("package manifest is not a regular file: {}", path.display());
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        anyhow::bail!(
            "package manifest {} exceeds the {MAX_MANIFEST_BYTES}-byte limit",
            path.display()
        );
    }
    let source = fs::read_to_string(path)
        .with_context(|| format!("cannot read package manifest {}", path.display()))?;
    toml::from_str(&source).with_context(|| format!("invalid package manifest {}", path.display()))
}

fn validate_manifest(manifest: &PackageManifest) -> anyhow::Result<()> {
    if manifest.schema_version != 1 {
        anyhow::bail!(
            "unsupported package manifest schema {}; expected 1",
            manifest.schema_version
        );
    }
    validate_supported_name(&manifest.id)?;

    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let package_version = Version::parse(&manifest.version)
        .context("package version is not valid semantic versioning")?;
    if package_version != current {
        anyhow::bail!(
            "package version {package_version} is incompatible with octet {current}; install the matching release"
        );
    }
    let expected_requirement = format!("={current}");
    let requirement = VersionReq::parse(&manifest.requires_octet)
        .context("package requires_octet is not a valid semantic version requirement")?;
    if manifest.requires_octet != expected_requirement || !requirement.matches(&current) {
        anyhow::bail!(
            "package requires octet {:?}; this release requires an exact {:?} package",
            manifest.requires_octet,
            expected_requirement
        );
    }

    let target = target_triple()?;
    if manifest.target != target {
        anyhow::bail!(
            "package target {:?} does not match this octet binary ({target})",
            manifest.target
        );
    }
    if manifest.entrypoint.path != ENTRYPOINT || manifest.entrypoint.args != ["serve"] {
        anyhow::bail!("package entrypoint must be {ENTRYPOINT} with the single argument 'serve'");
    }
    validate_sha256(&manifest.entrypoint.sha256)?;
    if manifest.capabilities.network != "loopback"
        || !manifest.capabilities.process
        || manifest.capabilities.filesystem != "workspace"
    {
        anyhow::bail!(
            "octet Serve must declare network='loopback', process=true, and filesystem='workspace'"
        );
    }
    Ok(())
}

fn stage_validated_entrypoint(
    path: &Path,
    expected_sha256: &str,
) -> anyhow::Result<(tempfile::TempDir, PathBuf)> {
    let mut source = octet_agent::secure_fs::open_regular_file_for_read(path)
        .with_context(|| format!("cannot open package entrypoint {}", path.display()))?;
    let metadata = source.metadata()?;
    if metadata.len() > MAX_ENTRYPOINT_BYTES {
        anyhow::bail!(
            "package entrypoint {} exceeds the {MAX_ENTRYPOINT_BYTES}-byte limit",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            anyhow::bail!("package entrypoint is not executable: {}", path.display());
        }
    }

    let temporary = tempfile::Builder::new()
        .prefix("octet-package-entrypoint-")
        .tempdir()
        .context("cannot create private package entrypoint snapshot")?;
    let staged = temporary.path().join("octet-serve-runtime");
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o700);
    }
    let mut destination = options.open(&staged)?;
    let mut hasher = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| anyhow::anyhow!("package entrypoint size overflow"))?;
        if copied > MAX_ENTRYPOINT_BYTES {
            anyhow::bail!("package entrypoint grew beyond its byte limit");
        }
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    let actual = digest_hex(hasher.finalize().as_slice());
    if actual != expected_sha256 {
        anyhow::bail!(
            "package entrypoint checksum mismatch: expected {expected_sha256}, found {actual}"
        );
    }
    destination.flush()?;
    destination.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        destination.set_permissions(std::fs::Permissions::from_mode(0o700))?;
        destination.sync_all()?;
    }
    Ok((temporary, staged))
}

fn validate_entrypoint(path: &Path, expected_sha256: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("cannot inspect package entrypoint {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!(
            "package entrypoint is not a regular file: {}",
            path.display()
        );
    }
    if metadata.len() > MAX_ENTRYPOINT_BYTES {
        anyhow::bail!(
            "package entrypoint {} exceeds the {MAX_ENTRYPOINT_BYTES}-byte limit",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            anyhow::bail!("package entrypoint is not executable: {}", path.display());
        }
    }
    let actual = sha256_file_bounded(path, MAX_ENTRYPOINT_BYTES)?;
    if actual != expected_sha256 {
        anyhow::bail!(
            "package entrypoint checksum mismatch: expected {expected_sha256}, found {actual}"
        );
    }
    Ok(())
}

fn write_install_record(
    package: &Path,
    manifest: &PackageManifest,
    source: &str,
    archive_sha256: &str,
) -> anyhow::Result<()> {
    validate_sha256(archive_sha256)?;
    let record = InstallRecord {
        schema_version: 1,
        id: &manifest.id,
        version: &manifest.version,
        target: &manifest.target,
        source,
        archive_sha256,
        entrypoint_sha256: &manifest.entrypoint.sha256,
        installed_by_octet: env!("CARGO_PKG_VERSION"),
    };
    let mut encoded = serde_json::to_vec_pretty(&record)?;
    encoded.push(b'\n');
    let path = package.join(INSTALL_RECORD);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot create install record {}", path.display()))?;
    file.write_all(&encoded)?;
    file.sync_all()?;
    sync_directory(package);
    Ok(())
}

fn list_all_installed(root: &Path) -> anyhow::Result<()> {
    let mut rows = Vec::<(String, String, String, String, String, String)>::new();
    match load_installed(root) {
        Ok(manifest) => rows.push((
            manifest.id,
            manifest.version,
            "application".to_owned(),
            "-".to_owned(),
            manifest.requires_octet,
            manifest.target,
        )),
        Err(error) if error_chain_has_io_kind(&error, io::ErrorKind::NotFound) => {}
        Err(error) => return Err(error),
    }
    for bundle in crate::extension_bundle::list_installed(root)? {
        rows.push((
            bundle.id,
            bundle.version,
            "executable".to_owned(),
            bundle.api_version,
            bundle.requires_octet,
            "any".to_owned(),
        ));
    }
    rows.sort_by(|left, right| left.0.cmp(&right.0));

    if rows.is_empty() {
        crate::output::stdout_line("No extension packages installed.");
        return Ok(());
    }
    crate::output::stdout_table_line("ID\tVERSION\tKIND\tAPI\tOCTET\tTARGET");
    for (id, version, kind, api, octet, target) in rows {
        crate::output::stdout_table_line(format!(
            "{id}\t{version}\t{kind}\t{api}\t{octet}\t{target}"
        ));
    }
    Ok(())
}

fn remove_installed(root: &Path) -> anyhow::Result<()> {
    let _lock = acquire_lock(root)?;
    let package = ensure_package_directory(root)?;
    let removed = root.join(format!(
        ".{PACKAGE_ID}.remove-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::rename(&package, &removed)
        .with_context(|| format!("cannot remove extension directory {}", package.display()))?;
    sync_directory(root);
    fs::remove_dir_all(&removed)
        .with_context(|| format!("cannot delete removed package files {}", removed.display()))?;
    Ok(())
}

fn error_chain_has_io_kind(error: &anyhow::Error, kind: io::ErrorKind) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
        .any(|error| error.kind() == kind)
}

fn target_triple() -> anyhow::Result<&'static str> {
    if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Ok("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else {
        anyhow::bail!(
            "octet Serve has no v{} package for {}/{}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
}

pub(super) async fn download_bytes(url: &str, maximum: usize) -> anyhow::Result<Vec<u8>> {
    let url = reqwest::Url::parse(url).context("invalid release download URL")?;
    let client = download_client(is_trusted_release_url)?;
    download_bytes_with_client(
        &client,
        url,
        maximum,
        is_trusted_release_url,
        DOWNLOAD_RETRY_POLICY,
    )
    .await
}

async fn download_bytes_with_client(
    client: &reqwest::Client,
    url: reqwest::Url,
    maximum: usize,
    is_trusted: ReleaseUrlTrust,
    retry_policy: DownloadRetryPolicy,
) -> anyhow::Result<Vec<u8>> {
    let max_attempts = retry_policy.attempts();
    for attempt in 1..=max_attempts {
        // Headers and the complete bounded body share this attempt budget.
        let result: anyhow::Result<_> = async {
            let response = send_download_with_client(client, &url, is_trusted).await?;
            if response
                .content_length()
                .is_some_and(|length| length > maximum as u64)
            {
                anyhow::bail!("download exceeds the {maximum}-byte limit: {url}");
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.with_context(|| format!("cannot download {url}"))?;
                if bytes.len().saturating_add(chunk.len()) > maximum {
                    anyhow::bail!("download exceeds the {maximum}-byte limit: {url}");
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        }
        .await;
        match result {
            Err(error) if retryable_download_error(&error) && attempt < max_attempts => {
                tokio::time::sleep(retry_policy.backoff(attempt)).await;
            }
            result => return result,
        }
    }
    unreachable!("release download retry loop always has at least one attempt")
}

pub(super) async fn download_file(url: &str, path: &Path, maximum: u64) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(url).context("invalid release download URL")?;
    let client = download_client(is_trusted_release_url)?;
    download_file_with_client(
        &client,
        url,
        path,
        maximum,
        is_trusted_release_url,
        DOWNLOAD_RETRY_POLICY,
    )
    .await
}

async fn download_file_with_client(
    client: &reqwest::Client,
    url: reqwest::Url,
    path: &Path,
    maximum: u64,
    is_trusted: ReleaseUrlTrust,
    retry_policy: DownloadRetryPolicy,
) -> anyhow::Result<String> {
    let mut destination: Option<File> = None;
    let max_attempts = retry_policy.attempts();
    for attempt in 1..=max_attempts {
        let result: anyhow::Result<_> = async {
            let response = send_download_with_client(client, &url, is_trusted).await?;
            if response
                .content_length()
                .is_some_and(|length| length > maximum)
            {
                anyhow::bail!("download exceeds the {maximum}-byte limit: {url}");
            }
            let file = match &mut destination {
                Some(file) => {
                    // Reset only the file we created, never reopen a retry path.
                    file.set_len(0)?;
                    file.rewind()?;
                    file
                }
                slot @ None => slot.insert(
                    OpenOptions::new()
                        .create_new(true)
                        .write(true)
                        .open(path)
                        .with_context(|| {
                            format!("cannot create extension download {}", path.display())
                        })?,
                ),
            };
            let mut hasher = Sha256::new();
            let mut downloaded = 0u64;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.with_context(|| format!("cannot download {url}"))?;
                downloaded = downloaded
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| anyhow::anyhow!("download size overflow for {url}"))?;
                if downloaded > maximum {
                    anyhow::bail!("download exceeds the {maximum}-byte limit: {url}");
                }
                hasher.update(&chunk);
                file.write_all(&chunk)?;
            }
            file.sync_all()?;
            Ok(digest_hex(hasher.finalize().as_slice()))
        }
        .await;
        match result {
            Err(error) if retryable_download_error(&error) && attempt < max_attempts => {
                tokio::time::sleep(retry_policy.backoff(attempt)).await;
            }
            result => return result,
        }
    }
    unreachable!("release download retry loop always has at least one attempt")
}

fn is_trusted_release_url(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some("github.com" | "release-assets.githubusercontent.com")
        )
}

type ReleaseUrlTrust = fn(&reqwest::Url) -> bool;

#[derive(Clone, Copy)]
struct DownloadRetryPolicy {
    max_attempts: usize,
    initial_backoff: Duration,
    max_backoff: Duration,
}

impl DownloadRetryPolicy {
    fn attempts(self) -> usize {
        self.max_attempts.max(1)
    }

    fn backoff(self, retry_number: usize) -> Duration {
        if retry_number == 0 || self.initial_backoff.is_zero() || self.max_backoff.is_zero() {
            return Duration::ZERO;
        }
        let mut delay = self.initial_backoff.min(self.max_backoff);
        for _ in 1..retry_number.min(64) {
            delay = delay.saturating_mul(2).min(self.max_backoff);
        }
        delay
    }
}

const DOWNLOAD_RETRY_POLICY: DownloadRetryPolicy = DownloadRetryPolicy {
    max_attempts: DOWNLOAD_MAX_ATTEMPTS,
    initial_backoff: DOWNLOAD_INITIAL_BACKOFF,
    max_backoff: DOWNLOAD_MAX_BACKOFF,
};

fn redirect_policy(is_trusted: ReleaseUrlTrust) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if is_trusted(attempt.url()) {
            reqwest::redirect::Policy::default().redirect(attempt)
        } else {
            let url = attempt.url().clone();
            attempt.error(io::Error::other(format!(
                "refusing untrusted release redirect to {url}"
            )))
        }
    })
}

fn download_client(is_trusted: ReleaseUrlTrust) -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("octet/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(DOWNLOAD_CONNECT_TIMEOUT)
        .read_timeout(DOWNLOAD_READ_TIMEOUT)
        .retry(reqwest::retry::never())
        .redirect(redirect_policy(is_trusted))
        .build()
        .context("cannot build release download client")
}

fn retryable_download_status(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

fn retryable_download_error(error: &anyhow::Error) -> bool {
    // Local I/O and validation errors are terminal. Only transport timeouts and
    // explicitly transient HTTP statuses may replay a trusted release GET.
    error.downcast_ref::<reqwest::Error>().is_some_and(|error| {
        !error.is_redirect()
            && (error.is_timeout() || error.status().is_some_and(retryable_download_status))
    })
}

async fn send_download_with_client(
    client: &reqwest::Client,
    url: &reqwest::Url,
    is_trusted: ReleaseUrlTrust,
) -> anyhow::Result<reqwest::Response> {
    if !is_trusted(url) {
        anyhow::bail!("refusing untrusted release URL: {url}");
    }

    // One attempt only: the caller owns the shared headers + body retry budget.
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("cannot download {url}"))?;
    if !is_trusted(response.url()) {
        anyhow::bail!("refusing untrusted release redirect to {}", response.url());
    }
    response
        .error_for_status()
        .map_err(reqwest::Error::without_url)
        .with_context(|| format!("release download failed: {url}"))
}

pub(super) fn checksum_for_asset(checksums: &str, asset: &str) -> anyhow::Result<String> {
    let mut found = None;
    for line in checksums.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 {
            anyhow::bail!("invalid SHA256SUMS line: {line:?}");
        }
        let digest = fields[0].to_ascii_lowercase();
        validate_sha256(&digest)?;
        let name = fields[1]
            .strip_prefix('*')
            .unwrap_or(fields[1])
            .strip_prefix("./")
            .unwrap_or(fields[1].strip_prefix('*').unwrap_or(fields[1]));
        if name == asset && found.replace(digest).is_some() {
            anyhow::bail!("SHA256SUMS contains duplicate entries for {asset}");
        }
    }
    found.ok_or_else(|| anyhow::anyhow!("SHA256SUMS does not contain {asset}"))
}

pub(super) fn validate_sha256(digest: &str) -> anyhow::Result<()> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        anyhow::bail!("invalid lowercase SHA-256 digest {digest:?}")
    }
}

fn sha256_file_bounded(path: &Path, maximum: u64) -> anyhow::Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        anyhow::bail!("not a regular file: {}", path.display());
    }
    if metadata.len() > maximum {
        anyhow::bail!("{} exceeds the {maximum}-byte limit", path.display());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| anyhow::anyhow!("file size overflow for {}", path.display()))?;
        if total > maximum {
            anyhow::bail!("{} exceeds the {maximum}-byte limit", path.display());
        }
        hasher.update(&buffer[..read]);
    }
    Ok(digest_hex(hasher.finalize().as_slice()))
}

pub(super) fn digest_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(super) fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

pub(super) fn sync_directory(path: &Path) {
    if let Ok(directory) = File::open(path) {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use flate2::write::GzEncoder;
    use flate2::Compression;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn package_manifest(binary: &[u8]) -> String {
        let digest = digest_hex(Sha256::digest(binary).as_slice());
        format!(
            "schema_version = 1\n\
             id = \"octet-serve\"\n\
             version = \"{}\"\n\
             requires_octet = \"={}\"\n\
             target = \"{}\"\n\n\
             [entrypoint]\n\
             path = \"bin/octet-serve-runtime\"\n\
             args = [\"serve\"]\n\
             sha256 = \"{digest}\"\n\n\
             [capabilities]\n\
             network = \"loopback\"\n\
             process = true\n\
             filesystem = \"workspace\"\n",
            env!("CARGO_PKG_VERSION"),
            env!("CARGO_PKG_VERSION"),
            target_triple().unwrap()
        )
    }

    fn create_package(directory: &Path, binary: &[u8]) -> PathBuf {
        let path = directory.join("package.tar.gz");
        let file = File::create(&path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append(
            &mut archive,
            PACKAGE_MANIFEST,
            package_manifest(binary).as_bytes(),
        );
        append(&mut archive, ENTRYPOINT, binary);
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
        path
    }

    fn append<W: Write>(archive: &mut tar::Builder<W>, relative: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{PACKAGE_ID}/{relative}"), bytes)
            .unwrap();
    }

    fn append_directory<W: Write>(archive: &mut tar::Builder<W>, relative: &str) {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_size(0);
        header.set_cksum();
        archive
            .append_data(&mut header, relative, std::io::empty())
            .unwrap();
    }

    fn is_trusted_test_release_url(url: &reqwest::Url) -> bool {
        url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
    }

    fn test_download_client(timeout: Option<Duration>) -> reqwest::Client {
        let mut builder = reqwest::Client::builder()
            .user_agent("octet-release-download-test")
            .no_proxy()
            .retry(reqwest::retry::never())
            .redirect(redirect_policy(is_trusted_test_release_url));
        if let Some(timeout) = timeout {
            builder = builder.read_timeout(timeout);
        }
        builder.build().unwrap()
    }

    const TEST_RETRY_POLICY: DownloadRetryPolicy = DownloadRetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
    };

    async fn test_download(url: String, client: &reqwest::Client) -> anyhow::Result<Vec<u8>> {
        download_bytes_with_client(
            client,
            reqwest::Url::parse(&url).unwrap(),
            MAX_CHECKSUM_BYTES,
            is_trusted_test_release_url,
            TEST_RETRY_POLICY,
        )
        .await
    }

    async fn test_download_to(
        url: &str,
        client: &reqwest::Client,
        maximum: usize,
        destination: Option<&Path>,
    ) -> anyhow::Result<Vec<u8>> {
        let url = reqwest::Url::parse(url).unwrap();
        if let Some(destination) = destination {
            let digest = download_file_with_client(
                client,
                url,
                destination,
                maximum as u64,
                is_trusted_test_release_url,
                TEST_RETRY_POLICY,
            )
            .await?;
            let bytes = fs::read(destination)?;
            assert_eq!(digest, digest_hex(Sha256::digest(&bytes).as_slice()));
            Ok(bytes)
        } else {
            download_bytes_with_client(
                client,
                url,
                maximum,
                is_trusted_test_release_url,
                TEST_RETRY_POLICY,
            )
            .await
        }
    }

    // Wiremock delays headers, not individual body chunks. Keep these loopback
    // sockets open after scripted bytes to exercise real reqwest read timeouts.
    struct ScriptedDownloadServer {
        url: String,
        attempts: Arc<AtomicUsize>,
        task: tokio::task::JoinHandle<()>,
    }

    impl Drop for ScriptedDownloadServer {
        fn drop(&mut self) {
            self.task.abort();
        }
    }

    async fn scripted_download_server(responses: Vec<&'static [u8]>) -> ScriptedDownloadServer {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        let attempts = Arc::new(AtomicUsize::new(0));
        let requests = Arc::clone(&attempts);
        let task = tokio::spawn(async move {
            let mut connections = Vec::new();
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let byte = socket.read_u8().await.unwrap();
                    request.push(byte);
                    assert!(request.len() <= 8192);
                }
                assert!(request.starts_with(b"GET /asset HTTP/1.1\r\n"));
                let attempt = requests.fetch_add(1, Ordering::SeqCst);
                let response = responses.get(attempt).copied().unwrap_or(
                    b"HTTP/1.1 500 Unexpected retry\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
                socket.write_all(response).await.unwrap();
                connections.push(socket);
            }
        });
        ScriptedDownloadServer {
            url,
            attempts,
            task,
        }
    }

    const PARTIAL_DOWNLOAD: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n10\r\nold-partial-body\r\n";
    const SHORT_PARTIAL_DOWNLOAD: &[u8] =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nshort\r\n";
    const COMPLETE_DOWNLOAD: &[u8] =
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
    const UNAVAILABLE_DOWNLOAD: &[u8] =
        b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

    #[test]
    fn checksum_parser_accepts_release_tool_spelling() {
        let digest = "a".repeat(64);
        let sums = format!("{digest}  ./other.tar.gz\n{digest}  *wanted.tar.gz\n");
        assert_eq!(checksum_for_asset(&sums, "wanted.tar.gz").unwrap(), digest);
    }

    #[test]
    fn checksum_parser_rejects_duplicates() {
        let digest = "b".repeat(64);
        let sums = format!("{digest}  wanted.tar.gz\n{digest}  ./wanted.tar.gz\n");
        assert!(checksum_for_asset(&sums, "wanted.tar.gz").is_err());
    }

    #[test]
    fn official_downloads_only_trust_github_https_hosts() {
        assert_eq!(
            RELEASE_REPOSITORY,
            "https://github.com/skaft-software/octet"
        );
        for accepted in [
            "https://github.com/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
            "https://github.com/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
            "https://release-assets.githubusercontent.com/github-production-release-asset/file?token=signed",
        ] {
            assert!(is_trusted_release_url(
                &reqwest::Url::parse(accepted).unwrap()
            ));
        }

        for rejected in [
            "http://github.com/skaft-software/octet/releases/download/file",
            "https://github.com.example.com/file",
            "https://raw.githubusercontent.com/skaft-software/octet/main/file",
            "https://github.com:8443/file",
        ] {
            assert!(!is_trusted_release_url(
                &reqwest::Url::parse(rejected).unwrap()
            ));
        }
    }

    #[tokio::test]
    async fn release_download_follows_the_historical_repository_redirect() {
        let server = MockServer::start().await;
        let canonical_path = "/skaft-software/octet/releases/download/v0.7.0/SHA256SUMS";
        Mock::given(method("GET"))
            .and(path(
                "/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
            ))
            .respond_with(
                ResponseTemplate::new(301)
                    .insert_header("location", format!("{}{canonical_path}", server.uri())),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(canonical_path))
            .respond_with(ResponseTemplate::new(200).set_body_string("canonical checksums"))
            .expect(1)
            .mount(&server)
            .await;

        let bytes = test_download(
            format!(
                "{}/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
                server.uri()
            ),
            &test_download_client(None),
        )
        .await
        .unwrap();

        assert_eq!(bytes, b"canonical checksums");
        server.verify().await;
    }

    #[tokio::test]
    async fn canonical_release_download_succeeds_without_a_redirect() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("checksums"))
            .expect(1)
            .mount(&server)
            .await;

        let bytes = test_download(
            format!(
                "{}/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
                server.uri()
            ),
            &test_download_client(None),
        )
        .await
        .unwrap();

        assert_eq!(bytes, b"checksums");
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_retries_a_transient_gateway_failure() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let response_attempts = Arc::clone(&attempts);
        Mock::given(method("GET"))
            .and(path("/transient"))
            .respond_with(move |_: &wiremock::Request| {
                if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(504)
                } else {
                    ResponseTemplate::new(200).set_body_string("recovered")
                }
            })
            .expect(2)
            .mount(&server)
            .await;

        let bytes = test_download(
            format!("{}/transient", server.uri()),
            &test_download_client(None),
        )
        .await
        .unwrap();

        assert_eq!(bytes, b"recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_stops_after_the_bounded_attempt_count() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/unavailable"))
            .respond_with(ResponseTemplate::new(503))
            .expect(TEST_RETRY_POLICY.max_attempts as u64)
            .mount(&server)
            .await;

        let error = test_download(
            format!("{}/unavailable", server.uri()),
            &test_download_client(None),
        )
        .await
        .unwrap_err();

        assert!(format!("{error:#}").contains("503"), "{error:#}");
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_retries_a_transient_timeout() {
        let server = MockServer::start().await;
        let attempts = Arc::new(AtomicUsize::new(0));
        let response_attempts = Arc::clone(&attempts);
        Mock::given(method("GET"))
            .and(path("/timeout"))
            .respond_with(move |_: &wiremock::Request| {
                if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(200)
                        .set_body_string("late")
                        .set_delay(Duration::from_millis(250))
                } else {
                    ResponseTemplate::new(200).set_body_string("recovered")
                }
            })
            .expect(2)
            .mount(&server)
            .await;
        let client = test_download_client(Some(Duration::from_millis(50)));

        let bytes = test_download(format!("{}/timeout", server.uri()), &client)
            .await
            .unwrap();

        assert_eq!(bytes, b"recovered");
        assert_eq!(attempts.load(Ordering::SeqCst), 2);
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_does_not_retry_an_untrusted_redirect() {
        let server = MockServer::start().await;
        let rejected_target = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/untrusted-redirect"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/trusted-hop", server.uri())),
            )
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/trusted-hop"))
            .respond_with(ResponseTemplate::new(302).insert_header(
                "location",
                rejected_target.uri().replace("127.0.0.1", "localhost"),
            ))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200))
            .expect(0)
            .mount(&rejected_target)
            .await;

        for to_file in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("archive");
            let error = test_download_to(
                &format!("{}/untrusted-redirect", server.uri()),
                &test_download_client(None),
                16,
                to_file.then_some(destination.as_path()),
            )
            .await
            .unwrap_err();
            assert!(
                format!("{error:#}").contains("refusing untrusted release redirect"),
                "{error:#}"
            );
            assert!(!retryable_download_error(&error));
            assert!(!destination.exists());
        }
        assert!(rejected_target
            .received_requests()
            .await
            .unwrap()
            .is_empty());
        rejected_target.verify().await;
        server.verify().await;
    }

    #[test]
    fn release_download_backoff_is_bounded() {
        assert_eq!(DOWNLOAD_RETRY_POLICY.attempts(), 3);
        assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(1), Duration::from_millis(250));
        assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(2), Duration::from_millis(500));
        for retry in [3, 4, 64, usize::MAX] {
            assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(retry), Duration::from_secs(1));
        }
        assert_eq!(TEST_RETRY_POLICY.backoff(2), Duration::ZERO);
        let local_timeout = anyhow::Error::new(io::Error::from(io::ErrorKind::TimedOut))
            .context("local file operation timed out");
        assert!(!retryable_download_error(&local_timeout));
    }

    #[tokio::test]
    async fn release_download_mid_body_timeout_restarts_bytes_and_files() {
        for to_file in [false, true] {
            // A status and a body timeout must use the same three-attempt budget.
            let server = scripted_download_server(vec![
                UNAVAILABLE_DOWNLOAD,
                PARTIAL_DOWNLOAD,
                COMPLETE_DOWNLOAD,
            ])
            .await;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("archive");
            let client = test_download_client(Some(Duration::from_millis(100)));
            let bytes = test_download_to(
                &server.url,
                &client,
                // The discarded prefix alone fills the limit. A leaked counter,
                // hash, buffer, file offset, or tail would fail this recovery.
                b"old-partial-body".len(),
                to_file.then_some(destination.as_path()),
            )
            .await
            .unwrap();

            assert_eq!(bytes, b"ok");
            assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
            assert!(!server.task.is_finished());
        }
    }

    #[tokio::test]
    async fn release_download_mid_body_timeout_exhausts_one_budget_for_bytes_and_files() {
        for to_file in [false, true] {
            for first_response in [PARTIAL_DOWNLOAD, UNAVAILABLE_DOWNLOAD] {
                let server = scripted_download_server(vec![
                    first_response,
                    PARTIAL_DOWNLOAD,
                    SHORT_PARTIAL_DOWNLOAD,
                    COMPLETE_DOWNLOAD,
                ])
                .await;
                let directory = tempfile::tempdir().unwrap();
                let destination = directory.path().join("archive");
                let client = test_download_client(Some(Duration::from_millis(100)));
                let error = test_download_to(
                    &server.url,
                    &client,
                    b"old-partial-body".len(),
                    to_file.then_some(destination.as_path()),
                )
                .await
                .unwrap_err();

                assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
                assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
                assert!(!server.task.is_finished());
                if to_file {
                    // Failure returns no digest and retains only the last attempt,
                    // not earlier partial data. The caller owns tempdir cleanup.
                    assert_eq!(fs::read(&destination).unwrap(), b"short");
                }
            }
        }
    }

    #[tokio::test]
    async fn release_download_transient_statuses_recover_or_exhaust_bytes_and_files() {
        for status in [408, 429, 500, 503, 504] {
            for to_file in [false, true] {
                for recover in [false, true] {
                    let server = MockServer::start().await;
                    let attempts = Arc::new(AtomicUsize::new(0));
                    let requests = Arc::clone(&attempts);
                    Mock::given(method("GET"))
                        .respond_with(move |_: &wiremock::Request| {
                            if requests.fetch_add(1, Ordering::SeqCst) == 1 && recover {
                                ResponseTemplate::new(200).set_body_string("ok")
                            } else {
                                ResponseTemplate::new(status)
                            }
                        })
                        .expect(if recover { 2 } else { 3 })
                        .mount(&server)
                        .await;
                    let directory = tempfile::tempdir().unwrap();
                    let destination = directory.path().join("archive");
                    let result = test_download_to(
                        &server.uri(),
                        &test_download_client(None),
                        16,
                        to_file.then_some(destination.as_path()),
                    )
                    .await;
                    if recover {
                        assert_eq!(result.unwrap(), b"ok");
                    } else {
                        let error = result.unwrap_err();
                        assert_eq!(
                            error.downcast_ref::<reqwest::Error>().unwrap().status(),
                            Some(reqwest::StatusCode::from_u16(status).unwrap())
                        );
                        assert!(!destination.exists());
                    }
                    server.verify().await;
                }
            }
        }
    }

    #[tokio::test]
    async fn release_download_terminal_4xx_are_not_replayed() {
        for status in [400, 401, 403, 404, 410, 422] {
            for to_file in [false, true] {
                let server = MockServer::start().await;
                Mock::given(method("GET"))
                    .respond_with(ResponseTemplate::new(status))
                    .expect(1)
                    .mount(&server)
                    .await;
                let directory = tempfile::tempdir().unwrap();
                let destination = directory.path().join("archive");
                let error = test_download_to(
                    &server.uri(),
                    &test_download_client(None),
                    16,
                    to_file.then_some(destination.as_path()),
                )
                .await
                .unwrap_err();
                assert!(!retryable_download_error(&error));
                assert_eq!(
                    error.downcast_ref::<reqwest::Error>().unwrap().status(),
                    Some(reqwest::StatusCode::from_u16(status).unwrap())
                );
                assert!(!destination.exists());
                server.verify().await;
            }
        }
    }

    #[tokio::test]
    async fn release_download_size_and_protocol_failures_are_not_replayed() {
        for to_file in [false, true] {
            for (response, maximum, size_error) in [
                (COMPLETE_DOWNLOAD, 1, true),
                (PARTIAL_DOWNLOAD, 8, true),
                (b"not an HTTP response\r\n\r\n".as_slice(), 16, false),
                (
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZZ\r\n".as_slice(),
                    16,
                    false,
                ),
            ] {
                let server = scripted_download_server(vec![response, COMPLETE_DOWNLOAD]).await;
                let directory = tempfile::tempdir().unwrap();
                let destination = directory.path().join("archive");
                let error = test_download_to(
                    &server.url,
                    &test_download_client(Some(Duration::from_millis(100))),
                    maximum,
                    to_file.then_some(destination.as_path()),
                )
                .await
                .unwrap_err();
                assert!(!retryable_download_error(&error), "{error:#}");
                if size_error {
                    assert!(format!("{error:#}").contains("byte limit"), "{error:#}");
                } else {
                    assert!(!error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
                }
                assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
                assert!(!server.task.is_finished());
            }
        }
    }

    #[tokio::test]
    async fn release_download_file_creation_errors_are_terminal_and_preserve_existing_files() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("replacement"))
            .expect(2)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("existing");
        fs::write(&existing, b"keep me").unwrap();
        let missing_parent = directory.path().join("missing/archive");
        for destination in [&existing, &missing_parent] {
            let error = test_download_to(
                &server.uri(),
                &test_download_client(None),
                16,
                Some(destination),
            )
            .await
            .unwrap_err();
            assert!(format!("{error:#}").contains("cannot create extension download"));
            assert!(!retryable_download_error(&error));
        }
        assert_eq!(fs::read(existing).unwrap(), b"keep me");
        assert!(!missing_parent.exists());
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_checksum_and_archive_validation_are_terminal() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not a release archive"))
            .expect(2)
            .mount(&server)
            .await;
        let client = test_download_client(None);
        let checksums = test_download(server.uri(), &client).await.unwrap();
        let error =
            checksum_for_asset(std::str::from_utf8(&checksums).unwrap(), "archive").unwrap_err();
        assert!(!retryable_download_error(&error));

        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("archive");
        let digest = download_file_with_client(
            &client,
            reqwest::Url::parse(&server.uri()).unwrap(),
            &archive,
            64,
            is_trusted_test_release_url,
            TEST_RETRY_POLICY,
        )
        .await
        .unwrap();
        let root = directory.path().join("extensions");
        let checksum_error =
            install_archive(&root, &archive, &server.uri(), &"0".repeat(64), false).unwrap_err();
        assert!(format!("{checksum_error:#}").contains("changed before extraction"));
        assert!(!retryable_download_error(&checksum_error));
        let archive_error =
            install_archive(&root, &archive, &server.uri(), &digest, false).unwrap_err();
        assert!(!retryable_download_error(&archive_error));
        assert!(!root.join(PACKAGE_ID).exists());
        server.verify().await;
    }

    #[tokio::test]
    async fn release_download_rejects_untrusted_initial_urls_before_sending() {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("archive");
        for error in [
            download_bytes(&server.uri(), 16).await.unwrap_err(),
            download_file(&server.uri(), &destination, 16)
                .await
                .unwrap_err(),
        ] {
            assert!(format!("{error:#}").contains("refusing untrusted release URL"));
            assert!(!retryable_download_error(&error));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
        assert!(!destination.exists());
    }

    #[test]
    fn local_archive_classifier_keeps_application_and_bundle_formats_distinct() {
        let directory = tempfile::tempdir().unwrap();
        let application = create_package(directory.path(), b"runtime");
        assert_eq!(
            resolve_local_archive_path_from(Path::new("package.tar.gz"), directory.path()).unwrap(),
            application.canonicalize().unwrap()
        );
        #[cfg(unix)]
        {
            let linked_parent = directory.path().join("linked-parent");
            std::os::unix::fs::symlink(directory.path(), &linked_parent).unwrap();
            assert_eq!(
                resolve_local_archive_path_from(
                    &linked_parent.join("package.tar.gz"),
                    directory.path()
                )
                .unwrap(),
                application.canonicalize().unwrap()
            );
        }
        assert_eq!(
            classify_local_archive(&application).unwrap(),
            LocalArchiveKind::Application
        );

        let bundle = directory.path().join("bundle.tar.gz");
        let encoder = GzEncoder::new(File::create(&bundle).unwrap(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_directory(&mut archive, "example");
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(0);
        header.set_cksum();
        archive
            .append_data(&mut header, "example/extension.toml", std::io::empty())
            .unwrap();
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
        assert_eq!(
            classify_local_archive(&bundle).unwrap(),
            LocalArchiveKind::ExecutableBundle
        );
    }

    #[test]
    fn local_archive_installs_expected_shape_and_can_be_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("extensions");
        let first = create_package(directory.path(), b"first runtime");
        let digest = sha256_file_bounded(&first, MAX_ARCHIVE_BYTES).unwrap();
        let manifest = install_archive(&root, &first, "test", &digest, false).unwrap();
        assert_eq!(manifest.id, PACKAGE_ID);
        assert!(root.join(PACKAGE_ID).join(PACKAGE_MANIFEST).is_file());
        assert!(root.join(PACKAGE_ID).join(INSTALL_RECORD).is_file());
        assert_eq!(
            fs::read(root.join(PACKAGE_ID).join(ENTRYPOINT)).unwrap(),
            b"first runtime"
        );
        assert!(install_archive(&root, &first, "test", &digest, false).is_err());

        fs::write(
            root.join(PACKAGE_ID).join(PACKAGE_MANIFEST),
            "damaged = [\n",
        )
        .unwrap();
        fs::remove_file(&first).unwrap();
        let second = create_package(directory.path(), b"second runtime");
        let digest = sha256_file_bounded(&second, MAX_ARCHIVE_BYTES).unwrap();
        install_archive(&root, &second, "test", &digest, true).unwrap();
        assert_eq!(
            fs::read(root.join(PACKAGE_ID).join(ENTRYPOINT)).unwrap(),
            b"second runtime"
        );

        fs::write(
            root.join(PACKAGE_ID).join(PACKAGE_MANIFEST),
            "damaged = [\n",
        )
        .unwrap();
        remove_installed(&root).unwrap();
        assert!(!root.join(PACKAGE_ID).exists());
    }

    #[test]
    fn archive_rejects_unexpected_and_nonportable_members() {
        assert!(archive_member(Path::new("../escape")).is_err());
        assert!(archive_member(Path::new("/absolute")).is_err());

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bad.tar.gz");
        let encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append(&mut archive, "extra", b"bad");
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        assert!(extract_archive(&path, &output).is_err());
    }

    #[test]
    fn archive_rejects_duplicate_directories() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("duplicate.tar.gz");
        let encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_directory(&mut archive, PACKAGE_ID);
        append_directory(&mut archive, PACKAGE_ID);
        let encoder = archive.into_inner().unwrap();
        encoder.finish().unwrap();
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        assert!(extract_archive(&path, &output).is_err());
    }

    #[test]
    fn incompatible_manifest_is_rejected() {
        let manifest: PackageManifest = toml::from_str(&package_manifest(b"runtime")).unwrap();
        validate_manifest(&manifest).unwrap();

        let incompatible = package_manifest(b"runtime").replace(
            &format!("requires_octet = \"={}\"", env!("CARGO_PKG_VERSION")),
            "requires_octet = \">=0.1.0\"",
        );
        let manifest: PackageManifest = toml::from_str(&incompatible).unwrap();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn removal_does_not_touch_data_outside_the_package() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("extensions");
        let archive = create_package(directory.path(), b"runtime");
        let digest = sha256_file_bounded(&archive, MAX_ARCHIVE_BYTES).unwrap();
        install_archive(&root, &archive, "test", &digest, false).unwrap();
        let data = directory.path().join("serve-data");
        fs::write(&data, "keep").unwrap();

        remove_installed(&root).unwrap();

        assert!(!root.join(PACKAGE_ID).exists());
        assert_eq!(fs::read_to_string(data).unwrap(), "keep");
    }
}
