#![allow(missing_docs)]

//! Bounded, redirect-free HTTP primitives shared by every subscription login.
//!
//! Every provider response is untrusted input. Bodies are size bounded before
//! they are buffered, redirect responses are surfaced instead of followed so a
//! token endpoint cannot bounce a credential to a host octet did not choose,
//! and only allow-listed error *codes* ever reach a diagnostic — free-form
//! provider text is dropped because it can echo the submitted secret.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures_util::StreamExt as _;

/// Largest OAuth response body octet buffers from any subscription provider.
///
/// Token and device-authorization payloads are a few hundred bytes. The limit
/// exists so a hostile or broken endpoint cannot exhaust memory, and so a
/// chunked response without a `Content-Length` is still bounded.
pub(crate) const MAX_RESPONSE_BYTES: usize = 64 * 1024;

/// Connect timeout applied to every subscription HTTP client.
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total timeout for one non-polling subscription request.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// The OAuth client every subscription flow uses.
///
/// Redirects are disabled on purpose: an authorization or token endpoint that
/// answers with a redirect is reported as a failure rather than replayed
/// against whatever host the `Location` header names.
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("static subscription HTTP client settings are valid")
}

/// Seconds since the Unix epoch, saturating at zero if the clock predates it.
pub(crate) fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// A completed token-endpoint exchange, with an absolute expiry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Tokens {
    pub access: String,
    /// `None` when the provider omitted a refresh token it did not rotate.
    pub refresh: Option<String>,
    pub expires_at: u64,
}

/// Whether a provider's token endpoint expects form encoding or JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    Form,
    Json,
}

/// Read a response body, refusing anything above [`MAX_RESPONSE_BYTES`].
///
/// The declared `Content-Length` is checked first so an oversized body is
/// rejected before it is transferred, and the streamed length is checked again
/// so a chunked response cannot exceed the bound.
pub(crate) async fn bounded_body(response: reqwest::Response, label: &str) -> Result<String> {
    if response
        .content_length()
        .is_some_and(|declared| declared > MAX_RESPONSE_BYTES as u64)
    {
        bail!("{label} response exceeded the {MAX_RESPONSE_BYTES}-byte limit");
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("reading {label} response failed"))?;
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            bail!("{label} response exceeded the {MAX_RESPONSE_BYTES}-byte limit");
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).with_context(|| format!("{label} response was not valid UTF-8"))
}

/// Whether an error code is safe to place in a user-visible message.
///
/// Only recognized OAuth codes are echoed. Token-shaped strings are not safe:
/// a provider can reflect a submitted device code or token in this field.
fn is_safe_error_code(code: &str) -> bool {
    matches!(
        code,
        "invalid_request"
            | "invalid_client"
            | "invalid_grant"
            | "unauthorized_client"
            | "unsupported_grant_type"
            | "invalid_scope"
            | "access_denied"
            | "authorization_denied"
            | "authorization_pending"
            | "slow_down"
            | "expired_token"
            | "unsupported_response_type"
            | "server_error"
            | "temporarily_unavailable"
    )
}

/// Extract an allow-listed OAuth error code from a response body.
///
/// Accepts both the flat `{"error": "invalid_grant"}` and the nested
/// `{"error": {"code": "..."}}` spellings. `error_description` is deliberately
/// never read: providers put request context there, which can include the
/// submitted credential.
pub(crate) fn safe_error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?;
    let code = match error {
        serde_json::Value::String(code) => code.as_str(),
        serde_json::Value::Object(object) => object.get("code")?.as_str()?,
        _ => return None,
    };
    is_safe_error_code(code).then(|| code.to_owned())
}

/// Report a failed OAuth request using only its status and a safe error code.
pub(crate) fn request_failed(
    action: &str,
    status: reqwest::StatusCode,
    body: &str,
) -> anyhow::Error {
    match safe_error_code(body) {
        Some(code) => anyhow::anyhow!("{action} failed with status {status} ({code})"),
        None => anyhow::anyhow!("{action} failed with status {status}"),
    }
}

