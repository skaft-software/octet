#![allow(missing_docs)]

//! Meta ("Sign in with Meta") OAuth subscription login.
//!
//! Meta splits identity from API access. The device grant yields an *identity*
//! token that the inference surface will not accept, so octet immediately
//! exchanges it for a short-lived Model API key. Both halves are stored: the
//! identity token in `refresh_token`, the minted key in `access_token`.
//!
//! That shape is why this flow is [`RefreshMode::Minting`] rather than a
//! renewal. The identity token cannot itself be refreshed — Meta answers a
//! refresh grant with `404` and issues no replacement — so "refresh" means
//! spending the stored identity to mint a new key. When that fails, the session
//! is genuinely over and the user must sign in again; the key mint is also where
//! a `401` proves the identity has lapsed, which is reported as a login prompt
//! rather than a retry.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::subscription::device_grant::DeviceGrant;
use super::subscription::flow::{reconcile, RefreshMode, SubscriptionFlow};
use super::subscription::store::StoredCredential;
use super::subscription::wire::{bounded_body, tokens_from_response, Encoding};

/// Public OAuth client id issued to the first-party Muse Code CLI (not a secret).
const CLIENT_ID: &str = "1031625952748946";

/// Meta's OIDC device authorization endpoint.
const DEVICE_CODE_URL: &str = "https://auth.meta.com/oidc/device/authorization/";

/// Meta's OIDC device token endpoint.
const DEVICE_CODE_TOKEN_URL: &str = "https://auth.meta.com/oidc/device/token/";

/// Endpoint that trades a Meta identity token for a Model API key.
const API_KEY_MINT_URL: &str = "https://api.meta.ai/muse-code/key";

/// API version Meta requires on the key-mint request.
const API_KEY_MINT_VERSION: &str = "1.0.0";

/// Assumed lifetime of a minted key, used when the mint response omits one.
///
/// Meta's own tooling treats a minted key as good for about a day, and the
/// identity token behind it is the thing that actually expires.
const MINTED_KEY_LIFETIME_SECS: u64 = 24 * 60 * 60;

/// Seconds before nominal expiry at which a key is proactively re-minted.
const REFRESH_SKEW_SECS: u64 = 5 * 60;

/// The Meta subscription flow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MetaFlow;

#[async_trait]
impl SubscriptionFlow for MetaFlow {
    fn provider_id(&self) -> &'static str {
        crate::providers::META_SUBSCRIPTION.id
    }

    fn label(&self) -> &'static str {
        "Meta (Muse subscription)"
    }

    fn login(&self) -> &'static str {
        "meta"
    }

    fn refresh_skew_secs(&self) -> u64 {
        REFRESH_SKEW_SECS
    }

    fn token_encoding(&self) -> Encoding {
        Encoding::Form
    }

    fn refresh_mode(&self) -> RefreshMode {
        RefreshMode::Minting
    }

    fn fallback_token_lifetime_secs(&self) -> u64 {
        MINTED_KEY_LIFETIME_SECS
    }

    async fn authorize(&self, http: &reqwest::Client, headless: bool) -> Result<StoredCredential> {
        let identity =
            DeviceGrant::new(CLIENT_ID, DEVICE_CODE_URL, DEVICE_CODE_TOKEN_URL, &[], &[])
                .run(self, http, headless)
                .await?;
        // The device grant stored the identity where a refresh token belongs;
        // re-mint so the user finishes login holding a usable key.
        self.mint(http, &identity).await
    }

    async fn refresh(
        &self,
        http: &reqwest::Client,
        credential: &StoredCredential,
    ) -> Result<StoredCredential> {
        self.mint(http, credential).await
    }
}

impl MetaFlow {
    /// Trade a stored identity token for a fresh Model API key.
    async fn mint(
        &self,
        http: &reqwest::Client,
        identity: &StoredCredential,
    ) -> Result<StoredCredential> {
        self.mint_with_url(http, identity, API_KEY_MINT_URL).await
    }

