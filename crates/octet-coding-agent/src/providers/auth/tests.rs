//! Tests for provider credential resolution: environment variables, AWS
//! profile files, and the ordering the chain prefers between them.
//!
//! Moved out of auth.rs so the resolution chain itself stays readable on its
//! own. Every case here is a statement about precedence between credential
//! sources, and the assertions are far longer than the code they pin.

use super::*;
use crate::providers::contract::{
    ProviderAuthentication, ANTHROPIC, CLOUDFLARE_AI_GATEWAY, GEMINI, META, OPENAI,
};

#[test]
fn meta_api_key_is_private_bearer_auth_not_a_subscription_login() {
    let mut store_reads = 0;
    let credential = resolve_environment_with(
        &META,
        |variable| {
            assert_eq!(variable, "META_API_KEY");
            Ok(Some("meta-fixture-key".into()))
        },
        |_| {
            store_reads += 1;
            Ok(Some("stored-fixture-key".into()))
        },
    )
    .unwrap()
    .expect("environment key");
    assert_eq!(store_reads, 0);
    assert_eq!(credential.source, CredentialSource::Environment);
    let headers = environment_discovery_headers(&META.routes[0], &credential).unwrap();
    assert_eq!(
        headers[http::header::AUTHORIZATION].to_str().unwrap(),
        "Bearer meta-fixture-key"
    );
    assert!(headers[http::header::AUTHORIZATION].is_sensitive());
    assert!(matches!(
        environment_auth(&META.routes[0], &credential).unwrap(),
        Auth::BearerEnv { .. }
    ));
    assert!(!format!("{credential:?}").contains("meta-fixture-key"));

    let stored = resolve_environment_with(
        &META,
        |_| Ok(None),
        |provider_id| {
            assert_eq!(provider_id, "meta");
            Ok(Some("stored-fixture-key".into()))
        },
    )
    .unwrap()
    .expect("stored key");
    assert_eq!(stored.source, CredentialSource::Stored);
    assert!(matches!(
        environment_auth(&META.routes[0], &stored).unwrap(),
        Auth::Bearer(_)
    ));
    assert!(!format!("{stored:?}").contains("stored-fixture-key"));
}