/// Read a JSON object response, failing on any non-success status.
pub(crate) async fn post_for_value(
    client: &reqwest::Client,
    url: &str,
    encoding: Encoding,
    fields: &[(String, String)],
    action: &str,
) -> Result<serde_json::Value> {
    let (status, value) = post_for_status(client, url, encoding, fields, action).await?;
    if !status.is_success() {
        return Err(request_failed(action, status, ""));
    }
    Ok(value)
}

/// Read a JSON object response together with its status, without treating a
/// non-success status as a failure.
///
/// Device polling needs this: RFC 8628 reports `authorization_pending` and
/// `slow_down` as ordinary non-success responses that mean "keep polling", so
/// the caller must be able to see the decoded body. A non-success response that
/// is not a JSON object is still reported as a failure, because there is
/// nothing left for the caller to classify.
pub(crate) async fn post_for_status(
    client: &reqwest::Client,
    url: &str,
    encoding: Encoding,
    fields: &[(String, String)],
    action: &str,
) -> Result<(reqwest::StatusCode, serde_json::Value)> {
    let mut request = client.post(url).header("accept", "application/json");
    request = match encoding {
        Encoding::Form => request.form(fields),
        Encoding::Json => request.json(
            &fields
                .iter()
                .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        ),
    };
    let response = request
        .send()
        .await
        .with_context(|| format!("{action} request failed"))?;
    let status = response.status();
    let body = bounded_body(response, action).await?;
    let value: serde_json::Value = match serde_json::from_str::<serde_json::Value>(&body) {
        Ok(value) if value.is_object() => value,
        _ if status.is_success() => {
            bail!("{action} did not return a JSON object")
        }
        _ => return Err(request_failed(action, status, &body)),
    };
    Ok((status, value))
}

/// Read a required non-empty string field from a decoded response.
pub(crate) fn required_string(
    body: &serde_json::Value,
    field: &str,
    action: &str,
) -> Result<String> {
    body.get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .with_context(|| format!("{action} response is missing {field}"))
}

/// Read an optional positive number field, tolerating a numeric string.
///
/// Providers disagree on whether `interval` and `expires_in` are JSON numbers or
/// strings, and RFC 8628 explicitly allows a server to report no interval at
/// all. A non-positive or malformed value is reported as absent so the caller
/// applies its own default rather than adopting a hostile cadence.
pub(crate) fn optional_positive_u64(body: &serde_json::Value, field: &str) -> Option<u64> {
    let value = body.get(field)?;
    let parsed = value
        .as_u64()
        .or_else(|| value.as_str()?.trim().parse::<u64>().ok());
    parsed.filter(|number| *number > 0)
}

