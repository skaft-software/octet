//! Bounded startup release notice, explicit update check, and self-update.
//!
//! The check fetches the latest GitHub release with a short timeout, a hard
//! response-size limit, and no redirects. The update delegates to the channel
//! that installed the running binary: the version-pinned installer for
//! installer installs, a pinned `cargo install` for Cargo installs, or an exact
//! npm install for a validated global npm package. The update itself never
//! replaces the running image: the channel swaps the installed files under the
//! running process, and the user restarts octet — or an interactive `/reload`
//! safely re-execs into the new on-disk generation at the same PID with the
//! exact session resumed (see `crate::reexec`). The install/swap boundary and
//! the re-exec boundary stay separate: this module owns installation, and
//! `crate::reexec` owns deciding when the running process may enter the newly
//! installed files.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use anyhow::Context;

mod progress;

const REPOSITORY: &str = "https://github.com/skaft-software/octet";
const RELEASE_DOWNLOAD_BASE: &str = "https://github.com/skaft-software/octet/releases/download";
const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/skaft-software/octet/releases/latest";
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_RELEASE_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Debug, serde::Deserialize)]
struct LatestRelease {
    tag_name: String,
    html_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateStatus {
    Current {
        version: semver::Version,
    },
    Available {
        current: semver::Version,
        latest: semver::Version,
        url: String,
    },
}

impl std::fmt::Display for UpdateStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Current { version } => write!(formatter, "octet {version} is up to date."),
            Self::Available {
                current,
                latest,
                url,
            } => write!(
                formatter,
                "octet {latest} is available (current: {current}).\n{url}"
            ),
        }
    }
}

pub(crate) async fn check() -> anyhow::Result<UpdateStatus> {
    check_url(LATEST_RELEASE_URL, env!("CARGO_PKG_VERSION")).await
}

/// One best-effort, unauthenticated HTTPS check for a newer stable release.
///
/// Spawn this future once per interactive startup, never await it before showing
/// the UI, and skip it entirely when the caller's resolved `offline` setting is
/// true. The caller owns task cancellation on exit and notice presentation.
/// This performs no installation, provider access, retries, polling, or writes.
/// Network/metadata failures quietly return `None`. Only parsed major/minor/patch
/// numbers cross into the UI; release URLs and other remote text never do.
/// A single five-second deadline bounds the whole check, with a 64-KiB body
/// limit and no redirects. Proxy auto-discovery is disabled to avoid acquiring
/// environment proxy credentials. No persisted cache is created. OS DNS may
/// outlive cancellation or the deadline, but runs at most one job on a detached
/// standard thread, never Tokio's shutdown-blocking pool. Its late result cannot
/// start HTTP after the check is dropped, and process exit does not wait for it.
pub(crate) async fn startup_available_update() -> Option<semver::Version> {
    startup_available_update_url(LATEST_RELEASE_URL, env!("CARGO_PKG_VERSION"), CHECK_TIMEOUT).await
}

// Production always uses the fixed HTTPS endpoint above. Injection here allows
// synthetic loopback tests without contacting GitHub or using credentials.
async fn startup_available_update_url(
    url: &str,
    current: &str,
    timeout: Duration,
) -> Option<semver::Version> {
    use std::net::ToSocketAddrs;

    let resolver = StartupResolver::new("api.github.com", || {
        ("api.github.com", 0)
            .to_socket_addrs()
            .map(|addrs| Box::new(addrs) as reqwest::dns::Addrs)
    });
    startup_available_update_with_resolver(url, current, timeout, resolver).await
}

// Only optional background clients use this resolver: the startup update check
// and the models.dev metadata refresh. Explicit update and provider clients
// retain their existing DNS behavior. FnOnce enforces a single lookup per
// client, including any unexpected repeated resolver calls.
pub(crate) struct StartupResolver<F> {
    hostname: &'static str,
    lookup: std::sync::Mutex<Option<F>>,
}

impl<F> StartupResolver<F> {
    pub(crate) fn new(hostname: &'static str, lookup: F) -> Self {
        Self {
            hostname,
            lookup: std::sync::Mutex::new(Some(lookup)),
        }
    }
}

