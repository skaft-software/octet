#![allow(missing_docs)]

//! Owner-private credential storage shared by every subscription login.
//!
//! Each provider owns exactly one file under `~/.octet/credentials/`. The file
//! is created `0600` before any secret byte is written, replaced atomically, and
//! never read through a symlink. Refresh-token rotation is serialized by an
//! advisory lock on the directory (Unix) or a stable private lock file (Windows),
//! so two octet processes cannot
//! both spend the same one-shot refresh token and lock the user out.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Largest credential file octet will read from any subscription provider.
const MAX_CREDENTIAL_BYTES: usize = 64 * 1024;

/// Current on-disk schema version.
pub(crate) const CREDENTIAL_VERSION: u8 = 1;

/// How long a caller waits for the cross-process refresh lock before failing
/// closed.
///
/// The lock protects a short critical section: token rotation, and logout. A
/// peer can be suspended, killed while holding it, or waiting on a token
/// endpoint, and none of that belongs in *this* process's readiness path. The
/// wait is therefore deliberately short, acquisition is never blocking, and the
/// failure is explicit rather than a hang.
pub(crate) const REFRESH_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Retry cadence while waiting for the refresh lock. The uncontended path never
/// sleeps: the first attempt either takes the lock or the deadline has passed.
const REFRESH_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Directory holding every provider credential file.
fn credentials_directory() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".octet")
        .join("credentials")
}

/// Default credential path for a provider login selector, such as `grok`.
pub(crate) fn default_path(login: &str) -> PathBuf {
    credentials_directory().join(format!("{login}.json"))
}

/// A stored OAuth credential. `expires_at` is Unix seconds.
///
/// Every field is redacted in `Debug` output: this type is logged, and a
/// derived `Debug` would otherwise put live tokens in a crash report.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct StoredCredential {
    pub version: u8,
    pub access_token: String,
    /// Empty when the provider issues no refresh token (a minted API key, for
    /// example). Such a credential is refreshed by re-minting.
    #[serde(default)]
    pub refresh_token: String,
    pub expires_at: u64,
    /// Provider-granted scope, echoed back by some flows and retained so a
    /// rotated token keeps the grant it was minted under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Non-secret account identity derived from the token, retained for
    /// diagnostics without re-decoding a JWT on every resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl fmt::Debug for StoredCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredCredential")
            .field("version", &self.version)
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("scope", &self.scope)
            .field("account_id", &self.account_id)
            .finish()
    }
}

impl StoredCredential {
    /// Whether the credential is usable without contacting the token endpoint.
    pub(crate) fn is_fresh(&self, skew_secs: u64) -> bool {
        super::wire::now_unix().saturating_add(skew_secs) < self.expires_at
    }

    /// Whether a non-empty refresh token is stored.
    pub(crate) fn has_refresh_token(&self) -> bool {
        !self.refresh_token.trim().is_empty()
    }
}

/// Cross-process refresh serialization guard.
///
/// The lock is held on the credential *directory* rather than the credential
/// file, because octet replaces credential files atomically: a lock taken on
/// the file inode would be silently abandoned by the replacement. Windows uses a
/// dedicated lock file because `LockFileEx` cannot lock a directory.
#[must_use = "the refresh lock must be retained until the protected operation completes"]
pub(crate) struct RefreshLock {
    directory: std::fs::File,
    path: PathBuf,
    locked: bool,
}

impl std::fmt::Debug for RefreshLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The path is user-private state; only whether the lock is held matters.
        formatter
            .debug_struct("RefreshLock")
            .field("path", &"<private>")
            .field("locked", &self.locked)
            .finish()
    }
}

#[cfg(not(unix))]
const REFRESH_LOCK_FILE_NAME: &str = ".subscription-refresh.lock";

fn lock_contention(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
        || error.raw_os_error() == fs2::lock_contended_error().raw_os_error()
}

