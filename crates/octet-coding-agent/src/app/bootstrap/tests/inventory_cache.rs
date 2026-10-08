//! Inventory cache
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::*;

#[test]
fn generic_discovery_accepts_openai_codex_and_bare_array_shapes() {
    for body in [
        serde_json::json!({"data": [{"id": "a", "context_length": 10}]}),
        serde_json::json!({"models": [{"slug": "b", "max_model_len": 20}]}),
        serde_json::json!([{"id": "c", "max_context_tokens": 30}]),
    ] {
        let models = api_models_from_response(&body).unwrap();
        assert_eq!(models.len(), 1);
        assert!(models[0].context_window.is_some());
    }
}

#[test]
fn discovery_rejects_error_objects_instead_of_hiding_them_as_empty() {
    assert!(api_models_from_response(&serde_json::json!({
        "error": {"message": "unauthorized"}
    }))
    .is_err());
    assert!(codex_models_from_response(&serde_json::json!({"models": []}), None).is_err());
}

#[test]
fn provider_inventory_cache_is_private_and_scoped_to_provider_url_and_account() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache/openrouter.json");
    let body = serde_json::json!({"data": [{"id": "model-a"}]});
    let first_key = credential_fingerprint("key-one");
    save_provider_inventory_cache(
        &path,
        "openrouter",
        "https://one.test/v1/models",
        &first_key,
        Some(&body),
    )
    .unwrap();
    match load_provider_inventory_cache(
        &path,
        "openrouter",
        "https://one.test/v1/models",
        &first_key,
    )
    .unwrap()
    {
        Some(CachedProviderInventory::Available(cached)) => assert_eq!(cached, body),
        _ => panic!("expected cached provider inventory"),
    }
    assert!(load_provider_inventory_cache(
        &path,
        "opencode",
        "https://one.test/v1/models",
        &first_key,
    )
    .unwrap()
    .is_none());
    assert!(load_provider_inventory_cache(
        &path,
        "openrouter",
        "https://two.test/v1/models",
        &first_key,
    )
    .unwrap()
    .is_none());
    assert!(
        load_provider_inventory_cache(
            &path,
            "openrouter",
            "https://one.test/v1/models",
            &credential_fingerprint("key-two"),
        )
        .unwrap()
        .is_none(),
        "changing accounts must invalidate the cached inventory"
    );
    save_provider_inventory_cache(
        &path,
        "openrouter",
        "https://one.test/v1/models",
        &first_key,
        None,
    )
    .unwrap();
    assert!(
        matches!(
            load_provider_inventory_cache(
                &path,
                "openrouter",
                "https://one.test/v1/models",
                &first_key,
            )
            .unwrap(),
            Some(CachedProviderInventory::Unavailable)
        ),
        "failed discovery must leave a reusable negative cache marker"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn provider_inventory_cache_names_and_future_timestamps_are_collision_safe() {
    assert_ne!(
        provider_inventory_cache_path("provider/a"),
        provider_inventory_cache_path("provider:a"),
    );
    assert!(cache_modified_is_stale(
        std::time::SystemTime::now() + Duration::from_secs(60),
        Duration::from_secs(1),
    ));
}

#[test]
fn negative_provider_cache_recovers_in_the_current_launch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache/openrouter.json");
    let url = "https://openrouter.test/v1/models";
    let credential = "key-one";
    let fingerprint = credential_fingerprint(credential);
    save_provider_inventory_cache(&path, "openrouter", url, &fingerprint, None).unwrap();
    let recovered = serde_json::json!({"data": [{"id": "recovered-model"}]});

    let body = cached_provider_inventory_with_fetch(
        path.clone(),
        "openrouter",
        url.to_string(),
        http::HeaderMap::new(),
        credential,
        ColdInventory::Wait,
        |_, _| Ok(recovered.clone()),
    )
    .unwrap()
    .expect("a foreground retry should recover the inventory");
    assert_eq!(body, recovered);
    assert!(matches!(
        load_provider_inventory_cache(&path, "openrouter", url, &fingerprint).unwrap(),
        Some(CachedProviderInventory::Available(body)) if body == recovered
    ));
}

/// A launch that already named its model must not call the endpoint while the
/// user waits: the cold cache refreshes in the background instead.
#[test]
fn cold_inventory_refresh_never_fetches_on_the_launch_thread() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache/deepseek.json");
    let url = "https://deepseek.test/v1/models";
    let credential = "key-one";
    let fingerprint = credential_fingerprint(credential);

    for cache_state in ["absent", "negative-marker"] {
        if cache_state == "negative-marker" {
            save_provider_inventory_cache(&path, "deepseek", url, &fingerprint, None).unwrap();
        } else {
            std::fs::remove_file(&path).ok();
        }
        let fetched = std::cell::Cell::new(false);
        let body = cached_provider_inventory_with_fetch(
            path.clone(),
            "deepseek",
            url.to_string(),
            http::HeaderMap::new(),
            credential,
            ColdInventory::Refresh,
            |_, _| {
                fetched.set(true);
                anyhow::bail!("the launch thread must not call the endpoint")
            },
        )
        .unwrap();
        assert!(body.is_none(), "no cached body is available yet");
        assert!(
            !fetched.get(),
            "a cold cache must refresh in the background, not on the launch thread"
        );
    }
}

