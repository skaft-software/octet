#![allow(missing_docs)]

//! Interactive OpenAI Codex login: PKCE browser authorization or device code.

use std::io::{self, IsTerminal as _};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::store::{CredentialFile, CredentialStore, Tokens};
use super::{
    browser, oauth, BROWSER_REDIRECT_URI, DEVICE_CODE_TIMEOUT_SECS, DEVICE_VERIFICATION_URI,
    MODELS, TOKEN_URL,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginMethod {
    Browser,
    Device,
}

fn default_method(headless: bool, ssh: bool, opener_available: bool) -> LoginMethod {
    if headless || ssh || !opener_available {
        LoginMethod::Device
    } else {
        LoginMethod::Browser
    }
}

fn ssh_session() -> bool {
    ["SSH_CONNECTION", "SSH_TTY", "SSH_CLIENT"]
        .iter()
        .any(|key| std::env::var_os(key).is_some())
}

fn opener_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(target_os = "windows") {
        "explorer.exe"
    } else {
        "xdg-open"
    }
}

fn opener_available() -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|path| path.join(opener_name()).is_file())
}

fn choose_method(headless: bool, ssh: bool, opener: bool) -> Result<LoginMethod> {
    let default = default_method(headless, ssh, opener);
    if headless || !io::stdin().is_terminal() {
        return Ok(default);
    }
    crate::output::stdout_multiline(format!(
        "Choose a Codex sign-in method:\n  1) Sign in with your browser (recommended)\n  2) Use a device code (SSH/headless)\nPress Enter for {} or type 1/2:",
        if default == LoginMethod::Browser { "1" } else { "2" }
    ));
    let mut choice = String::new();
    io::stdin()
        .read_line(&mut choice)
        .context("reading login choice failed")?;
    match choice.trim() {
        "" => Ok(default),
        "1" => Ok(LoginMethod::Browser),
        "2" => Ok(LoginMethod::Device),
        _ => bail!("unknown sign-in choice; select 1 for browser or 2 for device code"),
    }
}

async fn browser_listener(
    address: std::net::SocketAddr,
) -> Result<Option<tokio::net::TcpListener>> {
    match browser::bind(address).await {
        Ok(listener) => Ok(Some(listener)),
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
            crate::output::stdout_line(format!(
                "Port {} is already in use; using a device code instead.",
                address.port()
            ));
            Ok(None)
        }
        Err(error) => Err(error).context("binding the Codex loopback callback failed"),
    }
}

/// Sign in via the browser by default when a local opener is available, or use
/// hosted device authorization for SSH, headless mode, or a busy callback port.
pub async fn login(store: &CredentialStore, headless: bool) -> Result<()> {
    let opener = opener_available();
    let method = choose_method(headless, ssh_session(), opener)?;
    if method == LoginMethod::Browser && opener {
        if let Some(listener) = browser_listener(browser::production_address()).await? {
            let authorization = browser::Authorization::generate()?;
            let url = authorization.url(BROWSER_REDIRECT_URI);
            crate::output::stdout_multiline(format!(
                "Opening OpenAI sign-in in your browser. If it does not open, copy this URL:\n\n  {url}\n\nWaiting for a callback on 127.0.0.1:1455 (up to 5 minutes)…"
            ));
            if open_browser(&url) {
                let http = super::http_client();
                browser::serve(
                    listener,
                    store,
                    &http,
                    TOKEN_URL,
                    BROWSER_REDIRECT_URI,
                    &authorization,
                )
                .await?;
                signed_in_notice();
                return Ok(());
            }
            crate::output::stdout_line(
                "No browser opener could be started; using a device code instead.",
            );
        }
    } else if method == LoginMethod::Browser {
        crate::output::stdout_line("No browser opener is available; using a device code instead.");
    }
    login_device(store, headless).await?;
    signed_in_notice();
    Ok(())
}

/// Store both OAuth flows through the same private credential path and claims check.
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
    let save_store = store.clone();
    tokio::task::spawn_blocking(move || save_store.save(&credential))
        .await
        .context("credential-save worker failed")??;
    Ok(())
}

async fn login_device(store: &CredentialStore, headless: bool) -> Result<()> {
    let http = super::http_client();
    let device = oauth::start_device_auth(&http).await?;

    let terminal = crate::output::stdout_is_terminal();
    let user_code = crate::output::table_field(&device.user_code, terminal);
    crate::output::stdout_multiline(format!(
        "Open this URL and enter the code shown below:\n\n  {DEVICE_VERIFICATION_URI}\n\n  Code: {user_code}\n\nWaiting for authorization…"
    ));
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
            }
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(interval.min(remaining)).await;
    };

    let tokens = oauth::exchange_device_code(&http, &authorization_code, &code_verifier)
        .await
        .context("exchanging the device authorization failed")?;
    save_tokens(store, tokens).await
}

fn signed_in_notice() {
    crate::output::stdout_multiline(format!(
        "\nSigned in to OpenAI Codex. Your model catalog will be discovered on startup.\nIf discovery is unavailable, the fallback models are: {}.\nSelect one with `octet --model {}` or set `model = \"{}\"` in ~/.octet/config.toml.",
        MODELS.join(", "), MODELS[0], MODELS[0]
    ));
}

/// Remove the stored credential.
pub async fn logout(store: &CredentialStore) -> Result<()> {
    store.delete_async().await?;
    crate::output::stdout_line("Signed out of OpenAI Codex.");
    Ok(())
}

fn open_browser(url: &str) -> bool {
    let Ok(mut child) = Command::new(opener_name())
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_and_no_opener_default_to_device_without_disabling_browser_choice() {
        assert_eq!(default_method(false, false, true), LoginMethod::Browser);
        assert_eq!(default_method(false, true, true), LoginMethod::Device);
        assert_eq!(default_method(false, false, false), LoginMethod::Device);
        assert_eq!(default_method(true, false, true), LoginMethod::Device);
        assert_eq!(default_method(true, true, false), LoginMethod::Device);
    }

    #[tokio::test]
    async fn busy_browser_port_offers_device_fallback() {
        let occupied = browser::bind(std::net::SocketAddr::from((
            std::net::Ipv4Addr::LOCALHOST,
            0,
        )))
        .await
        .unwrap();
        let address = occupied.local_addr().unwrap();
        assert!(browser_listener(address).await.unwrap().is_none());
        drop(occupied);
        assert!(browser_listener(address).await.unwrap().is_some());
    }
}