    async fn mint_with_url(
        &self,
        http: &reqwest::Client,
        identity: &StoredCredential,
        url: &str,
    ) -> Result<StoredCredential> {
        if !identity.has_refresh_token() {
            bail!(
                "{} credential has no identity token; sign in again",
                self.label()
            );
        }
        let mut authorization =
            http::HeaderValue::from_str(&format!("Bearer {}", identity.refresh_token)).map_err(
                |_| anyhow::anyhow!("{} identity token is not a usable credential", self.label()),
            )?;
        // Marked sensitive so a future debug statement cannot print the identity
        // token this request spends.
        authorization.set_sensitive(true);
        let response = http
            .post(url)
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .header("x-api-version", API_KEY_MINT_VERSION)
            .header("authorization", authorization)
            .body("{}")
            .send()
            .await
            .context("Meta API key mint request failed")?;
        let status = response.status();
        let body = bounded_body(response, "Meta API key mint").await?;

        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // The identity token is not renewable, so this is terminal. Say so
            // plainly rather than leaving the user to retry a dead credential.
            bail!(
                "{} session expired; sign in again with `octet --login {}`",
                self.label(),
                self.login()
            );
        }
        if !status.is_success() {
            bail!("Meta API key mint failed with status {status}");
        }
        let value: serde_json::Value =
            serde_json::from_str(&body).context("Meta API key mint returned invalid JSON")?;
        let key = match value.get("api_key").and_then(serde_json::Value::as_str) {
            Some(key) if !key.trim().is_empty() => key.trim().to_owned(),
            _ => {
                // Meta sometimes requires account setup before keys can be
                // minted. Surface the provider's own completion link, which is
                // the only part of the response octet can safely quote.
                let action = value
                    .get("action_url")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|url| {
                        super::subscription::wire::https_url(url, "action_url", "Meta API key mint")
                            .ok()
                    });
                match action {
                    Some(url) => bail!(
                        "Meta did not issue an API key; finish account setup at {url} and sign in again"
                    ),
                    None => bail!("Meta did not issue an API key; sign in again"),
                }
            }
        };
        let tokens = tokens_from_response(
            &serde_json::json!({ "access_token": key, "expires_in": MINTED_KEY_LIFETIME_SECS }),
            "access_token",
            "refresh_token",
            MINTED_KEY_LIFETIME_SECS,
            "Meta API key mint",
        )?;
        // `reconcile` keeps the stored identity in `refresh_token`; carrying
        // `account_id` forward preserves the non-secret diagnostic without
        // re-decoding anything at resolution time.
        reconcile(self.label(), RefreshMode::Minting, tokens, Some(identity))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{
        matchers::{header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    #[tokio::test]
    async fn initial_device_identity_can_mint_persist_and_remint_api_keys() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "private-device", "user_code": "ABCD", "interval": 1,
                "expires_in": 30, "verification_uri": "https://auth.meta.com/verify"
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "identity-secret", "expires_in": 3600
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/mint"))
            .and(header("authorization", "Bearer identity-secret"))
            .and(header("x-api-version", API_KEY_MINT_VERSION))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "api_key": "inference-key"
            })))
            .expect(2)
            .mount(&server)
            .await;
        let http = reqwest::Client::new();
        let identity = DeviceGrant::new(
            CLIENT_ID,
            format!("{}/device", server.uri()),
            format!("{}/token", server.uri()),
            &[],
            &[],
        )
        .run(&MetaFlow, &http, true)
        .await
        .unwrap();
        assert_eq!(identity.refresh_token, "identity-secret");
        let credential = MetaFlow
            .mint_with_url(&http, &identity, &format!("{}/mint", server.uri()))
            .await
            .unwrap();
        assert_eq!(credential.access_token, "inference-key");
        assert_eq!(credential.refresh_token, "identity-secret");
        let directory = tempfile::tempdir().unwrap();
        let store = super::super::subscription::store::OAuthStore::new(
            directory.path().join("credentials/meta.json"),
            "Meta",
        );
        store.save(&credential).unwrap();
        let persisted = store.load().unwrap().unwrap();
        let reminted = MetaFlow
            .mint_with_url(&http, &persisted, &format!("{}/mint", server.uri()))
            .await
            .unwrap();
        assert_eq!(reminted.access_token, "inference-key");
        assert_eq!(reminted.refresh_token, "identity-secret");
    }
}
