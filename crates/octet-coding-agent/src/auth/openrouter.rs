#![allow(missing_docs)]

//! OpenRouter ("Sign in with OpenRouter") OAuth login.
//!
//! This is not a subscription grant. OpenRouter exchanges an authorization code
//! for a durable, user-owned API key rather than a short-lived access/refresh
//! pair, so the credential octet stores never expires and is never renewed —
//! revoking it is done in the OpenRouter dashboard.
//!
//! The flow is a PKCE authorization-code grant with a loopback `callback_url`.
//! Octet does not bind that port: the browser lands on nothing, and the user
//! pastes the final redirect URL back into the terminal. OpenRouter's authorize
//! endpoint carries no `state` parameter, so state cannot be validated; the
//! PKCE verifier is the sole binding between the authorize request and the token
//! exchange, and the authorization code is single-use either way.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::subscription::flow::{reconcile, RefreshMode, SubscriptionFlow};
use super::subscription::store::StoredCredential;
use super::subscription::wire::{
    bounded_body, https_url, now_unix, request_failed, Encoding, Tokens,
};

/// OpenRouter's authorization endpoint.
const AUTHORIZE_URL: &str = "https://openrouter.ai/auth";

/// OpenRouter's code-for-key endpoint.
const TOKEN_URL: &str = "https://openrouter.ai/api/v1/auth/keys";

/// Loopback redirect presented to OpenRouter. Nothing listens on it; the user
/// copies the resulting URL out of their browser instead.
const REDIRECT_URI: &str = "http://localhost:53693/openrouter/callback";

/// Lifetime recorded for a durable key.
///
/// OpenRouter does not expire these, so the value is deliberately far in the
/// future: it exists so the stored credential has a finite, inspectable expiry
/// and can never be mistaken for a token octet should expect to refresh.
const KEY_LIFETIME_SECS: u64 = 100 * 365 * 24 * 60 * 60;

/// The OpenRouter OAuth flow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OpenRouterFlow;

#[async_trait]
impl SubscriptionFlow for OpenRouterFlow {
    fn provider_id(&self) -> &'static str {
        crate::providers::OPENROUTER_OAUTH.id
    }

    fn label(&self) -> &'static str {
        "OpenRouter"
    }

    fn login(&self) -> &'static str {
        "openrouter"
    }

    fn refresh_skew_secs(&self) -> u64 {
        0
    }

    fn token_encoding(&self) -> Encoding {
        Encoding::Json
    }

    fn refresh_mode(&self) -> RefreshMode {
        RefreshMode::Never
    }

    fn fallback_token_lifetime_secs(&self) -> u64 {
        KEY_LIFETIME_SECS
    }

    async fn authorize(&self, http: &reqwest::Client, headless: bool) -> Result<StoredCredential> {
        let pkce = super::subscription::pkce::generate()?;
        let url = authorize_url(&pkce.challenge)?;
        if !headless {
            super::subscription::login::open_browser(url.as_str());
        } else {
            crate::output::stdout_multiline(format!("Open this URL to authorize:\n\n  {url}\n"));
        }
        let (code, _) = super::subscription::login::read_pasted_redirect(
            "Approve access in your browser, then paste the URL your browser was redirected to.\n\
             That page will fail to load; this is expected because octet is not listening.\n\n\
             Paste redirect URL: ",
            // OpenRouter does not round-trip a `state`, so there is nothing to
            // compare against. The PKCE verifier below is what binds the code to
            // this login attempt.
            "",
        )?;
        self.exchange(http, &code, &pkce.verifier).await
    }

    async fn refresh(
        &self,
        _http: &reqwest::Client,
        _credential: &StoredCredential,
    ) -> Result<StoredCredential> {
        // Unreachable in practice: a durable key records a lifetime far beyond
        // any refresh skew, so the resolver never asks for a renewal. Saying so
        // is better than returning the credential and pretending an exchange
        // happened.
        bail!(
            "{} issues durable API keys that do not expire; sign in again to replace it",
            self.label()
        )
    }
}

impl OpenRouterFlow {
    /// Trade an authorization code for a durable API key.
    async fn exchange(
        &self,
        http: &reqwest::Client,
        code: &str,
        verifier: &str,
    ) -> Result<StoredCredential> {
        let response = http
            .post(TOKEN_URL)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .json(&serde_json::json!({
                "code": code,
                "code_verifier": verifier,
                "code_challenge_method": "S256",
            }))
            .send()
            .await
            .context("OpenRouter token exchange request failed")?;
        let status = response.status();
        let body = bounded_body(response, "OpenRouter token exchange").await?;
        if !status.is_success() {
            return Err(request_failed("OpenRouter token exchange", status, &body));
        }
        let value: serde_json::Value = serde_json::from_str(&body)
            .context("OpenRouter token exchange returned invalid JSON")?;
        // OpenRouter answers with `key`, not `access_token`.
        let key =
            super::subscription::wire::required_string(&value, "key", "OpenRouter token exchange")?;
        reconcile(
            self.label(),
            RefreshMode::Never,
            Tokens {
                access: key,
                refresh: None,
                expires_at: now_unix().saturating_add(KEY_LIFETIME_SECS),
            },
            None,
        )
    }
}

/// Build the `https` authorize URL OpenRouter expects.
///
/// OpenRouter uses its own parameter names — `callback_url` rather than a
/// registered `redirect_uri`, and no `client_id`, `response_type`, or `state` —
/// so this is not shared with the standard authorization-code builders.
fn authorize_url(challenge: &str) -> Result<url::Url> {
    let mut url = https_url(AUTHORIZE_URL, "authorize endpoint", "authorization")?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("callback_url", REDIRECT_URI);
        query.append_pair("code_challenge", challenge);
        query.append_pair("code_challenge_method", "S256");
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_authorize_url_carries_the_pkce_challenge_and_loopback_callback() {
        let url = authorize_url("challenge-value").unwrap();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some("openrouter.ai"));
        let pairs: std::collections::HashMap<String, String> = url
            .query_pairs()
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        assert_eq!(
            pairs.get("callback_url").map(String::as_str),
            Some(REDIRECT_URI)
        );
        assert_eq!(
            pairs.get("code_challenge").map(String::as_str),
            Some("challenge-value")
        );
        assert_eq!(
            pairs.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        // OpenRouter does not accept the standard parameters, so asserting their
        // absence keeps a future refactor from silently sending them.
        assert!(!pairs.contains_key("client_id"));
        assert!(!pairs.contains_key("response_type"));
        assert!(!pairs.contains_key("state"));
    }

    #[test]
    fn a_durable_key_records_no_refresh_token() {
        let credential = reconcile(
            "OpenRouter",
            RefreshMode::Never,
            Tokens {
                access: "sk-or-key".into(),
                refresh: Some("ignored".into()),
                expires_at: now_unix() + KEY_LIFETIME_SECS,
            },
            None,
        )
        .unwrap();
        assert_eq!(credential.access_token, "sk-or-key");
        assert!(credential.refresh_token.is_empty());
        assert!(credential.is_fresh(0));
    }
}
