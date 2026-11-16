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

use anyhow::{bail, Context, Result};
use tokio::sync::watch;

use super::CredentialStore;
use super::{DEVICE_VERIFICATION_URI, MODELS};

use super::{DeviceLoginPhase, LoginFallback, LoginMethod, LoginProgress};
pub(crate) fn login_progress_channel(
) -> (watch::Sender<LoginProgress>, watch::Receiver<LoginProgress>) {
    watch::channel(LoginProgress::Starting)
}

tokio::task_local! {
    // Browser callbacks call save_tokens through the existing token-validation
    // path. This task-local scope lets that path publish the exact commit fact
    // without introducing process-global or shared ShellState authentication.
    static NATIVE_LOGIN_PROGRESS: watch::Sender<LoginProgress>;
}

fn publish_progress(progress: LoginProgress) {
    let _ = NATIVE_LOGIN_PROGRESS.try_with(|sender| sender.send_replace(progress));
}

fn native_presentation() -> bool {
    NATIVE_LOGIN_PROGRESS.try_with(|_| ()).is_ok()
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
    crate::auth::subscription::login::BROWSER_OPENER
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
        if !native_presentation() && !headless && default == LoginMethod::Browser {
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

/// Frontend-owned login: no stdin read or stdout authentication instructions.
/// Dropping this future cancels pre-commit work. The short credential commit is
/// synchronous in this scope, so no detached save worker can outlive cancellation;
/// SignedIn is published immediately after that commit, before the browser's
/// asynchronous success-page write. CLI login retains its existing output/worker.
/// Browser opening and provider traffic still belong to the existing OAuth flow.
pub(crate) async fn login_with_progress(
    store: &CredentialStore,
    progress: watch::Sender<LoginProgress>,
) -> Result<()> {
    NATIVE_LOGIN_PROGRESS
        .scope(progress, login_with(store, false, false))
        .await
}

struct Presentation {
    progress: Option<watch::Sender<LoginProgress>>,
    headless: bool,
}
impl super::LoginInteraction for Presentation {
    fn notify(&self, progress: LoginProgress) {
        if let Some(sender) = &self.progress {
            sender.send_replace(progress);
            return;
        }
        match progress {
            LoginProgress::Browser { url, callback_port } => crate::output::stdout_multiline(format!(
                "Opening OpenAI sign-in in your browser. If it does not open, copy this URL:\n\n  {url}\n\nWaiting for a callback on 127.0.0.1:{callback_port} (up to 5 minutes)…")),
            LoginProgress::Device { user_code, phase: DeviceLoginPhase::Waiting } => {
                let user_code = crate::output::table_field(&user_code, crate::output::stdout_is_terminal());
                crate::output::stdout_multiline(format!("Open this URL and enter the code shown below:\n\n  {DEVICE_VERIFICATION_URI}\n\n  Code: {user_code}\n\nWaiting for authorization…"));
            }
            LoginProgress::Fallback(reason) => crate::output::stdout_line(match reason {
                LoginFallback::BusyCallbackPorts => "Callback port 1455 and 1457 is already in use; using a device code instead.",
                LoginFallback::NoBrowserOpener => "No browser opener could be started; using a device code instead.",
                LoginFallback::LimitedCredential => "OpenAI issued a limited (localhost-only) credential through the browser, which cannot reach the ChatGPT model pool; continuing with a device code.",
            }),
            _ => {}
        }
    }
    fn open_browser(&self, url: &str) -> bool {
        if self.headless {
            false
        } else {
            open_browser(url)
        }
    }
}
async fn login_with(store: &CredentialStore, headless: bool, ask: bool) -> Result<()> {
    let method = choose_method(headless, ssh_session(), opener_available(), ask)?;
    let presentation = std::sync::Arc::new(Presentation {
        progress: NATIVE_LOGIN_PROGRESS.try_with(Clone::clone).ok(),
        headless,
    });
    tokio::select! {
        result = super::login_with_interaction(store, method, !headless, presentation) => result?,
        _ = crate::tui::terminal::wait_for_shutdown_signal() => bail!("browser sign-in cancelled"),
    }
    signed_in_notice();
    Ok(())
}

fn signed_in_notice() {
    if !native_presentation() {
        crate::output::stdout_multiline(signed_in_message());
    }
    publish_progress(LoginProgress::SignedIn);
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
    let Ok(mut child) = crate::auth::subscription::login::browser_command(url).spawn() else {
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
}
