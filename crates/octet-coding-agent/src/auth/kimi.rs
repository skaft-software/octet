#![allow(missing_docs)]

//! Kimi Code ("Sign in with Kimi Code") OAuth subscription login.
//!
//! Kimi exposes an RFC 8628 device grant whose token authenticates the
//! Anthropic-compatible `/coding` surface as a plain bearer. Octet already
//! declares that surface for `KIMI_API_KEY`, so the subscription route reuses
//! the same `anthropic_messages` codec and differs only in where the credential
//! comes from.
//!
//! Kimi always rotates the refresh token, and answers a renewal it considers
//! unauthorized with `invalid_grant`. Both are load-bearing: a response without
//! a new refresh token is rejected rather than persisted, and a rejected
//! credential is deleted so the next attempt starts from a clean login.

use anyhow::Result;
use async_trait::async_trait;

use super::subscription::device_grant::DeviceGrant;
use super::subscription::flow::{reconcile, RefreshMode, SubscriptionFlow};
use super::subscription::store::StoredCredential;
use super::subscription::wire::{post_for_value, tokens_from_response, Encoding};

/// Public OAuth client id issued to the first-party Kimi CLI (not a secret).
const CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";

/// Kimi's device authorization endpoint.
const DEVICE_CODE_URL: &str = "https://auth.kimi.com/api/oauth/device_authorization";

/// Kimi's token endpoint, shared by the device grant and by refresh.
const TOKEN_URL: &str = "https://auth.kimi.com/api/oauth/token";

/// Seconds before nominal expiry at which a token is proactively refreshed.
const REFRESH_SKEW_SECS: u64 = 60;

/// Lifetime assumed when a token response omits `expires_in`.
const FALLBACK_TOKEN_LIFETIME_SECS: u64 = 60 * 60;

/// The Kimi Code subscription flow.
#[derive(Clone, Copy, Debug)]
pub(crate) struct KimiFlow;

#[async_trait]
impl SubscriptionFlow for KimiFlow {
    fn provider_id(&self) -> &'static str {
        crate::providers::KIMI_CODING_SUBSCRIPTION.id
    }

    fn label(&self) -> &'static str {
        "Kimi Code"
    }

    fn login(&self) -> &'static str {
        "kimi"
    }

    fn refresh_skew_secs(&self) -> u64 {
        REFRESH_SKEW_SECS
    }

    fn token_encoding(&self) -> Encoding {
        Encoding::Form
    }

    fn refresh_mode(&self) -> RefreshMode {
        RefreshMode::Rotating
    }

    fn fallback_token_lifetime_secs(&self) -> u64 {
        FALLBACK_TOKEN_LIFETIME_SECS
    }

    async fn authorize(&self, http: &reqwest::Client, headless: bool) -> Result<StoredCredential> {
        DeviceGrant::new(CLIENT_ID, DEVICE_CODE_URL, TOKEN_URL, &[], &[])
            .run(self, http, headless)
            .await
    }

    async fn refresh(
        &self,
        http: &reqwest::Client,
        credential: &StoredCredential,
    ) -> Result<StoredCredential> {
        let body = post_for_value(
            http,
            TOKEN_URL,
            Encoding::Form,
            &[
                ("grant_type".to_owned(), "refresh_token".to_owned()),
                ("client_id".to_owned(), CLIENT_ID.to_owned()),
                ("refresh_token".to_owned(), credential.refresh_token.clone()),
            ],
            "token refresh",
        )
        .await?;
        let tokens = tokens_from_response(
            &body,
            "access_token",
            "refresh_token",
            FALLBACK_TOKEN_LIFETIME_SECS,
            "token refresh",
        )?;
        reconcile(
            self.label(),
            RefreshMode::Rotating,
            tokens,
            Some(credential),
        )
    }
}
