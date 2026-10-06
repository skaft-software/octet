//! Explicit, native-owned Python bootstrap. Discovery and ordinary startup never download.
//!
//! CPython 3.12.15 portable assets pinned to Astral's 20261003 release. URL, size
//! and SHA-256 were observed from the public unauthenticated GitHub release API:
//! https://api.github.com/repos/astral-sh/python-build-standalone/releases/tags/20261003
//! These are upstream digest-verified portable runtimes, NOT an Authenticode or
//! Apple-signature claim. Cua's separate desktop-app signature checks still apply.
//! All upstream files/licences are retained; archive links are validated and
//! materialized as regular files, never created as filesystem links.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::process::Command;

const VERSION: &str = "3.12.15";
const RELEASE: &str = "20261003";
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_RECEIPT_BYTES: usize = 16 * 1024 * 1024;
const RECEIPT: &str = ".octet-python-runtime.json";
const SOURCE_ARCHIVE: &str = ".octet-python-source.tar.gz";

type SourceManifest = BTreeMap<String, String>;
type SourceManifestCache = BTreeMap<String, Arc<SourceManifest>>;

// Only manifests derived from digest-verified source bytes enter this cache.
// Actual installed files are still checked on every launch; no mutable receipt
// or filesystem pathname can authorize the source manifest.
static SOURCE_MANIFESTS: std::sync::LazyLock<std::sync::Mutex<SourceManifestCache>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(BTreeMap::new()));

/// Host-owned private runtime location. Constructing it performs no I/O.
/// The coding host must derive this from its authoritative user home (including
/// Windows known folders), never the workspace or an extension-supplied path.
#[derive(Clone)]
pub struct PythonRuntimeConfig {
    /// Octet-owned absolute directory, normally ~/.octet/runtimes/python.
    pub root: PathBuf,
    /// Present only while an explicitly approved native setup action is active.
    pub setup: Option<PythonRuntimeSetup>,
}

/// Native action-scoped download consent, cancellation and themed progress.
/// This is not an extension protocol field or a persisted trust grant.
#[derive(Clone)]
pub struct PythonRuntimeSetup {
    /// Escape/shutdown must set this before dropping the setup future.
    pub cancelled: Arc<AtomicBool>,
    /// Plain stage text; the owning frontend applies its current theme.
    pub progress: Arc<dyn Fn(&str) + Send + Sync>,
}

impl std::fmt::Debug for PythonRuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PythonRuntimeConfig")
            .field("setup_authorized", &self.setup.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
struct Asset {
    target: &'static str,
    digest: &'static str,
    bytes: usize,
}

const ASSETS: &[Asset] = &[
    Asset {
        target: "aarch64-apple-darwin",
        digest: "ad8d0c637c0a36b967b310e2c07254f4d2ca8cabaa7699e55ed6290aceb481a2",
        bytes: 25_013_243,
    },
    Asset {
        target: "x86_64-apple-darwin",
        digest: "562c30864ece2cb1d3e0ad66a1acd498611a47e5a10ce81b99158bef1ccbd355",
        bytes: 24_733_148,
    },
    Asset {
        target: "aarch64-unknown-linux-gnu",
        digest: "6541297dd1798dec8b98c3ad7492808a5b9d1c126801ceb2011e7754cd20d1ce",
        bytes: 29_215_308,
    },
    Asset {
        target: "x86_64-unknown-linux-gnu",
        digest: "731af898886c5f821890dc901eca3c651cca8e51fa7308c159d12a1194aeac91",
        bytes: 34_285_590,
    },
    Asset {
        target: "aarch64-pc-windows-msvc",
        digest: "b39c6c3aac8a88ae42fd4f2ca5a832d1e78b55506f33f0498de4dd6fc38b5162",
        bytes: 21_048_802,
    },
    Asset {
        target: "x86_64-pc-windows-msvc",
        digest: "6fba7f2ae506facf41d457ea8293c7497910a675c69a4e954875169410a50402",
        bytes: 22_011_023,
    },
];

impl Asset {
    fn url(self) -> String {
        format!(
            "https://github.com/astral-sh/python-build-standalone/releases/download/{RELEASE}/cpython-{VERSION}%2B{RELEASE}-{}-install_only_stripped.tar.gz",
            self.target
        )
    }

    fn directory(self, root: &Path) -> PathBuf {
        root.join(format!("cpython-{VERSION}-{RELEASE}-{}", self.target))
    }

    fn interpreter(self) -> &'static str {
        if self.target.contains("windows") {
            "python.exe"
        } else {
            "bin/python3"
        }
    }
}

fn asset_for(os: &str, arch: &str) -> io::Result<Asset> {
    let environment = if cfg!(target_env = "gnu") {
        "gnu"
    } else {
        "other"
    };
    asset_for_environment(os, arch, environment)
}

fn asset_for_environment(os: &str, arch: &str, environment: &str) -> io::Result<Asset> {
    let target = match (os, arch) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") if environment == "gnu" => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") if environment == "gnu" => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => {
            return Err(io::Error::other(format!(
                "no verified private Python runtime is catalogued for {os}/{arch}; Linux requires glibc, not musl"
            )));
        }
    };
    ASSETS
        .iter()
        .find(|asset| asset.target == target)
        .copied()
        .ok_or_else(|| io::Error::other("private Python platform catalogue is incomplete"))
}

fn check_cancelled(setup: &PythonRuntimeSetup) -> io::Result<()> {
    if setup.cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "private Python setup cancelled; no partial runtime was published",
        ));
    }
    Ok(())
}