impl<F> reqwest::dns::Resolve for StartupResolver<F>
where
    F: FnOnce() -> std::io::Result<reqwest::dns::Addrs> + Send + 'static,
{
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let lookup = if name.as_str() == self.hostname {
            self.lookup.lock().unwrap().take()
        } else {
            None
        };
        Box::pin(async move {
            let lookup = lookup.ok_or_else(|| {
                std::io::Error::other("startup DNS permits only one lookup of the release host")
            })?;
            let (send, receive) = tokio::sync::oneshot::channel();
            // getaddrinfo cannot be cancelled. Unlike spawn_blocking, a detached
            // std thread does not hold runtime/process shutdown open. It owns
            // only DNS work and the sender, never the HTTP client or a runtime.
            let worker = std::thread::Builder::new()
                .name("octet-startup-dns".into())
                .spawn(move || {
                    let _ = send.send(lookup());
                })?;
            drop(worker);
            // Dropping the request drops this receiver. Late DNS completion
            // then discards its addresses instead of continuing to connect.
            Ok(receive.await??)
        })
    }
}

async fn startup_available_update_with_resolver(
    url: &str,
    current: &str,
    timeout: Duration,
    resolver: impl reqwest::dns::Resolve + 'static,
) -> Option<semver::Version> {
    tokio::time::timeout(timeout, async {
        let current = semver::Version::parse(current).ok()?;
        let client = reqwest::Client::builder()
            // Optional startup traffic must not acquire environment proxy credentials.
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(resolver))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .ok()?;
        let response = client
            .get(url)
            .header(reqwest::header::USER_AGENT, format!("octet/{current}"))
            .send()
            .await
            .ok()?;
        // error_for_status alone accepts redirects with a plausible JSON body.
        if !response.status().is_success() {
            return None;
        }
        let body = read_release_body(response).await.ok()?;
        newer_stable_release(&body, &current)
    })
    .await
    .ok()?
}

fn newer_stable_release(body: &[u8], current: &semver::Version) -> Option<semver::Version> {
    #[derive(serde::Deserialize)]
    struct StableRelease {
        tag_name: String,
        draft: bool,
        prerelease: bool,
    }

    let release: StableRelease = serde_json::from_slice(body).ok()?;
    if release.draft || release.prerelease {
        return None;
    }
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let latest = semver::Version::parse(tag).ok()?;
    if !latest.pre.is_empty() || !latest.cmp_precedence(current).is_gt() {
        return None;
    }
    // Build metadata is neither release precedence nor trusted display text.
    Some(semver::Version::new(
        latest.major,
        latest.minor,
        latest.patch,
    ))
}

async fn check_url(url: &str, current: &str) -> anyhow::Result<UpdateStatus> {
    let current = semver::Version::parse(current)?;
    let client = reqwest::Client::builder()
        .connect_timeout(CHECK_TIMEOUT)
        .timeout(CHECK_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let response = client
        .get(url)
        .header(reqwest::header::USER_AGENT, format!("octet/{current}"))
        .send()
        .await?
        .error_for_status()?;
    let body = read_release_body(response).await?;
    let release: LatestRelease = serde_json::from_slice(&body)?;
    let latest = semver::Version::parse(release.tag_name.trim().trim_start_matches('v'))?;
    if latest > current {
        Ok(UpdateStatus::Available {
            current,
            latest,
            url: release.html_url.unwrap_or_else(|| {
                format!(
                    "https://github.com/skaft-software/octet/releases/tag/{}",
                    release.tag_name
                )
            }),
        })
    } else {
        Ok(UpdateStatus::Current { version: current })
    }
}

async fn read_release_body(mut response: reqwest::Response) -> anyhow::Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RELEASE_RESPONSE_BYTES as u64)
    {
        anyhow::bail!("release metadata exceeds the {MAX_RELEASE_RESPONSE_BYTES}-byte limit");
    }
    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or(0)
            .min(MAX_RELEASE_RESPONSE_BYTES as u64) as usize,
    );
    while let Some(chunk) = response.chunk().await? {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_RELEASE_RESPONSE_BYTES)
        {
            anyhow::bail!("release metadata exceeds the {MAX_RELEASE_RESPONSE_BYTES}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// How the running octet binary was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallMethod {
    /// The version-pinned installer placed the binaries under a prefix whose
    /// documentation tree is still present.
    Installer { bin_dir: PathBuf },
    /// `cargo install` from the octet git repository.
    Cargo,
    /// A validated global npm installation of the public launcher and a
    /// platform package.
    Npm { package_root: PathBuf },
    /// An npm package was found, but it is local or npx rather than a
    /// corroborated global installation. It must never be mutated implicitly.
    NpmLocal { package_root: PathBuf },
    /// Workspace development build running from `target/debug` or
    /// `target/release`.
    Local,
    /// The executable location does not match a supported install layout.
    Unknown,
}

/// The update command for the channel that installed the running binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    /// Re-run the version-pinned installer for the target release.
    Installer { version: semver::Version },
    /// Reinstall the target release from the octet git repository.
    Cargo { version: semver::Version },
    /// Install the exact public npm package version globally without running
    /// package scripts or audit/funding network operations.
    Npm { version: semver::Version },
}

