//! Unit tests for `crate::auth`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::auth`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use http::header::{AUTHORIZATION, CONTENT_TYPE};

struct FakeResolver {
    value: String,
    extra: http::HeaderMap,
    fail: bool,
}

#[async_trait::async_trait]
impl CredentialResolver for FakeResolver {
    async fn resolve(&self) -> Result<ResolvedCredential, AuthError> {
        if self.fail {
            return Err(AuthError::Resolve);
        }
        Ok(ResolvedCredential {
            scheme: CredentialScheme::Bearer,
            value: Secret::from(self.value.clone()),
            extra_headers: self.extra.clone(),
        })
    }
}

#[tokio::test]
async fn test_resolve_headers_bearer_and_custom() {
    let auth_bearer = Auth::bearer("my-key");
    let resolved = resolve_headers(&auth_bearer).await.unwrap();
    assert_eq!(
        resolved
            .headers
            .get(AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer my-key"
    );
    assert_eq!(
        resolved.redactor.redact("provider echoed my-key"),
        "provider echoed [REDACTED]"
    );

    let auth_hdr = Auth::header(CONTENT_TYPE, "app-json");
    let resolved = resolve_headers(&auth_hdr).await.unwrap();
    assert_eq!(
        resolved
            .headers
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "app-json"
    );
}

#[test]
fn bounded_env_values_preserve_missing_and_invalid_unicode() {
    assert_eq!(
        bounded_env_value("MISSING", Err(std::env::VarError::NotPresent)).unwrap(),
        None
    );
    let error = bounded_env_value(
        "INVALID",
        Err(std::env::VarError::NotUnicode(std::ffi::OsString::from(
            "not-utf8",
        ))),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ConfigError::InvalidEnv(var) if var == "INVALID"
    ));
    assert!(matches!(
        secret_from_bounded_env("MISSING", Ok(None)),
        Err(ConfigError::MissingEnv(var)) if var == "MISSING"
    ));
    assert!(matches!(
        secret_from_bounded_env("INVALID", Err(ConfigError::InvalidEnv("INVALID".into()))),
        Err(ConfigError::MissingEnv(var)) if var == "INVALID"
    ));
}

#[test]
fn bounded_env_values_enforce_a_byte_limit() {
    let at_limit = "x".repeat(MAX_ENV_VALUE_BYTES);
    assert_eq!(
        bounded_env_value("BOUNDARY", Ok(at_limit.clone())).unwrap(),
        Some(at_limit)
    );

    let utf8_at_limit = "é".repeat(MAX_ENV_VALUE_BYTES / 2);
    assert_eq!(utf8_at_limit.len(), MAX_ENV_VALUE_BYTES);
    assert!(bounded_env_value("UTF8_BOUNDARY", Ok(utf8_at_limit)).is_ok());

    let error =
        bounded_env_value("TOO_LARGE", Ok("x".repeat(MAX_ENV_VALUE_BYTES + 1))).unwrap_err();
    assert!(matches!(
        error,
        ConfigError::EnvironmentValueTooLarge { var, max_bytes }
            if var == "TOO_LARGE" && max_bytes == MAX_ENV_VALUE_BYTES
    ));
}

