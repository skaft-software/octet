#![allow(missing_docs)]

//! Interactive OpenAI Codex login: PKCE browser authorization or device code.
//!
//! An earlier octet loopback flow minted credentials that OpenAI routed to a
//! reduced model pool, surfacing as misleading model 404s. This flow therefore
//! mirrors the current Codex CLI's authorize parameters (a `127.0.0.1`
//! redirect on its registered ports) and keeps device code as the fallback.
//! Re-check a browser-minted credential against the live model inventory
//! whenever an authorize parameter changes.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::store::{CredentialFile, CredentialStore, Tokens};
use super::{
    browser, oauth, BROWSER_CALLBACK_PORTS, DEVICE_CODE_TIMEOUT_SECS, DEVICE_VERIFICATION_URI,
    TOKEN_URL,
};

/// Transient presentation facts only: never OAuth tokens, verifiers or callback codes.
/// Each value replaces the previous one in a single-slot watch channel. Device
/// instruction bytes are bounded by the OAuth decoder and checked before publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginProgress {
    Starting,
    Browser {
        url: String,
        callback_port: u16,
    },
    Fallback(LoginFallback),
    Device {
        user_code: String,
        phase: DeviceLoginPhase,
    },
    Exchanging,
    /// Authorization succeeded; waiting for the existing private-store lock.
    Saving,
    /// The credential commit has completed; cancellation cannot undo it.
    SignedIn,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginFallback {
    BusyCallbackPorts,
    NoBrowserOpener,
    LimitedCredential,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceLoginPhase {
    Waiting,
    SlowDown,
    Exchanging,
}

/// Caller-owned presentation and optional browser launch. No callback receives tokens.
/// Pi separation reference: packages/ai/src/auth/types.ts:153-165 at 9c9ce854.
pub trait LoginInteraction: Send + Sync {
    fn notify(&self, progress: LoginProgress);
    fn open_browser(&self, url: &str) -> bool;
    /// Optional caller-owned callback-page presentation. Inputs are static public labels.
    fn browser_page(&self, _title: &str, heading: &str, message: &str) -> String {
        format!("{heading}\n{message}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoginMethod {
    Browser,
    Device,
}

tokio::task_local! {
    static INTERACTION: Arc<dyn LoginInteraction>;
}
fn publish_progress(progress: LoginProgress) {
    let _ = INTERACTION.try_with(|interaction| interaction.notify(progress));
}
pub(super) fn browser_page(title: &str, heading: &str, message: &str) -> String {
    INTERACTION
        .try_with(|interaction| interaction.browser_page(title, heading, message))
        .unwrap_or_else(|_| format!("{heading}\n{message}"))
}
pub(super) fn browser_exchange_started() {
    publish_progress(LoginProgress::Exchanging);
}
fn open_browser(url: &str) -> bool {
    INTERACTION
        .try_with(|interaction| interaction.open_browser(url))
        .unwrap_or(false)
}

/// Explicit login. Dropping the future cancels local pre-commit work.
/// Callbacks own all presentation, browser opening and cancellation input.
pub async fn login_with_interaction(
    store: &CredentialStore,
    method: LoginMethod,
    open_device_browser: bool,
    interaction: Arc<dyn LoginInteraction>,
) -> Result<()> {
    INTERACTION
        .scope(interaction, login_with(store, method, open_device_browser))
        .await
}

async fn browser_listener(ports: &[u16]) -> Result<Option<(tokio::net::TcpListener, u16)>> {
    for &port in ports {
        match browser::bind(browser::callback_address(port)).await {
            Ok(listener) => return Ok(Some((listener, port))),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(error) => {
                return Err(error).context("binding the Codex loopback callback failed");
            }
        }
    }
    publish_progress(LoginProgress::Fallback(LoginFallback::BusyCallbackPorts));
    Ok(None)
}

fn fallback(reason: LoginFallback, _message: &str) {
    publish_progress(LoginProgress::Fallback(reason));
}
async fn login_with(
    store: &CredentialStore,
    method: LoginMethod,
    open_device_browser: bool,
) -> Result<()> {
    if method == LoginMethod::Browser {
        if let Some((listener, port)) = browser_listener(&BROWSER_CALLBACK_PORTS).await? {
            let redirect_uri = browser::redirect_uri(port);
            let authorization = browser::Authorization::generate()?;
            let url = authorization.url(&redirect_uri);
            publish_progress(LoginProgress::Browser {
                url: url.clone(),
                callback_port: port,
            });
            if open_browser(&url) {
                let http = super::http_client();
                match browser::serve(
                    listener,
                    store,
                    &http,
                    TOKEN_URL,
                    &redirect_uri,
                    &authorization,
                )
                .await?
                {
                    browser::BrowserSignIn::Saved => {
                        publish_progress(LoginProgress::SignedIn);
                        return Ok(());
                    }
                    browser::BrowserSignIn::LimitedCredential => fallback(
                        LoginFallback::LimitedCredential,
                        "OpenAI issued a limited (localhost-only) credential through the browser, which cannot reach the ChatGPT model pool; continuing with a device code.",
                    ),
                }
            } else {
                fallback(
                    LoginFallback::NoBrowserOpener,
                    "No browser opener could be started; using a device code instead.",
                );
            }
        }
    }
    login_device(store, !open_device_browser).await?;
    publish_progress(LoginProgress::SignedIn);
    Ok(())
}

pub(super) async fn save_tokens(store: &CredentialStore, tokens: oauth::Tokens) -> Result<()> {
    let account_id = oauth::validate_subscription_token(&tokens.access)
        .context("OpenAI returned a credential that cannot access the ChatGPT model pool")?;
    let credential = CredentialFile {
        tokens: Tokens {
            access_token: tokens.access,
            refresh_token: tokens.refresh,
            account_id,
        },
        expires_at: tokens.expires_at,
    };

    publish_progress(LoginProgress::Saving);
    // Lock contention is cancellable and off the frontend. This worker owns
    // no credential and can only acquire/release the bounded store lock.
    let lock_store = store.clone();
    let lock = tokio::task::spawn_blocking(move || lock_store.lock_refresh())
        .await
        .context("credential-lock worker failed")??;
    // No await between publication and its reported commit fact. A cancelled
    // frontend must never detach a credential-writing worker. The renderer
    // remains independently owned and is never suspended.
    store.save_while_refresh_locked(&credential, &lock)?;
    publish_progress(LoginProgress::SignedIn);
    lock.finish()?;
    Ok(())
}

async fn login_device(store: &CredentialStore, headless: bool) -> Result<()> {
    let http = super::http_client();
    let device = oauth::start_device_auth(&http).await?;

    validate_presented_device_code(&device.user_code)?;
    publish_progress(LoginProgress::Device {
        user_code: device.user_code.clone(),
        phase: DeviceLoginPhase::Waiting,
    });
    if !headless {
        let _ = open_browser(DEVICE_VERIFICATION_URI);
    }

    let deadline = tokio::time::Instant::now() + Duration::from_secs(DEVICE_CODE_TIMEOUT_SECS);
    let mut interval = Duration::from_secs(device.interval_seconds);
    let (authorization_code, code_verifier) = loop {
        if tokio::time::Instant::now() >= deadline {
            bail!("device authorization timed out after 15 minutes");
        }
        match oauth::poll_device_auth(&http, &device).await? {
            oauth::DevicePoll::Complete {
                authorization_code,
                code_verifier,
            } => break (authorization_code, code_verifier),
            oauth::DevicePoll::Pending => {}
            oauth::DevicePoll::SlowDown => {
                interval = interval.saturating_add(Duration::from_secs(5));
                publish_progress(LoginProgress::Device {
                    user_code: device.user_code.clone(),
                    phase: DeviceLoginPhase::SlowDown,
                });
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(interval.min(remaining)).await;
    };

    publish_progress(LoginProgress::Device {
        user_code: device.user_code.clone(),
        phase: DeviceLoginPhase::Exchanging,
    });
    let tokens = oauth::exchange_device_code(&http, &authorization_code, &code_verifier)
        .await
        .context("exchanging the device authorization failed")?;
    save_tokens(store, tokens).await
}

fn validate_presented_device_code(code: &str) -> Result<()> {
    // A verification code is public login instruction, not a token or arbitrary
    // remote document. Reject instead of truncating into an unusable code.
    if code.len() > 256 || code.chars().any(char::is_control) {
        bail!("device authorization returned an invalid verification code");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Observer(tokio::sync::watch::Sender<LoginProgress>);
    impl LoginInteraction for Observer {
        fn notify(&self, progress: LoginProgress) {
            self.0.send_replace(progress);
        }
        fn open_browser(&self, _: &str) -> bool {
            false
        }
    }
    #[tokio::test]
    async fn progress_scope_is_task_local_and_retains_only_latest_fact() {
        assert!(!open_browser("https://example.invalid"));
        let (sender, mut receiver) = tokio::sync::watch::channel(LoginProgress::Starting);
        INTERACTION
            .scope(
                Arc::new(Observer(sender)) as Arc<dyn LoginInteraction>,
                async {
                    browser_exchange_started();
                    assert_eq!(*receiver.borrow(), LoginProgress::Exchanging);
                    publish_progress(LoginProgress::Fallback(LoginFallback::NoBrowserOpener));
                    publish_progress(LoginProgress::Device {
                        user_code: "INERT-1234".into(),
                        phase: DeviceLoginPhase::Waiting,
                    });
                },
            )
            .await;
        assert_eq!(
            *receiver.borrow_and_update(),
            LoginProgress::Device {
                user_code: "INERT-1234".into(),
                phase: DeviceLoginPhase::Waiting
            }
        );
        assert!(receiver.changed().await.is_err());
    }
    #[test]
    fn verification_code_rejects_unbounded_or_control_content() {
        assert!(validate_presented_device_code("INERT-1234").is_ok());
        assert!(validate_presented_device_code(&"A".repeat(257)).is_err());
        assert!(validate_presented_device_code("INERT\u{1b}[31m").is_err());
        assert!(validate_presented_device_code("INERT\n1234").is_err());
    }
    #[tokio::test]
    async fn busy_callback_ports_try_the_registered_fallback_then_device_code() {
        let first = browser::bind(browser::callback_address(0)).await.unwrap();
        let first_port = first.local_addr().unwrap().port();
        let second_port = browser::bind(browser::callback_address(0))
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        assert!(browser_listener(&[first_port]).await.unwrap().is_none());
        let (fallback, port) = browser_listener(&[first_port, second_port])
            .await
            .unwrap()
            .unwrap();
        assert_eq!(port, second_port);
        drop((first, fallback));
        assert_eq!(
            browser_listener(&[first_port, second_port])
                .await
                .unwrap()
                .unwrap()
                .1,
            first_port
        );
    }
    #[test]
    fn callback_ports_match_the_codex_client_registration() {
        assert_eq!(BROWSER_CALLBACK_PORTS, [1455, 1457]);
        assert_eq!(
            browser::redirect_uri(1457),
            "http://127.0.0.1:1457/auth/callback"
        );
    }
}
