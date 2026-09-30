#![allow(missing_docs)]

//! Interactive OpenAI Codex login: PKCE browser authorization or device code.
//!
//! An earlier octet loopback flow minted credentials that OpenAI routed to a
//! reduced model pool, surfacing as misleading model 404s. This flow therefore
//! mirrors the current Codex CLI's authorize parameters (a `127.0.0.1`
//! redirect on its registered ports) and keeps device code as the fallback.
//! Re-check a browser-minted credential against the live model inventory
//! whenever an authorize parameter changes.

use std::io::{self, IsTerminal as _};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};

use super::store::{CredentialFile, CredentialStore, Tokens};
use super::{
    browser, oauth, BROWSER_CALLBACK_PORTS, DEVICE_CODE_TIMEOUT_SECS, DEVICE_VERIFICATION_URI,
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

fn choose_method(headless: bool, ssh: bool, opener: bool, ask: bool) -> Result<LoginMethod> {
    let default = default_method(headless, ssh, opener);
    if headless || !ask || !io::stdin().is_terminal() {
        if !headless && default == LoginMethod::Browser {
            crate::output::stdout_line(
                "Using browser sign-in. For a device code instead, run `octet --login codex --headless`.",
            );
        }
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

/// Bind the first free registered callback port; `None` means all are busy.
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
    let ports = ports
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" and ");
    crate::output::stdout_line(format!(
        "Callback port {ports} is already in use; using a device code instead."
    ));
    Ok(None)
}

/// Sign in via the browser by default when a local opener is available, or use
/// hosted device authorization for SSH, headless mode, or busy callback ports.
/// An interactive terminal is asked which method to use.
pub async fn login(store: &CredentialStore, headless: bool) -> Result<()> {
    login_with(store, headless, true).await
}

/// [`login`] for callers inside the TUI: never block on a stdin prompt, which
/// could not observe Ctrl+C there. Uses the default method and names the other.
pub async fn login_without_prompt(store: &CredentialStore) -> Result<()> {
    login_with(store, false, false).await
}

async fn login_with(store: &CredentialStore, headless: bool, ask: bool) -> Result<()> {
    let opener = opener_available();
    let method = choose_method(headless, ssh_session(), opener, ask)?;
    if method == LoginMethod::Browser && opener {
        if let Some((listener, port)) = browser_listener(&BROWSER_CALLBACK_PORTS).await? {
            let redirect_uri = browser::redirect_uri(port);
            let authorization = browser::Authorization::generate()?;
            let url = authorization.url(&redirect_uri);
            crate::output::stdout_multiline(format!(
                "Opening OpenAI sign-in in your browser. If it does not open, copy this URL:\n\n  {url}\n\nWaiting for a callback on 127.0.0.1:{port} (up to 5 minutes)…"
            ));
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
                        signed_in_notice();
                        return Ok(());
                    }
                    browser::BrowserSignIn::LimitedCredential => crate::output::stdout_line(
                        "OpenAI issued a limited (localhost-only) credential through the browser, which cannot reach the ChatGPT model pool; continuing with a device code.",
                    ),
                }
            } else {
                crate::output::stdout_line(
                    "No browser opener could be started; using a device code instead.",
                );
            }
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
    crate::output::stdout_multiline(signed_in_message());
}

/// GPT-6 routes always register under the provider's namespace, so the
/// example selection must name `codex/gpt-6.1-sol`: a bare `gpt-6.1-sol` is
/// the public OpenAI API route, which needs an API key.
fn signed_in_message() -> String {
    let catalog_id = |model: &str| {
        if crate::app::bootstrap::known_gpt_6_model(model) {
            format!("{}/{model}", crate::providers::CODEX.id)
        } else {
            model.to_owned()
        }
    };
    let fallback = MODELS
        .iter()
        .map(|model| catalog_id(model))
        .collect::<Vec<_>>()
        .join(", ");
    let selection = catalog_id(MODELS[0]);
    format!(
        "\nSigned in to OpenAI Codex. Your model catalog will be discovered on startup.\nIf discovery is unavailable, the fallback models are: {fallback}.\nSelect one with `octet --model {selection}` or set `model = \"{selection}\"` in ~/.octet/config.toml."
    )
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
    async fn busy_callback_ports_try_the_registered_fallback_then_device_code() {
        let bind = |port| browser::bind(browser::callback_address(port));
        let first = bind(0).await.unwrap();
        let first_port = first.local_addr().unwrap().port();
        let second_port = bind(0).await.unwrap().local_addr().unwrap().port();
        assert!(browser_listener(&[first_port]).await.unwrap().is_none());
        let (fallback, port) = browser_listener(&[first_port, second_port])
            .await
            .unwrap()
            .expect("the fallback port is free");
        assert_eq!(port, second_port);
        drop((first, fallback));
        let (_, port) = browser_listener(&[first_port, second_port])
            .await
            .unwrap()
            .expect("the preferred port is free again");
        assert_eq!(port, first_port);
    }

    #[test]
    fn signed_in_message_names_the_namespaced_codex_route() {
        let message = signed_in_message();
        assert!(
            message.contains("`octet --model codex/gpt-6.1-sol`"),
            "{message}"
        );
        assert!(
            message.contains("model = \"codex/gpt-6.1-sol\""),
            "{message}"
        );
        assert!(
            message.contains("codex/gpt-6-astra, codex/gpt-6-sol"),
            "{message}"
        );
        assert!(message.contains(", gpt-5.5, "), "{message}");
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
