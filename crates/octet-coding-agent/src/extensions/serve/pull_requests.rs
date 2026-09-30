//! GitHub pull requests: the gh CLI boundary, the persisted store and its refresh.

use super::*;

pub(super) const PULL_REQUEST_STORE_VERSION: u16 = 1;

pub(super) const PULL_REQUEST_STORE_FILE: &str = "pull-requests-v1.json";

pub(super) const MAX_PULL_REQUEST_STORE_BYTES: u64 = 2 * 1024 * 1024;

pub(super) const MAX_PULL_REQUEST_RECORDS: usize = 2_000;

pub(super) const MAX_GITHUB_CLI_OUTPUT_BYTES: u64 = 16 * 1024;

pub(super) const MAX_CONCURRENT_GITHUB_QUERIES: usize = 4;

pub(super) const GITHUB_CLI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

pub(super) const GITHUB_CLI_GRACE_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(100);

pub(super) const GITHUB_CLI_FORCE_PERIOD: std::time::Duration =
    std::time::Duration::from_millis(400);

pub(super) const GITHUB_CLI_CLEANUP_POLL_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(5);

pub(super) const PULL_REQUEST_REFRESH_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(30);

/// Explicitly retained values for the host-owned `gh` helper. Provider
/// credentials, dynamic-loader controls, and arbitrary dotenv values must not
/// cross this boundary.
pub(super) const GITHUB_CLI_INHERITED_ENVIRONMENT: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GH_HOST",
    "GH_CONFIG_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "CURL_CA_BUNDLE",
    "REQUESTS_CA_BUNDLE",
    "GIT_SSL_CAINFO",
    "GIT_SSL_CAPATH",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_COLLATE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "__CF_USER_TEXT_ENCODING",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
    "SYSTEMROOT",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "TERM",
    "COLORTERM",
];

#[cfg(windows)]
pub(super) const GITHUB_CLI_EXECUTABLE_NAMES: &[&str] = &["gh.exe", "gh"];

#[cfg(not(windows))]
pub(super) const GITHUB_CLI_EXECUTABLE_NAMES: &[&str] = &["gh"];

pub(super) static GITHUB_QUERY_PERMITS: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_GITHUB_QUERIES);

pub(super) fn external_github_path_directory(root: &Path, directory: &Path) -> Option<PathBuf> {
    if !directory.is_absolute() || directory.starts_with(root) {
        return None;
    }
    let directory = directory.canonicalize().ok()?;
    if !directory.is_absolute()
        || directory.starts_with(root)
        || !directory.symlink_metadata().ok()?.is_dir()
    {
        return None;
    }
    Some(directory)
}

pub(super) fn resolve_github_cli_executable_from_path(
    workspace: &Path,
    path: &OsStr,
) -> Option<PathBuf> {
    let root = workspace.canonicalize().ok()?;
    if !root.is_absolute() || !root.symlink_metadata().ok()?.is_dir() {
        return None;
    }
    for raw_directory in std::env::split_paths(path) {
        let Some(directory) = external_github_path_directory(&root, &raw_directory) else {
            continue;
        };
        for name in GITHUB_CLI_EXECUTABLE_NAMES {
            let Ok(candidate) = directory.join(name).canonicalize() else {
                continue;
            };
            if !candidate.is_absolute() || candidate.starts_with(&root) {
                continue;
            }
            let Ok(metadata) = candidate.symlink_metadata() else {
                continue;
            };
            let file_type = metadata.file_type();
            if !file_type.is_file() || file_type.is_symlink() {
                continue;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if metadata.permissions().mode() & 0o111 == 0 {
                    continue;
                }
            }
            return Some(candidate);
        }
    }
    None
}

pub(super) fn resolve_github_cli_executable(workspace: &Path) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    resolve_github_cli_executable_from_path(workspace, &path)
}

pub(super) fn sanitized_github_cli_path_from(workspace: &Path, path: &OsStr) -> Option<OsString> {
    let root = workspace.canonicalize().ok()?;
    let mut directories = Vec::new();
    for raw_directory in std::env::split_paths(path) {
        let Some(directory) = external_github_path_directory(&root, &raw_directory) else {
            continue;
        };
        if !directories.contains(&directory) {
            directories.push(directory);
        }
    }
    (!directories.is_empty())
        .then(|| std::env::join_paths(directories).ok())
        .flatten()
}