/// Read a token response into [`Tokens`].
///
/// `expires_in` is optional: when a provider omits it, `fallback_lifetime_secs`
/// supplies the lifetime so the credential still gets a finite, refreshable
/// expiry instead of being treated as permanent.
pub(crate) fn tokens_from_response(
    body: &serde_json::Value,
    access_field: &str,
    refresh_field: &str,
    fallback_lifetime_secs: u64,
    action: &str,
) -> Result<Tokens> {
    let access = required_string(body, access_field, action)?;
    let refresh = body
        .get(refresh_field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let lifetime = optional_positive_u64(body, "expires_in").unwrap_or(fallback_lifetime_secs);
    Ok(Tokens {
        access,
        refresh,
        expires_at: now_unix().saturating_add(lifetime),
    })
}

/// Validate a provider-supplied URL that octet will open in a browser or
/// present for a user to visit.
///
/// Only absolute `https` URLs are accepted. A provider that answers with
/// `javascript:`, `file:`, or a bare host cannot cause octet to launch an
/// arbitrary handler, and the check happens before the value is ever rendered.
pub(crate) fn https_url(value: &str, field: &str, action: &str) -> Result<url::Url> {
    let parsed =
        url::Url::parse(value).with_context(|| format!("{action} returned an unusable {field}"))?;
    if parsed.scheme() != "https" || !parsed.has_host() {
        bail!("{action} returned an untrusted {field}");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allow_listed_error_codes_reach_a_diagnostic() {
        assert_eq!(
            safe_error_code(r#"{"error":"invalid_grant"}"#).as_deref(),
            Some("invalid_grant")
        );
        assert_eq!(
            safe_error_code(r#"{"error":{"code":"slow_down"}}"#).as_deref(),
            Some("slow_down")
        );
        // Free-form prose is dropped: `error_description` is never read, and a
        // long or non-ASCII "code" is refused rather than echoed.
        assert_eq!(
            safe_error_code(
                r#"{"error":"invalid_grant","error_description":"leaked-secret-value"}"#
            )
            .as_deref(),
            Some("invalid_grant")
        );
        assert!(safe_error_code(r#"{"error":"token rejected: sk-secret"}"#).is_none());
        assert!(safe_error_code(r#"{"error":{"code":"bad code with spaces"}}"#).is_none());
        assert!(safe_error_code(r#"{"error":"é"}"#).is_none());
        assert!(safe_error_code("not json").is_none());
        assert!(safe_error_code("[]").is_none());
    }

    #[test]
    fn reflected_credentials_never_reach_error_diagnostics() {
        for secret in ["device-SECRET", "access_TOKEN", "sk-secret", "abc123"] {
            for error in [
                serde_json::json!(secret),
                serde_json::json!({"code": secret}),
            ] {
                let body = serde_json::json!({"error": error}).to_string();
                assert!(safe_error_code(&body).is_none());
                let diagnostic =
                    request_failed("device poll", reqwest::StatusCode::BAD_REQUEST, &body);
                assert!(!format!("{diagnostic:#} {diagnostic:?}").contains(secret));
            }
        }
    }

    #[test]
    fn optional_numbers_tolerate_strings_and_reject_non_positive() {
        let body = serde_json::json!({
            "interval": "5",
            "zero": 0,
            "negative": -3,
            "text": "abc",
            "real": 1.5
        });
        assert_eq!(optional_positive_u64(&body, "interval"), Some(5));
        assert_eq!(optional_positive_u64(&body, "zero"), None);
        assert_eq!(optional_positive_u64(&body, "negative"), None);
        assert_eq!(optional_positive_u64(&body, "text"), None);
        assert_eq!(optional_positive_u64(&body, "real"), None);
        assert_eq!(optional_positive_u64(&body, "absent"), None);
    }

    #[test]
    fn token_responses_apply_the_fallback_lifetime_only_when_expiry_is_absent() {
        let explicit = tokens_from_response(
            &serde_json::json!({"access_token":"a","refresh_token":"r","expires_in":3600}),
            "access_token",
            "refresh_token",
            60,
            "token",
        )
        .unwrap();
        assert_eq!(explicit.refresh.as_deref(), Some("r"));
        assert!(explicit.expires_at >= now_unix() + 3599);

        let implicit = tokens_from_response(
            &serde_json::json!({"access_token":"a"}),
            "access_token",
            "refresh_token",
            60,
            "token",
        )
        .unwrap();
        assert_eq!(implicit.refresh, None);
        assert!(implicit.expires_at >= now_unix() + 59);

        // A blank refresh token is treated as absent, not as an empty rotation.
        let blank = tokens_from_response(
            &serde_json::json!({"access_token":"a","refresh_token":"  "}),
            "access_token",
            "refresh_token",
            60,
            "token",
        )
        .unwrap();
        assert_eq!(blank.refresh, None);

        assert!(tokens_from_response(
            &serde_json::json!({"refresh_token":"r"}),
            "access_token",
            "refresh_token",
            60,
            "token",
        )
        .is_err());
    }

    #[test]
    fn browser_urls_must_be_absolute_https() {
        assert!(https_url("https://auth.x.ai/verify", "verification_uri", "device").is_ok());
        for hostile in [
            "http://auth.x.ai/verify",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "/relative/only",
            "not a url",
            "",
        ] {
            assert!(
                https_url(hostile, "verification_uri", "device").is_err(),
                "{hostile} must be refused"
            );
        }
    }
}