fn progress(setup: &PythonRuntimeSetup, stage: &str) -> io::Result<()> {
    check_cancelled(setup)?;
    (setup.progress)(stage);
    check_cancelled(setup)
}

/// Signals owned background extraction when its parent future is dropped.
#[derive(Debug)]
struct SetupCancellationGuard {
    cancelled: Arc<AtomicBool>,
    armed: bool,
}

impl Drop for SetupCancellationGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

/// Explicit setup entry point for the native extension action. The caller must
/// already have admitted the selected source through existing enable/trust and
/// process policy; this function writes no activation, trust or credential data.
pub async fn provision_python_runtime(config: &PythonRuntimeConfig) -> io::Result<PathBuf> {
    let setup = config.setup.as_ref().ok_or_else(|| io::Error::new(io::ErrorKind::PermissionDenied,
        "private Python is not installed; choose Set up runtime in /extensions to approve its verified download"))?;
    check_cancelled(setup)?;
    let asset = asset_for(std::env::consts::OS, std::env::consts::ARCH)?;
    if !config.root.is_absolute() {
        return Err(io::Error::other(
            "the host must supply an absolute Octet-owned Python runtime location",
        ));
    }
    crate::secure_fs::create_private_directory_all(&config.root).map_err(io::Error::other)?;
    let lock = crate::secure_fs::open_private_directory_for_lock(&config.root)
        .map_err(io::Error::other)?;
    lock.try_lock_exclusive().map_err(|_| {
        io::Error::other("private Python setup is already running; wait or cancel it, then retry")
    })?;
    if let Some(binary) = cached_runtime_async(&config.root, asset, Some(setup)).await? {
        progress(setup, "Verified private Python is already installed.")?;
        return Ok(binary);
    }
    let staging = private_staging(&config.root)?;
    progress(setup, "Downloading the pinned private Python runtime…")?;
    let bytes = download(asset, setup).await?;
    progress(setup, "Verifying Python's published SHA-256…")?;
    verify_archive(asset, &bytes)?;
    progress(
        setup,
        "Extracting and validating private Python and its bundled tooling…",
    )?;
    let (mut cancellation_guard, _staging, _lock, runtime) =
        prepare_candidate(bytes, staging, lock, asset, setup.clone(), extract_archive).await?;
    // Only verified source bytes are executed, in a private candidate directory.
    if !probe_python(&runtime.join(asset.interpreter()), &[], Some(setup)).await? {
        return Err(io::Error::other(
            "verified private Python could not run with venv/ensurepip; check this platform's OS/library requirements and retry",
        ));
    }
    progress(setup, "Publishing the verified private Python runtime…")?;
    let destination = asset.directory(&config.root);
    if std::fs::symlink_metadata(&destination).is_ok() {
        return Err(io::Error::other(
            "the private Python destination changed during setup; existing state was not replaced",
        ));
    }
    std::fs::rename(&runtime, &destination)?;
    cancellation_guard.armed = false;
    // Publication is the local commit point. No cancellation/error after it is
    // misrepresented as rollback; readiness of Cua still needs separate checks.
    (setup.progress)(
        "Private Python and its bundled venv tooling are installed. Desktop driver setup and OS permission checks are still required.",
    );
    Ok(destination.join(asset.interpreter()))
}

fn private_staging(root: &Path) -> io::Result<tempfile::TempDir> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("python-setup-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    // Windows inherits the already validated private parent's DACL. Enforce
    // the secure boundary again before any artifact bytes are written.
    let directory = builder.tempdir_in(root)?;
    crate::secure_fs::create_private_directory_all(directory.path()).map_err(io::Error::other)?;
    Ok(directory)
}

// The narrow extraction callback makes cancellation/ownership deterministic
// in offline tests. Production always passes the safe extractor above.
async fn prepare_candidate<F>(
    bytes: Vec<u8>,
    staging: tempfile::TempDir,
    lock: std::fs::File,
    asset: Asset,
    setup: PythonRuntimeSetup,
    extract: F,
) -> io::Result<(
    SetupCancellationGuard,
    tempfile::TempDir,
    std::fs::File,
    PathBuf,
)>
where
    F: FnOnce(&[u8], &Path, &PythonRuntimeSetup) -> io::Result<PathBuf> + Send + 'static,
{
    let guard = SetupCancellationGuard {
        cancelled: Arc::clone(&setup.cancelled),
        armed: true,
    };
    // Ownership moves into the worker. Dropping this actual provisioning stage
    // cannot release its lock or remove files from under running extraction.
    // Only the async owner, never the background worker, may publish.
    let (staging, lock, runtime) = tokio::task::spawn_blocking(move || -> io::Result<_> {
        verify_archive(asset, &bytes)?;
        let runtime = extract(&bytes, staging.path(), &setup)?;
        let files = inventory(&runtime, Some(&setup))?;
        let expected = source_manifest(asset, &bytes, Some(&setup))?;
        if files != *expected || !files.contains_key(asset.interpreter()) {
            return Err(io::Error::other(
                "private Python extraction differs from its verified archive or has no interpreter",
            ));
        }
        crate::secure_fs::write_private_atomic(
            &runtime.join(SOURCE_ARCHIVE),
            &bytes,
            MAX_ARCHIVE_BYTES,
        )
        .map_err(io::Error::other)?;
        let receipt = Receipt {
            schema: 1,
            source: asset.url(),
            archive_sha256: asset.digest.to_owned(),
            files,
        };
        crate::secure_fs::write_private_atomic(
            &runtime.join(RECEIPT),
            &serde_json::to_vec(&receipt).map_err(io::Error::other)?,
            MAX_RECEIPT_BYTES,
        )
        .map_err(io::Error::other)?;
        Ok((staging, lock, runtime))
    })
    .await
    .map_err(io::Error::other)??;
    Ok((guard, staging, lock, runtime))
}