impl UpdateAction {
    /// The action for a detected install method, when that method has an
    /// automated update path.
    pub(crate) fn for_method(
        method: &InstallMethod,
        version: &semver::Version,
    ) -> Option<UpdateAction> {
        match method {
            InstallMethod::Installer { .. } => Some(Self::Installer {
                version: version.clone(),
            }),
            InstallMethod::Cargo => Some(Self::Cargo {
                version: version.clone(),
            }),
            InstallMethod::Npm { .. } => Some(Self::Npm {
                version: version.clone(),
            }),
            InstallMethod::Local | InstallMethod::NpmLocal { .. } | InstallMethod::Unknown => None,
        }
    }

    /// The exact process invocation that updates this channel.
    pub(crate) fn command_args(&self) -> (OsString, Vec<OsString>) {
        match self {
            Self::Installer { version } => (
                OsString::from("sh"),
                vec![
                    OsString::from("-c"),
                    OsString::from(install_script(&version.to_string())),
                ],
            ),
            Self::Cargo { version } => (
                OsString::from("cargo"),
                vec![
                    OsString::from("install"),
                    OsString::from("--locked"),
                    OsString::from("--git"),
                    OsString::from(REPOSITORY),
                    OsString::from("--tag"),
                    OsString::from(format!("v{version}")),
                    OsString::from("--bins"),
                    OsString::from("octet-coding-agent"),
                ],
            ),
            Self::Npm { version } => (
                OsString::from("npm"),
                npm_command_args(&version.to_string()),
            ),
        }
    }

    /// The user-runnable form of the update command, matching the commands
    /// documented in the README.
    pub(crate) fn command_str(&self) -> String {
        match self {
            Self::Installer { version } => install_script(&version.to_string()),
            Self::Cargo { version } => format!(
                "cargo install --locked --git {REPOSITORY} --tag v{version} --bins octet-coding-agent"
            ),
            Self::Npm { version } => npm_command_str(&version.to_string()),
        }
    }
}

const NPM_LAUNCHER: &str = "@skaft/octet";
const NPM_PLATFORM_PACKAGES: [&str; 3] = [
    "@skaft/octet-darwin-arm64",
    "@skaft/octet-darwin-x64",
    "@skaft/octet-linux-x64-gnu",
];

fn npm_command_args(version: &str) -> Vec<OsString> {
    vec![
        OsString::from("install"),
        OsString::from("--global"),
        OsString::from("--ignore-scripts"),
        OsString::from("--no-audit"),
        OsString::from("--no-fund"),
        OsString::from(format!("{NPM_LAUNCHER}@{version}")),
    ]
}

fn npm_command_str(version: &str) -> String {
    format!("npm install --global --ignore-scripts --no-audit --no-fund {NPM_LAUNCHER}@{version}")
}

/// The version-pinned installer invocation for a release, as documented in
/// the README.
fn install_script(version: &str) -> String {
    format!(
        "curl --proto '=https' --tlsv1.2 -LsSf {RELEASE_DOWNLOAD_BASE}/v{version}/install-octet.sh | sh"
    )
}

/// Environment inputs for install-method detection, separated from process
/// state so detection is deterministic in tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct InstallEnvironment {
    /// The user home directory.
    home: Option<PathBuf>,
    /// Override for the Cargo home directory.
    cargo_home: Option<PathBuf>,
    /// Override for the installer binary directory.
    install_dir: Option<PathBuf>,
    /// Override for the installer data directory.
    data_dir: Option<PathBuf>,
    /// The exact root returned by `npm root -g`; injected in tests so layout
    /// detection itself never needs to spawn a process.
    npm_root: Option<PathBuf>,
}

