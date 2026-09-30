#![allow(missing_docs)]

//! The contract every subscription login implements, plus the shared rules for
//! turning a token response into a stored credential.
//!
//! Each provider contributes only what actually differs between them: its
//! endpoints, its client id, whether its refresh token rotates, and the wire
//! details of its grant. Everything else — bounded transport, the cross-process
//! rotation lock, credential storage, request derivation — is shared, so a new
//! provider cannot accidentally skip a safety rule that an existing one relies
//! on.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;

use super::store::StoredCredential;
use super::wire::{now_unix, Encoding, Tokens};

/// How a provider treats the refresh token across a rotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefreshMode {
    /// The provider issues a new refresh token with every access token, and the
    /// old one stops working. A response that omits it is a failure: persisting
    /// the old value would keep a credential the provider has already revoked.
    Rotating,
    /// The provider may omit the refresh token when it is not rotating. The
    /// stored value is kept so a non-rotating response does not log the user
    /// out.
    Retaining,
    /// There is no refresh token. The stored identity credential is spent to
    /// mint a fresh short-lived access key, and the identity survives the
    /// exchange.
    Minting,
    /// The credential is a long-lived key that is never exchanged.
    Never,
}

/// One provider's OAuth subscription login.
#[async_trait]
pub(crate) trait SubscriptionFlow: Send + Sync + std::fmt::Debug {
    /// Declaration id, which is also the model-catalog namespace.
    fn provider_id(&self) -> &'static str;

    /// Human-facing provider name used in status output and onboarding.
    fn label(&self) -> &'static str;

    /// The `--login` / `--logout` selector, and the credential file stem.
    fn login(&self) -> &'static str;

    /// The endpoint id registered in the model catalog.
    fn endpoint_id(&self) -> &'static str {
        self.provider_id()
    }

    /// Seconds before nominal expiry at which a token is proactively refreshed.
    fn refresh_skew_secs(&self) -> u64;

    /// How this provider's token endpoint encodes requests.
    fn token_encoding(&self) -> Encoding;

    /// Whether this provider rotates, retains, or has no refresh token.
    fn refresh_mode(&self) -> RefreshMode;

    /// Lifetime assumed when a token response omits `expires_in`.
    ///
    /// A provider that omits the lifetime still gets a finite, refreshable
    /// expiry; treating the response as permanent would strand a credential the
    /// server has already invalidated.
    fn fallback_token_lifetime_secs(&self) -> u64;

    /// Additional fields sent on every token request, such as a non-standard
    /// `referrer` or audience hint.
    fn token_request_extras(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }

    /// Headers inference requires beyond the bearer token, such as a
    /// client-identity header. Returned values are never redacted by octet, so
    /// a provider must not place a secret here.
    fn request_headers(&self, _credential: &StoredCredential) -> Vec<(&'static str, String)> {
        Vec::new()
    }

    /// Run the interactive authorization and return a credential to persist.
    ///
    /// `headless` suppresses only the best-effort browser launch; the URL and
    /// code are printed either way, so the flow still works over SSH.
    async fn authorize(&self, http: &reqwest::Client, headless: bool) -> Result<StoredCredential>;

    /// Exchange the stored credential for a fresh access token.
    async fn refresh(
        &self,
        http: &reqwest::Client,
        credential: &StoredCredential,
    ) -> Result<StoredCredential>;

    /// The value of the `Authorization: Bearer` header for a valid credential.
    fn bearer(&self, credential: &StoredCredential) -> Result<String> {
        let value = credential.access_token.trim();
        if value.is_empty() {
            bail!("{} credential has no access token", self.label());
        }
        Ok(value.to_owned())
    }
}