async fn download(asset: Asset, setup: &PythonRuntimeSetup) -> io::Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(180))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() < 5
                && url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
            {
                attempt.follow()
            } else {
                attempt.error("untrusted private Python download redirect")
            }
        }))
        .build()
        .map_err(io::Error::other)?;
    let transfer = async {
        let mut response = client
            .get(asset.url())
            .send()
            .await
            .map_err(io::Error::other)?
            .error_for_status()
            .map_err(io::Error::other)?;
        let mut bytes = Vec::with_capacity(asset.bytes);
        while let Some(chunk) = response.chunk().await.map_err(io::Error::other)? {
            check_cancelled(setup)?;
            if bytes.len().saturating_add(chunk.len()) > asset.bytes.min(MAX_ARCHIVE_BYTES) {
                return Err(io::Error::other(
                    "private Python download exceeds its pinned size",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        check_cancelled(setup)?;
        Ok(bytes)
    };
    tokio::select! {
        result = transfer => result,
        () = wait_cancelled(setup) => Err(io::Error::new(io::ErrorKind::Interrupted, "private Python download cancelled")),
    }
}

async fn wait_cancelled(setup: &PythonRuntimeSetup) {
    while !setup.cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn verify_archive(asset: Asset, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() != asset.bytes || format!("{:x}", Sha256::digest(bytes)) != asset.digest {
        return Err(io::Error::other(
            "private Python download does not match its pinned size/SHA-256; no code was executed",
        ));
    }
    Ok(())
}

fn safe_relative(name: &str) -> io::Result<PathBuf> {
    if name.is_empty()
        || name.contains(['\\', ':', '\0'])
        || name.starts_with('/')
        || name.split('/').any(|part| {
            part.is_empty()
                || part == "."
                || part == ".."
                || part.ends_with(['.', ' '])
                || matches!(
                    part.split('.')
                        .next()
                        .unwrap_or("")
                        .to_ascii_uppercase()
                        .as_str(),
                    "CON"
                        | "PRN"
                        | "AUX"
                        | "NUL"
                        | "COM1"
                        | "COM2"
                        | "COM3"
                        | "COM4"
                        | "COM5"
                        | "COM6"
                        | "COM7"
                        | "COM8"
                        | "COM9"
                        | "LPT1"
                        | "LPT2"
                        | "LPT3"
                        | "LPT4"
                        | "LPT5"
                        | "LPT6"
                        | "LPT7"
                        | "LPT8"
                        | "LPT9"
                )
        })
    {
        return Err(io::Error::other(
            "unsafe path in private Python archive/receipt",
        ));
    }
    Ok(PathBuf::from(name))
}

fn link_target(member: &Path, target: &str, hard: bool) -> io::Result<PathBuf> {
    if target.contains(['\\', ':', '\0']) || target.starts_with('/') {
        return Err(io::Error::other("unsafe link in private Python archive"));
    }
    let mut parts: Vec<OsString> = if hard {
        Vec::new()
    } else {
        member
            .parent()
            .unwrap_or(Path::new(""))
            .components()
            .map(|part| part.as_os_str().to_owned())
            .collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(io::Error::other(
                        "private Python link escapes its archive root",
                    ));
                }
            }
            _ => parts.push(part.into()),
        }
    }
    let resolved: PathBuf = parts.into_iter().collect();
    if !resolved.starts_with("python") || resolved == Path::new("python") {
        return Err(io::Error::other("private Python link escapes its runtime"));
    }
    Ok(resolved)
}

/// Runtime members may be owner-executable; receipts and source archives keep
/// using the stricter 0600-only private reader. Validate the opened descriptor,
/// never a separate path lookup, after the secure no-follow traversal.
fn open_private_runtime_member(path: &Path) -> io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let file = crate::secure_fs::open_regular_file_for_read(path).map_err(io::Error::other)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no preconditions and accepts no pointers.
        let owner = unsafe { libc::geteuid() };
        if metadata.uid() != owner
            || metadata.nlink() != 1
            || !matches!(metadata.mode() & 0o7777, 0o600 | 0o700)
        {
            return Err(io::Error::other(
                "runtime member is not an owner-only 0600/0700 regular file",
            ));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        // Retain the existing owner-only DACL and reparse-point validation.
        crate::secure_fs::open_private_file_for_read(path).map_err(io::Error::other)
    }
}