impl InstallEnvironment {
    pub(crate) fn current() -> Self {
        Self {
            home: dirs::home_dir().filter(|path| path.is_absolute()),
            cargo_home: std::env::var_os("CARGO_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            install_dir: std::env::var_os("OCTET_INSTALL_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            data_dir: std::env::var_os("OCTET_DATA_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute()),
            npm_root: None,
        }
    }
}

/// Detects how the executable at `exe` was installed.
pub(crate) fn detect_install_method(exe: &Path) -> InstallMethod {
    let mut environment = InstallEnvironment::current();
    // Preserve the existing workspace, installer, and Cargo paths without
    // spawning npm. Only a structurally valid npm layout reaches the global
    // root probe, which is the trust boundary for automatic npm updates.
    if is_workspace_build(exe) {
        return InstallMethod::Local;
    }
    let Some(bin_dir) = exe.parent().map(Path::to_path_buf) else {
        return InstallMethod::Unknown;
    };
    if installer_docs_present(&bin_dir, &environment)
        && installer_target_matches(&bin_dir, &environment)
    {
        return InstallMethod::Installer { bin_dir };
    }
    if is_cargo_bin_dir(&bin_dir, &environment) {
        return InstallMethod::Cargo;
    }
    if validated_npm_local_package(&bin_dir).is_some() {
        environment.npm_root = npm_global_root();
    }
    detect_install_method_in(exe, &environment)
}

pub(crate) fn detect_install_method_in(exe: &Path, env: &InstallEnvironment) -> InstallMethod {
    if is_workspace_build(exe) {
        return InstallMethod::Local;
    }
    let Some(bin_dir) = exe.parent().map(Path::to_path_buf) else {
        return InstallMethod::Unknown;
    };
    if installer_docs_present(&bin_dir, env) && installer_target_matches(&bin_dir, env) {
        return InstallMethod::Installer { bin_dir };
    }
    if is_cargo_bin_dir(&bin_dir, env) {
        return InstallMethod::Cargo;
    }
    if let Some(package_root) = validated_npm_global_package(&bin_dir, env) {
        return InstallMethod::Npm { package_root };
    }
    if let Some(package_root) = validated_npm_local_package(&bin_dir) {
        return InstallMethod::NpmLocal { package_root };
    }
    InstallMethod::Unknown
}

/// Ask npm for the global package root without a shell. Any malformed or
/// unsuccessful response is deliberately treated as unavailable.
fn npm_global_root() -> Option<PathBuf> {
    let output = Command::new("npm").args(["root", "-g"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    let mut lines = stdout.lines();
    let root = lines.next()?.trim();
    if root.is_empty() || lines.next().is_some() {
        return None;
    }
    let root = PathBuf::from(root);
    (root.is_absolute() && real_directory(&root)).then_some(root)
}

fn expected_npm_platform() -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "@skaft/octet-darwin-arm64",
            "aarch64-apple-darwin",
            "darwin",
            "arm64",
        ))
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some((
            "@skaft/octet-darwin-x64",
            "x86_64-apple-darwin",
            "darwin",
            "x64",
        ))
    } else if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) {
        Some((
            "@skaft/octet-linux-x64-gnu",
            "x86_64-unknown-linux-gnu",
            "linux",
            "x64",
        ))
    } else {
        None
    }
}

/// Returns true only when every component of `path` is an ordinary directory.
/// This keeps a package rooted in a symlinked parent from being mistaken for a
/// package installed at the path the user actually invoked.
fn real_directory(path: &Path) -> bool {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return false;
        }
        current.push(component);
        let Ok(metadata) = std::fs::symlink_metadata(&current) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
    }
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn regular_path(path: &Path) -> bool {
    path.parent().is_some_and(real_directory)
        && std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

/// Validate a package resource tree without following a link or accepting a
/// special file. The tarball verifier applies the same rule before packaging;
/// keeping it here makes update-channel detection fail closed after install.
fn safe_directory_tree(path: &Path) -> bool {
    if !real_directory(path) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let entry_path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&entry_path) else {
            return false;
        };
        if metadata.file_type().is_symlink() {
            return false;
        }
        if metadata.is_dir() {
            if !safe_directory_tree(&entry_path) {
                return false;
            }
        } else if !metadata.is_file() {
            return false;
        }
    }
    true
}

#[cfg(unix)]
fn executable_path(path: &Path) -> bool {
    regular_path(path)
        && std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable_path(_path: &Path) -> bool {
    false
}

fn json_string<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key)?.as_str()
}

