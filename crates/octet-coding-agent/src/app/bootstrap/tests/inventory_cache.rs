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
