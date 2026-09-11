//! Bounded startup release notice, explicit update check, and self-update.
//!
//! The check fetches the latest GitHub release with a short timeout, a hard
//! response-size limit, and no redirects. The update delegates to the channel
//! that installed the running binary: the version-pinned installer for
//! installer installs, a pinned `cargo install` for Cargo installs, or an exact
//! npm install for a validated global npm package. octet never replaces itself
//! in process; the channel swaps the installed files under the running
//! process, and the user restarts octet.

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

// Only the optional startup client uses this resolver; explicit update and
// provider clients retain their existing DNS behavior. FnOnce enforces a single
// lookup per startup check, including any unexpected repeated resolver calls.
struct StartupResolver<F> {
    hostname: &'static str,
    lookup: std::sync::Mutex<Option<F>>,
}

impl<F> StartupResolver<F> {
    fn new(hostname: &'static str, lookup: F) -> Self {
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
    if let Some(serve_version) = crate::extension_package::installed_version() {
        if serve_version != *latest {
            crate::output::stdout_line(format!(
                "octet Serve is still at {serve_version}. Run `octet extension update octet-serve` to match the new release."
            ));
        }
    }
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
    use std::io::{Seek, SeekFrom};
    use std::process::Stdio;
    // An unnamed file is removed by the OS even on terminating signals. Fetch
    // stdout goes only to that private file, never the UI or command output.
    let mut installer = tempfile::tempfile()?;
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
        .arg(&url)
        .stdout(Stdio::from(installer.try_clone()?))
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = progress::Activity::new("Downloading installer")
        .wait(download.status())
        .await
        .context("failed to download the version-pinned installer")?;
    if !status.success() {
        anyhow::bail!("installer download failed ({status}) for {url}; no installer was executed");
    }
    let size = installer.metadata()?.len();
    if size == 0 || size > 262_144 {
        anyhow::bail!("downloaded installer is empty or exceeds its size limit");
    }
    installer.seek(SeekFrom::Start(0))?;
    tokio::process::Command::new("sh")
        .stdin(Stdio::from(installer))
        .env("OCTET_UPDATE_PARENT_UI", "1")
        .kill_on_drop(true)
        .status()
        .await
        .context("failed to run the version-pinned installer")
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
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[cfg(unix)]
    #[tokio::test]
    async fn installed_version_probe_requires_success_and_exact_target_without_exposing_output() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("octet");
        let version = semver::Version::new(0, 7, 5);
        for (script, succeeds) in [
            ("printf 'octet 0.7.5\\n'", true),
            ("printf 'octet 0.7.4\\n'", false),
            ("printf 'octet 0.7.5\\n'; exit 1", false),
            ("printf 'private diagnostic\\n'", false),
            ("dd if=/dev/zero bs=1025 count=1 2>/dev/null", false),
        ] {
            std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
            let result = verify_installed_version(&executable, &version).await;
            assert_eq!(result.is_ok(), succeeds);
            if let Err(error) = result {
                assert!(!error.to_string().contains("private diagnostic"));
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn installed_version_probe_has_a_bounded_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("octet");
        std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let error = tokio::time::timeout(
            Duration::from_secs(7),
            verify_installed_version(&executable, &semver::Version::new(0, 7, 5)),
        )
        .await
        .expect("version probe deadline")
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
    }

    fn stable_release(tag: &str) -> serde_json::Value {
        serde_json::json!({ "tag_name": tag, "draft": false, "prerelease": false })
    }

    #[test]
    fn startup_only_reports_newer_stable_semver_precedence() {
        for (current, tag, expected) in [
            ("0.7.4", "v0.7.5", Some((0, 7, 5))),
            ("0.7.4", "0.8.0", Some((0, 8, 0))),
            ("0.9.0", "v0.10.0", Some((0, 10, 0))),
            ("0.7.5-rc.1", "v0.7.5", Some((0, 7, 5))),
            ("0.7.4", "v0.7.5+remote.text", Some((0, 7, 5))),
            ("0.7.4", "v0.7.4", None),
            ("0.7.4", "v0.7.3", None),
            ("0.10.0", "v0.9.0", None),
            ("0.7.4", "v0.7.4+remote.text", None),
            ("0.7.4+aaa", "v0.7.4+zzz", None),
            ("0.7.4", "v0.7.5-rc.1", None),
            ("0.7.4", "v1.0.0-alpha", None),
            ("0.7.4", "vv0.7.5", None),
            ("0.7.4", " v0.7.5 ", None),
            ("0.7.4", "v0.07.5", None),
            ("0.7.4", "v0.7", None),
            ("0.7.4", "v0.7.5\ninstall something", None),
            ("0.7.4", "v0.7.5+\u{1b}[31m", None),
            ("0.7.4", "v18446744073709551616.0.0", None),
        ] {
            let body = serde_json::to_vec(&stable_release(tag)).unwrap();
            assert_eq!(
                newer_stable_release(&body, &current.parse().unwrap()),
                expected.map(|(major, minor, patch)| semver::Version::new(major, minor, patch)),
                "current={current}, tag={tag:?}",
            );
        }
    }

    #[test]
    fn startup_rejects_draft_prerelease_and_malformed_metadata() {
        let current = semver::Version::new(0, 7, 4);
        for (field, value) in [
            ("draft", serde_json::json!(true)),
            ("prerelease", serde_json::json!(true)),
            ("draft", serde_json::json!("false")),
            ("prerelease", serde_json::Value::Null),
            ("tag_name", serde_json::json!(75)),
        ] {
            let mut release = stable_release("v0.7.5");
            release[field] = value;
            assert_eq!(
                newer_stable_release(&serde_json::to_vec(&release).unwrap(), &current),
                None,
                "{release}",
            );
        }
        for field in ["draft", "prerelease", "tag_name"] {
            let mut release = stable_release("v0.7.5");
            release.as_object_mut().unwrap().remove(field);
            assert_eq!(
                newer_stable_release(&serde_json::to_vec(&release).unwrap(), &current),
                None,
            );
        }
        for body in [b"not json".as_slice(), b"[]", b"null", b"\xff"] {
            assert_eq!(newer_stable_release(body, &current), None);
        }
    }

    #[tokio::test]
    async fn startup_requests_once_without_credentials_and_returns_only_version_numbers() {
        assert_eq!(
            LATEST_RELEASE_URL,
            "https://api.github.com/repos/skaft-software/octet/releases/latest"
        );
        let server = MockServer::start().await;
        let mut release = stable_release("v0.7.5+remote.build.metadata");
        release["html_url"] = serde_json::json!("\u{1b}]8;;https://evil.test\u{7}click");
        release["name"] = serde_json::json!("run a remote installer");
        release["body"] = serde_json::json!("\u{1b}[31mremote instructions");
        Mock::given(method("GET"))
            .and(path("/latest"))
            .and(header("user-agent", "octet/0.7.4"))
            .respond_with(ResponseTemplate::new(200).set_body_json(release))
            .expect(1)
            .mount(&server)
            .await;

        let result = startup_available_update_url(
            &format!("{}/latest", server.uri()),
            "0.7.4",
            CHECK_TIMEOUT,
        )
        .await;
        assert_eq!(result, Some(semver::Version::new(0, 7, 5)));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(request.body.is_empty());
        assert!(request.url.query().is_none());
        for header in [
            "authorization",
            "proxy-authorization",
            "cookie",
            "x-api-key",
        ] {
            assert!(!request.headers.contains_key(header), "{header}");
        }
    }

    #[test]
    fn startup_ignores_environment_proxy_credentials() {
        let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let proxy_url = format!("http://test-only:synthetic@{}", proxy.local_addr().unwrap());
        // Isolate proxy variables from concurrently running tests. Reuse the
        // request test in a child process, with every proxy setting aimed at a
        // synthetic sink that must receive no connection.
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args([
            "--exact",
            "update::tests::startup_requests_once_without_credentials_and_returns_only_version_numbers",
        ]);
        for variable in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            child.env(variable, &proxy_url);
        }
        child.env("NO_PROXY", "").env("no_proxy", "");
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        proxy.set_nonblocking(true).unwrap();
        assert_eq!(
            proxy.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn startup_is_quiet_for_http_errors_and_redirects_even_with_valid_metadata() {
        let server = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(stable_release("v9.0.0")))
            .expect(0)
            .mount(&destination)
            .await;
        for status in [301, 302, 303, 307, 308, 403, 404, 429, 500, 503] {
            let route = format!("/status/{status}");
            Mock::given(method("GET"))
                .and(path(&route))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("location", destination.uri())
                        .set_body_json(stable_release("v9.0.0")),
                )
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                startup_available_update_url(
                    &format!("{}{route}", server.uri()),
                    "0.7.4",
                    CHECK_TIMEOUT,
                )
                .await,
                None,
                "status={status}",
            );
        }
        assert!(destination.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn startup_is_quiet_for_bad_json_and_unavailable_network() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            startup_available_update_url(&server.uri(), "0.7.4", CHECK_TIMEOUT).await,
            None,
        );
        // A bound, non-listening socket keeps the failed connection local without
        // a port-reuse race. Some platforms time out rather than refusing it.
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        assert_eq!(
            startup_available_update_url(
                &format!("http://{}", socket.local_addr().unwrap()),
                "0.7.4",
                Duration::from_millis(250),
            )
            .await,
            None,
        );
    }

