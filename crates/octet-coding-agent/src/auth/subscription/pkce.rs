#![allow(missing_docs)]

//! PKCE (RFC 7636) verifier/challenge generation for browser-based logins.
//!
//! Octet never opens a listening socket for these flows. The provider redirects
//! to a loopback URI that nothing serves, and the user pastes the final redirect
//! URL back into the terminal. Binding a port would add a fixed, guessable
//! local listener whose availability the login would then depend on; a pasted
//! redirect keeps the flow working over SSH and inside containers, which is
//! where subscription sign-in usually happens.

use anyhow::{bail, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

/// Bytes of entropy behind a verifier, matching the RFC 7636 recommendation of
/// 32 octets of random input hashed into a 43-character base64url string.
const VERIFIER_BYTES: usize = 32;

/// A PKCE verifier and its S256 challenge.
pub(crate) struct Pkce {
    /// The secret held by the client and replayed at the token endpoint.
    pub verifier: String,
    /// `BASE64URL(SHA256(ASCII(verifier)))`, sent on the authorize request.
    pub challenge: String,
}

/// Generate a fresh S256 PKCE pair from the operating system CSPRNG.
pub(crate) fn generate() -> Result<Pkce> {
    let mut entropy = [0_u8; VERIFIER_BYTES];
    SystemRandom::new()
        .fill(&mut entropy)
        .map_err(|_| anyhow::anyhow!("secure random source is unavailable"))?;
    // RFC 7636 §4.1 restricts the verifier to unreserved characters; base64url
    // without padding is a subset of those, so no further filtering is needed.
    let verifier = URL_SAFE_NO_PAD.encode(entropy);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok(Pkce {
        verifier,
        challenge,
    })
}

/// Recover the authorization code and state a user pasted back from the browser.
///
/// The user may paste the whole redirect URL, a `code#state` fragment, a bare
/// query string, or just the code. `expected_state` is the value octet sent on
/// the authorize request; a mismatched state means the response belongs to a
/// different authorization attempt and is refused rather than exchanged.
pub(crate) fn parse_pasted_redirect(
    pasted: &str,
    expected_state: &str,
) -> Result<(String, String)> {
    let value = pasted.trim();
    if value.is_empty() {
        bail!("no authorization code was provided");
    }

    let (code, state) = if let Ok(url) = url::Url::parse(value) {
        if url.scheme().is_empty() {
            (String::new(), String::new())
        } else {
            (
                url.query_pairs()
                    .find(|(name, _)| name == "code")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_default(),
                url.query_pairs()
                    .find(|(name, _)| name == "state")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or_default(),
            )
        }
    } else if value.contains('#') {
        let mut parts = value.splitn(2, '#');
        (
            parts.next().unwrap_or_default().to_owned(),
            parts.next().unwrap_or_default().to_owned(),
        )
    } else if let Some(index) = value.find("code=") {
        let query = &value[index..];
        let pairs: Vec<(String, String)> = url::form_urlencoded::parse(query.as_bytes())
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect();
        (
            pairs
                .iter()
                .find(|(name, _)| name == "code")
                .map(|(_, value)| value.clone())
                .unwrap_or_default(),
            pairs
                .iter()
                .find(|(name, _)| name == "state")
                .map(|(_, value)| value.clone())
                .unwrap_or_default(),
        )
    } else {
        (value.to_owned(), String::new())
    };

    if let Some(error) = value_error(value) {
        bail!("the provider reported an authorization error: {error}");
    }

    let code = code.trim();
    if code.is_empty() {
        bail!("the pasted redirect URL did not contain an authorization code");
    }
    if !state.is_empty() && state != expected_state {
        bail!("OAuth state mismatch; start the login again");
    }
    Ok((code.to_owned(), expected_state.to_owned()))
}

/// Read an `error` query parameter, if the pasted value carries one.
fn value_error(value: &str) -> Option<String> {
    let trimmed = value
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let query = trimmed.split_once('?').map(|(_, rest)| rest)?;
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == "error")
        .map(|(_, value)| value.into_owned())
        .filter(|error| !error.is_empty() && error.len() <= 64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pairs_are_random_verifiers_with_s256_challenges() {
        let first = generate().unwrap();
        let second = generate().unwrap();
        assert_ne!(first.verifier, second.verifier);
        assert_eq!(first.verifier.len(), 43);
        assert_eq!(first.challenge.len(), 43);
        // S256 is `BASE64URL(SHA256(ASCII(verifier)))`.
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(first.verifier.as_bytes()));
        assert_eq!(first.challenge, expected);
        assert!(first
            .verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte)));
    }

    #[test]
    fn a_pasted_redirect_url_supplies_the_code_and_state() {
        let expected = "state-value";
        let cases = [
            "http://localhost:53692/callback?code=abc&state=state-value",
            "http://localhost:53692/callback?state=state-value&code=abc",
            "code=abc&state=state-value",
            "abc#state-value",
            "abc",
        ];
        for pasted in cases {
            let (code, state) = parse_pasted_redirect(pasted, expected).unwrap();
            assert_eq!(code, "abc", "{pasted}");
            assert_eq!(state, expected, "{pasted}");
        }
        // Surrounding whitespace from a terminal paste is tolerated.
        assert_eq!(
            parse_pasted_redirect(
                "  http://localhost/callback?code=abc&state=state-value \n",
                expected
            )
            .unwrap()
            .0,
            "abc"
        );
    }

    #[test]
    fn a_mismatched_or_missing_state_is_refused() {
        assert!(parse_pasted_redirect(
            "http://localhost/callback?code=abc&state=other",
            "expected"
        )
        .is_err());
        assert!(parse_pasted_redirect("", "expected").is_err());
        assert!(parse_pasted_redirect("   ", "expected").is_err());
        assert!(
            parse_pasted_redirect("http://localhost/callback?state=expected", "expected").is_err()
        );
    }

    #[test]
    fn a_provider_reported_authorization_error_surfaces() {
        let error =
            parse_pasted_redirect("http://localhost/callback?error=access_denied", "expected")
                .unwrap_err()
                .to_string();
        assert!(error.contains("access_denied"), "{error}");
    }
}
