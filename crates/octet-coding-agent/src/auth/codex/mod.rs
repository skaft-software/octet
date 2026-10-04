#![allow(missing_docs)]

//! OpenAI Codex ("Sign in with ChatGPT") OAuth.
//!
//! Constants and the flow are grounded in the live Codex CLI and cross-checked
//! against the provider's device-authorization behavior. The subscription
//! backend is `https://chatgpt.com/backend-api/codex/`, wire-compatible with
//! octet's existing `Protocol::OpenAiResponses` codec; Codex-specific headers
//! are composed by the endpoint, the request codec's session affinity, and the
//! resolver's dynamic account routing.

mod browser;
mod login;
mod oauth;
mod resolver;
mod store;

pub use login::{login, login_without_prompt, logout};
pub(crate) use login::{
    login_progress_channel, login_with_progress, DeviceLoginPhase, LoginFallback, LoginProgress,
};
pub(crate) use oauth::{ChatGptPlan, SubscriptionClaims};
pub use resolver::CodexResolver;
pub(crate) use store::REFRESH_LOCK_WAIT;
pub use store::{default_path, CredentialStore};

/// Public OAuth client id of the Codex CLI (not a secret).
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Token endpoint (device code, browser code exchange + refresh).
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// Registered loopback callback ports for browser sign-in, in preference order.
/// The OAuth client registers exactly these; the Codex CLI uses the same pair.
pub const BROWSER_CALLBACK_PORTS: [u16; 2] = [1455, 1457];
/// The same originator value sent to the Codex backend in declarations.json.
pub const ORIGINATOR: &str = "octet";
/// OAuth device authorization start endpoint.
pub const DEVICE_USER_CODE_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
/// OAuth device authorization polling endpoint.
pub const DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
/// OpenAI-hosted device verification page.
pub const DEVICE_VERIFICATION_URI: &str = "https://auth.openai.com/codex/device";
/// Redirect used when exchanging a completed device authorization.
pub const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
/// Device codes expire after fifteen minutes.
pub const DEVICE_CODE_TIMEOUT_SECS: u64 = 15 * 60;
/// JWT claim namespace carrying the ChatGPT account id.
pub const JWT_AUTH_CLAIM: &str = "https://api.openai.com/auth";
/// Endpoint id registered in the model catalog.
pub const ENDPOINT_ID: &str = "openai-codex";

/// Models used only as a fallback when the live Codex catalog cannot be
/// reached. The authenticated `/models` response is authoritative at runtime,
/// so account-specific and newly released models are not gated on this list.
pub const MODELS: &[&str] = &[
    "gpt-6.1-sol",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-luna",
    "gpt-5.6-sol",
    "gpt-5.6-terra",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
];

/// Seconds before nominal expiry at which a token is proactively refreshed.
pub const REFRESH_SKEW_SECS: u64 = 60;

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("static Codex HTTP client settings are valid")
}

/// Return the account and plan claims for a usable ChatGPT subscription
/// credential. Legacy browser credentials marked `localhost` are rejected
/// because OpenAI routes them as local/free credentials and returns misleading
/// model 404s.
pub(crate) fn usable_subscription_claims(
    store: &CredentialStore,
) -> anyhow::Result<Option<SubscriptionClaims>> {
    let Some(credential) = store.load()? else {
        return Ok(None);
    };
    if credential.tokens.refresh_token.trim().is_empty() {
        anyhow::bail!("OpenAI Codex credential has no refresh token; sign in again");
    }
    Ok(Some(oauth::subscription_claims(
        &credential.tokens.access_token,
    )?))
}

/// Seconds since the Unix epoch.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