    #[tokio::test]
    async fn startup_enforces_declared_response_size_limit() {
        let server = MockServer::start().await;
        for (size, expected) in [
            (
                MAX_RELEASE_RESPONSE_BYTES,
                Some(semver::Version::new(0, 7, 5)),
            ),
            (MAX_RELEASE_RESPONSE_BYTES + 1, None),
        ] {
            let mut body = serde_json::to_vec(&stable_release("v0.7.5")).unwrap();
            body.resize(size, b' ');
            let route = format!("/size/{size}");
            Mock::given(method("GET"))
                .and(path(&route))
                .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
                .expect(1)
                .mount(&server)
                .await;
            assert_eq!(
                startup_available_update_url(
                    &format!("{}{route}", server.uri()),
                    "0.7.4",
                    CHECK_TIMEOUT,
                )
                .await,
                expected,
                "size={size}",
            );
        }
    }

    async fn chunked_release_server(
        size: usize,
        delay: Duration,
    ) -> (
        String,
        tokio::task::JoinHandle<()>,
        tokio::sync::oneshot::Receiver<()>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let received = stream.read(&mut request).await.unwrap();
            assert!(received > 0, "client must start the release request");
            if stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .await
                .is_err()
            {
                return;
            }
            let _ = started.send(());
            let mut body = serde_json::to_vec(&stable_release("v0.7.5")).unwrap();
            body.resize(size, b' ');
            for chunk in body.chunks(1024) {
                let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
                frame.extend_from_slice(chunk);
                frame.extend_from_slice(b"\r\n");
                if stream.write_all(&frame).await.is_err() {
                    return;
                }
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n").await;
        });
        (url, task, ready)
    }

    #[tokio::test]
    async fn startup_enforces_chunked_response_size_limit() {
        for (size, expected) in [
            (
                MAX_RELEASE_RESPONSE_BYTES,
                Some(semver::Version::new(0, 7, 5)),
            ),
            (MAX_RELEASE_RESPONSE_BYTES + 1, None),
        ] {
            let (url, task, _) = chunked_release_server(size, Duration::ZERO).await;
            assert_eq!(
                startup_available_update_url(&url, "0.7.4", CHECK_TIMEOUT).await,
                expected,
                "size={size}",
            );
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn startup_deadline_bounds_slow_headers_without_retrying() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(stable_release("v0.7.5"))
                    .set_delay(Duration::from_secs(10)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let started = std::time::Instant::now();
        assert_eq!(
            startup_available_update_url(&server.uri(), "0.7.4", Duration::from_millis(250)).await,
            None,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn startup_deadline_bounds_slow_streaming_body() {
        let (url, task, ready) =
            chunked_release_server(16 * 1024, Duration::from_millis(100)).await;
        let started = std::time::Instant::now();
        let result = startup_available_update_url(&url, "0.7.4", Duration::from_millis(250)).await;
        task.abort();
        let _ = task.await;
        assert_eq!(result, None);
        assert!(started.elapsed() < Duration::from_secs(2));
        ready.await.unwrap();
    }

    #[tokio::test]
    async fn startup_check_can_be_cancelled_on_exit() {
        let (url, server, ready) = chunked_release_server(16 * 1024, Duration::from_secs(1)).await;
        let check = tokio::spawn(async move {
            startup_available_update_url(&url, "0.7.4", CHECK_TIMEOUT).await
        });
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        check.abort();
        assert!(check.await.unwrap_err().is_cancelled());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn startup_dns_allows_only_one_lookup_of_its_expected_host() {
        use reqwest::dns::Resolve;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let address = "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap();
        let resolver = StartupResolver::new("startup-update.invalid", move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs)
        });
        assert!(resolver
            .resolve("other.invalid".parse().unwrap())
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let addresses = resolver
            .resolve("startup-update.invalid".parse().unwrap())
            .await
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(addresses, vec![address]);
        assert!(resolver
            .resolve("startup-update.invalid".parse().unwrap())
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn startup_dns_failure_is_quiet() {
        let resolver = StartupResolver::new("startup-update.invalid", || {
            Err(std::io::Error::other("injected DNS failure"))
        });
        assert_eq!(
            startup_available_update_with_resolver(
                "http://startup-update.invalid/latest",
                "0.7.4",
                CHECK_TIMEOUT,
                resolver,
            )
            .await,
            None,
        );
    }

    #[tokio::test]
    async fn startup_dns_cancellation_discards_late_result_without_connecting() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        // Observe disposal of the actual DNS result, not merely that the request
        // future returned. Iterating a late answer would permit a TCP connect.
        struct DnsAnswer {
            address: Option<std::net::SocketAddr>,
            used: Arc<AtomicBool>,
            dropped: Option<tokio::sync::oneshot::Sender<()>>,
        }
        impl Iterator for DnsAnswer {
            type Item = std::net::SocketAddr;

            fn next(&mut self) -> Option<Self::Item> {
                self.used.store(true, Ordering::SeqCst);
                self.address.take()
            }
        }
        impl Drop for DnsAnswer {
            fn drop(&mut self) {
                let _ = self.dropped.take().unwrap().send(());
            }
        }

        for abort in [false, true] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            let url = format!("http://startup-update.invalid:{}/latest", address.port());
            let (started, ready) = tokio::sync::oneshot::channel();
            let (release, hold) = std::sync::mpsc::channel();
            let (dropped, disposed) = tokio::sync::oneshot::channel();
            let used = Arc::new(AtomicBool::new(false));
            let answer = DnsAnswer {
                address: Some(address),
                used: used.clone(),
                dropped: Some(dropped),
            };
            let resolver = StartupResolver::new("startup-update.invalid", move || {
                started.send(()).unwrap();
                hold.recv().unwrap();
                Ok(Box::new(answer) as reqwest::dns::Addrs)
            });
            let deadline = if abort {
                CHECK_TIMEOUT
            } else {
                Duration::from_millis(250)
            };
            let check = tokio::spawn(async move {
                startup_available_update_with_resolver(&url, "0.7.4", deadline, resolver).await
            });
            tokio::time::timeout(Duration::from_secs(2), ready)
                .await
                .unwrap()
                .unwrap();
            if abort {
                check.abort();
                assert!(check.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(2), check)
                        .await
                        .unwrap()
                        .unwrap(),
                    None,
                );
            }
            // DNS stays blocked until after the check has completed/been aborted.
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(2), disposed)
                .await
                .unwrap()
                .unwrap();
            assert!(!used.load(Ordering::SeqCst), "late addresses were consumed");
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn startup_pending_dns_does_not_delay_runtime_or_process_exit() {
        const CHILD_MODE: &str = "OCTET_TEST_STARTUP_DNS_EXIT";
        const TEST: &str =
            "update::tests::startup_pending_dns_does_not_delay_runtime_or_process_exit";
        const DROPPED: &str = "runtime dropped while DNS remains held";

        if let Ok(mode) = std::env::var(CHILD_MODE) {
            let mut builder = if mode.starts_with("multi-") {
                let mut builder = tokio::runtime::Builder::new_multi_thread();
                builder.worker_threads(1);
                builder
            } else {
                tokio::runtime::Builder::new_current_thread()
            };
            let runtime = builder.enable_all().build().unwrap();
            runtime.block_on(async {
                let (started, ready) = tokio::sync::oneshot::channel();
                let resolver = StartupResolver::new("startup-update.invalid", move || {
                    started.send(()).unwrap();
                    // Deliberately never release this DNS job, even after the
                    // runtime drops. Only child process exit terminates it.
                    loop {
                        std::thread::park();
                    }
                });
                let abort = mode.ends_with("abort");
                let deadline = if abort {
                    CHECK_TIMEOUT
                } else {
                    Duration::from_millis(250)
                };
                let check = tokio::spawn(startup_available_update_with_resolver(
                    "http://startup-update.invalid/latest",
                    "0.7.4",
                    deadline,
                    resolver,
                ));
                tokio::time::timeout(Duration::from_secs(2), ready)
                    .await
                    .unwrap()
                    .unwrap();
                if abort {
                    check.abort();
                    assert!(check.await.unwrap_err().is_cancelled());
                } else {
                    assert_eq!(check.await.unwrap(), None);
                }
            });
            let start = std::time::Instant::now();
            drop(runtime); // Normal shutdown, NOT shutdown_timeout/background.
            println!("{DROPPED}: {mode}, {:?}", start.elapsed());
            return;
        }

        // A subprocess makes shutdown regressions bounded failures instead of
        // hanging the suite. Both Tokio runtime flavors must exit normally with
        // resolution still held, after either the outer deadline or task abort.
        for mode in [
            "current-deadline",
            "current-abort",
            "multi-deadline",
            "multi-abort",
        ] {
            let start = std::time::Instant::now();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", TEST, "--nocapture"])
                .env(CHILD_MODE, mode)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut timed_out = false;
            while child.try_wait().unwrap().is_none() {
                if start.elapsed() >= Duration::from_secs(3) {
                    timed_out = true;
                    child.kill().unwrap();
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !timed_out && output.status.success() && stdout.contains(DROPPED),
                "{mode}: held DNS blocked runtime/process exit or child failed\n{stdout}\n{stderr}",
            );
            println!("{mode}: process exited in {:?}\n{stdout}", start.elapsed());
        }
    }

    #[tokio::test]
    async fn reports_newer_release_without_treating_older_tags_as_updates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/latest"))
            .and(header("user-agent", "octet/0.1.1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v0.2.0",
                "html_url": "https://example.test/octet/v0.2.0"
            })))
            .mount(&server)
            .await;
        assert!(matches!(
            check_url(&format!("{}/latest", server.uri()), "0.1.1")
                .await
                .unwrap(),
            UpdateStatus::Available { latest, .. } if latest == semver::Version::new(0, 2, 0)
        ));

        let old = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v0.1.0-alpha",
                "html_url": null
            })))
            .mount(&old)
            .await;
        assert!(matches!(
            check_url(&format!("{}/latest", old.uri()), "0.1.1")
                .await
                .unwrap(),
            UpdateStatus::Current { .. }
        ));
    }

    #[tokio::test]
    async fn rejects_malformed_release_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "not a version"
            })))
            .mount(&server)
            .await;
        assert!(check_url(&server.uri(), "0.1.1").await.is_err());
    }

    #[tokio::test]
    async fn rejects_chunked_release_metadata_over_the_hard_limit() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                )
                .unwrap();
            let chunk = vec![b'x'; 4096];
            for _ in 0..=(MAX_RELEASE_RESPONSE_BYTES / chunk.len()) {
                if write!(stream, "{:x}\r\n", chunk.len()).is_err()
                    || stream.write_all(&chunk).is_err()
                    || stream.write_all(b"\r\n").is_err()
                {
                    return;
                }
            }
            let _ = stream.write_all(b"0\r\n\r\n");
        });

        let result = check_url(&format!("http://{address}/latest"), "0.1.1").await;
        server.join().unwrap();
        let error = result.unwrap_err();
        assert!(error.to_string().contains("65536-byte limit"), "{error:#}");
    }

    #[tokio::test]
    async fn does_not_follow_release_metadata_redirects() {
        let origin = MockServer::start().await;
        let destination = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/latest"))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", format!("{}/sink", destination.uri())),
            )
            .mount(&origin)
            .await;
        Mock::given(method("GET"))
            .and(path("/sink"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "tag_name": "v9.0.0"
            })))
            .mount(&destination)
            .await;

        assert!(check_url(&format!("{}/latest", origin.uri()), "0.1.1")
            .await
            .is_err());
        assert!(destination.received_requests().await.unwrap().is_empty());
    }

    fn home_environment(home: &Path) -> InstallEnvironment {
        InstallEnvironment {
            home: Some(home.to_path_buf()),
            ..InstallEnvironment::default()
        }
    }

    fn create_npm_manifest(
        root: &Path,
        name: &str,
        version: &str,
        os: Option<&str>,
        cpu: Option<&str>,
    ) {
        let optional = NPM_PLATFORM_PACKAGES
            .iter()
            .map(|package| format!(r#""{package}":"{version}""#))
            .collect::<Vec<_>>()
            .join(",");
        let manifest = if let (Some(os), Some(cpu)) = (os, cpu) {
            format!(
                r#"{{"name":"{name}","version":"{version}","description":"Native octet runtime for {target}","license":"MIT","repository":"https://github.com/skaft-software/octet","os":["{os}"],"cpu":["{cpu}"],"files":["README.md","LICENSE","bin/","share/octet/"]}}"#,
                target = expected_npm_platform().unwrap().1,
            )
        } else {
            format!(
                r#"{{"name":"{name}","version":"{version}","description":"Native octet coding agent launcher","license":"MIT","repository":"https://github.com/skaft-software/octet","files":["README.md","LICENSE","bin/","lib/"],"bin":{{"octet":"bin/octet","octet-host":"bin/octet-host"}},"optionalDependencies":{{{optional}}}}}"#
            )
        };
        std::fs::write(root.join("package.json"), manifest).unwrap();
    }

    fn create_npm_fixture(root: &Path) -> (PathBuf, PathBuf, String) {
        let (platform_package, _target, os, cpu) = expected_npm_platform().unwrap();
        let platform_name = platform_package.rsplit('/').next().unwrap();
        let npm_root = root.join("prefix/node_modules");
        let launcher_root = npm_root.join(NPM_LAUNCHER);
        let platform_root = launcher_root
            .join("node_modules/@skaft")
            .join(platform_name);
        std::fs::create_dir_all(launcher_root.join("bin")).unwrap();
        std::fs::create_dir_all(launcher_root.join("lib")).unwrap();
        create_npm_manifest(
            &launcher_root,
            NPM_LAUNCHER,
            env!("CARGO_PKG_VERSION"),
            None,
            None,
        );
        for file in ["README.md", "LICENSE"] {
            std::fs::write(launcher_root.join(file), file).unwrap();
        }
        for file in ["bin/octet", "bin/octet-host", "lib/launch.sh"] {
            let path = launcher_root.join(file);
            std::fs::write(&path, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::create_dir_all(platform_root.join("bin")).unwrap();
        for directory in ["docs", "examples", "sdk"] {
            std::fs::create_dir_all(platform_root.join("share/octet").join(directory)).unwrap();
        }
        create_npm_manifest(
            &platform_root,
            platform_package,
            env!("CARGO_PKG_VERSION"),
            Some(os),
            Some(cpu),
        );
        for file in ["README.md", "LICENSE"] {
            std::fs::write(platform_root.join(file), file).unwrap();
        }
        for file in ["bin/octet", "bin/octet-host"] {
            let path = platform_root.join(file);
            std::fs::write(&path, "native").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        std::fs::write(
            platform_root.join("share/octet/.octet-version"),
            format!("{}\n", env!("CARGO_PKG_VERSION")),
        )
        .unwrap();
        std::fs::write(platform_root.join("share/octet/README.md"), "# octet\n").unwrap();
        (npm_root, platform_root, platform_name.to_owned())
    }

    fn create_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
    }

    #[test]
    fn detects_installer_installation_by_docs_tree_and_target() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin_dir = home.join(".local/bin");
        create_dir(&bin_dir);
        create_dir(&home.join(".local/share/octet"));
        let exe = bin_dir.join("octet");
        assert_eq!(
            detect_install_method_in(&exe, &home_environment(&home)),
            InstallMethod::Installer { bin_dir }
        );
    }

    #[test]
    fn detects_installer_installation_with_explicit_install_dir() {
        let root = tempfile::tempdir().unwrap();
        let bin_dir = root.path().join("octet/bin");
        create_dir(&bin_dir);
        create_dir(&root.path().join("octet/share/octet"));
        let exe = bin_dir.join("octet");
        let env = InstallEnvironment {
            install_dir: Some(bin_dir.clone()),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&exe, &env),
            InstallMethod::Installer { bin_dir }
        );
    }

    #[test]
    fn refuses_installer_installation_that_the_installer_would_not_update() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin_dir = root.path().join("octet/bin");
        create_dir(&bin_dir);
        create_dir(&root.path().join("octet/share/octet"));
        let exe = bin_dir.join("octet");
        assert_eq!(
            detect_install_method_in(&exe, &home_environment(&home)),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn detects_cargo_installation() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let bin_dir = home.join(".cargo/bin");
        create_dir(&bin_dir);
        let exe = bin_dir.join("octet");
        assert_eq!(
            detect_install_method_in(&exe, &home_environment(&home)),
            InstallMethod::Cargo
        );

        let custom_home = root.path().join("cargo-home");
        let custom_bin = custom_home.join("bin");
        create_dir(&custom_bin);
        let env = InstallEnvironment {
            home: Some(home),
            cargo_home: Some(custom_home),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&custom_bin.join("octet"), &env),
            InstallMethod::Cargo
        );
    }

    #[test]
    fn detects_workspace_builds() {
        let debug = Path::new("/repo/target/debug/octet");
        let release = Path::new("/Users/x/octet/target/release/octet");
        let env = InstallEnvironment::default();
        assert_eq!(detect_install_method_in(debug, &env), InstallMethod::Local);
        assert_eq!(
            detect_install_method_in(release, &env),
            InstallMethod::Local
        );
    }

    #[test]
    fn reports_unrecognized_installations() {
        let env = home_environment(Path::new("/Users/x"));
        assert_eq!(
            detect_install_method_in(Path::new("/opt/custom/octet"), &env),
            InstallMethod::Unknown
        );
        assert_eq!(
            detect_install_method_in(Path::new("octet"), &env),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn detects_only_a_corroborated_global_npm_layout() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let (npm_root, platform_root, platform_name) = create_npm_fixture(&root_path);
        let exe = platform_root.join("bin/octet");
        let environment = InstallEnvironment {
            npm_root: Some(npm_root.clone()),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&exe, &environment),
            InstallMethod::Npm {
                package_root: platform_root.clone()
            }
        );

        let local_environment = InstallEnvironment::default();
        assert_eq!(
            detect_install_method_in(&exe, &local_environment),
            InstallMethod::NpmLocal {
                package_root: platform_root.clone()
            }
        );
        let wrong_root = root.path().join("other/node_modules");
        create_dir(&wrong_root);
        let wrong_environment = InstallEnvironment {
            npm_root: Some(wrong_root),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&exe, &wrong_environment),
            InstallMethod::NpmLocal {
                package_root: platform_root
            }
        );
        assert_eq!(
            platform_name,
            expected_npm_platform()
                .unwrap()
                .0
                .rsplit('/')
                .next()
                .unwrap()
        );
    }

    #[test]
    fn rejects_npm_layout_outside_skaft_scope() {
        let root = tempfile::tempdir().unwrap();
        let (npm_root, platform_root, platform_name) = create_npm_fixture(root.path());
        let other_scope = npm_root.join("@other");
        std::fs::create_dir(&other_scope).unwrap();
        let other_platform = other_scope.join(platform_name);
        std::fs::rename(platform_root, &other_platform).unwrap();
        let environment = InstallEnvironment {
            npm_root: Some(npm_root),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&other_platform.join("bin/octet"), &environment),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn rejects_npm_layout_with_wrong_platform_metadata() {
        let root = tempfile::tempdir().unwrap();
        let (npm_root, platform_root, _) = create_npm_fixture(root.path());
        let manifest_path = platform_root.join("package.json");
        let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
        manifest = manifest.replace("\"license\":\"MIT\"", "\"license\":\"GPL\"");
        std::fs::write(manifest_path, manifest).unwrap();
        let environment = InstallEnvironment {
            npm_root: Some(npm_root),
            ..InstallEnvironment::default()
        };
        assert_eq!(
            detect_install_method_in(&platform_root.join("bin/octet"), &environment),
            InstallMethod::Unknown
        );
    }

    #[test]
    fn maps_install_methods_to_update_actions() {
        let version = "0.5.0".parse::<semver::Version>().unwrap();
        let bin_dir = PathBuf::from("/home/user/.local/bin");
        assert_eq!(
            UpdateAction::for_method(
                &InstallMethod::Installer {
                    bin_dir: bin_dir.clone()
                },
                &version
            ),
            Some(UpdateAction::Installer {
                version: version.clone()
            })
        );
        assert_eq!(
            UpdateAction::for_method(&InstallMethod::Cargo, &version),
            Some(UpdateAction::Cargo {
                version: version.clone()
            })
        );
        assert_eq!(
            UpdateAction::for_method(
                &InstallMethod::Npm {
                    package_root: PathBuf::from("/npm/lib/node_modules/@skaft/octet-linux-x64-gnu"),
                },
                &version,
            ),
            Some(UpdateAction::Npm {
                version: version.clone(),
            })
        );
        assert_eq!(
            UpdateAction::for_method(
                &InstallMethod::NpmLocal {
                    package_root: bin_dir.clone(),
                },
                &version
            ),
            None
        );

        assert_eq!(
            UpdateAction::for_method(&InstallMethod::Unknown, &version),
            None
        );
    }

    #[test]
    fn renders_documented_update_commands() {
        let installer = UpdateAction::Installer {
            version: "0.5.0".parse().unwrap(),
        };
        assert_eq!(
            installer.command_str(),
            "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/skaft-software/octet/releases/download/v0.5.0/install-octet.sh | sh"
        );
        let (program, args) = installer.command_args();
        assert_eq!(program, OsString::from("sh"));
        assert_eq!(
            args,
            vec![
                OsString::from("-c"),
                OsString::from(installer.command_str())
            ]
        );

        let cargo = UpdateAction::Cargo {
            version: "0.5.0".parse().unwrap(),
        };
        assert_eq!(
            cargo.command_str(),
            "cargo install --locked --git https://github.com/skaft-software/octet --tag v0.5.0 --bins octet-coding-agent"
        );
        let (program, args) = cargo.command_args();
        assert_eq!(program, OsString::from("cargo"));
        assert_eq!(
            args,
            vec![
                OsString::from("install"),
                OsString::from("--locked"),
                OsString::from("--git"),
                OsString::from(REPOSITORY),
                OsString::from("--tag"),
                OsString::from("v0.5.0"),
                OsString::from("--bins"),
                OsString::from("octet-coding-agent"),
            ]
        );

        let npm = UpdateAction::Npm {
            version: "0.5.0".parse().unwrap(),
        };
        assert_eq!(
            npm.command_str(),
            "npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@0.5.0"
        );
        let (program, args) = npm.command_args();
        assert_eq!(program, OsString::from("npm"));
        assert_eq!(
            args,
            vec![
                OsString::from("install"),
                OsString::from("--global"),
                OsString::from("--ignore-scripts"),
                OsString::from("--no-audit"),
                OsString::from("--no-fund"),
                OsString::from("@skaft/octet@0.5.0"),
            ]
        );
    }
}