/// Turn a token-endpoint response into a credential this provider can store.
///
/// The refresh token is resolved here rather than by each flow so the
/// rotation rules hold uniformly: a rotating provider that omits a new refresh
/// token fails closed instead of persisting one the provider has revoked, and a
/// retaining provider that omits it keeps the value it already had.
pub(crate) fn reconcile(
    label: &str,
    mode: RefreshMode,
    tokens: Tokens,
    previous: Option<&StoredCredential>,
) -> Result<StoredCredential> {
    let previous = previous.cloned().unwrap_or_else(|| StoredCredential {
        version: super::store::CREDENTIAL_VERSION,
        access_token: String::new(),
        refresh_token: String::new(),
        expires_at: 0,
        scope: None,
        account_id: None,
    });
    let refresh_token = match mode {
        RefreshMode::Rotating => tokens.refresh.with_context(|| {
            format!("{label} did not return a rotated refresh token; sign in again")
        })?,
        RefreshMode::Retaining => tokens
            .refresh
            .unwrap_or_else(|| previous.refresh_token.clone()),
        RefreshMode::Minting => previous.refresh_token.clone(),
        RefreshMode::Never => String::new(),
    };
    Ok(StoredCredential {
        version: super::store::CREDENTIAL_VERSION,
        access_token: tokens.access,
        refresh_token,
        expires_at: tokens.expires_at.max(now_unix()),
        scope: previous.scope,
        account_id: previous.account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str, refresh: Option<&str>, lifetime: u64) -> Tokens {
        Tokens {
            access: access.into(),
            refresh: refresh.map(str::to_owned),
            expires_at: now_unix() + lifetime,
        }
    }

    fn previous(refresh: &str) -> StoredCredential {
        StoredCredential {
            version: super::super::store::CREDENTIAL_VERSION,
            access_token: "old".into(),
            refresh_token: refresh.into(),
            expires_at: 0,
            scope: Some("kept-scope".into()),
            account_id: Some("acct".into()),
        }
    }

    #[test]
    fn a_rotating_provider_refuses_a_response_without_a_new_refresh_token() {
        let error = reconcile(
            "kimi",
            RefreshMode::Rotating,
            tokens("new", None, 3600),
            Some(&previous("old-refresh")),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("rotated refresh token"), "{error}");
        assert!(error.contains("kimi"), "{error}");
    }

    #[test]
    fn a_rotating_provider_persists_the_rotated_value() {
        let credential = reconcile(
            "kimi",
            RefreshMode::Rotating,
            tokens("new", Some("rotated"), 60),
            None,
        )
        .unwrap();
        assert_eq!(credential.refresh_token, "rotated");
        assert_eq!(credential.access_token, "new");
    }

    #[test]
    fn a_retaining_provider_keeps_the_stored_refresh_token() {
        let credential = reconcile(
            "xAI",
            RefreshMode::Retaining,
            tokens("new", None, 60),
            Some(&previous("kept")),
        )
        .unwrap();
        assert_eq!(credential.refresh_token, "kept");
        // A rotation is still honored when the provider does send one.
        let rotated = reconcile(
            "xAI",
            RefreshMode::Retaining,
            tokens("new", Some("rotated"), 60),
            Some(&previous("kept")),
        )
        .unwrap();
        assert_eq!(rotated.refresh_token, "rotated");
    }

    #[test]
    fn a_minting_provider_keeps_its_identity_and_never_stores_a_refresh_token() {
        let credential = reconcile(
            "Meta",
            RefreshMode::Minting,
            tokens("minted-key", Some("should-be-ignored"), 60),
            Some(&previous("identity-token")),
        )
        .unwrap();
        assert_eq!(credential.access_token, "minted-key");
        assert_eq!(credential.refresh_token, "identity-token");

        let minted = reconcile(
            "Meta",
            RefreshMode::Minting,
            tokens("minted-key", None, 60),
            Some(&previous("identity-token")),
        )
        .unwrap();
        assert_eq!(minted.refresh_token, "identity-token");
    }

    #[test]
    fn a_non_refreshing_provider_never_stores_a_refresh_token() {
        let credential = reconcile(
            "OpenRouter",
            RefreshMode::Never,
            tokens("sk-or-key", Some("ignored"), 60),
            Some(&previous("stale")),
        )
        .unwrap();
        assert_eq!(credential.refresh_token, "");
        assert_eq!(credential.access_token, "sk-or-key");
    }

    #[test]
    fn reconciliation_preserves_non_secret_diagnostics_across_a_rotation() {
        let credential = reconcile(
            "kimi",
            RefreshMode::Rotating,
            tokens("new", Some("rotated"), 60),
            Some(&previous("old")),
        )
        .unwrap();
        assert_eq!(credential.scope.as_deref(), Some("kept-scope"));
        assert_eq!(credential.account_id.as_deref(), Some("acct"));
    }
}
