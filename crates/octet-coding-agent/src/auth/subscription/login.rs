#![allow(missing_docs)]

//! The shared login and logout driver.
//!
//! Presentation is deliberately plain stdout: both the CLI and the first-run
//! onboarding suspend the renderer and run these flows, so a device code, an
//! authorize URL, and a pasted redirect are all read and written on the
//! terminal itself. A secret is never echoed back and no provider response text
//! is quoted to the user.

use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use super::flow::SubscriptionFlow;
use super::store::{OAuthStore, StoredCredential};

/// Run a provider's interactive login and persist the credential.
///
/// `headless` suppresses only the best-effort browser launch. The verification
/// URL and code are always printed, so the flow also works over SSH and in a
/// container with no browser.
pub(crate) async fn login(
    flow: &Arc<dyn SubscriptionFlow>,
    store: &OAuthStore,
    headless: bool,
) -> Result<()> {
    let http = super::wire::http_client();
    let credential = flow.authorize(&http, headless).await?;
    // Persist through a blocking worker so a large or slow filesystem cannot
    // stall the runtime, and so a cancelled login still commits the credential
    // the provider already issued rather than dropping it on the floor.
    let committed = store.clone();
    let persisted = credential.clone();
    tokio::task::spawn_blocking(move || committed.save(&persisted))
        .await
        .context("credential-save worker failed")??;
    describe_success(flow, &credential);
    Ok(())
}

/// Remove a provider's stored credential. No network call, no remote revocation.
pub(crate) async fn logout(flow: &Arc<dyn SubscriptionFlow>, store: &OAuthStore) -> Result<()> {
    store.delete_async().await?;
    crate::output::stdout_line(format!("Signed out of {}.", flow.label()));
    Ok(())
}

/// Report what a completed login now has access to.
///
/// The model inventory is not named here: it is discovered from the provider on
/// the next catalog build, and quoting a list discovered at some earlier moment
/// would be a claim octet cannot stand behind.
fn describe_success(flow: &Arc<dyn SubscriptionFlow>, credential: &StoredCredential) {
    let refreshable = if credential.has_refresh_token() {
        ""
    } else {
        " This provider issues no refresh token, so octet will ask you to sign in again when the\ncurrent key expires."
    };
    crate::output::stdout_multiline(format!(
        "\nSigned in to {}.{refreshable}\nSelect one of its models with `octet --model {}/<model>` or run `/model`.",
        flow.label(),
        flow.provider_id(),
    ));
}

/// Best-effort open of a provider-hosted page.
///
/// Failure is ignored on purpose: the URL was already printed, and a headless
/// machine simply has nothing to launch.
pub(crate) fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer"
    } else {
        "xdg-open"
    };
    let _ = spawn_browser(
        std::process::Command::new(opener)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    );
}

const MAX_BROWSER_OPENERS: usize = 2;
const BROWSER_OPENER_TIMEOUT: Duration = Duration::from_secs(5);
static ACTIVE_BROWSER_OPENERS: AtomicUsize = AtomicUsize::new(0);

fn spawn_browser(command: &mut std::process::Command) -> bool {
    spawn_browser_with(command, BROWSER_OPENER_TIMEOUT, MAX_BROWSER_OPENERS)
}

fn spawn_browser_with(
    command: &mut std::process::Command,
    timeout: Duration,
    limit: usize,
) -> bool {
    if !reserve_browser_opener(limit) {
        return false;
    }
    let (send_child, receive_child) = std::sync::mpsc::sync_channel(1);
    if std::thread::Builder::new()
        .name("octet-browser-opener".to_owned())
        .spawn(move || {
            if let Ok(Some(mut child)) = receive_child.recv() {
                supervise_browser_opener(&mut child, timeout);
            } else {
                ACTIVE_BROWSER_OPENERS.fetch_sub(1, Ordering::AcqRel);
            }
        })
        .is_err()
    {
        ACTIVE_BROWSER_OPENERS.fetch_sub(1, Ordering::AcqRel);
        return false;
    }
    // The owner exists before any child can be spawned, so every successful
    // spawn has an owner even if subsequent local work is cancelled.
    match command.spawn() {
        Ok(child) => {
            let _ = send_child.send(Some(child));
            true
        }
        Err(_) => {
            let _ = send_child.send(None);
            false
        }
    }
}

fn reserve_browser_opener(limit: usize) -> bool {
    let mut active = ACTIVE_BROWSER_OPENERS.load(Ordering::Acquire);
    loop {
        if active >= limit {
            return false;
        }
        match ACTIVE_BROWSER_OPENERS.compare_exchange_weak(
            active,
            active + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(current) => active = current,
        }
    }
}

fn supervise_browser_opener(child: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    ACTIVE_BROWSER_OPENERS.fetch_sub(1, Ordering::AcqRel);
}