#[test]
fn configured_environment_wins_without_reading_stored_keys() {
    for declaration in [&OPENAI, &ANTHROPIC, &GEMINI, &CLOUDFLARE_AI_GATEWAY] {
        let mut variables_read = Vec::new();
        let credential = resolve_environment_with(
            declaration,
            |variable| {
                variables_read.push(variable.to_owned());
                Ok(Some("synthetic-environment-key".into()))
            },
            |_| panic!("configured environment must not access credential storage"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(variables_read.len(), 1);
        assert_eq!(credential.source, CredentialSource::Environment);
        let auth = environment_auth(&declaration.routes[0], &credential).unwrap();
        assert!(matches!(
            auth,
            Auth::BearerEnv { .. } | Auth::HeaderEnv { .. } | Auth::HeaderBearerEnv { .. }
        ));
        assert!(!format!("{auth:?} {credential:?}").contains("synthetic-environment-key"));
    }
}

#[test]
fn invalid_environment_never_falls_back_to_a_different_identity() {
    for error in [
        octet_ai::ConfigError::InvalidEnv("OPENAI_API_KEY".into()),
        octet_ai::ConfigError::EnvironmentValueTooLarge {
            var: "OPENAI_API_KEY".into(),
            max_bytes: octet_ai::auth::MAX_ENV_VALUE_BYTES,
        },
    ] {
        let mut error = Some(error);
        assert!(resolve_environment_with(
            &OPENAI,
            |_| Err(error.take().unwrap()),
            |_| panic!("an invalid environment source must fail closed"),
        )
        .is_err());
    }
}

#[test]
fn environment_alias_order_is_preserved_before_stored_fallback() {
    let mut variables_read = Vec::new();
    let credential = resolve_environment_with(
        &ANTHROPIC,
        |variable| {
            variables_read.push(variable.to_owned());
            Ok(match variable {
                "ANTHROPIC_AUTH_TOKEN" => Some(" \n".into()),
                "ANTHROPIC_OAUTH_TOKEN" => Some("synthetic-oauth-token".into()),
                _ => panic!("later sources must not override a configured alias"),
            })
        },
        |_| panic!("environment aliases precede stored API keys"),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        variables_read,
        ["ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_OAUTH_TOKEN"]
    );
    assert!(
        matches!(environment_auth(&ANTHROPIC.routes[0], &credential).unwrap(), Auth::BearerEnv { var } if var == "ANTHROPIC_OAUTH_TOKEN")
    );
}

#[test]
fn absent_or_ineligible_credentials_contribute_no_stored_route() {
    assert!(
        resolve_environment_with(&OPENAI, |_| Ok(None), |_| Ok(None))
            .unwrap()
            .is_none()
    );
    for declaration in [
        &super::super::contract::CODEX,
        &super::super::contract::VERTEX,
        &super::super::contract::BEDROCK,
        &super::super::contract::AZURE_OPENAI,
        &CLOUDFLARE_AI_GATEWAY,
    ] {
        assert!(resolve_environment_with(
            declaration,
            |_| Ok(None),
            |_| panic!("one-key setup does not own this credential route"),
        )
        .unwrap()
        .is_none());
    }
}

#[test]
fn stored_keys_reactivate_native_catalogs_after_a_fresh_store_load() {
    use crate::provider_setup::BuiltinApiKeyStore;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("credentials/api-keys");
    let store = BuiltinApiKeyStore::for_test(root.clone());
    for id in ["openai", "anthropic", "gemini"] {
        store
            .save(id, "synthetic-stored-key".into(), false)
            .unwrap();
    }
    drop(store);
    let restarted = BuiltinApiKeyStore::for_test(root);
    for (declaration, expected_header) in [
        (&OPENAI, "authorization"),
        (&ANTHROPIC, "x-api-key"),
        (&GEMINI, "x-goog-api-key"),
    ] {
        let credential = resolve_environment_with(
            declaration,
            |_| Ok(Some(" \n".into())),
            |id| restarted.load(id).map_err(Into::into),
        )
        .unwrap()
        .unwrap();
        assert_eq!(credential.source, CredentialSource::Stored);
        assert_eq!(credential.value(), "synthetic-stored-key");
        if declaration.id == "anthropic" {
            assert_eq!(credential.variable(), "ANTHROPIC_API_KEY");
        }
        let headers = environment_discovery_headers(&declaration.routes[0], &credential).unwrap();
        assert!(headers[expected_header].is_sensitive());
        assert_eq!(
            headers[expected_header],
            if declaration.id == "openai" {
                "Bearer synthetic-stored-key"
            } else {
                "synthetic-stored-key"
            }
        );
        let auth = environment_auth(&declaration.routes[0], &credential).unwrap();
        if declaration.id == "openai" {
            assert!(matches!(auth, Auth::Bearer(_)));
        } else {
            assert!(matches!(&auth, Auth::Header { name, .. } if name == expected_header));
        }
        assert!(!format!("{credential:?} {auth:?}").contains("synthetic-stored-key"));
        // Same canonical registration seam startup uses; no GET /models or
        // inference traffic. Gemini supplies an offline static inventory.
        let mut catalog = octet_ai::ModelCatalog::default();
        crate::providers::register_environment_endpoints(
            &mut catalog,
            declaration,
            &credential,
            std::time::Duration::from_secs(30),
        )
        .unwrap();
        crate::providers::register_static_models(&mut catalog, declaration).unwrap();
        if declaration.id == "gemini" {
            assert!(catalog.models().next().is_some());
            for model in catalog.models() {
                let resolved = catalog.resolve(&model.id).unwrap();
                assert!(
                    matches!(&resolved.endpoint.auth, Auth::Header { name, .. } if name == "x-goog-api-key")
                );
            }
        }
    }
}

#[test]
fn malformed_stored_keys_fail_closed_without_echoing_file_contents() {
    use crate::provider_setup::BuiltinApiKeyStore;
    let directory = tempfile::tempdir().unwrap();
    let store = BuiltinApiKeyStore::for_test(directory.path().join("credentials/api-keys"));
    let path = store.path("openai").unwrap();
    octet_agent::secure_fs::write_private_atomic(&path, b"synthetic-secret-invalid-json", 4096)
        .unwrap();
    let error = resolve_environment_with(
        &OPENAI,
        |_| Ok(None),
        |id| store.load(id).map_err(Into::into),
    )
    .unwrap_err();
    assert!(!format!("{error:#} {error:?}").contains("synthetic-secret"));
    assert!(error.to_string().contains("credential store"));
}

fn fixture_credentials(label: &str) -> octet_ai::AwsCredentials {
    octet_ai::AwsCredentials::new(format!("{label}-access"), format!("{label}-secret"), None)
        .expect("fixture AWS credentials")
}

#[test]
fn aws_profile_parser_selects_only_the_requested_bounded_section() {
    let contents = "\
[default]
aws_access_key_id = default-access
aws_secret_access_key = default-secret

[profile enterprise]
aws_access_key_id = enterprise-access
aws_secret_access_key = enterprise-secret
aws_session_token = enterprise-token
ignored key = ignored
";
    let values = parse_aws_ini_section(contents, "profile enterprise").unwrap();
    assert_eq!(
        values.get("aws_access_key_id").map(String::as_str),
        Some("enterprise-access")
    );
    assert_eq!(
        values.get("aws_secret_access_key").map(String::as_str),
        Some("enterprise-secret")
    );
    assert_eq!(
        values.get("aws_session_token").map(String::as_str),
        Some("enterprise-token")
    );
    assert!(!values.contains_key("ignored key"));
}

#[test]
fn aws_chain_precedence_is_ordered_without_metadata_fallback() {
    // The documented chain order: environment keys, web identity, the
    // selected profile, then the indicated metadata sources. Every source
    // after the one that resolves must stay untouched.
    let web_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let web_flag = web_called.clone();
    let profile_flag = profile_called.clone();
    let metadata_flag = metadata_called.clone();
    let selected = resolve_aws_credentials_with(
        || Ok(Some(fixture_credentials("environment"))),
        move || {
            web_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("web-identity")))
        },
        move || {
            profile_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("profile")))
        },
        move || {
            metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("metadata")))
        },
    )
    .unwrap();
    assert!(selected.is_some());
    assert!(!web_called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!profile_called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

    let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let profile_flag = profile_called.clone();
    let metadata_flag = metadata_called.clone();
    let selected = resolve_aws_credentials_with(
        || Ok(None),
        || Ok(Some(fixture_credentials("web-identity"))),
        move || {
            profile_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("profile")))
        },
        move || {
            metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("metadata")))
        },
    )
    .unwrap();
    assert!(selected.is_some());
    assert!(!profile_called.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

    let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let metadata_flag = metadata_called.clone();
    let selected = resolve_aws_credentials_with(
        || Ok(None),
        || Ok(None),
        || Ok(Some(fixture_credentials("profile"))),
        move || {
            metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("metadata")))
        },
    )
    .unwrap();
    assert!(selected.is_some());
    assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

    let selected = resolve_aws_credentials_with(
        || Ok(None),
        || Ok(None),
        || Ok(None),
        || Ok(Some(fixture_credentials("metadata"))),
    )
    .unwrap();
    assert!(selected.is_some());

    // A failing earlier source fails the chain: a later source must not be
    // silently substituted for an environment/role the operator selected.
    let web_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let web_flag = web_called.clone();
    assert!(resolve_aws_credentials_with(
        || Err(anyhow::anyhow!("environment source failed")),
        move || {
            web_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("web-identity")))
        },
        || Ok(None),
        || Ok(None),
    )
    .is_err());
    assert!(!web_called.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn partial_environment_credentials_are_rejected_without_process_fallback() {
    let error = aws_environment_credentials_with(|name| {
        Ok(match name {
            "AWS_ACCESS_KEY_ID" => Some("environment-access".to_owned()),
            _ => None,
        })
    })
    .unwrap_err();
    assert!(error.to_string().contains("configured together"));
}

#[test]
fn profile_credential_process_is_not_executed_or_selected() {
    let values = parse_aws_ini_section(
        "[default]\ncredential_process = touch /tmp/must-not-run\n",
        "default",
    )
    .unwrap();
    assert!(values.contains_key("credential_process"));
    assert!(aws_profile_credentials_from_values(&values)
        .unwrap()
        .is_none());

    let values = parse_aws_ini_section(
        "[default]\naws_access_key_id = profile-access\naws_secret_access_key = profile-secret\ncredential_process = touch /tmp/must-not-run\n",
        "default",
    )
    .unwrap();
    assert!(aws_profile_credentials_from_values(&values)
        .unwrap()
        .is_some());
}

#[test]
fn container_metadata_urls_are_allowlisted_and_proxy_independent() {
    let relative = ecs_metadata_url_with(|name| {
        Ok(match name {
            "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI" => Some("/v2/credentials".to_owned()),
            "AWS_CONTAINER_CREDENTIALS_FULL_URI" => {
                Some("http://example.invalid/should-not-win".to_owned())
            }
            _ => None,
        })
    })
    .unwrap()
    .unwrap();
    assert_eq!(relative.host_str(), Some("169.254.170.2"));
    assert_eq!(relative.path(), "/v2/credentials");

    let local = ecs_metadata_url_with(|name| {
        Ok(if name == "AWS_CONTAINER_CREDENTIALS_FULL_URI" {
            Some("http://localhost:1234/credentials".to_owned())
        } else {
            None
        })
    })
    .unwrap()
    .unwrap();
    assert_eq!(local.host_str(), Some("localhost"));
    assert_eq!(local.port(), Some(1234));

    for value in [
        "http://example.invalid/credentials",
        "http://user@localhost/credentials",
        "http://localhost/credentials#fragment",
        "http://[::1]/credentials",
    ] {
        let result = ecs_metadata_url_with(|name| {
            Ok(if name == "AWS_CONTAINER_CREDENTIALS_FULL_URI" {
                Some(value.to_owned())
            } else {
                None
            })
        });
        assert!(result.is_err(), "metadata URL must be rejected: {value}");
    }
}

#[tokio::test]
async fn aws_bedrock_signer_resolves_current_credentials_for_each_request() {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls_for_resolver = calls.clone();
    let resolver: AwsCredentialsResolver = std::sync::Arc::new(move || {
        let call = calls_for_resolver.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(fixture_credentials(&format!("refresh-{call}"))))
    });
    let signer = AwsBedrockSigner {
        region: "us-west-2".to_owned(),
        resolver,
    };
    let request = octet_ai::SigningRequest::new(
        http::Method::POST,
        url::Url::parse(
            "https://bedrock-runtime.us-west-2.amazonaws.com/model/example/converse-stream",
        )
        .unwrap(),
        bytes::Bytes::from_static(b"{}"),
        http::HeaderMap::new(),
    );
    octet_ai::RequestSigner::sign(&signer, &request)
        .await
        .unwrap();
    octet_ai::RequestSigner::sign(&signer, &request)
        .await
        .unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn aws_metadata_credentials_are_bounded_and_require_both_key_components() {
    assert!(credentials_from_metadata_body(
        r#"{"AccessKeyId":"metadata-access","SecretAccessKey":"metadata-secret","Token":"metadata-token"}"#
            .to_owned(),
    )
    .is_ok());
    assert!(
        credentials_from_metadata_body(r#"{"AccessKeyId":"metadata-access"}"#.to_owned(),).is_err()
    );
    let oversized = "x".repeat(octet_ai::auth::MAX_ENV_VALUE_BYTES + 1);
    assert!(credentials_from_metadata_body(format!(
        r#"{{"AccessKeyId":"{oversized}","SecretAccessKey":"metadata-secret"}}"#
    ))
    .is_err());
    assert!(credentials_from_metadata_body(
        r#"{"AccessKeyId":"metadata\naccess","SecretAccessKey":"metadata-secret"}"#.to_owned(),
    )
    .is_err());
}

#[test]
fn aws_region_validation_rejects_untrusted_endpoint_components() {
    assert_eq!(
        checked_aws_region("eu-west-1".to_owned(), "test").unwrap(),
        "eu-west-1"
    );
    assert!(checked_aws_region("eu/west-1".to_owned(), "test").is_err());
}

#[test]
fn diagnostics_never_format_a_resolved_value() {
    let credential = EnvironmentCredential {
        variable: "TEST_PROVIDER_KEY",
        value: "secret-value-must-not-appear".to_owned(),
        source: CredentialSource::Environment,
    };
    assert!(!format!("{credential:?}").contains(&credential.value));
    assert!(missing_environment_diagnostic(&OPENAI)
        .action()
        .contains("OPENAI_API_KEY"));
}

#[test]
fn generated_presentation_selects_auth_header_without_a_secret() {
    let credential = EnvironmentCredential {
        variable: "TEST_PROVIDER_KEY",
        value: "not-formatted".to_owned(),
        source: CredentialSource::Environment,
    };
    let auth = environment_auth(&ANTHROPIC.routes[0], &credential).unwrap();
    assert!(matches!(auth, Auth::HeaderEnv { .. }));
    let gateway = environment_auth(&CLOUDFLARE_AI_GATEWAY.routes[0], &credential).unwrap();
    assert!(matches!(
        gateway,
        Auth::HeaderBearerEnv { ref name, .. }
            if name == http::HeaderName::from_static("cf-aig-authorization")
    ));
}

#[test]
fn discovery_headers_are_sensitive_and_route_selected() {
    let credential = EnvironmentCredential {
        variable: "TEST_PROVIDER_KEY",
        value: "not-formatted".to_owned(),
        source: CredentialSource::Environment,
    };
    let bearer = environment_discovery_headers(&OPENAI.routes[0], &credential).unwrap();
    assert!(bearer[http::header::AUTHORIZATION].is_sensitive());

    let api_key = environment_discovery_headers(&ANTHROPIC.routes[0], &credential).unwrap();
    assert!(api_key[http::HeaderName::from_static("x-api-key")].is_sensitive());

    let gateway =
        environment_discovery_headers(&CLOUDFLARE_AI_GATEWAY.routes[0], &credential).unwrap();
    let gateway_header = &gateway[http::HeaderName::from_static("cf-aig-authorization")];
    assert_eq!(gateway_header.to_str().unwrap(), "Bearer not-formatted");
    assert!(gateway_header.is_sensitive());

    let google = environment_discovery_headers(&GEMINI.routes[0], &credential).unwrap();
    assert!(google[http::HeaderName::from_static("x-goog-api-key")].is_sensitive());
}

#[test]
fn bearer_token_aliases_override_the_route_api_key_header() {
    // An Anthropic OAuth/subscription alias must be sent as
    // `Authorization: Bearer` even though the route's default presentation
    // is the `x-api-key` header. This keys on the credential variable.
    let auth_token = EnvironmentCredential::for_test("ANTHROPIC_AUTH_TOKEN", "token-value");
    assert!(matches!(
        environment_auth(&ANTHROPIC.routes[0], &auth_token).unwrap(),
        Auth::BearerEnv { .. }
    ));
    let headers = environment_discovery_headers(&ANTHROPIC.routes[0], &auth_token).unwrap();
    let authorization = &headers[http::header::AUTHORIZATION];
    assert_eq!(authorization.to_str().unwrap(), "Bearer token-value");
    assert!(authorization.is_sensitive());
    assert!(headers
        .get(http::HeaderName::from_static("x-api-key"))
        .is_none());

    let api_key = EnvironmentCredential::for_test("ANTHROPIC_API_KEY", "key-value");
    assert!(matches!(
        environment_auth(&ANTHROPIC.routes[0], &api_key).unwrap(),
        Auth::HeaderEnv { .. }
    ));
    let headers = environment_discovery_headers(&ANTHROPIC.routes[0], &api_key).unwrap();
    assert_eq!(
        headers[http::HeaderName::from_static("x-api-key")],
        "key-value"
    );
}

// --- AWS metadata activation (startup latency) -------------------------
//
// These are the deterministic acceptance tests for the activation rule. They
// assert *request counts*, never elapsed milliseconds: the defect was "an
// unrelated-provider launch issues a metadata request at all", and a timing
// threshold would pass or fail with machine load.

/// The ordinary case: a laptop with no AWS environment, no AWS profile, and
/// no EC2 DMI markers.
fn laptop_inputs() -> AwsMetadataActivationInputs {
    aws_metadata_activation_inputs_with(|_| Ok(None), |_| Ok(None), || None).unwrap()
}

/// A loopback metadata fixture. Returns the base URL and a request counter.
///
/// The server answers exactly `routes.len()` requests and then exits, so a
/// test that asserts the counter also asserts how many requests were made.
/// A request is counted as soon as it is read, before its response is
/// written, so the counter is already authoritative once a client can
/// observe that response and an assert never races the fixture thread.
fn metadata_fixture(
    routes: Vec<(&'static str, &'static str, &'static str)>,
) -> (url::Url, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = requests.clone();
    let expected = routes.len();
    std::thread::spawn(move || {
        for _ in 0..expected {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => request.push(byte[0]),
                }
            }
            let line = String::from_utf8_lossy(&request);
            let path = line
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_owned();
            let (status, body) = routes
                .iter()
                .find(|(route, _, _)| *route == path)
                .map(|(_, status, body)| (*status, *body))
                .unwrap_or(("404 Not Found", ""));
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            // Count the request before answering it: the client cannot
            // observe this response until the write below, so incrementing
            // afterwards would let a caller that has already read the final
            // body observe a stale count.
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        }
    });
    (
        url::Url::parse(&format!("http://{address}/")).unwrap(),
        requests,
    )
}

#[test]
fn unrelated_provider_launch_makes_zero_aws_metadata_requests() {
    // Headline acceptance criterion. The activation rule is evaluated from
    // empty environment/profile reads, exactly like a Codex user's laptop,
    // and the stubbed metadata sources count every request they receive.
    let activation = aws_metadata_activation_from(&laptop_inputs());
    assert_eq!(
        activation,
        AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
    );

    let container_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let ec2_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let container_counter = container_requests.clone();
    let ec2_counter = ec2_requests.clone();
    let credentials = aws_metadata_credentials_with(
        activation,
        None,
        |_| Ok(None),
        move |_, _| {
            container_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(fixture_credentials("container"))
        },
        move |_| {
            ec2_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("ec2")))
        },
    )
    .unwrap();

    assert!(credentials.is_none());
    assert_eq!(
        container_requests.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no container metadata request may be issued for an unrelated provider"
    );
    assert_eq!(
        ec2_requests.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "no EC2 metadata request may be issued for an unrelated provider"
    );
}