#[test]
fn failed_provider_refresh_never_overwrites_last_good_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cache/openrouter.json");
    let url = "https://openrouter.test/v1/models";
    let fingerprint = credential_fingerprint("key-one");
    let last_good = serde_json::json!({"data": [{"id": "last-good"}]});
    save_provider_inventory_cache(&path, "openrouter", url, &fingerprint, Some(&last_good))
        .unwrap();

    let recovered = fetch_and_cache_provider_inventory_with(
        &path,
        "openrouter",
        url.to_string(),
        http::HeaderMap::new(),
        &fingerprint,
        |_, _| anyhow::bail!("transient failure"),
    )
    .expect("a transient refresh failure should retain last-good metadata");
    assert_eq!(recovered, last_good);
    assert!(matches!(
        load_provider_inventory_cache(&path, "openrouter", url, &fingerprint).unwrap(),
        Some(CachedProviderInventory::Available(body)) if body == last_good
    ));
}

#[test]
fn supplied_discovery_secrets_are_masked_before_truncation_regardless_of_shape() {
    for secret in [
        "alphabeticcredentialwithnodigit",
        "sk-test-quotedsecret",
        "short-key",
        "sëcret-秘密-without-digits",
    ] {
        let mut headers = http::HeaderMap::new();
        headers.insert(
            http::header::AUTHORIZATION,
            http::HeaderValue::from_str(&format!("Bearer {secret}")).unwrap(),
        );
        for reflected in [
            format!("key='{secret}' rejected"),
            format!("prefix:{secret}:suffix"),
            format!("{}{}", "x".repeat(MAX_REJECTION_DETAIL_CHARS - 2), secret),
        ] {
            let body = serde_json::json!({"error": {"message": reflected}}).to_string();
            let message =
                discovery_rejection(http::StatusCode::UNAUTHORIZED, body.as_bytes(), &headers);
            assert!(!message.contains(secret), "{message}");
            // A truncation boundary must not disclose the credential's prefix.
            let prefix: String = secret.chars().take(2).collect();
            assert!(!message.contains(&prefix), "{message}");
        }
    }
    let mut headers = http::HeaderMap::new();
    headers.insert(
        "x-custom-secret",
        http::HeaderValue::from_static("customalphabeticcredential"),
    );
    let message = discovery_rejection(
        http::StatusCode::FORBIDDEN,
        br#"{"message":"quoted='customalphabeticcredential'"}"#,
        &headers,
    );
    assert!(!message.contains("customalphabeticcredential"));
    assert!(message.contains("[redacted]"));

    let mut invalid = http::HeaderMap::new();
    invalid.insert(
        "x-custom-secret",
        http::HeaderValue::from_bytes(b"opaque-\xff-secret").unwrap(),
    );
    let message = discovery_rejection(
        http::StatusCode::FORBIDDEN,
        br#"{"message":"opaque secret reflected"}"#,
        &invalid,
    );
    assert_eq!(
        message,
        "model discovery request was rejected (HTTP 403 Forbidden)"
    );
}

/// Issue #454: an actionable provider rejection (Anthropic's 400 asking for
/// `anthropic-workspace-id`) must reach the user, bounded and redacted.
#[test]
fn rejected_discovery_reports_status_and_a_redacted_provider_message() {
    let anthropic = br#"{"type":"error","error":{"type":"invalid_request_error","message":"anthropic-workspace-id header is required for this API key"}}"#;
    assert_eq!(
        discovery_rejection(
            http::StatusCode::BAD_REQUEST,
            anthropic,
            &http::HeaderMap::new()
        ),
        "model discovery request was rejected (HTTP 400 Bad Request): \
         anthropic-workspace-id header is required for this API key"
    );

    let leaked = br#"{"error":{"message":"Invalid key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789\n\tsent"}}"#;
    let message = discovery_rejection(
        http::StatusCode::UNAUTHORIZED,
        leaked,
        &http::HeaderMap::new(),
    );
    assert!(message.starts_with("model discovery request was rejected (HTTP 401 Unauthorized): "));
    assert!(
        message.ends_with("Invalid key [redacted] sent"),
        "{message}"
    );
    assert!(!message.contains("sk-ant"));

    let opaque = "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
    let message = discovery_rejection(
        http::StatusCode::FORBIDDEN,
        serde_json::json!({"detail": format!("token {opaque} expired")})
            .to_string()
            .as_bytes(),
        &http::HeaderMap::new(),
    );
    assert!(message.ends_with("token [redacted] expired"), "{message}");

    // Raw bodies never pass through, and long messages are cut.
    assert_eq!(
        discovery_rejection(
            http::StatusCode::FORBIDDEN,
            b"<html>denied</html>",
            &http::HeaderMap::new()
        ),
        "model discovery request was rejected (HTTP 403 Forbidden)"
    );
    let long = serde_json::json!({"message": "word ".repeat(200)}).to_string();
    let message = discovery_rejection(
        http::StatusCode::BAD_REQUEST,
        long.as_bytes(),
        &http::HeaderMap::new(),
    );
    assert!(message.ends_with('…'));
    let prefix = "model discovery request was rejected (HTTP 400 Bad Request): ";
    assert_eq!(
        message.chars().count(),
        prefix.chars().count() + MAX_REJECTION_DETAIL_CHARS + 1
    );
}