fn json_string_array(value: &serde_json::Value, key: &str, expected: &[&str]) -> bool {
    let Some(array) = value.get(key).and_then(|value| value.as_array()) else {
        return false;
    };
    array.len() == expected.len()
        && array
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual.as_str() == Some(*expected))
}

fn json_string_map(value: &serde_json::Value, key: &str, expected: &[(&str, &str)]) -> bool {
    let Some(object) = value.get(key).and_then(|value| value.as_object()) else {
        return false;
    };
    object.len() == expected.len()
        && expected.iter().all(|(key, expected)| {
            object.get(*key).and_then(|value| value.as_str()) == Some(*expected)
        })
}

fn manifest_keys_exact(value: &serde_json::Value, expected: &[&str]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
}

fn npm_manifest(path: &Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn valid_npm_platform_root(
    root: &Path,
    package_name: &str,
    target: &str,
    operating_system: &str,
    cpu: &str,
) -> bool {
    let version = env!("CARGO_PKG_VERSION");
    let description = format!("Native octet runtime for {target}");
    if !safe_directory_tree(root)
        || !real_directory(&root.join("bin"))
        || !regular_path(&root.join("package.json"))
        || !regular_path(&root.join("README.md"))
        || !regular_path(&root.join("LICENSE"))
        || !executable_path(&root.join("bin/octet"))
        || !executable_path(&root.join("bin/octet-host"))
        || !real_directory(&root.join("share/octet"))
        || !regular_path(&root.join("share/octet/.octet-version"))
        || !regular_path(&root.join("share/octet/README.md"))
        || !real_directory(&root.join("share/octet/docs"))
        || !real_directory(&root.join("share/octet/examples"))
        || !real_directory(&root.join("share/octet/sdk"))
    {
        return false;
    }
    let Some(manifest) = npm_manifest(&root.join("package.json")) else {
        return false;
    };
    manifest_keys_exact(
        &manifest,
        &[
            "name",
            "version",
            "description",
            "license",
            "repository",
            "os",
            "cpu",
            "files",
        ],
    ) && json_string(&manifest, "name") == Some(package_name)
        && json_string(&manifest, "version") == Some(version)
        && json_string(&manifest, "description") == Some(description.as_str())
        && json_string(&manifest, "license") == Some("MIT")
        && json_string(&manifest, "repository") == Some(REPOSITORY)
        && json_string_array(&manifest, "os", &[operating_system])
        && json_string_array(&manifest, "cpu", &[cpu])
        && json_string_array(
            &manifest,
            "files",
            &["README.md", "LICENSE", "bin/", "share/octet/"],
        )
        && std::fs::read_to_string(root.join("share/octet/.octet-version"))
            .map(|contents| contents == format!("{version}\n"))
            .unwrap_or(false)
}

fn valid_npm_launcher_root(root: &Path, platform_name: &str) -> bool {
    let version = env!("CARGO_PKG_VERSION");
    if !safe_directory_tree(root)
        || !real_directory(&root.join("bin"))
        || !real_directory(&root.join("lib"))
        || !regular_path(&root.join("package.json"))
        || !regular_path(&root.join("README.md"))
        || !regular_path(&root.join("LICENSE"))
        || !executable_path(&root.join("bin/octet"))
        || !executable_path(&root.join("bin/octet-host"))
        || !executable_path(&root.join("lib/launch.sh"))
    {
        return false;
    }
    let Some(manifest) = npm_manifest(&root.join("package.json")) else {
        return false;
    };
    manifest_keys_exact(
        &manifest,
        &[
            "name",
            "version",
            "description",
            "license",
            "repository",
            "files",
            "bin",
            "optionalDependencies",
        ],
    ) && json_string(&manifest, "name") == Some(NPM_LAUNCHER)
        && json_string(&manifest, "version") == Some(version)
        && json_string(&manifest, "description") == Some("Native octet coding agent launcher")
        && json_string(&manifest, "license") == Some("MIT")
        && json_string(&manifest, "repository") == Some(REPOSITORY)
        && json_string_array(
            &manifest,
            "files",
            &["README.md", "LICENSE", "bin/", "lib/"],
        )
        && json_string_map(
            &manifest,
            "bin",
            &[("octet", "bin/octet"), ("octet-host", "bin/octet-host")],
        )
        && json_string_map(
            &manifest,
            "optionalDependencies",
            &[
                (NPM_PLATFORM_PACKAGES[0], version),
                (NPM_PLATFORM_PACKAGES[1], version),
                (NPM_PLATFORM_PACKAGES[2], version),
            ],
        )
        && NPM_PLATFORM_PACKAGES
            .iter()
            .any(|package| package.rsplit('/').next() == Some(platform_name))
}

struct NpmLayout {
    launcher_root: PathBuf,
    platform_root: PathBuf,
}

fn npm_layout(bin_dir: &Path) -> Option<NpmLayout> {
    if bin_dir.file_name()?.to_str()? != "bin" {
        return None;
    }
    let platform_root = bin_dir.parent()?.to_path_buf();
    let platform_name = platform_root.file_name()?.to_str()?;
    let scope_directory = platform_root.parent()?;
    if scope_directory.file_name()?.to_str()? != "@skaft" || !real_directory(scope_directory) {
        return None;
    }
    let node_modules = scope_directory.parent()?;
    if node_modules.file_name()?.to_str()? != "node_modules" || !real_directory(node_modules) {
        return None;
    }
    if !real_directory(bin_dir) || !real_directory(&platform_root) {
        return None;
    }

    let node_modules_parent = node_modules.parent()?;
    if !real_directory(node_modules_parent) {
        return None;
    }
    let (launcher_root, expected_platform) = if node_modules_parent
        .file_name()
        .and_then(|name| name.to_str())
        == Some("octet")
    {
        let launcher_root = node_modules_parent.to_path_buf();
        let nested_node_modules = launcher_root.join("node_modules");
        if !real_directory(&nested_node_modules)
            || !same_directory(node_modules, &nested_node_modules)
        {
            return None;
        }
        (
            launcher_root.clone(),
            launcher_root
                .join("node_modules/@skaft")
                .join(platform_name),
        )
    } else {
        (
            node_modules.join("@skaft/octet"),
            node_modules.join("@skaft").join(platform_name),
        )
    };
    if !real_directory(&launcher_root) || !same_directory(&platform_root, &expected_platform) {
        return None;
    }
    Some(NpmLayout {
        launcher_root,
        platform_root,
    })
}

fn validated_npm_layout(bin_dir: &Path) -> Option<PathBuf> {
    let (platform_package, target, operating_system, cpu) = expected_npm_platform()?;
    let platform_name = platform_package.rsplit('/').next()?;
    let layout = npm_layout(bin_dir)?;
    if layout
        .platform_root
        .file_name()
        .and_then(|name| name.to_str())
        != Some(platform_name)
    {
        return None;
    }
    if !valid_npm_launcher_root(&layout.launcher_root, platform_name)
        || !valid_npm_platform_root(
            &layout.platform_root,
            platform_package,
            target,
            operating_system,
            cpu,
        )
    {
        return None;
    }
    Some(layout.platform_root)
}

fn validated_npm_global_package(bin_dir: &Path, env: &InstallEnvironment) -> Option<PathBuf> {
    let package_root = validated_npm_layout(bin_dir)?;
    let npm_root = env.npm_root.as_ref()?;
    if npm_root.file_name()?.to_str()? != "node_modules" || !real_directory(npm_root) {
        return None;
    }
    let layout = npm_layout(bin_dir)?;
    let global_public = npm_root.join("@skaft/octet");
    if !real_directory(&global_public) || !same_directory(&layout.launcher_root, &global_public) {
        return None;
    }
    Some(package_root)
}

fn validated_npm_local_package(bin_dir: &Path) -> Option<PathBuf> {
    validated_npm_layout(bin_dir)
}

/// The install method of the running binary.
pub(crate) fn current_install_method() -> InstallMethod {
    std::env::current_exe()
        .ok()
        .map(|exe| detect_install_method(&exe))
        .unwrap_or(InstallMethod::Unknown)
}

fn is_workspace_build(exe: &Path) -> bool {
    let components: Vec<_> = exe.iter().collect();
    components.windows(2).any(|pair| {
        pair[0].to_str() == Some("target") && matches!(pair[1].to_str(), Some("debug" | "release"))
    })
}

fn is_cargo_bin_dir(bin_dir: &Path, env: &InstallEnvironment) -> bool {
    let cargo_home = env
        .cargo_home
        .clone()
        .or_else(|| env.home.clone().map(|home| home.join(".cargo")));
    cargo_home
        .as_ref()
        .is_some_and(|home| same_directory(bin_dir, &home.join("bin")))
}

fn installer_docs_present(bin_dir: &Path, env: &InstallEnvironment) -> bool {
    let docs = env.data_dir.clone().or_else(|| {
        bin_dir
            .parent()
            .map(|prefix| prefix.join("share").join("octet"))
    });
    docs.as_ref().is_some_and(|path| path.is_dir())
}

/// The installer only updates this binary when the directory it would write
/// to is the directory this binary lives in.
fn installer_target_matches(bin_dir: &Path, env: &InstallEnvironment) -> bool {
    let target = env
        .install_dir
        .clone()
        .or_else(|| env.home.clone().map(|home| home.join(".local").join("bin")));
    target
        .as_ref()
        .is_some_and(|target| same_directory(bin_dir, target))
}

/// Treats two directories as equal via canonicalized paths when both exist,
/// falling back to raw comparison when either side cannot be canonicalized
/// (a missing directory must not equal another missing directory).
fn same_directory(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(canonical_left), Ok(canonical_right)) => canonical_left == canonical_right,
        _ => left == right,
    }
}

