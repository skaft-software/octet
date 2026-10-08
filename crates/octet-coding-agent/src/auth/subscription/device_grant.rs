#![allow(missing_docs)]

//! A reusable RFC 8628 device authorization grant, driven end to end.
//!
//! xAI, Kimi, and Meta differ only in endpoints and a couple of extra request
//! fields, so they share this driver. The cadence rules live in
//! [`super::device`]; this file is the wire half plus the user-facing
//! presentation, which is identical across every device-code provider.

use anyhow::{bail, Context, Result};

use super::device::{poll_device_authorization, PollOutcome};
use super::flow::{reconcile, SubscriptionFlow};
use super::wire::{
    https_url, optional_positive_u64, post_for_status, post_for_value, request_failed,
    required_string, Encoding,
};

/// RFC 8628 device grant type, sent verbatim on the token request.
const DEVICE_CODE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// How long octet waits for the user to finish authorizing before giving up,
/// when the provider does not state a device-code lifetime.
///
/// Providers that do publish `expires_in` are trusted to shorten this; the cap
/// only bounds a provider that publishes nothing, so a forgotten terminal can
/// never poll a token endpoint forever.
const DEFAULT_DEVICE_LIFETIME: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// Description of a provider's RFC 8628 endpoints.
///
/// The two URLs are owned rather than borrowed so tests can point the whole
/// grant at a local server and exercise the real wire behaviour, the way the
/// Codex device flow's `*_with_url` seams already do.
pub(crate) struct DeviceGrant {
    pub client_id: &'static str,
    /// Device authorization endpoint.
    pub device_url: String,
    /// Token endpoint, shared by the device grant and by refresh.
    pub token_url: String,
    /// Scopes, sent space-separated as RFC 6749 requires.
    pub scopes: &'static [&'static str],
    /// Non-standard extra fields some providers require on the device request.
    pub device_extras: &'static [(&'static str, &'static str)],
}

impl DeviceGrant {
    /// Build a grant from a provider's constants.
    pub(crate) fn new(
        client_id: &'static str,
        device_url: impl Into<String>,
        token_url: impl Into<String>,
        scopes: &'static [&'static str],
        device_extras: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            client_id,
            device_url: device_url.into(),
            token_url: token_url.into(),
            scopes,
            device_extras,
        }
    }
}

impl DeviceGrant {
    /// Run the whole grant: start, present, poll, and reconcile.
    pub(crate) async fn run(
        &self,
        flow: &dyn SubscriptionFlow,
        http: &reqwest::Client,
        headless: bool,
    ) -> Result<super::store::StoredCredential> {
        let label = flow.label();
        let device = self.start(http, flow.token_encoding()).await?;

        crate::output::stdout_multiline(format!(
            "Open this URL and enter the code shown below:\n\n  {}\n\n  Code: {}\n\nWaiting for authorization…",
            device.verification_uri,
            crate::output::table_field(&device.user_code, crate::output::stdout_is_terminal()),
        ));
        if !headless {
            super::login::open_browser(&device.verification_uri);
        }

        let expires_in_seconds = device
            .expires_in_seconds
            .unwrap_or(DEFAULT_DEVICE_LIFETIME.as_secs());
        let credential = poll_device_authorization(
            label,
            device.interval_seconds,
            Some(expires_in_seconds),
            true,
            || self.poll(http, flow, &device),
        )
        .await?;
        Ok(credential)
    }