#[test]
fn disabled_activation_opens_no_connection_to_a_live_metadata_endpoint() {
    // The measured defect, asserted against a *live* loopback endpoint rather
    // than a stub closure: an unrelated-provider launch must not put a single
    // packet on the wire. The fixture is a blocking accept (so the control
    // connection is observed without racing a non-blocking accept against the
    // TCP handshake), the disabled call runs synchronously, and only then is
    // the listener drained: every connection the probe would have queued is
    // counted, and the assertion is a request count, not a timing threshold.
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let base = url::Url::parse(&format!("http://{address}/")).unwrap();
    let control_seen = std::sync::Arc::new(AtomicUsize::new(0));
    let observer = control_seen.clone();
    let (drain, drain_ready) = std::sync::mpsc::channel::<()>();
    let fixture = std::thread::spawn(move || {
        let _control = listener
            .accept()
            .expect("the loopback metadata fixture must be reachable");
        observer.fetch_add(1, AtomicOrdering::SeqCst);
        // Hold the listener open until the disabled call has returned, then
        // count every connection still queued behind the control connection.
        let _ = drain_ready.recv_timeout(std::time::Duration::from_secs(30));
        listener.set_nonblocking(true).unwrap();
        let mut drained = 0_usize;
        while listener.accept().is_ok() {
            drained += 1;
        }
        drained
    });
    let control_connection = std::net::TcpStream::connect(address)
        .expect("the loopback metadata fixture must be reachable");
    // Wait for the fixture's accept to observe the control connection: this
    // proves reachability before the assertion, without a timing threshold on
    // the behavior under test.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while control_seen.load(AtomicOrdering::SeqCst) == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "the control connection must be observed before the assertion runs"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    assert_eq!(control_seen.load(AtomicOrdering::SeqCst), 1);

    let activation = aws_metadata_activation_from(&laptop_inputs());
    assert_eq!(
        activation,
        AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
    );
    let endpoint = base.to_string();
    let credentials = aws_metadata_credentials_with(
        activation,
        None,
        move |name| Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone())),
        metadata_credentials_from_url,
        ec2_metadata_credentials,
    )
    .unwrap();
    assert!(credentials.is_none());

    drop(control_connection);
    drain.send(()).unwrap();
    let after = fixture.join().unwrap();
    assert_eq!(
        after, 0,
        "a disabled activation must not connect to the metadata endpoint at all"
    );
}