/// Runs the `octet update` command.
///
/// `check_only` reports the latest release and the command that would run.
/// Otherwise the update is executed for installer, Cargo, and validated global
/// npm installs; development builds, local/npx npm layouts, and unrecognized
/// install locations fail with manual instructions.
pub(crate) async fn run(check_only: bool) -> anyhow::Result<()> {
    if !check_only && cfg!(debug_assertions) {
        anyhow::bail!(
            "octet update cannot update a debug build; install a release build of octet first"
        );
    }
    let status = progress::Activity::new("Checking for updates")
        .wait(check())
        .await?;
    match &status {
        UpdateStatus::Current { version } => {
            progress::banner(version, "Already up to date");
            crate::output::stdout_line(status.to_string());
            Ok(())
        }
        UpdateStatus::Available {
            current, latest, ..
        } => {
            let method = current_install_method();
            let action = UpdateAction::for_method(&method, latest);
            if check_only {
                progress::banner(latest, "Update available");
                crate::output::stdout_multiline(status.to_string());
                match action {
                    Some(action) => {
                        crate::output::stdout_line(format!("To update: {}", action.command_str()))
                    }
                    None => crate::output::stdout_line(manual_update_hint(&method, latest)),
                }
                return Ok(());
            }
            let action =
                action.ok_or_else(|| anyhow::anyhow!(manual_update_hint(&method, latest)))?;
            run_update(current, latest, &action).await
        }
    }
}