fn extract_archive(
    bytes: &[u8],
    staging: &Path,
    setup: &PythonRuntimeSetup,
) -> io::Result<PathBuf> {
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut names = BTreeSet::new();
    let mut links = BTreeMap::new();
    let mut expanded = 0_u64;
    for entry in archive.entries()? {
        check_cancelled(setup)?;
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let raw = String::from_utf8(entry.path_bytes().into_owned()).map_err(io::Error::other)?;
        let name = if kind.is_dir() {
            raw.trim_end_matches('/')
        } else {
            raw.as_str()
        };
        let relative = safe_relative(name)?;
        let unique_name = name.to_lowercase();
        if !relative.starts_with("python") || !names.insert(unique_name) || names.len() > MAX_FILES
        {
            return Err(io::Error::other(
                "unexpected root, duplicate or excessive paths in private Python archive",
            ));
        }
        let path = staging.join(&relative);
        if kind.is_dir() {
            crate::secure_fs::create_private_directory_all(&path).map_err(io::Error::other)?;
        } else if kind.is_file() {
            let size = entry.size();
            expanded = expanded
                .checked_add(size)
                .ok_or_else(|| io::Error::other("private Python size overflow"))?;
            if size > MAX_FILE_BYTES as u64 || expanded > MAX_EXPANDED_BYTES {
                return Err(io::Error::other(
                    "private Python archive exceeds its expanded-size limit",
                ));
            }
            crate::secure_fs::create_private_directory_all(
                path.parent()
                    .ok_or_else(|| io::Error::other("archive path has no parent"))?,
            )
            .map_err(io::Error::other)?;
            let mode = entry.header().mode()?;
            let mut file = crate::secure_fs::create_regular_file_for_append(&path)
                .map_err(io::Error::other)?;
            let mut buffer = [0_u8; 65536];
            loop {
                check_cancelled(setup)?;
                let count = entry.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                file.write_all(&buffer[..count])?;
            }
            file.sync_all()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(if mode & 0o111 != 0 {
                    0o700
                } else {
                    0o600
                }))?;
            }
            #[cfg(not(unix))]
            let _ = mode;
        } else if kind.is_symlink() || kind.is_hard_link() {
            let target = entry
                .link_name_bytes()
                .ok_or_else(|| io::Error::other("archive link has no target"))?;
            let target = std::str::from_utf8(&target).map_err(io::Error::other)?;
            links.insert(
                relative.clone(),
                link_target(&relative, target, kind.is_hard_link())?,
            );
        } else {
            return Err(io::Error::other("special file in private Python archive"));
        }
    }
    // Resolve forward links/cycles only after every regular member is safely
    // extracted. Copying materializes legitimate bin/python3 aliases without
    // requiring Windows symlink grants or allowing linked parent traversal.
    for (name, first_target) in &links {
        check_cancelled(setup)?;
        let mut target = first_target;
        let mut seen = BTreeSet::new();
        while let Some(next) = links.get(target) {
            if !seen.insert(target) || seen.len() > 32 {
                return Err(io::Error::other("cyclic private Python archive link"));
            }
            target = next;
        }
        let source = staging.join(target);
        let mut input = open_private_runtime_member(&source)?;
        let size = input.metadata()?.len();
        expanded = expanded
            .checked_add(size)
            .ok_or_else(|| io::Error::other("private Python size overflow"))?;
        if size > MAX_FILE_BYTES as u64 || expanded > MAX_EXPANDED_BYTES {
            return Err(io::Error::other(
                "materialized private Python links exceed the expanded-size limit",
            ));
        }
        let destination = staging.join(name);
        crate::secure_fs::create_private_directory_all(
            destination
                .parent()
                .ok_or_else(|| io::Error::other("link has no parent"))?,
        )
        .map_err(io::Error::other)?;
        let mut output = crate::secure_fs::create_regular_file_for_append(&destination)
            .map_err(io::Error::other)?;
        io::copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())?;
        output.sync_all()?;
    }
    Ok(staging.join("python"))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    source: String,
    archive_sha256: String,
    files: BTreeMap<String, String>,
}

fn inventory(
    root: &Path,
    setup: Option<&PythonRuntimeSetup>,
) -> io::Result<BTreeMap<String, String>> {
    let mut pending = vec![root.to_owned()];
    let mut files = BTreeMap::new();
    let mut entries = 0_usize;
    let mut expanded = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            if let Some(setup) = setup {
                check_cancelled(setup)?;
            }
            let entry = entry?;
            entries += 1;
            if entries > MAX_FILES {
                return Err(io::Error::other("excessive private Python runtime paths"));
            }
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(io::Error::other)?
                    .to_str()
                    .ok_or_else(|| io::Error::other("non-UTF8 private Python path"))?
                    .replace('\\', "/");
                if relative == RECEIPT || relative == SOURCE_ARCHIVE {
                    continue;
                }
                safe_relative(&relative)?;
                let file = open_private_runtime_member(&path)?;
                let mut bytes = Vec::new();
                file.take(MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_FILE_BYTES {
                    return Err(io::Error::other(
                        "private Python runtime member exceeds its size limit",
                    ));
                }
                expanded = expanded
                    .checked_add(bytes.len() as u64)
                    .ok_or_else(|| io::Error::other("private Python size overflow"))?;
                if expanded > MAX_EXPANDED_BYTES {
                    return Err(io::Error::other(
                        "private Python runtime exceeds its expanded-size limit",
                    ));
                }
                files.insert(relative, format!("{:x}", Sha256::digest(bytes)));
                if files.len() > MAX_FILES {
                    return Err(io::Error::other("excessive private Python runtime files"));
                }
            } else {
                return Err(io::Error::other(
                    "private Python runtime contains a link or special file",
                ));
            }
        }
    }
    Ok(files)
}