#[tokio::test]
async fn opt_in_activation_resolves_and_signs_with_live_metadata_credentials() {
    // The intentional path, end to end over real HTTP against the fixture:
    // token, role list, role credentials, then a Bedrock signature computed
    // from exactly those credentials.
    let (base, requests) = metadata_fixture(vec![
        ("/latest/api/token", "200 OK", "metadata-token"),
        (
            "/latest/meta-data/iam/security-credentials/",
            "200 OK",
            "fixture-role\n",
        ),
        (
            "/latest/meta-data/iam/security-credentials/fixture-role",
            "200 OK",
            r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
        ),
    ]);
    let mut inputs = laptop_inputs();
    inputs.profile_credential_source = Some("Ec2InstanceMetadata".to_owned());
    let activation = aws_metadata_activation_from(&inputs);
    assert_eq!(
        activation,
        AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource)
    );

    let endpoint = base.to_string();
    // Resolve on a blocking thread, exactly like the production signer
    // (`AwsBedrockSigner::sign` wraps its resolver in `spawn_blocking`): the
    // metadata client is a blocking client, and dropping its internal
    // runtime from inside the async test context panics.
    let credentials = tokio::task::spawn_blocking(move || {
        resolve_aws_credentials_with(
            || Ok(None),
            || Ok(None),
            || Ok(None),
            move || {
                aws_metadata_credentials_with(
                    activation,
                    None,
                    |name| Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone())),
                    metadata_credentials_from_url,
                    ec2_metadata_credentials,
                )
            },
        )
    })
    .await
    .expect("the blocking credential resolver must not panic")
    .unwrap()
    .expect("an indicated machine must still resolve instance credentials");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);

    let resolves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let resolves_for_signer = resolves.clone();
    let resolver: AwsCredentialsResolver = std::sync::Arc::new(move || {
        resolves_for_signer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Some(credentials.clone()))
    });
    let signer = AwsBedrockSigner {
        region: "us-east-1".to_owned(),
        resolver,
    };
    let request = octet_ai::SigningRequest::new(
        http::Method::POST,
        url::Url::parse("https://bedrock-runtime.us-east-1.amazonaws.com/model/example/converse")
            .unwrap(),
        bytes::Bytes::from_static(b"{}"),
        http::HeaderMap::new(),
    );
    octet_ai::RequestSigner::sign(&signer, &request)
        .await
        .expect("metadata credentials must sign a Bedrock request");
    assert_eq!(resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn static_keys_and_a_plain_profile_never_activate_the_metadata_probe() {
    // Static keys and a profile that declares no metadata credential source
    // are the ordinary laptop launch: the rule stays closed and the chain
    // never reaches the metadata source.
    let inputs = aws_metadata_activation_inputs_with(
        |name| {
            Ok(match name {
                "AWS_ACCESS_KEY_ID" => Some("AKIAEXAMPLE".to_owned()),
                "AWS_SECRET_ACCESS_KEY" => Some("fixture-secret".to_owned()),
                _ => None,
            })
        },
        |_| {
            Ok(Some(std::collections::BTreeMap::from([(
                "region".to_owned(),
                "us-east-1".to_owned(),
            )])))
        },
        || None,
    )
    .unwrap();
    assert_eq!(
        aws_metadata_activation_from(&inputs),
        AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
    );

    let metadata_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = metadata_calls.clone();
    let credentials = resolve_aws_credentials_with(
        || Ok(Some(fixture_credentials("environment"))),
        || panic!("the web-identity source must not run once environment keys resolved"),
        || panic!("the profile source must not run once environment keys resolved"),
        move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("metadata")))
        },
    )
    .unwrap();
    assert!(credentials.is_some());
    assert_eq!(metadata_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn activation_rule_is_a_pure_function_of_the_local_inputs() {
    let rule = aws_metadata_activation_from;
    let enabled = AwsMetadataActivation::Enabled;
    let disabled = AwsMetadataActivation::Disabled;
    use AwsMetadataIndication as Indication;
    use AwsMetadataSuppression as Suppression;

    assert_eq!(
        rule(&laptop_inputs()),
        disabled(Suppression::Unindicated),
        "no local indication must stay closed"
    );

    let mut inputs = laptop_inputs();
    inputs.ec2_metadata_disabled = Some("true".to_owned());
    assert_eq!(
        rule(&inputs),
        disabled(Suppression::ExplicitlyDisabled),
        "the standard AWS disable switch is respected"
    );

    let mut inputs = laptop_inputs();
    inputs.ec2_metadata_disabled = Some("TRUE".to_owned());
    assert_eq!(rule(&inputs), disabled(Suppression::ExplicitlyDisabled));

    let mut inputs = laptop_inputs();
    inputs.ec2_metadata_disabled = Some("maybe".to_owned());
    assert_eq!(
        rule(&inputs),
        disabled(Suppression::UnrecognizedDisableSetting),
        "unknown state stays closed instead of probing"
    );

    let mut inputs = laptop_inputs();
    inputs.ec2_metadata_disabled = Some("false".to_owned());
    assert_eq!(rule(&inputs), enabled(Indication::ExplicitlyNotDisabled));

    for value in ["1", "true", "TRUE", "yes", "on"] {
        let mut inputs = laptop_inputs();
        inputs.product_opt_in = Some(value.to_owned());
        assert_eq!(
            rule(&inputs),
            enabled(Indication::ProductOptIn),
            "opt-in value {value} must enable the probe"
        );
    }

    for value in ["0", "false", "no", "off"] {
        let mut inputs = laptop_inputs();
        inputs.product_opt_in = Some(value.to_owned());
        assert_eq!(
            rule(&inputs),
            disabled(Suppression::ExplicitlyDisabled),
            "opt-in value {value} must keep the probe closed"
        );
    }

    for (relative, full) in [
        (Some("/v2/credentials".to_owned()), None),
        (None, Some("http://localhost:1234/credentials".to_owned())),
    ] {
        let mut inputs = laptop_inputs();
        inputs.container_credentials_relative_uri = relative;
        inputs.container_credentials_full_uri = full;
        assert_eq!(rule(&inputs), enabled(Indication::ContainerCredentialsUri));
    }

    let mut inputs = laptop_inputs();
    inputs.metadata_service_endpoint = Some("http://169.254.169.254/".to_owned());
    assert_eq!(rule(&inputs), enabled(Indication::MetadataServiceEndpoint));

    let mut inputs = laptop_inputs();
    inputs.metadata_service_endpoint_mode = Some("IPv6".to_owned());
    assert_eq!(rule(&inputs), enabled(Indication::MetadataServiceEndpoint));

    let mut inputs = laptop_inputs();
    inputs.profile_credential_source = Some("Ec2InstanceMetadata".to_owned());
    assert_eq!(rule(&inputs), enabled(Indication::ProfileCredentialSource));

    let mut inputs = laptop_inputs();
    inputs.profile_credential_source = Some("Environment".to_owned());
    assert_eq!(
        rule(&inputs),
        disabled(Suppression::Unindicated),
        "a profile that does not declare metadata is not an indication"
    );

    // A bare EC2 instance profile: no environment marker and no profile
    // statement, only the local DMI vendor. It must still resolve, and it
    // must be re-checked here rather than trusted from the reader.
    let mut inputs = laptop_inputs();
    inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
    assert_eq!(rule(&inputs), enabled(Indication::Ec2InstanceIdentity));

    let mut inputs = laptop_inputs();
    inputs.instance_identity = Some(" VMware, Inc. ".to_owned());
    assert_eq!(
        rule(&inputs),
        disabled(Suppression::Unindicated),
        "another hypervisor's DMI vendor is not an indication"
    );

    let mut inputs = laptop_inputs();
    inputs.instance_identity = Some(String::new());
    assert_eq!(rule(&inputs), disabled(Suppression::Unindicated));

    // An explicit off wins over every enabling input.
    let mut inputs = laptop_inputs();
    inputs.ec2_metadata_disabled = Some("true".to_owned());
    inputs.container_credentials_relative_uri = Some("/v2/credentials".to_owned());
    inputs.product_opt_in = Some("true".to_owned());
    inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
    assert_eq!(rule(&inputs), disabled(Suppression::ExplicitlyDisabled));
}

#[test]
fn activation_reads_the_documented_opt_in_variable_and_profile_source() {
    let inputs = aws_metadata_activation_inputs_with(
        |name| {
            Ok(match name {
                AWS_METADATA_OPT_IN_VARIABLE => Some("1".to_owned()),
                _ => None,
            })
        },
        |_| Ok(None),
        || None,
    )
    .unwrap();
    assert_eq!(
        aws_metadata_activation_from(&inputs),
        AwsMetadataActivation::Enabled(AwsMetadataIndication::ProductOptIn)
    );

    let profile = std::collections::BTreeMap::from([(
        "credential_source".to_owned(),
        "EcsContainer".to_owned(),
    )]);
    let inputs = aws_metadata_activation_inputs_with(
        |_| Ok(None),
        move |config_file| {
            Ok(if config_file {
                None
            } else {
                Some(profile.clone())
            })
        },
        || None,
    )
    .unwrap();
    assert_eq!(
        aws_metadata_activation_from(&inputs),
        AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource)
    );
}