impl RefreshLock {
    fn release(&mut self) -> Result<()> {
        if !std::mem::replace(&mut self.locked, false) {
            return Ok(());
        }
        fs2::FileExt::unlock(&self.directory)
            .with_context(|| format!("unlocking refresh state {}", self.path.display()))
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        self.release()
    }

    pub(crate) fn finish_with<T>(self, result: Result<T>) -> Result<T> {
        match result {
            Ok(value) => {
                self.finish()?;
                Ok(value)
            }
            Err(error) => Err(error),
        }
    }
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

/// One provider's private credential file.
#[derive(Clone)]
pub(crate) struct OAuthStore {
    path: PathBuf,
    provider_label: &'static str,
}

impl fmt::Debug for OAuthStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The path itself is user-private state; only its shape is safe to log.
        formatter
            .debug_struct("OAuthStore")
            .field("provider", &self.provider_label)
            .field("path", &"<private>")
            .finish()
    }
}

impl OAuthStore {
    pub(crate) fn new(path: impl Into<PathBuf>, provider_label: &'static str) -> Self {
        Self {
            path: path.into(),
            provider_label,
        }
    }

    fn refresh_lock_directory(&self) -> Result<PathBuf> {
        self.path
            .parent()
            .filter(|path| path.is_absolute())
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "credential path has no absolute parent: {}",
                    self.path.display()
                )
            })
    }

    /// Acquire the cross-process refresh lock, waiting at most `wait`.
    ///
    /// Unlike a blocking `flock` this can never wedge the caller: every attempt
    /// is non-blocking and the loop stops at the deadline, so a peer that is
    /// suspended, killed while holding the lock, or waiting on a token endpoint
    /// delays this process by at most `wait`. A timed-out attempt holds nothing
    /// and rotates nothing, leaving the credential exactly as its owner had it.
    pub(crate) fn lock_refresh_within(&self, wait: std::time::Duration) -> Result<RefreshLock> {
        let path = self.refresh_lock_directory()?;
        #[cfg(unix)]
        let directory = octet_agent::secure_fs::open_private_directory_for_lock(&path)
            .with_context(|| format!("opening refresh lock directory {}", path.display()))?;
        #[cfg(not(unix))]
        let directory = {
            let lock_path = path.join(REFRESH_LOCK_FILE_NAME);
            octet_agent::secure_fs::open_private_lock_file(&lock_path)
                .with_context(|| format!("opening refresh lock file {}", lock_path.display()))?
        };
        let deadline = std::time::Instant::now() + wait;
        loop {
            match fs2::FileExt::try_lock_exclusive(&directory) {
                Ok(()) => {
                    return Ok(RefreshLock {
                        directory,
                        path,
                        locked: true,
                    });
                }
                Err(error) if lock_contention(&error) => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        bail!(
                            "another octet process is holding the {} refresh lock on {} \
                             (waited {} ms); refusing to block on it: retry, or stop the other \
                             octet process",
                            self.provider_label,
                            path.display(),
                            wait.as_millis()
                        );
                    }
                    std::thread::sleep(REFRESH_LOCK_POLL.min(remaining));
                }
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("locking refresh state {}", path.display()));
                }
            }
        }
    }

    /// The credential file this store owns, for tests that inspect it directly.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn lock_refresh(&self) -> Result<RefreshLock> {
        self.lock_refresh_within(REFRESH_LOCK_WAIT)
    }

    /// Read the stored credential, or `None` when the user is not signed in.
    pub(crate) fn load(&self) -> Result<Option<StoredCredential>> {
        let Some(bytes) = crate::auth::read_bounded_private(&self.path, MAX_CREDENTIAL_BYTES)
            .with_context(|| format!("reading {}", self.path.display()))?
        else {
            return Ok(None);
        };
        self.parse(&bytes).map(Some)
    }

    /// Read the credential while the caller owns the refresh lock.
    ///
    /// This is the authoritative re-check after acquiring the lock: another
    /// process may have completed a refresh or a logout while this one waited,
    /// and acting on a stale read would double-spend a rotated refresh token.
    pub(crate) fn load_while_refresh_locked(
        &self,
        _lock: &RefreshLock,
    ) -> Result<Option<StoredCredential>> {
        self.load()
    }

    fn parse(&self, bytes: &[u8]) -> Result<StoredCredential> {
        let credential: StoredCredential = serde_json::from_slice(bytes)
            .with_context(|| format!("corrupt credential file {}", self.path.display()))?;
        if credential.version != CREDENTIAL_VERSION {
            bail!(
                "unsupported {} credential version {}; sign in again",
                self.provider_label,
                credential.version
            );
        }
        if credential.access_token.trim().is_empty() {
            bail!(
                "{} credential has no access token; sign in again",
                self.provider_label
            );
        }
        Ok(credential)
    }

    /// Persist a credential, serializing with rotation across processes.
    pub(crate) fn save(&self, credential: &StoredCredential) -> Result<()> {
        let lock = self.lock_refresh()?;
        let result = self.save_while_refresh_locked(credential, &lock);
        lock.finish_with(result)
    }

    /// Persist while the caller owns this store's refresh lock.
    pub(crate) fn save_while_refresh_locked(
        &self,
        credential: &StoredCredential,
        _lock: &RefreshLock,
    ) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("credential path has no parent"))?;
        octet_agent::secure_fs::create_private_directory_all(parent)
            .with_context(|| format!("preparing {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(credential)?;
        octet_agent::secure_fs::write_private_atomic(&self.path, &bytes, MAX_CREDENTIAL_BYTES)
            .map_err(anyhow::Error::from)
            .with_context(|| format!("writing {}", self.path.display()))
    }

    /// Remove exactly this provider's credential.
    pub(crate) fn delete(&self) -> Result<()> {
        let lock = self.lock_refresh()?;
        let result = remove_if_present(&self.path);
        lock.finish_with(result)
    }

    pub(crate) async fn delete_async(&self) -> Result<()> {
        let store = self.clone();
        tokio::task::spawn_blocking(move || store.delete())
            .await
            .context("credential-delete worker failed")?
    }
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(anyhow::Error::from(error)).with_context(|| format!("removing {}", path.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_store(label: &'static str) -> (OAuthStore, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        // Elevated Windows runners may give tempfile's root to Administrators.
        // Have secure_fs create a current-user-owned child, as production does.
        let credentials = directory.path().join("credentials");
        octet_agent::secure_fs::create_private_directory_all(&credentials).unwrap();
        let store = OAuthStore::new(credentials.join("credential.json"), label);
        (store, directory)
    }

    /// Write raw bytes the way the store does, so the reader's permission check
    /// is exercised rather than bypassed by a world-readable `fs::write`.
    fn write_bytes(store: &OAuthStore, bytes: &[u8]) {
        octet_agent::secure_fs::write_private_atomic(
            store.path(),
            bytes,
            super::MAX_CREDENTIAL_BYTES,
        )
        .unwrap();
    }

    fn credential() -> StoredCredential {
        StoredCredential {
            version: CREDENTIAL_VERSION,
            access_token: "access-sentinel".into(),
            refresh_token: "refresh-sentinel".into(),
            expires_at: super::super::wire::now_unix() + 3600,
            scope: Some("grok-cli:access".into()),
            account_id: None,
        }
    }

    #[test]
    fn a_credential_round_trips_through_owner_only_storage() {
        let (store, _guard) = temporary_store("grok");
        assert!(store.load().unwrap().is_none());
        store.save(&credential()).unwrap();

        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.access_token, "access-sentinel");
        assert_eq!(loaded.refresh_token, "refresh-sentinel");
        assert_eq!(loaded.scope.as_deref(), Some("grok-cli:access"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o077,
                0,
                "credential must not be group or world readable"
            );
        }
    }

    #[test]
    fn a_corrupt_or_foreign_version_credential_is_refused_not_guessed() {
        let (store, _guard) = temporary_store("grok");
        write_bytes(&store, b"{ not json");
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("corrupt credential file"), "{error}");

        // A credential with no expiry cannot be refreshed on time, so it is
        // refused rather than treated as never expiring.
        write_bytes(&store, br#"{"version":1,"access_token":"a"}"#);
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("corrupt credential file"), "{error}");

        write_bytes(
            &store,
            br#"{"version":99,"access_token":"a","expires_at":1}"#,
        );
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("unsupported"), "{error}");

        write_bytes(
            &store,
            br#"{"version":1,"access_token":"   ","expires_at":1}"#,
        );
        let error = store.load().unwrap_err().to_string();
        assert!(error.contains("no access token"), "{error}");

        // A world-readable credential file is refused before it is even parsed.
        write_bytes(
            &store,
            br#"{"version":1,"access_token":"a","expires_at":4102444800}"#,
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                store.load().is_err(),
                "a loose credential file must be refused"
            );
        }
    }

    #[test]
    fn a_credential_is_absent_after_logout_and_logout_is_idempotent() {
        let (store, _guard) = temporary_store("grok");
        store.save(&credential()).unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
        store.delete().unwrap();
    }

    #[test]
    fn debug_output_never_contains_a_token() {
        let rendered = format!("{:?}", credential());
        assert!(!rendered.contains("access-sentinel"), "{rendered}");
        assert!(!rendered.contains("refresh-sentinel"), "{rendered}");
        assert!(rendered.contains("[REDACTED]"), "{rendered}");

        let (store, _guard) = temporary_store("grok");
        let rendered = format!("{store:?}");
        assert!(!rendered.contains(".octet"), "{rendered}");
    }

    #[test]
    fn freshness_honours_the_providers_refresh_skew() {
        let now = super::super::wire::now_unix();
        let mut stored = credential();
        stored.expires_at = now + 30;
        assert!(
            !stored.is_fresh(60),
            "expiring inside the skew must refresh"
        );
        stored.expires_at = now + 600;
        assert!(stored.is_fresh(60));
        assert!(stored.has_refresh_token());
        stored.refresh_token = "  ".into();
        assert!(!stored.has_refresh_token());
    }

    #[test]
    fn a_contended_refresh_lock_fails_closed_without_waiting_forever() {
        let (store, _guard) = temporary_store("grok");
        let held = store.lock_refresh().unwrap();
        let error = store
            .lock_refresh_within(std::time::Duration::from_millis(60))
            .unwrap_err()
            .to_string();
        assert!(error.contains("another octet process"), "{error}");
        assert!(error.contains("grok"), "{error}");
        // The contended attempt must leave no phantom holder behind.
        drop(held);
        let _reacquired = store.lock_refresh().unwrap();
    }

    #[test]
    fn refresh_replacement_keeps_the_lock_until_the_new_credential_is_durable() {
        let (store, _guard) = temporary_store("grok");
        store.save(&credential()).unwrap();
        let held = store.lock_refresh().unwrap();
        let mut rotated = store.load_while_refresh_locked(&held).unwrap().unwrap();
        rotated.access_token = "rotated-access".into();
        rotated.refresh_token = "rotated-refresh".into();
        store.save_while_refresh_locked(&rotated, &held).unwrap();
        assert!(store
            .lock_refresh_within(std::time::Duration::ZERO)
            .is_err());
        held.finish().unwrap();
        let lock = store.lock_refresh().unwrap();
        let loaded = store.load_while_refresh_locked(&lock).unwrap().unwrap();
        assert_eq!(loaded.access_token, "rotated-access");
        assert_eq!(loaded.refresh_token, "rotated-refresh");
        lock.finish().unwrap();
        store.delete().unwrap();
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn default_paths_are_provider_scoped_private_files() {
        let path = default_path("grok");
        assert!(path.ends_with("credentials/grok.json"), "{path:?}");
        assert_ne!(default_path("grok"), default_path("kimi"));
    }
}