/// Instructions for reaching a release when the install method has no
/// automated update path.
fn manual_update_hint(method: &InstallMethod, latest: &semver::Version) -> String {
    match method {
        InstallMethod::Local => format!(
            "this is a development build; rebuild it in the octet workspace, or install a release from {REPOSITORY}#install"
        ),
        InstallMethod::NpmLocal { .. } => format!(
            "this octet is installed in a local or npx npm layout; update that project explicitly, or install globally with:\n  {}",
            npm_command_str(&latest.to_string())
        ),
        InstallMethod::Unknown => format!(
            "could not detect how this octet was installed; update manually:\n  {}\nSee {REPOSITORY}#install",
            install_script(&latest.to_string())
        ),
        _ => unreachable!("methods with an update action do not need manual instructions"),
    }
}

/// Executes the selected channel, streams its output, and verifies the result.
async fn run_update(
    current: &semver::Version,
    latest: &semver::Version,
    action: &UpdateAction,
) -> anyhow::Result<()> {
    // Capture the installed path before the channel replaces it. A successful
    // child exit alone does not prove that the requested version was installed.
    let executable = std::env::current_exe().context("could not locate the installed octet")?;
    progress::banner(latest, &format!("Updating from v{current}"));
    let status = match action {
        UpdateAction::Installer { version } => run_installer(version).await?,
        UpdateAction::Cargo { .. } | UpdateAction::Npm { .. } => {
            let (program, args) = action.command_args();
            let mut command = tokio::process::Command::new(&program);
            command.args(args);
            let label = if matches!(action, UpdateAction::Cargo { .. }) {
                "Building and installing with Cargo"
            } else {
                "Installing with npm"
            };
            progress::command(&mut command, label)
                .await
                .with_context(|| format!("failed to run {}", program.to_string_lossy()))?
        }
    };
    if !status.success() {
        let detail = status
            .code()
            .map(|code| format!("exit code {code}"))
            .unwrap_or_else(|| "interrupted".to_string());
        anyhow::bail!(
            "the update command failed ({detail}); run it manually to update:\n  {}",
            action.command_str()
        );
    }
    progress::Activity::new("Verifying installed version")
        .wait(verify_installed_version(&executable, latest))
        .await?;
    crate::output::stdout_line(format!(
        "octet updated to {latest}. Restart octet to use it."
    ));
    for extension in crate::extension_package::installed_official_bundle_ids() {
        crate::output::stdout_line(format!(
            "Run `octet extension update {extension}` to install the bundle matching octet {latest}."
        ));
    }
    Ok(())
}