    async fn start(
        &self,
        http: &reqwest::Client,
        encoding: Encoding,
    ) -> Result<DeviceAuthorization> {
        let mut fields = vec![("client_id".to_owned(), self.client_id.to_owned())];
        if !self.scopes.is_empty() {
            fields.push(("scope".to_owned(), self.scopes.join(" ")));
        }
        for (name, value) in self.device_extras {
            fields.push(((*name).to_owned(), (*value).to_owned()));
        }
        let body = post_for_value(
            http,
            &self.device_url,
            encoding,
            &fields,
            "device authorization",
        )
        .await?;

        let device_code = required_string(&body, "device_code", "device authorization")?;
        let user_code = required_string(&body, "user_code", "device authorization")?;
        if device_code == user_code {
            bail!("device authorization returned the same code for the device and the user");
        }
        // Prefer the pre-filled verification URL when the provider offers one;
        // it removes the manual code entry step.
        let verification_uri = match body
            .get("verification_uri_complete")
            .and_then(|value| value.as_str())
        {
            Some(complete) if !complete.trim().is_empty() => https_url(
                complete.trim(),
                "verification_uri_complete",
                "device authorization",
            )?,
            _ => https_url(
                &required_string(&body, "verification_uri", "device authorization")?,
                "verification_uri",
                "device authorization",
            )?,
        };
        Ok(DeviceAuthorization {
            device_code,
            user_code,
            verification_uri: verification_uri.to_string(),
            interval_seconds: optional_positive_u64(&body, "interval"),
            expires_in_seconds: optional_positive_u64(&body, "expires_in"),
        })
    }

    async fn poll(
        &self,
        http: &reqwest::Client,
        flow: &dyn SubscriptionFlow,
        device: &DeviceAuthorization,
    ) -> Result<PollOutcome<super::store::StoredCredential>> {
        let label = flow.label();
        let (status, body) = post_for_status(
            http,
            &self.token_url,
            flow.token_encoding(),
            &[
                ("grant_type".to_owned(), DEVICE_CODE_GRANT.to_owned()),
                ("client_id".to_owned(), self.client_id.to_owned()),
                ("device_code".to_owned(), device.device_code.clone()),
            ],
            "device token polling",
        )
        .await?;

        if !status.is_success() {
            // RFC 8628 §3.5: the still-authorizing and back-off cases are
            // ordinary non-success responses, so they are classified from the
            // decoded body. Every other code terminates the login.
            return match device_poll_outcome(&body) {
                Some(outcome) => Ok(outcome),
                None => Err(request_failed(
                    "device token polling",
                    status,
                    &body.to_string(),
                ))
                .with_context(|| format!("{label} device authorization")),
            };
        }

        let tokens = super::wire::tokens_from_response(
            &body,
            "access_token",
            "refresh_token",
            flow.fallback_token_lifetime_secs(),
            "device token",
        )?;
        let credential = reconcile(label, flow.refresh_mode(), tokens, None)?;
        Ok(PollOutcome::Complete(credential))
    }
}

/// A device authorization awaiting the user.
struct DeviceAuthorization {
    device_code: String,
    user_code: String,
    /// Pre-filled, `https`-validated URL to open in a browser.
    verification_uri: String,
    interval_seconds: Option<u64>,
    expires_in_seconds: Option<u64>,
}

impl std::fmt::Debug for DeviceAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The device code is exchanged for tokens, so it is a credential in its
        // own right; the user code and URL are printed to the user anyway.
        formatter
            .debug_struct("DeviceAuthorization")
            .field("device_code", &"[REDACTED]")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("interval_seconds", &self.interval_seconds)
            .field("expires_in_seconds", &self.expires_in_seconds)
            .finish()
    }
}