#[test]
fn profile_credential_source_is_read_from_the_config_file_it_is_documented_in() {
    // `credential_source` is documented for `~/.aws/config` (where
    // role-assumption profiles are declared), so a profile that only exists
    // there must still activate: reading the credentials file alone silently
    // suppressed an intentional EC2/ECS-backed Bedrock profile. The
    // credentials file stays a tolerated fallback for the profiles that
    // declare it there.
    use std::collections::BTreeMap;

    let in_config = |values: BTreeMap<String, String>| {
        move |config_file: bool| {
            Ok(if config_file {
                Some(values.clone())
            } else {
                None
            })
        }
    };

    for source in ["Ec2InstanceMetadata", "EcsContainer", "ec2instancemetadata"] {
        let inputs = aws_metadata_activation_inputs_with(
            |_| Ok(None),
            in_config(BTreeMap::from([(
                "credential_source".to_owned(),
                source.to_owned(),
            )])),
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource),
            "credential_source = {source} in ~/.aws/config must indicate the metadata probe"
        );
    }

    let inputs = aws_metadata_activation_inputs_with(
        |_| Ok(None),
        in_config(BTreeMap::from([(
            "credential_source".to_owned(),
            "Environment".to_owned(),
        )])),
        || None,
    )
    .unwrap();
    assert_eq!(
        aws_metadata_activation_from(&inputs),
        AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated),
        "a config profile that declares another credential source is not an indication"
    );
}

#[test]
fn indicated_metadata_probe_reaches_the_ec2_source() {
    // The intentional path: an EC2 user who opts in still resolves, and the
    // probe runs against the default IMDS endpoint.
    let mut inputs = laptop_inputs();
    inputs.product_opt_in = Some("true".to_owned());
    let activation = aws_metadata_activation_from(&inputs);
    let ec2_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_base = std::sync::Arc::new(std::sync::Mutex::new(None));
    let ec2_counter = ec2_requests.clone();
    let base_slot = seen_base.clone();
    let credentials = aws_metadata_credentials_with(
        activation,
        None,
        |_| Ok(None),
        |_, _| panic!("the container source must not run without a container URI"),
        move |base| {
            ec2_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *base_slot.lock().unwrap() = Some(base.to_string());
            Ok(Some(fixture_credentials("ec2")))
        },
    )
    .unwrap();

    assert!(credentials.is_some(), "the opt-in must still resolve");
    assert_eq!(ec2_requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        seen_base.lock().unwrap().as_deref(),
        Some(AWS_EC2_METADATA_ENDPOINT)
    );
}