// Fetch completely before executing: `curl | sh` can otherwise report success
// after a failed/partial transfer. Only a successful installed-version probe may
// produce the updater's final success message.
async fn run_installer(version: &semver::Version) -> anyhow::Result<std::process::ExitStatus> {
    use std::io::{Seek, SeekFrom, Write};
    use std::process::Stdio;
    let url = format!("{RELEASE_DOWNLOAD_BASE}/v{version}/install-octet.sh");
    let mut download = tokio::process::Command::new("curl");
    download
        .args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--tlsv1.2",
            "--location",
            "--max-redirs",
            "5",
            "--fail",
            "--silent",
            "--show-error",
            "--connect-timeout",
            "15",
            "--max-time",
            "300",
            "--max-filesize",
            "262144",
            "--output",
            "-",
        ])
        .arg(&url);
    let script = progress::Activity::new("Downloading installer")
        .wait(download_installer_script(
            &mut download,
            &url,
            Duration::from_secs(300),
        ))
        .await?;
    // Only a complete, successful, size-bounded download reaches this private
    // unnamed file. The OS removes it even on terminating signals.
    let mut installer = tempfile::tempfile()?;
    installer.write_all(&script)?;
    installer.seek(SeekFrom::Start(0))?;
    tokio::process::Command::new("sh")
        .stdin(Stdio::from(installer))
        .env("OCTET_UPDATE_PARENT_UI", "1")
        .kill_on_drop(true)
        .status()
        .await
        .context("failed to run the version-pinned installer")
}

async fn download_installer_script(
    command: &mut tokio::process::Command,
    url: &str,
    timeout: Duration,
) -> anyhow::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    const MAX_BYTES: u64 = 262_144;
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to download the version-pinned installer from {url}"))?;
    // Older curl versions cannot enforce --max-filesize on unknown-length
    // responses. Bound the consumed bytes and elapsed time ourselves; never
    // expose the script or retain an arbitrarily large temporary download.
    let result = tokio::time::timeout(timeout, async {
        let mut script = Vec::new();
        child
            .stdout
            .take()
            .expect("piped installer download")
            .take(MAX_BYTES + 1)
            .read_to_end(&mut script)
            .await
            .context("failed to read the installer download")?;
        anyhow::ensure!(
            script.len() <= MAX_BYTES as usize,
            "installer download exceeds its size limit for {url}; no installer was executed"
        );
        let status = child.wait().await?;
        anyhow::ensure!(
            status.success(),
            "installer download failed ({status}) for {url}; no installer was executed"
        );
        anyhow::ensure!(!script.is_empty(), "downloaded installer is empty");
        Ok(script)
    })
    .await
    .with_context(|| format!("installer download timed out for {url}; no installer was executed"))
    .and_then(|result| result);
    if result.is_err() {
        // Reap the direct downloader on failed validation or a deadline, rather
        // than allowing a still-writing curl to outlive the rejected update.
        let _ = child.kill().await;
    }
    result
}

async fn verify_installed_version(
    executable: &Path,
    latest: &semver::Version,
) -> anyhow::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut child = tokio::process::Command::new(executable)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("could not verify the installed octet version")?;
    tokio::time::timeout(CHECK_TIMEOUT, async {
        let mut output = Vec::new();
        child
            .stdout
            .take()
            .expect("piped version output")
            .take(1025)
            .read_to_end(&mut output)
            .await?;
        anyhow::ensure!(
            output.len() <= 1024,
            "installed octet version output exceeded its limit"
        );
        let status = child.wait().await?;
        anyhow::ensure!(
            status.success() && output == format!("octet {latest}\n").as_bytes(),
            "update command finished, but the installed octet does not report version {latest}"
        );
        Ok(())
    })
    .await
    .context("installed octet version check timed out")?
}

#[cfg(test)]
mod tests;