pub(super) fn github_cli_environment_from(
    workspace: &Path,
    mut get: impl FnMut(&str) -> Option<OsString>,
    path: Option<&OsStr>,
) -> BTreeMap<OsString, OsString> {
    let mut environment = GITHUB_CLI_INHERITED_ENVIRONMENT
        .iter()
        .filter_map(|name| get(name).map(|value| (OsString::from(*name), value)))
        .collect::<BTreeMap<_, _>>();
    if let Some(path) = path.and_then(|path| sanitized_github_cli_path_from(workspace, path)) {
        environment.insert(OsString::from("PATH"), path);
    }
    environment
}

pub(super) fn github_cli_environment(workspace: &Path) -> BTreeMap<OsString, OsString> {
    let path = std::env::var_os("PATH");
    github_cli_environment_from(workspace, |name| std::env::var_os(name), path.as_deref())
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct PullRequestIdentity {
    pub(super) host: String,
    pub(super) port: u16,
    pub(super) owner: String,
    pub(super) repository: String,
    pub(super) number: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredPullRequest {
    pub(super) session_id: String,
    pub(super) url: String,
    pub(super) number: u64,
    pub(super) state: PullRequestState,
    pub(super) refreshed_at_ms: u64,
}

impl StoredPullRequest {
    pub(super) fn summary(&self) -> PullRequestSummary {
        PullRequestSummary { state: self.state }
    }

    pub(super) fn validate(&self) -> bool {
        SessionId::new(self.session_id.clone()).is_ok()
            && self.number > 0
            && self.refreshed_at_ms > 0
            && pull_request_url_is_valid(&self.url, self.number)
    }
}

pub(super) fn pull_request_identity(value: &str, number: u64) -> Option<PullRequestIdentity> {
    if value.len() > 2_048 || number == 0 {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    let path_segments = url.path_segments()?.collect::<Vec<_>>();
    let host = url.host_str()?;
    let path_matches = path_segments.len() == 4
        && path_segments.iter().all(|segment| !segment.is_empty())
        && path_segments[..2]
            .iter()
            .all(|segment| !segment.contains('%'))
        && path_segments[2] == "pull"
        && path_segments[3] == number.to_string();
    if url.scheme() != "https"
        || url.cannot_be_a_base()
        || host.is_empty()
        || host.ends_with('.')
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !path_matches
    {
        return None;
    }
    Some(PullRequestIdentity {
        host: host.to_ascii_lowercase(),
        port: url.port_or_known_default()?,
        owner: path_segments[0].to_ascii_lowercase(),
        repository: path_segments[1].to_ascii_lowercase(),
        number,
    })
}

pub(super) fn pull_request_url_is_valid(value: &str, number: u64) -> bool {
    pull_request_identity(value, number).is_some()
}

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredPullRequestCatalog {
    pub(super) version: u16,
    #[serde(deserialize_with = "deserialize_unique_pull_request_records")]
    pub(super) records: BTreeMap<String, StoredPullRequest>,
}

pub(super) fn deserialize_unique_pull_request_records<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, StoredPullRequest>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct UniqueRecordsVisitor;

    impl<'de> serde::de::Visitor<'de> for UniqueRecordsVisitor {
        type Value = BTreeMap<String, StoredPullRequest>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a pull-request record map with unique session IDs")
        }

        fn visit_map<A>(self, mut entries: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut records = BTreeMap::new();
            while let Some((session_id, pull_request)) = entries.next_entry()? {
                if records.insert(session_id, pull_request).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate pull-request session ID",
                    ));
                }
            }
            Ok(records)
        }
    }

    deserializer.deserialize_map(UniqueRecordsVisitor)
}

pub(super) struct PullRequestStore {
    pub(super) path: PathBuf,
    pub(super) records: BTreeMap<String, StoredPullRequest>,
    pub(super) catalog_changes: BTreeSet<String>,
    pub(super) deleted_sessions: BTreeSet<String>,
}

impl PullRequestStore {
    pub(super) fn empty(serve_state_dir: &Path) -> Self {
        Self {
            path: serve_state_dir.join(PULL_REQUEST_STORE_FILE),
            records: BTreeMap::new(),
            catalog_changes: BTreeSet::new(),
            deleted_sessions: BTreeSet::new(),
        }
    }

