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
    // The loopback servers are IP literals, so no hostname lookup ever runs.
    fetch_client(|| Err(std::io::Error::other("tests never resolve a hostname"))).unwrap()
}

#[test]
fn the_refresh_resolves_only_its_api_host() {
    let url = reqwest::Url::parse(API_URL).unwrap();
    assert_eq!(url.host_str(), Some(API_HOST));
}

/// getaddrinfo cannot be cancelled. A lookup that never returns must not hold
/// the runtime or the process open once the refresh is abandoned, as it would
/// on the runtime's blocking pool.
#[test]
fn a_pending_dns_lookup_does_not_delay_runtime_or_process_exit() {
    const CHILD_MODE: &str = "OCTET_TEST_MODELS_DEV_DNS_EXIT";
    const TEST: &str =
        "models_dev::tests::a_pending_dns_lookup_does_not_delay_runtime_or_process_exit";
    const DROPPED: &str = "runtime dropped while DNS remains held";

    if std::env::var_os(CHILD_MODE).is_some() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (started, ready) = tokio::sync::oneshot::channel();
            let client = fetch_client(move || {
                started.send(()).unwrap();
                // Deliberately never release this lookup, even after the
                // runtime drops. Only child process exit ends it.
                loop {
                    std::thread::park();
                }
            })
            .unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("metadata.json");
            let refresh = tokio::spawn(async move {
                let _ = refresh_at(&client, "http://models.dev/api.json", &path, 0).await;
                drop(directory);
            });
            tokio::time::timeout(Duration::from_secs(2), ready)
                .await
                .unwrap()
                .unwrap();
            refresh.abort();
            assert!(refresh.await.unwrap_err().is_cancelled());
        });
        let start = std::time::Instant::now();
        drop(runtime); // Normal shutdown, as the binary's main does.
        println!("{DROPPED}: {:?}", start.elapsed());
        return;
    }

    // A subprocess turns a shutdown regression into a bounded failure instead
    // of a hung suite.
    let start = std::time::Instant::now();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env(CHILD_MODE, "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut timed_out = false;
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() >= Duration::from_secs(3) {
            timed_out = true;
            child.kill().unwrap();
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !timed_out && output.status.success() && stdout.contains(DROPPED),
        "held DNS blocked runtime/process exit or the child failed\n{stdout}\n{stderr}",
    );
}

/// Terminal input wakes on a Tokio timer. Blocking cache work that held the
/// worker driving timers would freeze typing and Ctrl+D until it finished.
#[test]
fn blocking_cache_work_leaves_runtime_timers_running() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (release, released) = std::sync::mpsc::channel::<()>();
    // Ends a stall that would otherwise hang the test.
    let watchdog = release.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(5));
        let _ = watchdog.send(());
    });
    runtime.block_on(async move {
        let work = tokio::spawn(async move {
            // A timer wakes this task on the driver's worker, as the response
            // body wakes the refresh.
            tokio::time::sleep(Duration::from_millis(20)).await;
            off_runtime(move || released.recv().is_ok()).await.unwrap()
        });
        let start = std::time::Instant::now();
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let elapsed = start.elapsed();
        let _ = release.send(());
        assert!(
            elapsed < Duration::from_secs(2),
            "runtime timers stalled for {elapsed:?} behind blocking cache work"
        );
        assert!(work.await.unwrap());
    });
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