#[tokio::test]
async fn env_auth_headers_accept_the_limit_and_reject_oversized_values() {
    let at_limit = "k".repeat(MAX_ENV_VALUE_BYTES);
    let read_at_limit = |var: &str| -> Result<Option<String>, ConfigError> {
        bounded_env_value(var, Ok(at_limit.clone()))
    };

    let bearer = resolve_headers_with_env(&Auth::bearer_env("BEARER_KEY"), &read_at_limit, None)
        .await
        .unwrap();
    assert_eq!(
        bearer.headers.get(AUTHORIZATION).unwrap().to_str().unwrap(),
        format!("Bearer {at_limit}")
    );
    assert!(bearer.headers.get(AUTHORIZATION).unwrap().is_sensitive());

    let header = resolve_headers_with_env(
        &Auth::header_env(http::HeaderName::from_static("x-api-key"), "HEADER_KEY"),
        &read_at_limit,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        header.headers.get("x-api-key").unwrap().to_str().unwrap(),
        at_limit
    );
    assert!(header.headers.get("x-api-key").unwrap().is_sensitive());

    let gateway = resolve_headers_with_env(
        &Auth::header_bearer_env(
            http::HeaderName::from_static("cf-aig-authorization"),
            "GATEWAY_KEY",
        ),
        &read_at_limit,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        gateway
            .headers
            .get("cf-aig-authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {at_limit}")
    );
    assert!(gateway
        .headers
        .get("cf-aig-authorization")
        .unwrap()
        .is_sensitive());

    let read_over_limit = |var: &str| -> Result<Option<String>, ConfigError> {
        bounded_env_value(var, Ok("x".repeat(MAX_ENV_VALUE_BYTES + 1)))
    };
    for auth in [
        Auth::bearer_env("BEARER_KEY"),
        Auth::header_env(http::HeaderName::from_static("x-api-key"), "HEADER_KEY"),
        Auth::header_bearer_env(
            http::HeaderName::from_static("cf-aig-authorization"),
            "GATEWAY_KEY",
        ),
    ] {
        let error = resolve_headers_with_env(&auth, &read_over_limit, None)
            .await
            .err()
            .expect("oversized environment credentials must fail");
        assert!(matches!(
            error,
            AuthError::EnvironmentValueTooLarge { var, max_bytes }
                if (var == "BEARER_KEY" || var == "HEADER_KEY" || var == "GATEWAY_KEY")
                    && max_bytes == MAX_ENV_VALUE_BYTES
        ));
    }
}

#[tokio::test]
async fn test_resolve_headers_dynamic() {
    let mut extra = http::HeaderMap::new();
    extra.insert(CONTENT_TYPE, http::HeaderValue::from_static("extra-val"));
    // insert colliding AUTHORIZATION to see if primary wins
    extra.insert(
        AUTHORIZATION,
        http::HeaderValue::from_static("extra-auth-colliding"),
    );

    let resolver = std::sync::Arc::new(FakeResolver {
        value: "dynamic-secret".to_string(),
        extra,
        fail: false,
    });

    let auth = Auth::dynamic(resolver);
    let resolved = resolve_headers(&auth).await.unwrap();

    // Check extra header is present
    assert_eq!(
        resolved
            .headers
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap(),
        "extra-val"
    );
    // Check primary auth header won the collision
    assert_eq!(
        resolved
            .headers
            .get(AUTHORIZATION)
            .unwrap()
            .to_str()
            .unwrap(),
        "Bearer dynamic-secret"
    );
    let redacted = resolved
        .redactor
        .redact("extra-val dynamic-secret Bearer dynamic-secret");
    assert!(!redacted.contains("extra-val"), "{redacted}");
    assert!(!redacted.contains("dynamic-secret"), "{redacted}");
}

#[test]
fn credential_redaction_prefers_longest_overlapping_value() {
    let mut redactor = CredentialRedactor::default();
    redactor.insert(Secret::from("key"));
    redactor.insert(Secret::from("key-long"));
    assert_eq!(redactor.redact("key-long/key"), "[REDACTED]/[REDACTED]");
}

#[test]
fn credential_redaction_includes_utf8_header_values() {
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-gateway-key",
        http::HeaderValue::from_bytes("clé-secrète".as_bytes()).unwrap(),
    );
    let mut redactor = CredentialRedactor::default();
    redactor.include_header_values(&headers);
    assert_eq!(
        redactor.redact("provider echoed clé-secrète"),
        "provider echoed [REDACTED]"
    );
}

#[tokio::test]
async fn sigv4_signs_a_fixed_request_against_the_aws_fixture() {
    let credentials = AwsCredentials::new(
        "AKIDEXAMPLE",
        "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
        None,
    )
    .unwrap();
    let signer = AwsSigV4Signer::new(credentials, "us-east-1", "iam")
        .unwrap()
        .with_clock(Arc::new(|| {
            UNIX_EPOCH + std::time::Duration::from_secs(1_440_938_160)
        }));
    let mut headers = http::HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/x-www-form-urlencoded; charset=utf-8"),
    );
    let request = SigningRequest::new(
        http::Method::GET,
        url::Url::parse("https://iam.amazonaws.com/?Action=ListUsers&Version=2010-05-08").unwrap(),
        bytes::Bytes::new(),
        headers,
    );

    let signed = signer.sign(&request).await.unwrap();
    assert_eq!(
        signed.headers.get(AUTHORIZATION).unwrap(),
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date, Signature=dd479fa8a80364edf2119ec24bebde66712ee9c9cb2b0d92eb3ab9ccdc0c3947"
    );
    assert_eq!(
        signed.headers.get("x-amz-date").unwrap(),
        "20150830T123600Z"
    );
    assert!(signed.headers.get(AUTHORIZATION).unwrap().is_sensitive());
}