fn source_manifest(
    asset: Asset,
    bytes: &[u8],
    setup: Option<&PythonRuntimeSetup>,
) -> io::Result<Arc<BTreeMap<String, String>>> {
    verify_archive(asset, bytes)?;
    if let Some(manifest) = SOURCE_MANIFESTS
        .lock()
        .map_err(|_| io::Error::other("Python source-manifest cache unavailable"))?
        .get(asset.digest)
        .cloned()
    {
        return Ok(manifest);
    }
    let decoder = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(decoder);
    let mut files = BTreeMap::new();
    let mut links = BTreeMap::new();
    let mut names = BTreeSet::new();
    let mut expanded = 0_u64;
    for entry in archive.entries()? {
        if let Some(setup) = setup {
            check_cancelled(setup)?;
        }
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let raw = String::from_utf8(entry.path_bytes().into_owned()).map_err(io::Error::other)?;
        let name = if kind.is_dir() {
            raw.trim_end_matches('/')
        } else {
            raw.as_str()
        };
        let relative = safe_relative(name)?;
        if !relative.starts_with("python")
            || !names.insert(name.to_lowercase())
            || names.len() > MAX_FILES
        {
            return Err(io::Error::other(
                "invalid paths in verified private Python source",
            ));
        }
        if kind.is_dir() {
            continue;
        }
        let key = relative
            .strip_prefix("python")
            .map_err(io::Error::other)?
            .to_str()
            .ok_or_else(|| io::Error::other("non-UTF8 source path"))?
            .replace('\\', "/");
        if key == RECEIPT || key == SOURCE_ARCHIVE {
            return Err(io::Error::other("reserved host path in Python source"));
        }
        if kind.is_file() {
            let size = entry.size();
            expanded = expanded
                .checked_add(size)
                .ok_or_else(|| io::Error::other("Python source size overflow"))?;
            if size > MAX_FILE_BYTES as u64 || expanded > MAX_EXPANDED_BYTES {
                return Err(io::Error::other("excessive private Python source size"));
            }
            let mut digest = Sha256::new();
            let mut buffer = [0_u8; 65536];
            loop {
                if let Some(setup) = setup {
                    check_cancelled(setup)?;
                }
                let count = entry.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                digest.update(&buffer[..count]);
            }
            files.insert(key, format!("{:x}", digest.finalize()));
        } else if kind.is_symlink() || kind.is_hard_link() {
            let target = entry
                .link_name_bytes()
                .ok_or_else(|| io::Error::other("source link has no target"))?;
            let target = std::str::from_utf8(&target).map_err(io::Error::other)?;
            let target = link_target(&relative, target, kind.is_hard_link())?;
            let target = target
                .strip_prefix("python")
                .map_err(io::Error::other)?
                .to_str()
                .ok_or_else(|| io::Error::other("non-UTF8 source link"))?
                .replace('\\', "/");
            links.insert(key, target);
        } else {
            return Err(io::Error::other(
                "special file in verified private Python source",
            ));
        }
    }
    for (name, first_target) in &links {
        let mut target = first_target;
        let mut seen = BTreeSet::new();
        while let Some(next) = links.get(target) {
            if !seen.insert(target) || seen.len() > 32 {
                return Err(io::Error::other("cyclic Python source link"));
            }
            target = next;
        }
        let digest = files
            .get(target)
            .ok_or_else(|| io::Error::other("Python source link is not a regular source member"))?
            .clone();
        files.insert(name.clone(), digest);
    }
    let manifest = Arc::new(files);
    SOURCE_MANIFESTS
        .lock()
        .map_err(|_| io::Error::other("Python source-manifest cache unavailable"))?
        .insert(asset.digest.to_owned(), Arc::clone(&manifest));
    Ok(manifest)
}

async fn cached_runtime_async(
    root: &Path,
    asset: Asset,
    setup: Option<&PythonRuntimeSetup>,
) -> io::Result<Option<PathBuf>> {
    let root = root.to_owned();
    let setup = setup.cloned();
    tokio::task::spawn_blocking(move || cached_runtime(&root, asset, setup.as_ref()))
        .await
        .map_err(io::Error::other)?
}

fn cached_runtime(
    root: &Path,
    asset: Asset,
    setup: Option<&PythonRuntimeSetup>,
) -> io::Result<Option<PathBuf>> {
    let runtime = asset.directory(root);
    match std::fs::symlink_metadata(&runtime) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    // Validate provenance and the complete private closure BEFORE executing a
    // candidate. An interrupted/malformed directory is not a reusable runtime.
    let bytes =
        crate::secure_fs::read_private_file_bounded(&runtime.join(RECEIPT), MAX_RECEIPT_BYTES)
            .map_err(io::Error::other)?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if receipt.schema != 1
        || receipt.source != asset.url()
        || receipt.archive_sha256 != asset.digest
        || !receipt.files.contains_key(asset.interpreter())
    {
        return Err(io::Error::other(
            "private Python runtime provenance changed; existing files were not executed or replaced",
        ));
    }
    let source = crate::secure_fs::read_private_file_bounded(
        &runtime.join(SOURCE_ARCHIVE),
        MAX_ARCHIVE_BYTES,
    )
    .map_err(io::Error::other)?;
    let expected = source_manifest(asset, &source, setup)?;
    if receipt.files != *expected || inventory(&runtime, setup)? != *expected {
        return Err(io::Error::other(
            "private Python runtime integrity changed; existing files were not executed or replaced",
        ));
    }
    Ok(Some(runtime.join(asset.interpreter())))
}

fn candidate_paths(path: Option<&std::ffi::OsStr>, os: &str) -> Vec<(PathBuf, Vec<OsString>)> {
    let directories: Vec<_> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|directory| directory.is_absolute())
        .collect();
    let names: &[&str] = if os == "windows" {
        &["py.exe", "python3.exe", "python.exe"]
    } else {
        &["python3", "python3.12", "python3.11"]
    };
    let mut candidates = Vec::new();
    for name in names {
        for directory in &directories {
            let candidate = directory.join(name);
            // Do not launch Store aliases: on a genuinely fresh Windows home
            // they open the Store instead of functioning as an interpreter.
            if candidate
                .to_string_lossy()
                .to_ascii_lowercase()
                .replace('/', "\\")
                .contains("\\microsoft\\windowsapps\\")
            {
                continue;
            }
            if candidate.is_file() {
                candidates.push((
                    candidate,
                    if *name == "py.exe" {
                        vec!["-3".into()]
                    } else {
                        Vec::new()
                    },
                ));
            }
        }
    }
    if os == "macos" {
        for path in [
            "/opt/homebrew/bin/python3",
            "/usr/local/bin/python3",
            "/Library/Frameworks/Python.framework/Versions/Current/bin/python3",
        ] {
            if Path::new(path).is_file() {
                candidates.push((PathBuf::from(path), Vec::new()));
            }
        }
    }
    candidates
}