/// Prompt for the authorization code a user pasted back from the browser.
///
/// Reads a single line from stdin, bounded so a pasted redirect URL or an
/// oversized paste cannot grow without limit, and refuses control characters so
/// a crafted paste cannot rewrite the terminal.
pub(crate) async fn read_pasted_redirect(
    prompt: &str,
    expected_state: &str,
) -> Result<(String, String)> {
    crate::output::stdout_multiline(prompt.to_owned());
    let line = pasted_input::read_line(MAX_PASTE_BYTES as usize)
        .await
        .context("reading the pasted authorization redirect failed")?;
    if line.is_empty() {
        bail!("no authorization redirect was pasted; sign in again");
    }
    if line.len() as u64 >= MAX_PASTE_BYTES && !line.ends_with('\n') {
        bail!("the pasted authorization redirect is too long");
    }
    let value = line.trim();
    if value.is_empty() {
        bail!("no authorization redirect was pasted; sign in again");
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\t')
    {
        bail!("the pasted authorization redirect contained control characters");
    }
    super::pkce::parse_pasted_redirect(value, expected_state)
}

/// Longest single-line paste octet accepts for a browser redirect.
const MAX_PASTE_BYTES: u64 = 16 * 1024;

mod pasted_input;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::subscription::flow::{RefreshMode, SubscriptionFlow};
    use crate::auth::subscription::store::CREDENTIAL_VERSION;
    use crate::auth::subscription::wire::Encoding;

    /// A flow whose `authorize` either fails or yields a fixed credential, so
    /// the login driver's persistence contract can be checked without a network.
    #[derive(Debug)]
    struct StubFlow {
        outcome: std::result::Result<(), &'static str>,
    }

    #[async_trait::async_trait]
    impl SubscriptionFlow for StubFlow {
        fn provider_id(&self) -> &'static str {
            "stub-subscription"
        }
        fn label(&self) -> &'static str {
            "Stub Provider"
        }
        fn login(&self) -> &'static str {
            "stub"
        }
        fn refresh_skew_secs(&self) -> u64 {
            60
        }
        fn token_encoding(&self) -> Encoding {
            Encoding::Form
        }
        fn refresh_mode(&self) -> RefreshMode {
            RefreshMode::Rotating
        }
        fn fallback_token_lifetime_secs(&self) -> u64 {
            3600
        }
        async fn authorize(
            &self,
            _http: &reqwest::Client,
            _headless: bool,
        ) -> Result<StoredCredential> {
            match self.outcome {
                Err(message) => Err(anyhow::anyhow!("{message}")),
                Ok(()) => Ok(StoredCredential {
                    version: CREDENTIAL_VERSION,
                    access_token: "stub-access-SENTINEL".into(),
                    refresh_token: "stub-refresh-SENTINEL".into(),
                    expires_at: super::super::wire::now_unix() + 3600,
                    scope: None,
                    account_id: None,
                }),
            }
        }
        async fn refresh(
            &self,
            _http: &reqwest::Client,
            _credential: &StoredCredential,
        ) -> Result<StoredCredential> {
            bail!("the stub flow never refreshes")
        }
    }

    fn stub(
        outcome: std::result::Result<(), &'static str>,
    ) -> (Arc<dyn SubscriptionFlow>, OAuthStore, tempfile::TempDir) {
        let guard = tempfile::tempdir().unwrap();
        let store = OAuthStore::new(
            guard.path().join("credentials/credential.json"),
            "Stub Provider",
        );
        (Arc::new(StubFlow { outcome }), store, guard)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_hung_browser_opener_is_killed_reaped_and_admission_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("pid");
        let mut command = std::process::Command::new("sh");
        command
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1\"; exec sleep 30",
                "fixture-opener",
            ])
            .arg(&pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        assert!(spawn_browser_with(
            &mut command,
            Duration::from_millis(150),
            1
        ));
        assert!(!spawn_browser_with(
            std::process::Command::new("sh").arg("-c").arg("exit 0"),
            Duration::from_millis(150),
            1
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            let pid = loop {
                if let Ok(pid) = std::fs::read_to_string(&pid_file) {
                    if let Ok(pid) = pid.parse::<libc::pid_t>() {
                        break pid;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            loop {
                // SAFETY: signal zero only inspects process existence. Zombies
                // still exist, so ESRCH proves the owner reaped the opener.
                if unsafe { libc::kill(pid, 0) } < 0 {
                    assert_eq!(
                        std::io::Error::last_os_error().raw_os_error(),
                        Some(libc::ESRCH)
                    );
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while ACTIVE_BROWSER_OPENERS.load(Ordering::Acquire) != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!spawn_browser(&mut std::process::Command::new(
            directory.path().join("missing-opener")
        )));
    }

    #[tokio::test]
    async fn a_failed_authorization_writes_no_credential() {
        let (flow, store, _guard) = stub(Err("the user denied the request"));
        let error = login(&flow, &store, true).await.unwrap_err();
        assert!(format!("{error:#}").contains("denied"), "{error:#}");
        // Nothing may be created for a login the provider never completed, or a
        // later launch would advertise models the user cannot actually call.
        assert!(store.load().unwrap().is_none());
        assert!(!store.path().exists());
    }

    #[tokio::test]
    async fn a_completed_authorization_persists_before_reporting_success() {
        let (flow, store, _guard) = stub(Ok(()));
        login(&flow, &store, true).await.unwrap();
        let stored = store
            .load()
            .unwrap()
            .expect("a successful login must persist");
        assert_eq!(stored.access_token, "stub-access-SENTINEL");
        assert_eq!(stored.refresh_token, "stub-refresh-SENTINEL");
    }

    #[tokio::test]
    async fn logout_removes_the_credential_and_is_safe_when_absent() {
        let (flow, store, _guard) = stub(Ok(()));
        login(&flow, &store, true).await.unwrap();
        logout(&flow, &store).await.unwrap();
        assert!(store.load().unwrap().is_none());
        logout(&flow, &store).await.unwrap();
    }
}