/// Classify a non-success device poll response.
///
/// Returns `None` for a code the poller must not retry.
fn device_poll_outcome<T>(body: &serde_json::Value) -> Option<PollOutcome<T>> {
    match body.get("error").and_then(serde_json::Value::as_str) {
        Some("authorization_pending") => Some(PollOutcome::Pending),
        Some("slow_down") => Some(PollOutcome::SlowDown {
            interval_seconds: optional_positive_u64(body, "interval"),
        }),
        Some("access_denied") | Some("authorization_denied") => None,
        Some("expired_token") => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_device_poll_error_maps_to_the_documented_outcomes() {
        fn outcome(body: &serde_json::Value) -> Option<PollOutcome<()>> {
            device_poll_outcome(body)
        }
        let pending = serde_json::json!({ "error": "authorization_pending" });
        assert!(matches!(outcome(&pending), Some(PollOutcome::Pending)));

        let slow = serde_json::json!({ "error": "slow_down", "interval": "9" });
        assert!(matches!(
            outcome(&slow),
            Some(PollOutcome::SlowDown {
                interval_seconds: Some(9)
            })
        ));

        let unthrottled = serde_json::json!({ "error": "slow_down" });
        assert!(matches!(
            outcome(&unthrottled),
            Some(PollOutcome::SlowDown {
                interval_seconds: None
            })
        ));

        // A denial, an expiry, and an unknown code all terminate the login
        // rather than being retried against a spent or abandoned device code.
        for terminal in [
            serde_json::json!({ "error": "access_denied" }),
            serde_json::json!({ "error": "authorization_denied" }),
            serde_json::json!({ "error": "expired_token" }),
            serde_json::json!({ "error": "server_meltdown" }),
            serde_json::json!({ "not_an_error": true }),
        ] {
            assert!(outcome(&terminal).is_none(), "{terminal}");
        }
    }
}

#[cfg(test)]
mod wire_tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    use super::*;
    use crate::auth::subscription::flow::{RefreshMode, SubscriptionFlow};
    use crate::auth::subscription::store::StoredCredential;
    use crate::auth::subscription::wire::Encoding;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ACCESS: &str = "access-PRIVATE-SENTINEL";
    const REFRESH: &str = "refresh-PRIVATE-SENTINEL";

    /// The minimal flow a device grant needs, with a zero skew so a credential
    /// that is fresh for one hour is not refreshed during the test.
    #[derive(Debug)]
    struct GrantFlow {
        mode: RefreshMode,
    }

    #[async_trait::async_trait]
    impl SubscriptionFlow for GrantFlow {
        fn provider_id(&self) -> &'static str {
            "test-subscription"
        }
        fn label(&self) -> &'static str {
            "Test Provider"
        }
        fn login(&self) -> &'static str {
            "test-provider"
        }
        fn refresh_skew_secs(&self) -> u64 {
            0
        }
        fn token_encoding(&self) -> Encoding {
            Encoding::Form
        }
        fn refresh_mode(&self) -> RefreshMode {
            self.mode
        }
        fn fallback_token_lifetime_secs(&self) -> u64 {
            3600
        }
        async fn authorize(
            &self,
            _http: &reqwest::Client,
            _headless: bool,
        ) -> Result<StoredCredential> {
            bail!("unused")
        }
        async fn refresh(
            &self,
            _http: &reqwest::Client,
            _credential: &StoredCredential,
        ) -> Result<StoredCredential> {
            bail!("unused")
        }
    }

    /// A provider that answers a device grant end to end: `pending` once, then a
    /// rotated pair.
    async fn pending_then_rotated() -> (MockServer, Arc<AtomicU64>) {
        let server = MockServer::start().await;
        let polls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&polls);
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(move |_request: &wiremock::Request| {
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(400).set_body_json(serde_json::json!({
                        "error": "authorization_pending"
                    }))
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({
                        "access_token": ACCESS,
                        "refresh_token": REFRESH,
                        "expires_in": 3600
                    }))
                }
            })
            .mount(&server)
            .await;
        (server, polls)
    }

    fn grant(server: &MockServer) -> DeviceGrant {
        DeviceGrant::new(
            "test-client",
            format!("{}/device", server.uri()),
            format!("{}/token", server.uri()),
            &["offline_access", "grok-cli:access"],
            &[("referrer", "octet")],
        )
    }

    // Wiremock services real sockets on another runtime; virtual time can
    // expire the device grant while its response is still in flight.
    #[tokio::test]
    async fn a_device_grant_presents_the_code_and_polls_until_the_user_finishes() {
        let (server, polls) = pending_then_rotated().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .and(body_string_contains("client_id=test-client"))
            .and(body_string_contains(
                "scope=offline_access+grok-cli%3Aaccess",
            ))
            .and(body_string_contains("referrer=octet"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-SECRET",
                "user_code": "ABCD-1234",
                "verification_uri": "https://auth.example.com/verify",
                "verification_uri_complete": "https://auth.example.com/verify?code=ABCD-1234",
                "interval": 1,
                "expires_in": 600
            })))
            .mount(&server)
            .await;

        let credential = grant(&server)
            .run(
                &GrantFlow {
                    mode: RefreshMode::Rotating,
                },
                &reqwest::Client::new(),
                // Headless: the test must never try to launch a browser.
                true,
            )
            .await
            .unwrap();
        assert_eq!(credential.access_token, ACCESS);
        assert_eq!(credential.refresh_token, REFRESH);
        assert!(
            polls.load(Ordering::SeqCst) >= 2,
            "an `authorization_pending` poll must be retried"
        );
    }

    #[tokio::test]
    async fn a_denial_stops_the_login_and_is_named() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-SECRET",
                "user_code": "ABCD-1234",
                "verification_uri": "https://auth.example.com/verify",
                "expires_in": 600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "access_denied"
            })))
            .mount(&server)
            .await;

        let error = grant(&server)
            .run(
                &GrantFlow {
                    mode: RefreshMode::Rotating,
                },
                &reqwest::Client::new(),
                true,
            )
            .await
            .expect_err("a denied grant must not produce a credential");
        let message = format!("{error:#}");
        assert!(message.contains("access_denied"), "{message}");
    }

    #[tokio::test]
    async fn an_untrusted_verification_uri_is_refused_before_anything_is_shown() {
        for hostile in [
            "javascript:alert(1)",
            "http://auth.example.com/verify",
            "file:///etc/passwd",
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/device"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "device_code": "device-SECRET",
                    "user_code": "ABCD-1234",
                    "verification_uri": hostile,
                    "expires_in": 600
                })))
                .mount(&server)
                .await;
            let error = grant(&server)
                .start(&reqwest::Client::new(), Encoding::Form)
                .await
                .expect_err("an untrusted verification URI must be refused");
            let message = format!("{error:#}");
            assert!(message.contains("untrusted"), "{hostile}: {message}");
        }
    }

    #[tokio::test]
    async fn a_device_response_reusing_one_code_for_both_roles_is_refused() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "SAME-CODE",
                "user_code": "SAME-CODE",
                "verification_uri": "https://auth.example.com/verify",
                "expires_in": 600
            })))
            .mount(&server)
            .await;
        assert!(grant(&server)
            .start(&reqwest::Client::new(), Encoding::Form)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_completed_grant_without_a_refresh_token_is_refused_for_a_rotating_provider() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/device"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_code": "device-SECRET",
                "user_code": "ABCD-1234",
                "verification_uri": "https://auth.example.com/verify",
                "expires_in": 600
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": ACCESS,
                "expires_in": 3600
            })))
            .mount(&server)
            .await;
        let error = grant(&server)
            .run(
                &GrantFlow {
                    mode: RefreshMode::Rotating,
                },
                &reqwest::Client::new(),
                true,
            )
            .await
            .expect_err("a login with no usable refresh token must not be persisted");
        assert!(
            format!("{error:#}").contains("rotated refresh token"),
            "{error:#}"
        );

        // A retaining provider accepts the same response, because xAI-style
        // non-rotation leaves the stored refresh token in place.
        let credential = grant(&server)
            .run(
                &GrantFlow {
                    mode: RefreshMode::Retaining,
                },
                &reqwest::Client::new(),
                true,
            )
            .await
            .unwrap();
        assert_eq!(credential.access_token, ACCESS);
        assert!(credential.refresh_token.is_empty());
    }
}