async fn probe_python(
    path: &Path,
    arguments: &[OsString],
    setup: Option<&PythonRuntimeSetup>,
) -> io::Result<bool> {
    if let Some(setup) = setup {
        check_cancelled(setup)?;
    }
    let mut command = Command::new(path);
    command
        .args(arguments)
        .args([
            "-I",
            "-B",
            "-c",
            "import sys, venv, ensurepip; sys.exit(0 if sys.version_info >= (3, 11) else 1)",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .env_clear()
        .envs(super::sanitized_subprocess_environment())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    let launch = super::WindowsProcessLaunch::extension(&mut command)?;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => return Ok(false),
    };
    #[cfg(unix)]
    let group = super::ProcessGroupGuard::extension(super::extension_process_group_id(&child));
    #[cfg(windows)]
    let group = launch.register(&child)?;
    let cancellation = async {
        match setup {
            Some(setup) => wait_cancelled(setup).await,
            None => std::future::pending::<()>().await,
        }
    };
    let result = tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(5), child.wait()) => Some(result),
        () = cancellation => None,
    };
    drop(group);
    match result {
        Some(Ok(Ok(status))) => Ok(status.success()),
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            if let Some(setup) = setup {
                check_cancelled(setup)?;
            }
            Ok(false)
        }
    }
}

