#![allow(missing_docs)]

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use clap::Subcommand;
use fs2::FileExt;
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

pub(super) const RELEASE_REPOSITORY: &str = "https://github.com/skaft-software/octet";
const DOWNLOAD_MAX_ATTEMPTS: usize = 3;
const DOWNLOAD_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const DOWNLOAD_MAX_BACKOFF: Duration = Duration::from_secs(1);
const DOWNLOAD_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const DOWNLOAD_READ_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_CHECKSUM_BYTES: usize = 1024 * 1024;
pub(super) const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Debug, Subcommand)]
pub enum ExtensionCommand {
    /// Install an official extension bundle or a local release archive.
    Install {
        /// Official extension bundle name.
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
    /// List installed extension bundles.
    List,
    /// Install the matching official release or a local replacement atomically.
    Update {
        /// Official extension bundle name.
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
    /// Remove an installed bundle without deleting external data.
    Remove { name: String },
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
                let manifest = crate::extension_bundle::install_local(&root, &path, false)?;
                print_bundle_installed("Installed", &manifest);
            } else {
                let name = name.expect("clap requires a name unless --path is present");
                let manifest =
                    crate::extension_bundle::install_official(&root, &name, false).await?;
                print_bundle_installed("Installed", &manifest);
            }
            Ok(())
        }
        ExtensionCommand::List => list_installed_bundles(&extensions_root()?),
        ExtensionCommand::Update { name, path } => {
            let root = extensions_root()?;
            if let Some(path) = path {
                let path = resolve_local_archive_path(&path)?;
                let manifest = crate::extension_bundle::install_local(&root, &path, true)?;
                print_bundle_installed("Updated", &manifest);
            } else {
                let name = name.expect("clap requires a name unless --path is present");
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
            Ok(())
        }
        ExtensionCommand::Remove { name } => {
            let root = extensions_root()?;
            crate::extension_bundle::remove_installed(&root, &name)?;
            crate::output::stdout_line(format!(
                "Removed {name}. Configuration and other data outside the bundle were preserved."
            ));
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

pub(super) fn extensions_root() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir()
        .filter(|path| path.is_absolute())
        .ok_or_else(|| anyhow::anyhow!("cannot manage extensions: user home is unavailable"))?;
    let home = home
        .canonicalize()
        .with_context(|| format!("cannot resolve user home {}", home.display()))?;
    Ok(home.join(".octet").join("extensions"))
}

fn resolve_local_archive_path(path: &Path) -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot resolve the current directory")?;
    let unresolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    unresolved
        .canonicalize()
        .with_context(|| format!("cannot resolve package archive {}", path.display()))
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

fn list_installed_bundles(root: &Path) -> anyhow::Result<()> {
    let mut rows = Vec::<(String, String, String, String, String, String)>::new();
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
        crate::output::stdout_line("No extension bundles installed.");
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
mod tests;