#[test]
fn indicated_metadata_probe_prefers_the_container_uri() {
    let mut inputs = laptop_inputs();
    inputs.container_credentials_relative_uri = Some("/v2/credentials".to_owned());
    let activation = aws_metadata_activation_from(&inputs);
    let container_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_url = std::sync::Arc::new(std::sync::Mutex::new(None));
    let container_counter = container_requests.clone();
    let url_slot = seen_url.clone();
    let credentials = aws_metadata_credentials_with(
        activation,
        None,
        |name| {
            Ok((name == "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
                .then(|| "/v2/credentials".to_owned()))
        },
        move |url, ecs| {
            assert!(ecs, "container credentials are fetched with the ECS flag");
            container_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *url_slot.lock().unwrap() = Some(url.to_string());
            Ok(fixture_credentials("container"))
        },
        |_| panic!("the EC2 source must not run when a container URI is configured"),
    )
    .unwrap();

    assert!(credentials.is_some());
    assert_eq!(
        container_requests.load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(
        seen_url.lock().unwrap().as_deref(),
        Some("http://169.254.170.2/v2/credentials")
    );
}

#[test]
fn ec2_metadata_flow_is_bounded_to_the_documented_requests() {
    // Functional evidence that the intentional path still works end to end:
    // token, role list, role credentials. The counter pins the request count.
    let (base, requests) = metadata_fixture(vec![
        ("/latest/api/token", "200 OK", "metadata-token"),
        (
            "/latest/meta-data/iam/security-credentials/",
            "200 OK",
            "fixture-role\n",
        ),
        (
            "/latest/meta-data/iam/security-credentials/fixture-role",
            "200 OK",
            r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
        ),
    ]);
    let credentials = ec2_metadata_credentials(&base).unwrap();
    assert!(credentials.is_some(), "a reachable IMDS must still resolve");
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn an_unavailable_metadata_endpoint_costs_one_bounded_request() {
    // The probe is entered only when indicated, and when the endpoint is
    // blocked it fails fast and closed: exactly one request, no retry, and a
    // `None` that lets the unrelated provider be skipped.
    let (base, requests) = metadata_fixture(vec![("/latest/api/token", "404 Not Found", "")]);
    let credentials = ec2_metadata_credentials(&base).unwrap();
    assert!(
        credentials.is_none(),
        "an unavailable IMDS resolves to no credentials"
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn aws_metadata_service_endpoint_override_is_validated_fail_closed() {
    let default = aws_metadata_service_endpoint_with(|_| Ok(None), None).unwrap();
    assert_eq!(default.as_str(), AWS_EC2_METADATA_ENDPOINT);

    for accepted in [
        "http://127.0.0.1:8080",
        "http://localhost:8080/imds",
        "http://[::1]:8080",
        "https://metadata.internal.example",
    ] {
        let endpoint = aws_metadata_service_endpoint_with(|_| Ok(Some(accepted.to_owned())), None)
            .unwrap_or_else(|error| panic!("{accepted} must be accepted: {error}"));
        assert!(endpoint.as_str().ends_with('/'), "{endpoint}");
    }

    for rejected in [
        "",
        "http://metadata.example.com",
        "http://user@169.254.169.254/",
        "http://169.254.169.254/?query=1",
        "http://169.254.169.254/#fragment",
        "ftp://169.254.169.254/",
    ] {
        assert!(
            aws_metadata_service_endpoint_with(|_| Ok(Some(rejected.to_owned())), None).is_err(),
            "metadata endpoint override must be rejected: {rejected}"
        );
        assert!(
            aws_metadata_service_endpoint_with(|_| Ok(None), Some(rejected)).is_err(),
            "a profile endpoint must be validated exactly like the environment override: {rejected}"
        );
    }
}

#[test]
fn metadata_endpoint_precedence_is_standard_name_then_alias_then_profile() {
    // The AWS-standard name wins, the older alias still works, and a profile
    // that pins IMDS actually selects that endpoint (an indication that is
    // not also the target would leave the host unprobed). Every accepted
    // endpoint is normalized with a trailing slash so that the IMDS path
    // segments append instead of replacing the last component.
    let standard = aws_metadata_service_endpoint_with(
        |name| {
            Ok(match name {
                "AWS_EC2_METADATA_SERVICE_ENDPOINT" => {
                    Some("http://127.0.0.1:1/standard".to_owned())
                }
                "AWS_METADATA_SERVICE_ENDPOINT" => Some("http://127.0.0.1:2/alias".to_owned()),
                _ => None,
            })
        },
        Some("http://127.0.0.1:3/profile"),
    )
    .unwrap();
    assert_eq!(standard.as_str(), "http://127.0.0.1:1/standard/");

    let alias = aws_metadata_service_endpoint_with(
        |name| {
            Ok((name == "AWS_METADATA_SERVICE_ENDPOINT")
                .then(|| "http://127.0.0.1:2/alias".to_owned()))
        },
        Some("http://127.0.0.1:3/profile"),
    )
    .unwrap();
    assert_eq!(alias.as_str(), "http://127.0.0.1:2/alias/");

    let profile =
        aws_metadata_service_endpoint_with(|_| Ok(None), Some("http://127.0.0.1:3/profile"))
            .unwrap();
    assert_eq!(profile.as_str(), "http://127.0.0.1:3/profile/");
}

#[test]
fn the_profile_endpoint_activates_and_is_the_probe_target() {
    // End to end from the rule to the network layer: a profile that pins
    // IMDS activates the probe, and the live request lands on that pinned
    // endpoint instead of the link-local default.
    let (base, requests) = metadata_fixture(vec![
        ("/latest/api/token", "200 OK", "metadata-token"),
        (
            "/latest/meta-data/iam/security-credentials/",
            "200 OK",
            "fixture-role\n",
        ),
        (
            "/latest/meta-data/iam/security-credentials/fixture-role",
            "200 OK",
            r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
        ),
    ]);
    let inputs = aws_metadata_activation_inputs_with(
        |_| Ok(None),
        |config_file| {
            Ok(config_file.then(|| {
                std::collections::BTreeMap::from([(
                    "ec2_metadata_service_endpoint".to_owned(),
                    base.to_string(),
                )])
            }))
        },
        || None,
    )
    .unwrap();
    assert_eq!(
        aws_metadata_activation_from(&inputs),
        AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint)
    );

    let credentials = aws_metadata_credentials_with(
        aws_metadata_activation_from(&inputs),
        inputs.profile_metadata_service_endpoint.clone(),
        |_| Ok(None),
        metadata_credentials_from_url,
        ec2_metadata_credentials,
    )
    .unwrap();
    assert!(
        credentials.is_some(),
        "the profile-pinned endpoint must be probed and must resolve"
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn activation_reads_the_standard_endpoint_variable_names() {
    for variable in [
        "AWS_EC2_METADATA_SERVICE_ENDPOINT",
        "AWS_METADATA_SERVICE_ENDPOINT",
        "AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE",
        "AWS_METADATA_SERVICE_ENDPOINT_MODE",
    ] {
        let name = variable.to_owned();
        let inputs = aws_metadata_activation_inputs_with(
            move |probe| Ok((probe == name).then(|| "IPv6".to_owned())),
            |_| Ok(None),
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint),
            "{variable} must be an indication"
        );
    }
}

#[test]
fn dmi_markers_identify_an_ec2_instance_without_touching_the_network() {
    // The local statement that keeps a bare EC2 instance-profile host
    // working. Only a marker that names Amazon EC2 counts; missing, foreign,
    // oversized, non-UTF-8, and non-file markers all fail closed.
    let dir = tempfile::tempdir().unwrap();
    let markers = dir.path().join("sys/class/dmi/id");
    std::fs::create_dir_all(&markers).unwrap();
    assert_eq!(
        aws_instance_identity_from(dir.path()),
        None,
        "an empty fixture tree is not an EC2 instance"
    );

    std::fs::write(markers.join("sys_vendor"), "VMware, Inc.\n").unwrap();
    assert_eq!(aws_instance_identity_from(dir.path()), None);

    std::fs::write(markers.join("product_name"), "Amazon EC2\n").unwrap();
    assert_eq!(
        aws_instance_identity_from(dir.path()).as_deref(),
        Some(AWS_EC2_DMI_VENDOR),
        "any DMI marker naming Amazon EC2 is enough"
    );

    let oversized = markers.join("bios_vendor");
    std::fs::write(&oversized, "x".repeat(MAX_AWS_DMI_BYTES as usize + 1)).unwrap();
    std::fs::remove_file(markers.join("sys_vendor")).unwrap();
    std::fs::remove_file(markers.join("product_name")).unwrap();
    assert_eq!(
        aws_instance_identity_from(dir.path()),
        None,
        "an oversized marker is not evidence of an instance"
    );

    std::fs::remove_file(&oversized).unwrap();
    std::fs::create_dir(&oversized).unwrap();
    assert_eq!(
        aws_instance_identity_from(dir.path()),
        None,
        "a directory is not a marker file"
    );
}

#[test]
fn dmi_reads_are_bounded_by_content_not_file_size() {
    assert_eq!(
        read_dmi_marker_from(&b"Amazon EC2\n"[..]).as_deref(),
        Some(AWS_EC2_DMI_VENDOR)
    );
    assert_eq!(read_dmi_marker_from(&b"\xff"[..]), None);
    assert_eq!(read_dmi_marker_from(&b" \n"[..]), None);

    // Even a source with no EOF consumes only the bound plus one byte.
    let mut infinite = std::io::repeat(b'x');
    assert_eq!(read_dmi_marker_from(&mut infinite), None);
    let data = vec![b'x'; MAX_AWS_DMI_BYTES as usize + 16];
    let mut reader = std::io::Cursor::new(data);
    assert_eq!(read_dmi_marker_from(&mut reader), None);
    assert_eq!(reader.position(), MAX_AWS_DMI_BYTES + 1);
}

#[cfg(target_os = "linux")]
#[test]
fn sysfs_dmi_page_sized_metadata_does_not_hide_a_short_marker() {
    // A real sysfs attribute has page-sized metadata, unlike a tempfile.
    // DMI is optional (containers/ARM may not expose it); no network or
    // credential sources are consulted by this regression.
    let path = std::path::Path::new(AWS_EC2_DMI_MARKERS[0]);
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let expected = read_dmi_marker_from(file);
    assert_eq!(read_dmi_marker(path), expected);
}

#[test]
fn an_ec2_instance_identity_still_resolves_instance_credentials() {
    // End to end over real HTTP: the DMI indication activates the probe, and
    // the EC2 source resolves and counts its requests.
    let (base, requests) = metadata_fixture(vec![
        ("/latest/api/token", "200 OK", "metadata-token"),
        (
            "/latest/meta-data/iam/security-credentials/",
            "200 OK",
            "fixture-role\n",
        ),
        (
            "/latest/meta-data/iam/security-credentials/fixture-role",
            "200 OK",
            r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
        ),
    ]);
    let mut inputs = laptop_inputs();
    inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
    let activation = aws_metadata_activation_from(&inputs);
    assert_eq!(
        activation,
        AwsMetadataActivation::Enabled(AwsMetadataIndication::Ec2InstanceIdentity)
    );

    let endpoint = base.to_string();
    let credentials = aws_metadata_credentials_with(
        activation,
        None,
        move |name| Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone())),
        metadata_credentials_from_url,
        ec2_metadata_credentials,
    )
    .unwrap();
    assert!(
        credentials.is_some(),
        "an EC2 instance identity must still resolve instance-profile credentials"
    );
    assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
}

#[test]
fn anthropic_declaration_lists_bearer_aliases_before_api_key() {
    match ANTHROPIC.authentication {
        ProviderAuthentication::Environment { variables } => assert_eq!(
            variables,
            &[
                "ANTHROPIC_AUTH_TOKEN",
                "ANTHROPIC_OAUTH_TOKEN",
                "ANTHROPIC_API_KEY"
            ]
        ),
        other => panic!("unexpected authentication {other:?}"),
    }
}

// --- Bedrock API key + web identity (roadmap row 1c.5) -----------------

/// A loopback STS fixture that answers exactly one POST and records its
/// form body, so a test can assert both the request and the exchange.
fn sts_fixture(
    status: &'static str,
    body: &'static str,
) -> (url::Url, std::sync::Arc<std::sync::Mutex<String>>) {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let recorder = seen.clone();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut request = Vec::new();
        let mut byte = [0_u8; 1];
        while !request.ends_with(b"\r\n\r\n") {
            match stream.read(&mut byte) {
                Ok(0) | Err(_) => break,
                Ok(_) => request.push(byte[0]),
            }
        }
        let head = String::from_utf8_lossy(&request).into_owned();
        let content_length = head
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, value)| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let mut form = vec![0_u8; content_length];
        let _ = stream.read_exact(&mut form);
        *recorder.lock().unwrap() = String::from_utf8_lossy(&form).into_owned();
        let response = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    });
    (
        url::Url::parse(&format!("http://{address}/")).unwrap(),
        seen,
    )
}