pub(super) async fn resolve_script_python(
    config: Option<&PythonRuntimeConfig>,
) -> io::Result<(PathBuf, Vec<OsString>)> {
    if let Some(config) = config {
        if let Some(setup) = config.setup.as_ref() {
            check_cancelled(setup)?;
        }
        // An unsupported private catalogue must not break a working installed
        // interpreter on other targets (for example generic Python on musl).
        // Provisioning still refuses those platforms explicitly if needed.
        if let Ok(asset) = asset_for(std::env::consts::OS, std::env::consts::ARCH) {
            if let Some(binary) =
                cached_runtime_async(&config.root, asset, config.setup.as_ref()).await?
            {
                return Ok((binary, Vec::new()));
            }
        }
    }
    for (path, arguments) in
        candidate_paths(std::env::var_os("PATH").as_deref(), std::env::consts::OS)
    {
        if probe_python(
            &path,
            &arguments,
            config.and_then(|config| config.setup.as_ref()),
        )
        .await?
        {
            return Ok((path, arguments));
        }
    }
    let config = config.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound,
        "no compatible Python is available; open /extensions and choose Set up runtime (a verified private runtime is required, not a global pip/PATH change)"))?;
    Ok((provision_python_runtime(config).await?, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(cancelled: bool) -> PythonRuntimeSetup {
        PythonRuntimeSetup {
            cancelled: Arc::new(AtomicBool::new(cancelled)),
            progress: Arc::new(|_| {}),
        }
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().expect("disposable native fixture home")
    }

    fn archive(members: &[(&str, u8, &str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, kind, link, content) in members {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o700);
            header.set_size(content.len() as u64);
            header.set_entry_type(tar::EntryType::new(*kind));
            // Raw names intentionally bypass Builder's safe-path guard so
            // malformed/traversing upstream bytes test our boundary itself.
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.as_mut_bytes()[157..157 + link.len()].copy_from_slice(link.as_bytes());
            header.set_cksum();
            builder.append(&header, *content).expect("append fixture");
        }
        let tar = builder.into_inner().expect("finish tar");
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    fn fixture_asset(target: Asset) -> (Asset, Vec<u8>) {
        let name = format!("python/{}", target.interpreter());
        let source = archive(&[(&name, b'0', "", b"fixture - never executed")]);
        let digest = Box::leak(format!("{:x}", Sha256::digest(&source)).into_boxed_str());
        (
            Asset {
                target: target.target,
                bytes: source.len(),
                digest,
            },
            source,
        )
    }

    fn cached_fixture(root: &Path, target: Asset) -> (Asset, PathBuf) {
        let (asset, source) = fixture_asset(target);
        let runtime = asset.directory(root);
        let binary = runtime.join(asset.interpreter());
        crate::secure_fs::create_private_directory_all(binary.parent().unwrap()).unwrap();
        crate::secure_fs::write_private_atomic(&binary, b"fixture - never executed", 1024).unwrap();
        let receipt = Receipt {
            schema: 1,
            source: asset.url(),
            archive_sha256: asset.digest.into(),
            files: inventory(&runtime, None).unwrap(),
        };
        crate::secure_fs::write_private_atomic(
            &runtime.join(RECEIPT),
            &serde_json::to_vec(&receipt).unwrap(),
            MAX_RECEIPT_BYTES,
        )
        .unwrap();
        crate::secure_fs::write_private_atomic(
            &runtime.join(SOURCE_ARCHIVE),
            &source,
            MAX_ARCHIVE_BYTES,
        )
        .unwrap();
        (asset, binary)
    }

    #[test]
    fn catalogue_has_six_exact_version_digest_size_pins_and_no_latest() {
        for os in ["macos", "linux", "windows"] {
            for arch in ["aarch64", "x86_64"] {
                let asset = asset_for_environment(os, arch, "gnu").unwrap();
                assert_eq!(asset.digest.len(), 64);
                assert!(asset.digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(asset.bytes > 20_000_000 && asset.bytes < MAX_ARCHIVE_BYTES);
                assert!(asset
                    .url()
                    .contains("/20261003/cpython-3.12.15%2B20261003-"));
                assert!(!asset.url().contains("latest"));
            }
        }
        assert_eq!(ASSETS.len(), 6);
        assert!(asset_for_environment("linux", "x86_64", "musl").is_err());
        assert!(asset_for_environment("windows", "x86", "msvc").is_err());
        assert!(asset_for_environment("freebsd", "x86_64", "gnu").is_err());
    }

    #[test]
    fn download_must_match_both_pinned_size_and_digest_before_execution() {
        let valid = b"abc";
        let asset = Asset {
            target: "fixture",
            bytes: 3,
            digest: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        };
        assert!(verify_archive(asset, valid).is_ok());
        assert!(verify_archive(asset, b"abd").is_err());
        assert!(verify_archive(asset, b"abcd").is_err());
        assert!(verify_archive(asset, b"ab").is_err());
    }

    #[test]
    fn archive_and_receipt_paths_are_portable_and_cannot_escape() {
        for name in [
            "",
            "/python/a",
            "../a",
            "python/../a",
            "python/./a",
            "python//a",
            "C:/a",
            "python/a:b",
            "python\\a",
            "python/NUL",
            "python/con.txt",
            "python/a.",
            "python/a ",
        ] {
            assert!(safe_relative(name).is_err(), "{name}");
        }
        assert_eq!(
            safe_relative("python/lib/python3.12/LICENSE.txt").unwrap(),
            PathBuf::from("python/lib/python3.12/LICENSE.txt")
        );
    }

    #[test]
    fn legitimate_archive_links_are_materialized_without_os_link_permissions() {
        let home = home();
        let staging = home.path().canonicalize().unwrap().join("stage");
        crate::secure_fs::create_private_directory_all(&staging).unwrap();
        let bytes = archive(&[
            ("python/bin/python3", b'2', "python3.12", b""),
            ("python/bin/python3.12", b'0', "", b"verified fixture bytes"),
            ("python/bin/python", b'1', "python/bin/python3", b""),
            (
                "python/lib/python3.12/LICENSE.txt",
                b'0',
                "",
                b"upstream licence fixture",
            ),
        ]);
        let runtime = extract_archive(&bytes, &staging, &setup(false)).unwrap();
        assert_eq!(inventory(&runtime, None).unwrap().len(), 4);
        for name in ["bin/python3", "bin/python3.12", "bin/python"] {
            let path = runtime.join(name);
            assert!(!std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(std::fs::read(path).unwrap(), b"verified fixture bytes");
        }
        assert_eq!(
            std::fs::read(runtime.join("lib/python3.12/LICENSE.txt")).unwrap(),
            b"upstream licence fixture"
        );
    }

    #[test]
    fn traversal_unsafe_links_cycles_special_files_and_case_duplicates_are_refused() {
        for members in [
            vec![("python/../../escape", b'0', "", b"bad".as_slice())],
            vec![("outside/file", b'0', "", b"bad".as_slice())],
            vec![("python/a", b'2', "../../escape", b"".as_slice())],
            vec![("python/a", b'2', "/tmp/escape", b"".as_slice())],
            vec![("python/a", b'2', "C:/escape", b"".as_slice())],
            vec![
                ("python/a", b'2', "b", b"".as_slice()),
                ("python/b", b'2', "a", b"".as_slice()),
            ],
            vec![("python/a", b'6', "", b"".as_slice())],
            vec![
                ("python/a", b'0', "", b"a".as_slice()),
                ("python/a", b'0', "", b"b".as_slice()),
            ],
            vec![
                ("python/A", b'0', "", b"a".as_slice()),
                ("python/a", b'0', "", b"b".as_slice()),
            ],
        ] {
            let home = home();
            let staging = home.path().canonicalize().unwrap().join("stage");
            crate::secure_fs::create_private_directory_all(&staging).unwrap();
            assert!(extract_archive(&archive(&members), &staging, &setup(false)).is_err());
            assert!(!home.path().join("escape").exists());
        }
    }

    #[test]
    fn missing_python_and_store_alias_discovery_does_not_create_state() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let path = std::env::join_paths([Path::new("relative"), &root]).unwrap();
        assert!(candidate_paths(Some(&path), "linux").is_empty());
        assert!(candidate_paths(None, "windows").is_empty());
        let aliases = root.join("Microsoft/WindowsApps");
        std::fs::create_dir_all(&aliases).unwrap();
        std::fs::write(aliases.join("python.exe"), b"not an interpreter").unwrap();
        let path = std::env::join_paths([&aliases]).unwrap();
        assert!(candidate_paths(Some(&path), "windows").is_empty());
        assert!(!root.join("runtimes").exists());
    }

    #[tokio::test]
    async fn setup_requires_explicit_consent_and_precancellation_has_no_effects() {
        let home = home();
        let root = home.path().canonicalize().unwrap().join("runtimes");
        let mut config = PythonRuntimeConfig {
            root: root.clone(),
            setup: None,
        };
        assert_eq!(
            provision_python_runtime(&config).await.unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(!root.exists());
        config.setup = Some(setup(true));
        assert_eq!(
            provision_python_runtime(&config).await.unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert!(!root.exists());
    }

    #[test]
    fn interrupted_candidates_are_never_executed_or_removed_as_unknown_state() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let asset = ASSETS[0];
        let runtime = asset.directory(&root);
        let binary = runtime.join(asset.interpreter());
        crate::secure_fs::create_private_directory_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, b"unfinished fixture - never executed").unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
        assert_eq!(
            std::fs::read(binary).unwrap(),
            b"unfinished fixture - never executed"
        );
    }

    #[test]
    fn cache_rejects_modified_provenance_and_full_closure_and_preserves_existing_state() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let (asset, binary) = cached_fixture(&root, ASSETS[0]);
        assert_eq!(
            cached_runtime(&root, asset, None).unwrap(),
            Some(binary.clone())
        );
        let runtime = asset.directory(&root);
        let marker = runtime.join(RECEIPT);
        let original = std::fs::read(&marker).unwrap();
        let mut receipt: Receipt = serde_json::from_slice(&original).unwrap();
        receipt.source = "https://evil.invalid/python.tar.gz".into();
        crate::secure_fs::write_private_atomic(
            &marker,
            &serde_json::to_vec(&receipt).unwrap(),
            MAX_RECEIPT_BYTES,
        )
        .unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
        crate::secure_fs::write_private_atomic(&marker, &original, MAX_RECEIPT_BYTES).unwrap();
        std::fs::write(&binary, b"tampered").unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
        // Even a self-consistent forged receipt cannot authorize changed code:
        // the expected closure comes from the retained pinned source archive.
        let mut forged: Receipt = serde_json::from_slice(&original).unwrap();
        forged.files = inventory(&runtime, None).unwrap();
        crate::secure_fs::write_private_atomic(
            &marker,
            &serde_json::to_vec(&forged).unwrap(),
            MAX_RECEIPT_BYTES,
        )
        .unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
        crate::secure_fs::write_private_atomic(&marker, &original, MAX_RECEIPT_BYTES).unwrap();
        std::fs::write(&binary, b"fixture - never executed").unwrap();
        crate::secure_fs::write_private_atomic(&runtime.join("injected.py"), b"untracked", 1024)
            .unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
        std::fs::remove_file(runtime.join("injected.py")).unwrap();
        assert_eq!(cached_runtime(&root, asset, None).unwrap(), Some(binary));
    }

    #[test]
    fn a_receipt_with_catalogued_digest_but_no_authentic_source_is_not_authority() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let (_, binary) = cached_fixture(&root, ASSETS[0]);
        let runtime = ASSETS[0].directory(&root);
        let marker = runtime.join(RECEIPT);
        let mut receipt: Receipt =
            serde_json::from_slice(&std::fs::read(&marker).unwrap()).unwrap();
        receipt.archive_sha256 = ASSETS[0].digest.into();
        crate::secure_fs::write_private_atomic(
            &marker,
            &serde_json::to_vec(&receipt).unwrap(),
            MAX_RECEIPT_BYTES,
        )
        .unwrap();
        assert!(cached_runtime(&root, ASSETS[0], None).is_err());
        assert_eq!(std::fs::read(binary).unwrap(), b"fixture - never executed");
    }

    #[tokio::test]
    async fn cached_retry_is_idempotent_and_reports_python_not_desktop_readiness() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let (asset, binary) = cached_fixture(&root, ASSETS[0]);
        let lines = Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = Arc::clone(&lines);
        let config = PythonRuntimeConfig {
            root: root.clone(),
            setup: Some(PythonRuntimeSetup {
                cancelled: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(move |line| output.lock().unwrap().push(line.to_owned())),
            }),
        };
        for _ in 0..2 {
            assert_eq!(
                cached_runtime_async(&root, asset, config.setup.as_ref())
                    .await
                    .unwrap(),
                Some(binary.clone())
            );
            progress(
                config.setup.as_ref().unwrap(),
                "Verified private Python is already installed.",
            )
            .unwrap();
        }
        assert_eq!(
            lines.lock().unwrap().as_slice(),
            [
                "Verified private Python is already installed.",
                "Verified private Python is already installed."
            ]
        );
        assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("python-setup")
        }));
    }

    #[tokio::test]
    async fn dropping_setup_signals_worker_and_holds_lock_until_its_owned_stage_exits() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let staging = private_staging(&root).unwrap();
        let stage_path = staging.path().to_owned();
        let lock = crate::secure_fs::open_private_directory_for_lock(&root).unwrap();
        lock.try_lock_exclusive().unwrap();
        let other = crate::secure_fs::open_private_directory_for_lock(&root).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let released = Arc::clone(&barrier);
        let setup = setup(false);
        let (asset, bytes) = fixture_asset(ASSETS[0]);
        let (entered, started) = tokio::sync::oneshot::channel();
        let cancelled = Arc::clone(&setup.cancelled);
        let owner = tokio::spawn(prepare_candidate(
            bytes,
            staging,
            lock,
            asset,
            setup,
            move |bytes, path, setup| {
                entered.send(()).unwrap();
                released.wait();
                extract_archive(bytes, path, setup)
            },
        ));
        started.await.unwrap();
        assert!(other.try_lock_exclusive().is_err());
        owner.abort();
        assert!(owner.await.unwrap_err().is_cancelled());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(other.try_lock_exclusive().is_err());
        barrier.wait();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !stage_path.exists() && other.try_lock_exclusive().is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(!asset.directory(&root).exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn redirected_runtime_root_is_rejected_before_network_or_execution() {
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let outside = root.join("outside");
        crate::secure_fs::create_private_directory_all(&outside).unwrap();
        let link = root.join("redirected");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let config = PythonRuntimeConfig {
            root: link,
            setup: Some(setup(false)),
        };
        assert!(provision_python_runtime(&config).await.is_err());
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn world_readable_runtime_files_are_not_reusable_private_state() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = home();
        let root = home.path().canonicalize().unwrap();
        let (asset, binary) = cached_fixture(&root, ASSETS[0]);
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(cached_runtime(&root, asset, None).is_err());
    }
}
