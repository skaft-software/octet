#![allow(missing_docs)]

//! xAI ("Sign in with SuperGrok or X Premium") OAuth subscription login.
//!
//! xAI issues a plain RFC 8628 device grant. The access token is an ordinary
//! bearer token for the public `api.x.ai` surface, so inference reuses octet's
//! existing `openai_responses` route; the only subscription-specific behaviour
//! is that the token comes from a paid plan rather than an API key.
//!
//! The flow mirrors the equivalent TypeScript provider, with one deliberate
//! difference: octet identifies itself as `octet` in the non-standard `referrer`
//! field, because that field names the first-party client to the provider.

use anyhow::{Context, Result};
use async_trait::async_trait;

use super::subscription::device_grant::DeviceGrant;
use super::subscription::flow::{reconcile, RefreshMode, SubscriptionFlow};
use super::subscription::store::StoredCredential;
use super::subscription::wire::{now_unix, post_for_value, Encoding};

/// Public OAuth client id issued to the first-party Grok CLI (not a secret).
const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";

/// Scopes granting Grok inference plus offline renewal.
const SCOPES: &[&str] = &[
    "openid",
    "profile",
    "email",
    "offline_access",
    "grok-cli:access",
    "api:access",
];

/// xAI's device authorization endpoint.
const DEVICE_CODE_URL: &str = "https://auth.x.ai/oauth2/device/code";

/// xAI's token endpoint, shared by the device grant and by refresh.
const TOKEN_URL: &str = "https://auth.x.ai/oauth2/token";

/// Identifier octet presents to xAI in place of the first-party client.
const REFERRER: &str = "octet";

/// Seconds before nominal expiry at which a token is proactively refreshed.
///
/// xAI's own CLI refreshes five minutes early, and a Grok request is long
/// enough that a token dying mid-stream is a real failure mode.
const REFRESH_SKEW_SECS: u64 = 5 * 60;

/// Lifetime assumed when a token response omits `expires_in`.
const FALLBACK_TOKEN_LIFETIME_SECS: u64 = 60 * 60;

/// The xAI subscription flow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct XaiFlow;

#[async_trait]
impl SubscriptionFlow for XaiFlow {
    fn provider_id(&self) -> &'static str {
        crate::providers::XAI_SUBSCRIPTION.id
    }

    fn label(&self) -> &'static str {
        "xAI (SuperGrok or X Premium)"
    }

    fn login(&self) -> &'static str {
        "grok"
    }

    fn refresh_skew_secs(&self) -> u64 {
        REFRESH_SKEW_SECS
    }

    fn token_encoding(&self) -> Encoding {
        Encoding::Form
    }

    fn refresh_mode(&self) -> RefreshMode {
        // xAI omits `refresh_token` on renewal when the token did not rotate.
        // Keeping the stored value is what keeps an un-rotated renewal working;
        // requiring a rotation would log the user out on the first refresh.
        RefreshMode::Retaining
    }

    fn fallback_token_lifetime_secs(&self) -> u64 {
        FALLBACK_TOKEN_LIFETIME_SECS
    }

    async fn authorize(&self, http: &reqwest::Client, headless: bool) -> Result<StoredCredential> {
        DeviceGrant::new(
            CLIENT_ID,
            DEVICE_CODE_URL,
            TOKEN_URL,
            SCOPES,
            &[("referrer", REFERRER)],
        )
        .run(self, http, headless)
        .await
    }

    async fn refresh(
        &self,
        http: &reqwest::Client,
        credential: &StoredCredential,
    ) -> Result<StoredCredential> {
        let mut fields = vec![
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            ("client_id".to_owned(), CLIENT_ID.to_owned()),
            ("refresh_token".to_owned(), credential.refresh_token.clone()),
        ];
        fields.extend(
            super::subscription::flow::SubscriptionFlow::token_request_extras(self)
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned())),
        );
        let body =
            post_for_value(http, TOKEN_URL, Encoding::Form, &fields, "token refresh").await?;
        let tokens = super::subscription::wire::tokens_from_response(
            &body,
            "access_token",
            "refresh_token",
            FALLBACK_TOKEN_LIFETIME_SECS,
            "token refresh",
        )?;
        let refreshed = reconcile(
            self.label(),
            RefreshMode::Retaining,
            tokens,
            Some(credential),
        )
        .with_context(|| format!("renewing the {} credential failed", self.label()))?;
        debug_assert!(refreshed.expires_at >= now_unix());
        Ok(refreshed)
    }
}