const STS_SUCCESS_XML: &str =
    "<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult>\
<Credentials><AccessKeyId>fixture-access</AccessKeyId>\
<SecretAccessKey>fixture-secret</SecretAccessKey>\
<SessionToken>fixture-token</SessionToken>\
<Expiration>2030-01-01T00:00:00Z</Expiration></Credentials>\
</AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>";

#[test]
fn bedrock_api_key_takes_precedence_over_the_sigv4_chain() {
    let auth = aws_bedrock_auth_with(
        "us-east-1",
        |name| Ok((name == AWS_BEDROCK_BEARER_VARIABLE).then(|| "bedrock-api-key".to_owned())),
        || panic!("the SigV4 credential chain must not run when a Bedrock API key is configured"),
    )
    .unwrap()
    .expect("the configured API key must select an auth strategy");
    match auth {
        Auth::BearerEnv { var } => assert_eq!(var, AWS_BEDROCK_BEARER_VARIABLE),
        _ => panic!("a configured Bedrock API key must select bearer auth"),
    }
}

#[test]
fn blank_bedrock_api_key_falls_back_to_the_sigv4_chain() {
    let resolves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = resolves.clone();
    let auth = aws_bedrock_auth_with(
        "us-east-1",
        |name| Ok((name == AWS_BEDROCK_BEARER_VARIABLE).then(|| "   ".to_owned())),
        move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials("sigv4")))
        },
    )
    .unwrap()
    .expect("the SigV4 chain must still provide an auth strategy");
    assert!(matches!(auth, Auth::RequestSigner(_)));
    assert_eq!(resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn bedrock_without_any_credential_source_offers_no_auth() {
    let auth = aws_bedrock_auth_with("us-east-1", |_| Ok(None), || Ok(None)).unwrap();
    assert!(
        auth.is_none(),
        "no credential source means no auth strategy"
    );
}

#[test]
fn web_identity_requires_both_standard_variables() {
    let token_only = AwsWebIdentityInputs {
        token_file: Some("/var/run/secrets/eks.amazonaws.com/serviceaccount/token".to_owned()),
        ..AwsWebIdentityInputs::default()
    };
    let error = aws_web_identity_credentials_from(
        &token_only,
        "us-east-1",
        |_| panic!("no token file may be read while the configuration is half set"),
        |_, _| panic!("no STS request may be sent while the configuration is half set"),
    )
    .unwrap_err();
    assert!(error.to_string().contains("AWS_ROLE_ARN"), "{error}");

    let role_only = AwsWebIdentityInputs {
        role_arn: Some("arn:aws:iam::123456789012:role/octet".to_owned()),
        ..AwsWebIdentityInputs::default()
    };
    let error = aws_web_identity_credentials_from(
        &role_only,
        "us-east-1",
        |_| panic!("no token file may be read while the configuration is half set"),
        |_, _| panic!("no STS request may be sent while the configuration is half set"),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("AWS_WEB_IDENTITY_TOKEN_FILE"),
        "{error}"
    );
}

#[test]
fn web_identity_without_configuration_makes_no_request() {
    let credentials = aws_web_identity_credentials_from(
        &AwsWebIdentityInputs::default(),
        "us-east-1",
        |_| panic!("no token file may be read without configuration"),
        |_, _| panic!("no STS request may be sent without configuration"),
    )
    .unwrap();
    assert!(credentials.is_none());
}

#[test]
fn web_identity_posts_the_documented_sts_form_and_resolves_credentials() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("token");
    std::fs::write(&token_path, "fixture-oidc-token\n").unwrap();
    let (endpoint, seen) = sts_fixture("200 OK", STS_SUCCESS_XML);
    let inputs = AwsWebIdentityInputs {
        token_file: Some(token_path.to_string_lossy().into_owned()),
        role_arn: Some("arn:aws:iam::123456789012:role/octet-web-identity".to_owned()),
        session_name: None,
        endpoint_override: Some(endpoint.to_string()),
    };

    let credentials = aws_web_identity_credentials_from(
        &inputs,
        "us-east-1",
        read_bounded_web_identity_token,
        sts_assume_role_with_web_identity,
    )
    .unwrap()
    .expect("a successful exchange must resolve credentials");
    // Signing proves the parsed credentials are structurally usable and
    // that the session token is presented as the SigV4 security token.
    let signer =
        octet_ai::AwsSigV4Signer::new(credentials, "us-east-1".to_owned(), "bedrock".to_owned())
            .expect("the STS credentials must be a valid SigV4 credential set");
    let request = octet_ai::SigningRequest::new(
        http::Method::POST,
        url::Url::parse("https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse")
            .unwrap(),
        bytes::Bytes::from_static(b"{}"),
        http::HeaderMap::new(),
    );
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(octet_ai::RequestSigner::sign(&signer, &request))
        .expect("web-identity credentials must sign a Bedrock request");

    // The exchange used the documented form and never put the token in a URL.
    let form = seen.lock().unwrap().clone();
    assert!(form.contains("Action=AssumeRoleWithWebIdentity"), "{form}");
    assert!(form.contains("Version=2011-06-15"), "{form}");
    assert!(
        form.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Foctet-web-identity"),
        "{form}"
    );
    assert!(form.contains("RoleSessionName=octet"), "{form}");
    assert!(
        form.contains("WebIdentityToken=fixture-oidc-token"),
        "{form}"
    );
    assert!(
        !form.contains("fixture-token"),
        "no response token in the request"
    );
}

