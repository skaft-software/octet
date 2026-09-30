//! Unit tests for `crate::models_dev`.

use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FIXTURE: &str = include_str!("../../../octet-ai/tests/fixtures/models-dev/api-subset.json");
const HOUR_MS: u64 = 60 * 60 * 1000;

#[test]
fn a_missing_stale_or_future_cache_needs_a_refresh() {
    let now = 100 * HOUR_MS;
    assert!(needs_refresh(None, now));
    assert!(!needs_refresh(Some(now - HOUR_MS), now));
    assert!(!needs_refresh(Some(now), now));
    assert!(needs_refresh(Some(now - 6 * HOUR_MS), now));
    assert!(needs_refresh(Some(now + HOUR_MS), now));
}

fn client() -> reqwest::Client {
    fetch_client().unwrap()
}

/// A first refresh caches the checked catalog; a fresh cache makes no request;
/// a stale one revalidates with its ETag and a 304 only renews the timestamp.
#[tokio::test]
async fn refresh_caches_revalidates_and_stays_quiet_while_fresh() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .and(header("if-none-match", "\"v1\""))
        .respond_with(ResponseTemplate::new(304))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"v1\"")
                .set_body_string(FIXTURE),
        )
        .expect(1)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("models-dev").join("metadata.json");
    let url = format!("{}/api.json", server.uri());
    let now = 1_000 * HOUR_MS;

    let outcome = refresh_at(&client(), &url, &cache, now).await.unwrap();
    let RefreshOutcome::Updated(metadata) = outcome else {
        panic!("first refresh must update: {outcome:?}");
    };
    assert_eq!(metadata.names["openai/gpt-9-preview"], "GPT-9 Preview");
    let stored = read_cache(&cache).expect("cache written");
    assert_eq!(stored.etag.as_deref(), Some("\"v1\""));
    assert_eq!(stored.fetched_at_ms, now);
    assert_eq!(stored.source_bytes, FIXTURE.len() as u64);
    assert!(stored
        .rejected
        .iter()
        .any(|line| line.starts_with("openrouter/vendor/model:free: ")));

    // Fresh: no request at all (each mock above expects exactly one hit).
    assert!(matches!(
        refresh_at(&client(), &url, &cache, now + HOUR_MS)
            .await
            .unwrap(),
        RefreshOutcome::Fresh
    ));

    let later = now + 7 * HOUR_MS;
    assert!(matches!(
        refresh_at(&client(), &url, &cache, later).await.unwrap(),
        RefreshOutcome::NotModified
    ));
    let renewed = read_cache(&cache).unwrap();
    assert_eq!(renewed.fetched_at_ms, later);
    assert_eq!(renewed.metadata, stored.metadata);
    server.verify().await;
}

#[tokio::test]
async fn failed_refreshes_keep_the_previous_cache() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("metadata.json");
    for response in [
        ResponseTemplate::new(500),
        ResponseTemplate::new(200).set_body_string("not json"),
        ResponseTemplate::new(200).set_body_string("{}"),
        ResponseTemplate::new(200).set_body_string("x".repeat(MAX_SOURCE_BYTES + 1)),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(response)
            .mount(&server)
            .await;
        let url = format!("{}/api.json", server.uri());
        assert!(refresh_at(&client(), &url, &cache, HOUR_MS).await.is_err());
        assert!(read_cache(&cache).is_none());
    }
}

#[test]
fn a_cache_from_another_octet_version_or_schema_is_ignored() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("metadata.json");
    let file = |schema: u32, version: &str| CacheFile {
        schema,
        octet_version: version.to_owned(),
        fetched_at_ms: 1,
        etag: None,
        source_sha256: String::new(),
        source_bytes: 0,
        rejected: Vec::new(),
        metadata: LiveModelMetadata::default(),
    };
    write_cache(&cache, &file(CACHE_SCHEMA, env!("CARGO_PKG_VERSION"))).unwrap();
    assert!(read_cache(&cache).is_some());
    write_cache(&cache, &file(CACHE_SCHEMA, "0.0.1")).unwrap();
    assert!(read_cache(&cache).is_none());
    write_cache(&cache, &file(CACHE_SCHEMA + 1, env!("CARGO_PKG_VERSION"))).unwrap();
    assert!(read_cache(&cache).is_none());
}