#[tokio::test]
async fn sigv4_session_credentials_are_sensitive_and_redacted() {
    let signer = AwsSigV4Signer::new(
        AwsCredentials::new(
            "session-access",
            "session-secret",
            Some(Secret::from("session-token")),
        )
        .unwrap(),
        "us-east-1",
        "bedrock",
    )
    .unwrap()
    .with_clock(Arc::new(|| UNIX_EPOCH));
    let resolved = resolve_headers_for_request(
        &Auth::request_signer(Arc::new(signer)),
        http::Method::POST,
        url::Url::parse(
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/example/converse-stream",
        )
        .unwrap(),
        bytes::Bytes::new(),
        http::HeaderMap::new(),
    )
    .await
    .unwrap();

    assert!(resolved.headers["x-amz-security-token"].is_sensitive());
    let authorization = resolved.headers[AUTHORIZATION].to_str().unwrap();
    let redacted = resolved
        .redactor
        .redact(&format!("session-secret session-token {authorization}"));
    assert!(!redacted.contains("session-secret"));
    assert!(!redacted.contains("session-token"));
    assert!(!redacted.contains("Credential=session-access"));
}

#[tokio::test]
async fn sigv4_canonicalizes_encoded_path_and_query_and_exact_body() {
    let signer = AwsSigV4Signer::new(
        AwsCredentials::new(
            "AKIDEXAMPLE",
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            None,
        )
        .unwrap(),
        "eu-west-1",
        "bedrock",
    )
    .unwrap()
    .with_clock(Arc::new(|| {
        UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
    }));
    let url = url::Url::parse(
        "https://bedrock-runtime.eu-west-1.amazonaws.com/model/anthropic.claude-3-7-sonnet-20250219-v1%3A0/converse-stream?z=two%20words&z=slash%2Fvalue&space=a+b",
    )
    .unwrap();
    assert_eq!(
        url.path(),
        "/model/anthropic.claude-3-7-sonnet-20250219-v1%3A0/converse-stream"
    );
    assert_eq!(
        canonical_uri(&url),
        "/model/anthropic.claude-3-7-sonnet-20250219-v1%253A0/converse-stream"
    );
    assert_eq!(
        canonical_query(&url),
        "space=a%20b&z=slash%2Fvalue&z=two%20words"
    );

    let body = bytes::Bytes::from_static(
        br#"{"messages":[{"role":"user","content":[{"text":"exact bytes"}]}]}"#,
    );
    let mut headers = http::HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );
    let signed = signer
        .sign(&SigningRequest::new(
            http::Method::POST,
            url.clone(),
            body.clone(),
            headers.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(
        signed.headers["x-amz-content-sha256"],
        sha256_hex(body.as_ref())
    );
    let authorization = signed.headers[AUTHORIZATION].to_str().unwrap();
    assert!(authorization.contains("/eu-west-1/bedrock/aws4_request"));

    let changed = signer
        .sign(&SigningRequest::new(
            http::Method::POST,
            url,
            bytes::Bytes::from_static(
                br#"{"messages":[{"role":"user","content":[{"text":"changed bytes"}]}]}"#,
            ),
            headers,
        ))
        .await
        .unwrap();
    assert_ne!(
        authorization,
        changed.headers[AUTHORIZATION].to_str().unwrap()
    );
}

#[test]
fn test_auth_header_name() {
    assert_eq!(auth_header_name(&Auth::none()), None);
    assert_eq!(auth_header_name(&Auth::bearer("a")), Some(AUTHORIZATION));
    assert_eq!(
        auth_header_name(&Auth::header(CONTENT_TYPE, "a")),
        Some(CONTENT_TYPE)
    );
}

#[test]
fn vertex_api_key_mode_never_falls_back_to_adc() {
    let both = BTreeMap::from([
        (GOOGLE_VERTEX_API_KEY_VAR.to_owned(), "key-value".to_owned()),
        (
            GOOGLE_APPLICATION_CREDENTIALS_VAR.to_owned(),
            "/tmp/adc.json".to_owned(),
        ),
    ]);
    assert_eq!(
        select_vertex_credential(&both),
        Some(VertexCredential::ApiKey)
    );

    let blank_key = BTreeMap::from([
        (GOOGLE_VERTEX_API_KEY_VAR.to_owned(), "   ".to_owned()),
        (
            GOOGLE_APPLICATION_CREDENTIALS_VAR.to_owned(),
            "/tmp/adc.json".to_owned(),
        ),
    ]);
    assert_eq!(
        select_vertex_credential(&blank_key),
        Some(VertexCredential::ApplicationDefault)
    );

    let adc_only = BTreeMap::from([(
        GOOGLE_APPLICATION_CREDENTIALS_VAR.to_owned(),
        "/tmp/adc.json".to_owned(),
    )]);
    assert_eq!(
        select_vertex_credential(&adc_only),
        Some(VertexCredential::ApplicationDefault)
    );
    assert_eq!(select_vertex_credential(&BTreeMap::new()), None);

    match vertex_api_key_auth() {
        Auth::HeaderEnv { name, var } => {
            assert_eq!(name.as_str(), "x-goog-api-key");
            assert_eq!(var, GOOGLE_VERTEX_API_KEY_VAR);
        }
        other => panic!("unexpected vertex auth binding: {other:?}"),
    }
}

#[test]
fn anthropic_bearer_alias_precedence_is_declaration_order() {
    let both = BTreeMap::from([
        ("ANTHROPIC_AUTH_TOKEN".to_owned(), "auth-token".to_owned()),
        ("ANTHROPIC_OAUTH_TOKEN".to_owned(), "oauth-token".to_owned()),
    ]);
    assert!(matches!(
        anthropic_bearer_auth(&both),
        Some(Auth::BearerEnv { var }) if var == "ANTHROPIC_AUTH_TOKEN"
    ));

    let oauth_only =
        BTreeMap::from([("ANTHROPIC_OAUTH_TOKEN".to_owned(), "oauth-token".to_owned())]);
    assert!(matches!(
        anthropic_bearer_auth(&oauth_only),
        Some(Auth::BearerEnv { var }) if var == "ANTHROPIC_OAUTH_TOKEN"
    ));

    // A plain API key is not an OAuth/bearer alias: the caller's ordinary
    // x-api-key route must be used instead.
    let api_key_only = BTreeMap::from([("ANTHROPIC_API_KEY".to_owned(), "key".to_owned())]);
    assert!(anthropic_bearer_auth(&api_key_only).is_none());
    assert!(anthropic_bearer_auth(&BTreeMap::new()).is_none());
}

#[tokio::test]
async fn api_key_override_replaces_env_and_refuses_other_schemes() {
    let var = "OCTET_TEST_API_KEY_OVERRIDE_VAR";
    let auth = Auth::HeaderEnv {
        name: http::HeaderName::from_static("x-api-key"),
        var: var.to_owned(),
    };

    let override_secret = Secret::from("override-value");
    let resolved = resolve_headers_with_api_key(&auth, &BTreeMap::new(), Some(&override_secret))
        .await
        .unwrap();
    assert_eq!(
        resolved.headers["x-api-key"].to_str().unwrap(),
        "override-value"
    );
    assert_eq!(resolved.redactor.redact("override-value"), "[REDACTED]");
    assert!(resolved.headers["x-api-key"].is_sensitive());

    // Without an override the request-local env map supplies the value.
    let env = BTreeMap::from([(var.to_owned(), "env-value".to_owned())]);
    let resolved = resolve_headers_with_api_key(&auth, &env, None)
        .await
        .unwrap();
    assert_eq!(resolved.headers["x-api-key"].to_str().unwrap(), "env-value");

    // A fixed credential, dynamic resolver, signer, or unauthenticated
    // endpoint refuses the override instead of silently ignoring it.
    for refused in [Auth::bearer("fixed"), Auth::none()] {
        assert!(matches!(
            resolve_headers_with_api_key(&refused, &BTreeMap::new(), Some(&override_secret)).await,
            Err(AuthError::Resolve)
        ));
    }
}