#[test]
fn web_identity_sts_failure_is_reported_and_never_downgraded() {
    let directory = tempfile::tempdir().unwrap();
    let token_path = directory.path().join("token");
    std::fs::write(&token_path, "fixture-oidc-token").unwrap();
    let (endpoint, _) = sts_fixture(
        "403 Forbidden",
        "<ErrorResponse><Error><Code>ExpiredTokenException</Code>\
         <Message>token expired</Message></Error></ErrorResponse>",
    );
    let inputs = AwsWebIdentityInputs {
        token_file: Some(token_path.to_string_lossy().into_owned()),
        role_arn: Some("arn:aws:iam::123456789012:role/octet-web-identity".to_owned()),
        session_name: Some("octet-test".to_owned()),
        endpoint_override: Some(endpoint.to_string()),
    };
    let error = aws_web_identity_credentials_from(
        &inputs,
        "us-east-1",
        read_bounded_web_identity_token,
        sts_assume_role_with_web_identity,
    )
    .unwrap_err();
    let message = error.to_string();
    assert!(message.contains("403"), "{message}");
    assert!(message.contains("ExpiredTokenException"), "{message}");
    // Provider prose and the token itself are never echoed.
    assert!(!message.contains("token expired"), "{message}");
    assert!(!message.contains("fixture-oidc-token"), "{message}");
}

#[test]
fn web_identity_token_file_is_bounded_and_never_empty() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing");
    assert!(read_bounded_web_identity_token(&missing.to_string_lossy()).is_err());

    let empty = directory.path().join("empty");
    std::fs::write(&empty, "  \n").unwrap();
    let error = read_bounded_web_identity_token(&empty.to_string_lossy()).unwrap_err();
    assert!(error.to_string().contains("empty"), "{error}");

    let oversized = directory.path().join("oversized");
    std::fs::write(
        &oversized,
        "x".repeat(MAX_AWS_WEB_IDENTITY_TOKEN_BYTES as usize + 1),
    )
    .unwrap();
    let error = read_bounded_web_identity_token(&oversized.to_string_lossy()).unwrap_err();
    assert!(error.to_string().contains("bounded"), "{error}");

    let valid = directory.path().join("valid");
    std::fs::write(&valid, " token-value\n").unwrap();
    assert_eq!(
        read_bounded_web_identity_token(&valid.to_string_lossy()).unwrap(),
        "token-value"
    );
}

#[test]
fn sts_xml_parsing_requires_every_credential_field() {
    // The full documented result parses into a usable credential set; a
    // missing field, an empty field, and an unknown XML entity all fail
    // closed instead of producing an approximate credential value.
    assert!(sts_credentials_from_xml(STS_SUCCESS_XML).is_ok());

    let missing_token = STS_SUCCESS_XML.replace("<SessionToken>fixture-token</SessionToken>", "");
    let error = sts_credentials_from_xml(&missing_token).unwrap_err();
    assert!(
        error.to_string().contains("incomplete credentials"),
        "{error}"
    );

    let empty_secret = STS_SUCCESS_XML.replace(
        "<SecretAccessKey>fixture-secret</SecretAccessKey>",
        "<SecretAccessKey></SecretAccessKey>",
    );
    assert!(sts_credentials_from_xml(&empty_secret).is_err());

    assert_eq!(
        xml_tag("<a>one&amp;two&lt;three&gt;</a>", "a").as_deref(),
        Some("one&two<three>")
    );
    assert_eq!(xml_tag("<a>bad&nbsp;value</a>", "a"), None);
    assert_eq!(xml_tag("<a></a>", "a"), None);
    assert_eq!(xml_tag("<b>x</b>", "a"), None);
}

#[test]
fn sts_endpoint_is_regional_and_validates_overrides_fail_closed() {
    assert_eq!(
        aws_sts_endpoint("us-west-2", None).unwrap().as_str(),
        "https://sts.us-west-2.amazonaws.com/"
    );
    assert_eq!(
        aws_sts_endpoint("us-east-1", Some("http://127.0.0.1:9/"))
            .unwrap()
            .as_str(),
        "http://127.0.0.1:9/"
    );
    for rejected in [
        "http://sts.example.com/",
        "https://user@sts.example.com/",
        "https://sts.example.com/?query=1",
        "https://sts.example.com/#fragment",
        "ftp://sts.example.com/",
    ] {
        assert!(
            aws_sts_endpoint("us-east-1", Some(rejected)).is_err(),
            "STS endpoint override must be rejected: {rejected}"
        );
    }
    assert!(aws_sts_endpoint("not a region", None).is_err());
}

#[test]
fn role_arn_and_session_name_are_validated_fail_closed() {
    assert!(checked_aws_role_arn("arn:aws:iam::123456789012:role/octet").is_ok());
    for rejected in ["", "role/octet", "arn:aws:iam::123:role/bad?query"] {
        assert!(checked_aws_role_arn(rejected).is_err(), "{rejected}");
    }
    for accepted in ["octet", "octet-session_1@example.com"] {
        assert!(
            checked_aws_role_session_name(accepted).is_ok(),
            "{accepted}"
        );
    }
    for rejected in ["", "a", "two words", "control\u{7}"] {
        assert!(
            checked_aws_role_session_name(rejected).is_err(),
            "{rejected:?}"
        );
    }
}