    pub(super) fn open(serve_state_dir: &Path) -> anyhow::Result<Self> {
        let path = serve_state_dir.join(PULL_REQUEST_STORE_FILE);
        let metadata = match path.symlink_metadata() {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::empty(serve_state_dir));
            }
            Err(error) => return Err(error.into()),
        };
        if !metadata.file_type().is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_PULL_REQUEST_STORE_BYTES
        {
            anyhow::bail!("pull-request evidence store is unsafe");
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let file = options.open(&path)?;
        let opened_metadata = file.metadata()?;
        if !opened_metadata.is_file() || opened_metadata.len() > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store changed during validation");
        }
        let mut bytes = Vec::with_capacity(opened_metadata.len() as usize);
        file.take(MAX_PULL_REQUEST_STORE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store is too large");
        }
        let catalog = serde_json::from_slice::<StoredPullRequestCatalog>(&bytes)?;
        let mut identities = BTreeSet::new();
        if catalog.version != PULL_REQUEST_STORE_VERSION
            || catalog.records.len() > MAX_PULL_REQUEST_RECORDS
            || catalog.records.iter().any(|(session_id, record)| {
                session_id != &record.session_id
                    || !record.validate()
                    || match pull_request_identity(&record.url, record.number) {
                        Some(identity) => !identities.insert(identity),
                        None => true,
                    }
            })
        {
            anyhow::bail!("pull-request evidence store is invalid");
        }
        Ok(Self {
            path,
            records: catalog.records,
            catalog_changes: BTreeSet::new(),
            deleted_sessions: BTreeSet::new(),
        })
    }

    pub(super) fn get(&self, session_id: &SessionId) -> Option<StoredPullRequest> {
        self.records.get(session_id.as_str()).cloned()
    }

    pub(super) fn summary(&self, session_id: &SessionId) -> Option<PullRequestSummary> {
        self.records
            .get(session_id.as_str())
            .map(StoredPullRequest::summary)
    }

    pub(super) fn summaries(&self) -> BTreeMap<String, PullRequestSummary> {
        self.records
            .iter()
            .map(|(session_id, pull_request)| (session_id.clone(), pull_request.summary()))
            .collect()
    }

    pub(super) fn refreshable(&self) -> Vec<StoredPullRequest> {
        let mut pull_requests = self
            .records
            .values()
            .filter(|pull_request| pull_request.state != PullRequestState::Merged)
            .cloned()
            .collect::<Vec<_>>();
        // Oldest evidence goes first so a permit race cannot repeatedly favor
        // the same session-ID prefix while the trailing inventory stays stale.
        pull_requests.sort_by(|left, right| {
            left.refreshed_at_ms
                .cmp(&right.refreshed_at_ms)
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        pull_requests
    }

    pub(super) fn take_catalog_changes(&mut self) -> BTreeSet<SessionId> {
        std::mem::take(&mut self.catalog_changes)
            .into_iter()
            .map(|session_id| SessionId::new(session_id).expect("stored pull-request session ID"))
            .collect()
    }

    pub(super) fn replace(
        &mut self,
        session_id: &SessionId,
        pull_request: Option<StoredPullRequest>,
    ) -> anyhow::Result<()> {
        self.transaction(|store| store.replace_unpersisted(session_id, pull_request))
    }

    pub(super) fn delete_session(&mut self, session_id: &SessionId) -> anyhow::Result<()> {
        // A hosted refresh may already be finishing on the blocking pool when
        // actor retirement begins. Fence the identity before removal so that a
        // late first-discovery result cannot recreate evidence after permanent
        // session deletion.
        self.deleted_sessions.insert(session_id.as_str().to_owned());
        if self.records.contains_key(session_id.as_str()) {
            self.replace(session_id, None)?;
        }
        Ok(())
    }

    pub(super) fn replace_unpersisted(
        &mut self,
        session_id: &SessionId,
        pull_request: Option<StoredPullRequest>,
    ) -> anyhow::Result<()> {
        let previous_summary = self.summary(session_id);
        if let Some(pull_request) = pull_request.as_ref() {
            if self.deleted_sessions.contains(session_id.as_str()) {
                anyhow::bail!("pull-request session was permanently deleted");
            }
            if self.records.len() >= MAX_PULL_REQUEST_RECORDS
                && !self.records.contains_key(session_id.as_str())
            {
                anyhow::bail!("pull-request evidence store is full");
            }
            if pull_request.session_id != session_id.as_str() || !pull_request.validate() {
                anyhow::bail!("pull-request evidence is invalid");
            }
            let identity = pull_request_identity(&pull_request.url, pull_request.number)
                .ok_or_else(|| anyhow::anyhow!("pull-request evidence is invalid"))?;
            if self.records.iter().any(|(other_session_id, other)| {
                other_session_id != session_id.as_str()
                    && pull_request_identity(&other.url, other.number).as_ref() == Some(&identity)
            }) {
                anyhow::bail!("pull-request evidence is already associated with another session");
            }
        }
        match pull_request {
            Some(pull_request) => self
                .records
                .insert(session_id.as_str().to_owned(), pull_request),
            None => self.records.remove(session_id.as_str()),
        };
        if self.summary(session_id) != previous_summary {
            self.catalog_changes.insert(session_id.as_str().to_owned());
        }
        Ok(())
    }

    pub(super) fn transaction<T>(
        &mut self,
        update: impl FnOnce(&mut Self) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let previous_records = self.records.clone();
        let previous_catalog_changes = self.catalog_changes.clone();
        let outcome = match update(self) {
            Ok(outcome) => outcome,
            Err(error) => {
                self.records = previous_records;
                self.catalog_changes = previous_catalog_changes;
                return Err(error);
            }
        };
        if self.records == previous_records {
            self.catalog_changes = previous_catalog_changes;
        } else if let Err(error) = self.persist() {
            self.records = previous_records;
            self.catalog_changes = previous_catalog_changes;
            return Err(error);
        }
        Ok(outcome)
    }

    pub(super) fn persist(&self) -> anyhow::Result<()> {
        let bytes = serde_json::to_vec(&StoredPullRequestCatalog {
            version: PULL_REQUEST_STORE_VERSION,
            records: self.records.clone(),
        })?;
        if bytes.len() as u64 > MAX_PULL_REQUEST_STORE_BYTES {
            anyhow::bail!("pull-request evidence store is too large");
        }
        let directory = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("pull-request evidence store has no parent"))?;
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)?;
        let temporary = directory.join(format!(".pull-requests-{}", stable_hash(&random)));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = options.open(&temporary)?;
        let result = (|| -> anyhow::Result<()> {
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &self.path)?;
            std::fs::File::open(directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct GitHubPullRequest {
    pub(super) number: u64,
    pub(super) url: String,
    pub(super) state: String,
    pub(super) is_draft: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum PullRequestObservation {
    Trackable {
        number: u64,
        url: String,
        state: PullRequestState,
    },
    Closed {
        number: u64,
        url: String,
    },
    Unavailable,
}

/// Serving binds a project directory to a host identity, and that binding is
/// only sound when the directory identity can be read back the same way on the
/// next start. Unix gives that for free through inode and device numbers;
/// everywhere else this crate has no stable identity to compare, so the host
/// refuses to start rather than serve a directory it cannot recognise later.
///
/// This lives in its own function so the guard reads as an ordinary fallible
/// precondition. An inline `bail!` under `#[cfg(not(unix))]` left the rest of
/// the constructor unreachable on non-unix targets, which hid the rest of the
/// body from the compiler's reachability analysis on those platforms.
#[cfg(unix)]
pub(super) fn require_stable_directory_identity() -> anyhow::Result<()> {
    Ok(())
}

/// See the unix definition for why this exists.
#[cfg(not(unix))]
pub(super) fn require_stable_directory_identity() -> anyhow::Result<()> {
    anyhow::bail!(
        "octet serve project trust is unavailable on this platform because stable directory identity checks are not implemented"
    )
}

#[derive(Clone)]
pub(super) struct PullRequestRefreshPlan {
    pub(super) workspace: PathBuf,
    pub(super) session_id: SessionId,
    pub(super) pull_requests: Arc<Mutex<PullRequestStore>>,
    pub(super) projection: Arc<Mutex<Option<PullRequestSummary>>>,
    pub(super) discovery_enabled: Arc<AtomicBool>,
    pub(super) refresh_requested: Arc<tokio::sync::Notify>,
    pub(super) process_execution_allowed: bool,
}

impl From<&WorkerPlan> for PullRequestRefreshPlan {
    fn from(plan: &WorkerPlan) -> Self {
        Self {
            workspace: plan.config.workspace.clone(),
            session_id: plan.session_id.clone(),
            pull_requests: Arc::clone(&plan.pull_requests),
            projection: Arc::clone(&plan.pull_request_projection),
            discovery_enabled: Arc::clone(&plan.pull_request_discovery_enabled),
            refresh_requested: Arc::clone(&plan.pull_request_refresh_requested),
            process_execution_allowed: plan.config.sandbox.process_execution_allowed(),
        }
    }
}

pub(super) fn project_github_pull_request(bytes: &[u8]) -> PullRequestObservation {
    let Ok(pull_request) = serde_json::from_slice::<GitHubPullRequest>(bytes) else {
        return PullRequestObservation::Unavailable;
    };
    if pull_request.number == 0
        || !pull_request_url_is_valid(&pull_request.url, pull_request.number)
    {
        return PullRequestObservation::Unavailable;
    }
    let state = match pull_request.state.as_str() {
        "OPEN" if pull_request.is_draft => PullRequestState::InProgress,
        "OPEN" => PullRequestState::Ready,
        "MERGED" => PullRequestState::Merged,
        "CLOSED" => {
            return PullRequestObservation::Closed {
                number: pull_request.number,
                url: pull_request.url,
            };
        }
        _ => return PullRequestObservation::Unavailable,
    };
    PullRequestObservation::Trackable {
        number: pull_request.number,
        url: pull_request.url,
        state,
    }
}

pub(super) async fn query_github_pull_request(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout(workspace, selector, executable, GITHUB_CLI_TIMEOUT)
        .await
}

pub(super) async fn query_hosted_github_pull_request(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout_and_queued_permit(
        workspace,
        selector,
        executable,
        GITHUB_CLI_TIMEOUT,
        &GITHUB_QUERY_PERMITS,
    )
    .await
}

pub(super) async fn query_github_pull_request_with_timeout(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
) -> PullRequestObservation {
    query_github_pull_request_with_timeout_and_permits(
        workspace,
        selector,
        executable,
        timeout,
        &GITHUB_QUERY_PERMITS,
    )
    .await
}

pub(super) async fn query_github_pull_request_with_timeout_and_permits(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    permits: &tokio::sync::Semaphore,
) -> PullRequestObservation {
    let Ok(_permit) = permits.try_acquire() else {
        return PullRequestObservation::Unavailable;
    };
    execute_github_pull_request_query(workspace, selector, executable, timeout).await
}

pub(super) async fn query_github_pull_request_with_timeout_and_queued_permit(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    permits: &tokio::sync::Semaphore,
) -> PullRequestObservation {
    let Ok(_permit) = permits.acquire().await else {
        return PullRequestObservation::Unavailable;
    };
    execute_github_pull_request_query(workspace, selector, executable, timeout).await
}

pub(super) async fn terminate_github_process(
    child: &mut tokio::process::Child,
    process_tree: &ProcessTree,
) {
    process_tree.signal(TerminationSignal::Graceful);
    let graceful_deadline = Instant::now() + GITHUB_CLI_GRACE_PERIOD;
    while Instant::now() < graceful_deadline {
        let child_settled = child.try_wait().ok().flatten().is_some();
        if child_settled && !process_tree.is_alive() {
            process_tree.disarm();
            return;
        }
        tokio::time::sleep(GITHUB_CLI_CLEANUP_POLL_INTERVAL).await;
    }

    process_tree.signal(TerminationSignal::Force);
    // Keep the direct-child fallback for platforms without process groups.
    let _ = child.start_kill();
    let force_deadline = Instant::now() + GITHUB_CLI_FORCE_PERIOD;
    while Instant::now() < force_deadline {
        let child_settled = child.try_wait().ok().flatten().is_some();
        if child_settled && !process_tree.is_alive() {
            process_tree.disarm();
            return;
        }
        tokio::time::sleep(GITHUB_CLI_CLEANUP_POLL_INTERVAL).await;
    }
    // Keep the guard armed through return so Drop makes one final group-wide
    // kill attempt without allowing cleanup to wait on inherited descriptors.
}

pub(super) async fn execute_github_pull_request_query(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
) -> PullRequestObservation {
    let environment = github_cli_environment(workspace);
    execute_github_pull_request_query_with_environment(
        workspace,
        selector,
        executable,
        timeout,
        &environment,
    )
    .await
}

pub(super) async fn execute_github_pull_request_query_with_environment(
    workspace: &Path,
    selector: Option<&str>,
    executable: &Path,
    timeout: std::time::Duration,
    environment: &BTreeMap<OsString, OsString>,
) -> PullRequestObservation {
    let mut command = tokio::process::Command::new(executable);
    command
        .env_clear()
        .envs(environment)
        .args(["pr", "view"])
        .current_dir(workspace)
        .env_remove("GH_REPO")
        .env_remove("GH_FORCE_TTY")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(selector) = selector {
        command.arg(selector);
    }
    command.args(["--json", "number,url,state,isDraft"]);
    isolate_process_group(command.as_std_mut());
    let Ok(mut child) = command.spawn() else {
        return PullRequestObservation::Unavailable;
    };
    let process_tree = ProcessTree::from_process_id(child.id());
    let Some(stdout) = child.stdout.take() else {
        terminate_github_process(&mut child, &process_tree).await;
        return PullRequestObservation::Unavailable;
    };
    let result = tokio::time::timeout(timeout, async {
        let mut bytes = Vec::new();
        let mut bounded = stdout.take(MAX_GITHUB_CLI_OUTPUT_BYTES + 1);
        bounded.read_to_end(&mut bytes).await?;
        drop(bounded);
        if bytes.len() as u64 > MAX_GITHUB_CLI_OUTPUT_BYTES {
            return Ok::<_, std::io::Error>(None);
        }
        let status = child.wait().await?;
        Ok(Some((status, bytes)))
    })
    .await;
    let Ok(Ok(Some((status, bytes)))) = result else {
        terminate_github_process(&mut child, &process_tree).await;
        return PullRequestObservation::Unavailable;
    };
    // A successful read and wait settle the direct child, but a helper may
    // still have escaped without retaining stdout. Do not let it escape.
    process_tree.signal(TerminationSignal::Force);
    process_tree.disarm();
    if !status.success() || bytes.len() as u64 > MAX_GITHUB_CLI_OUTPUT_BYTES {
        return PullRequestObservation::Unavailable;
    }
    project_github_pull_request(&bytes)
}

pub(super) fn apply_pull_request_observation(
    store: &mut PullRequestStore,
    session_id: &SessionId,
    observation: PullRequestObservation,
    refreshed_at_ms: u64,
) -> anyhow::Result<Option<Option<PullRequestSummary>>> {
    store.transaction(|store| {
        apply_pull_request_observation_unpersisted(store, session_id, observation, refreshed_at_ms)
    })
}

pub(super) fn apply_pull_request_observation_unpersisted(
    store: &mut PullRequestStore,
    session_id: &SessionId,
    observation: PullRequestObservation,
    refreshed_at_ms: u64,
) -> anyhow::Result<Option<Option<PullRequestSummary>>> {
    let previous = store.get(session_id);
    if previous
        .as_ref()
        .is_some_and(|pull_request| pull_request.state == PullRequestState::Merged)
    {
        return Ok(None);
    }
    if let Some(previous) = &previous {
        match &observation {
            PullRequestObservation::Trackable { number, url, .. }
            | PullRequestObservation::Closed { number, url }
                if pull_request_identity(&previous.url, previous.number)
                    != pull_request_identity(url, *number) =>
            {
                return Ok(None);
            }
            _ => {}
        }
    }
    let (next, summary) = match observation {
        PullRequestObservation::Trackable { number, url, state } => {
            let stored = StoredPullRequest {
                session_id: session_id.as_str().to_owned(),
                url,
                number,
                state,
                refreshed_at_ms,
            };
            let summary = Some(stored.summary());
            (Some(stored), summary)
        }
        PullRequestObservation::Closed { .. } if previous.is_some() => (None, None),
        PullRequestObservation::Closed { .. } | PullRequestObservation::Unavailable => {
            return Ok(None);
        }
    };
    let previous_summary = previous.as_ref().map(StoredPullRequest::summary);
    store.replace_unpersisted(session_id, next)?;
    Ok((previous_summary != summary).then_some(summary))
}

pub(super) async fn publish_pull_request_projection(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    summary: Option<PullRequestSummary>,
) -> Result<(), ServiceError> {
    {
        let mut projection = plan.projection.lock().map_err(|_| ServiceError::Internal)?;
        if projection.as_ref() == summary.as_ref() {
            return Ok(());
        }
        // Replacement commands read this projection independently of event
        // delivery. Advance it first so an event already observed by the actor
        // can never be overwritten by a replacement built from stale evidence.
        *projection = summary.clone();
    }
    events
        .send(event(EventPayload::SessionPullRequestChanged {
            pull_request: summary,
        }))
        .await
        .map_err(|_| ServiceError::Unavailable)
}

pub(super) async fn refresh_pull_request_projection(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
) -> Result<(), ServiceError> {
    let executable = plan
        .process_execution_allowed
        .then(|| resolve_github_cli_executable(&plan.workspace))
        .flatten();
    refresh_pull_request_projection_inner(plan, events, executable.as_deref()).await
}

pub(super) async fn refresh_pull_request_projection_inner(
    plan: &PullRequestRefreshPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    executable: Option<&Path>,
) -> Result<(), ServiceError> {
    let pull_requests = Arc::clone(&plan.pull_requests);
    let session_id = plan.session_id.clone();
    let previous = tokio::task::spawn_blocking(move || {
        pull_requests
            .lock()
            .map_err(|_| ServiceError::Internal)
            .map(|pull_requests| pull_requests.get(&session_id))
    })
    .await
    .map_err(|_| ServiceError::Internal)??;
    publish_pull_request_projection(
        plan,
        events,
        previous.as_ref().map(StoredPullRequest::summary),
    )
    .await?;
    if previous
        .as_ref()
        .is_some_and(|pull_request| pull_request.state == PullRequestState::Merged)
        || (previous.is_none() && !plan.discovery_enabled.load(Ordering::Acquire))
        || !plan.process_execution_allowed
    {
        return Ok(());
    }
    let Some(executable) = executable else {
        return Ok(());
    };
    let observation = query_hosted_github_pull_request(
        &plan.workspace,
        previous
            .as_ref()
            .map(|pull_request| pull_request.url.as_str()),
        executable,
    )
    .await;
    let pull_requests = Arc::clone(&plan.pull_requests);
    let session_id = plan.session_id.clone();
    let current = tokio::task::spawn_blocking(move || {
        let mut pull_requests = pull_requests.lock().map_err(|_| ServiceError::Internal)?;
        if pull_requests.get(&session_id) == previous {
            apply_pull_request_observation(&mut pull_requests, &session_id, observation, now_ms())
                .map_err(|_| ServiceError::Internal)?;
        }
        Ok::<_, ServiceError>(pull_requests.summary(&session_id))
    })
    .await
    .map_err(|_| ServiceError::Internal)??;
    publish_pull_request_projection(plan, events, current).await
}

pub(super) async fn run_hosted_pull_request_refresh(
    plan: PullRequestRefreshPlan,
    events: mpsc::Sender<TimestampedEvent>,
) {
    if !plan.process_execution_allowed {
        return;
    }
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + PULL_REQUEST_REFRESH_INTERVAL,
        PULL_REQUEST_REFRESH_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = events.closed() => return,
            _ = interval.tick() => {}
            () = plan.refresh_requested.notified() => {}
        }
        let _ = refresh_pull_request_projection(&plan, &events).await;
    }
}

// Only the `gh` boundary tests pin an explicit executable, and those bind real
// processes, so this seam exists on unix test builds only.
#[cfg(all(test, unix))]
pub(super) async fn refresh_pull_request_projection_with_executable(
    plan: &WorkerPlan,
    events: &mpsc::Sender<TimestampedEvent>,
    executable: &Path,
) -> Result<(), ServiceError> {
    refresh_pull_request_projection_inner(
        &PullRequestRefreshPlan::from(plan),
        events,
        Some(executable),
    )
    .await
}

pub(super) fn select_inactive_pull_request_batch(
    refreshable: Vec<StoredPullRequest>,
    hosted: &BTreeSet<SessionId>,
    attempted: &mut BTreeSet<String>,
    capacity: usize,
) -> Vec<StoredPullRequest> {
    let inactive = refreshable
        .into_iter()
        .filter(|pull_request| {
            !hosted
                .iter()
                .any(|session_id| session_id.as_str() == pull_request.session_id)
        })
        .collect::<Vec<_>>();
    let inactive_ids = inactive
        .iter()
        .map(|pull_request| pull_request.session_id.as_str())
        .collect::<BTreeSet<_>>();
    attempted.retain(|session_id| inactive_ids.contains(session_id.as_str()));
    if capacity == 0 || inactive.is_empty() {
        return Vec::new();
    }
    if inactive
        .iter()
        .all(|pull_request| attempted.contains(&pull_request.session_id))
    {
        attempted.clear();
    }
    let batch = inactive
        .into_iter()
        .filter(|pull_request| !attempted.contains(&pull_request.session_id))
        .take(capacity)
        .collect::<Vec<_>>();
    attempted.extend(
        batch
            .iter()
            .map(|pull_request| pull_request.session_id.clone()),
    );
    batch
}

pub(super) async fn refresh_inactive_pull_requests_once(
    host: &Arc<OctetHost>,
    supervisor: &Arc<SessionSupervisor<OctetHost>>,
    pending_catalog: &mut BTreeSet<SessionId>,
    attempted: &mut BTreeSet<String>,
    executable: &Path,
) {
    if !host.config.sandbox.process_execution_allowed() {
        return;
    }
    let hosted = supervisor.hosted_session_ids().await;
    let pull_requests = Arc::clone(&host.pull_requests);
    let refreshable = match tokio::task::spawn_blocking(move || {
        pull_requests
            .lock()
            .map_err(|_| ())
            .map(|pull_requests| pull_requests.refreshable())
    })
    .await
    {
        Ok(Ok(refreshable)) => refreshable,
        Ok(Err(())) | Err(_) => return,
    };
    let workspace = host.config.workspace.clone();
    let executable = executable.to_owned();
    // Match the one-shot batch width to permits available at its start. Keep a
    // round of attempted identities so temporary failures do not pin every
    // later inventory record behind the same oldest evidence.
    let query_concurrency = GITHUB_QUERY_PERMITS
        .available_permits()
        .min(MAX_CONCURRENT_GITHUB_QUERIES);
    let refreshable =
        select_inactive_pull_request_batch(refreshable, &hosted, attempted, query_concurrency);
    let observations = if query_concurrency == 0 {
        Vec::new()
    } else {
        futures_util::stream::iter(refreshable.into_iter().map(|stored| {
            let workspace = workspace.clone();
            let executable = executable.clone();
            async move {
                let observation =
                    query_github_pull_request(&workspace, Some(stored.url.as_str()), &executable)
                        .await;
                (stored, observation)
            }
        }))
        .buffer_unordered(query_concurrency)
        .collect::<Vec<_>>()
        .await
    };

    let refreshed_at_ms = now_ms();
    let pull_requests = Arc::clone(&host.pull_requests);
    let catalog_changes = tokio::task::spawn_blocking(move || {
        let Ok(mut pull_requests) = pull_requests.lock() else {
            return BTreeSet::new();
        };
        let _ = pull_requests.transaction(|pull_requests| {
            for (expected, observation) in observations {
                let session_id = SessionId::new(expected.session_id.clone())
                    .expect("stored pull-request session ID");
                if pull_requests.get(&session_id).as_ref() != Some(&expected) {
                    continue;
                }
                apply_pull_request_observation_unpersisted(
                    pull_requests,
                    &session_id,
                    observation,
                    refreshed_at_ms,
                )?;
            }
            Ok(())
        });
        pull_requests.take_catalog_changes()
    })
    .await
    .unwrap_or_default();
    // A hosted refresh can persist evidence just as its actor retires, after
    // the actor has stopped consuming driver events. Reconcile every durable
    // state change through the inactive ownership fence so such handoffs cannot
    // strand a stale catalog projection, including terminal merges or closure.
    pending_catalog.extend(catalog_changes);

    for session_id in pending_catalog.iter().cloned().collect::<Vec<_>>() {
        let summary_host = Arc::clone(host);
        let summary_session_id = session_id.clone();
        let summary = match tokio::task::spawn_blocking(move || {
            summary_host.stored_session_summary(&summary_session_id)
        })
        .await
        {
            Ok(Ok(summary)) => summary,
            Ok(Err(ServiceError::NotFound)) => {
                pending_catalog.remove(&session_id);
                continue;
            }
            Ok(Err(_)) | Err(_) => continue,
        };
        if let Ok(true) = supervisor.publish_inactive_catalog_summary(summary).await {
            pending_catalog.remove(&session_id);
        }
    }
}

pub(super) async fn run_pull_request_catalog_refresh(
    host: Arc<OctetHost>,
    supervisor: Arc<SessionSupervisor<OctetHost>>,
) {
    if !host.config.sandbox.process_execution_allowed() {
        return;
    }
    let mut pending_catalog = BTreeSet::new();
    let mut attempted = BTreeSet::new();
    let mut interval = tokio::time::interval_at(
        tokio::time::Instant::now() + PULL_REQUEST_REFRESH_INTERVAL,
        PULL_REQUEST_REFRESH_INTERVAL,
    );
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let Some(executable) = resolve_github_cli_executable(&host.config.workspace) else {
            continue;
        };
        refresh_inactive_pull_requests_once(
            &host,
            &supervisor,
            &mut pending_catalog,
            &mut attempted,
            &executable,
        )
        .await;
    }
}
