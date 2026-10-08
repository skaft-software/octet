//! Tests for shared extension release downloads and checksum verification.
//!
//! Kept separate from extension_package.rs to exercise redirect trust, bounded
//! downloads, retry budgets, and terminal validation failures end to end.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn is_trusted_test_release_url(url: &reqwest::Url) -> bool {
    url.scheme() == "http" && url.host_str() == Some("127.0.0.1")
}

fn test_download_client(timeout: Option<Duration>) -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .user_agent("octet-release-download-test")
        .no_proxy()
        .retry(reqwest::retry::never())
        .redirect(redirect_policy(is_trusted_test_release_url));
    if let Some(timeout) = timeout {
        builder = builder.read_timeout(timeout);
    }
    builder.build().unwrap()
}

const TEST_RETRY_POLICY: DownloadRetryPolicy = DownloadRetryPolicy {
    max_attempts: 3,
    initial_backoff: Duration::ZERO,
    max_backoff: Duration::ZERO,
};

async fn test_download(url: String, client: &reqwest::Client) -> anyhow::Result<Vec<u8>> {
    download_bytes_with_client(
        client,
        reqwest::Url::parse(&url).unwrap(),
        MAX_CHECKSUM_BYTES,
        is_trusted_test_release_url,
        TEST_RETRY_POLICY,
    )
    .await
}

async fn test_download_to(
    url: &str,
    client: &reqwest::Client,
    maximum: usize,
    destination: Option<&Path>,
) -> anyhow::Result<Vec<u8>> {
    let url = reqwest::Url::parse(url).unwrap();
    if let Some(destination) = destination {
        let digest = download_file_with_client(
            client,
            url,
            destination,
            maximum as u64,
            is_trusted_test_release_url,
            TEST_RETRY_POLICY,
        )
        .await?;
        let bytes = fs::read(destination)?;
        assert_eq!(digest, digest_hex(Sha256::digest(&bytes).as_slice()));
        Ok(bytes)
    } else {
        download_bytes_with_client(
            client,
            url,
            maximum,
            is_trusted_test_release_url,
            TEST_RETRY_POLICY,
        )
        .await
    }
}

// Wiremock delays headers, not individual body chunks. Keep these loopback
// sockets open after scripted bytes to exercise real reqwest read timeouts.
struct ScriptedDownloadServer {
    url: String,
    attempts: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ScriptedDownloadServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn scripted_download_server(responses: Vec<&'static [u8]>) -> ScriptedDownloadServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/asset", listener.local_addr().unwrap());
    let attempts = Arc::new(AtomicUsize::new(0));
    let requests = Arc::clone(&attempts);
    let task = tokio::spawn(async move {
        let mut connections = Vec::new();
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let byte = socket.read_u8().await.unwrap();
                request.push(byte);
                assert!(request.len() <= 8192);
            }
            assert!(request.starts_with(b"GET /asset HTTP/1.1\r\n"));
            let attempt = requests.fetch_add(1, Ordering::SeqCst);
            let response = responses.get(attempt).copied().unwrap_or(
                b"HTTP/1.1 500 Unexpected retry\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            socket.write_all(response).await.unwrap();
            connections.push(socket);
        }
    });
    ScriptedDownloadServer {
        url,
        attempts,
        task,
    }
}

const PARTIAL_DOWNLOAD: &[u8] = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n10\r\nold-partial-body\r\n";
const SHORT_PARTIAL_DOWNLOAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nshort\r\n";
const COMPLETE_DOWNLOAD: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
const UNAVAILABLE_DOWNLOAD: &[u8] =
    b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[test]
fn checksum_parser_accepts_release_tool_spelling() {
    let digest = "a".repeat(64);
    let sums = format!("{digest}  ./other.tar.gz\n{digest}  *wanted.tar.gz\n");
    assert_eq!(checksum_for_asset(&sums, "wanted.tar.gz").unwrap(), digest);
}

#[test]
fn checksum_parser_rejects_duplicates() {
    let digest = "b".repeat(64);
    let sums = format!("{digest}  wanted.tar.gz\n{digest}  ./wanted.tar.gz\n");
    assert!(checksum_for_asset(&sums, "wanted.tar.gz").is_err());
}

#[test]
fn official_downloads_only_trust_github_https_hosts() {
    assert_eq!(
        RELEASE_REPOSITORY,
        "https://github.com/skaft-software/octet"
    );
    for accepted in [
        "https://github.com/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
        "https://github.com/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
        "https://release-assets.githubusercontent.com/github-production-release-asset/file?token=signed",
    ] {
        assert!(is_trusted_release_url(
            &reqwest::Url::parse(accepted).unwrap()
        ));
    }

    for rejected in [
        "http://github.com/skaft-software/octet/releases/download/file",
        "https://github.com.example.com/file",
        "https://raw.githubusercontent.com/skaft-software/octet/main/file",
        "https://github.com:8443/file",
    ] {
        assert!(!is_trusted_release_url(
            &reqwest::Url::parse(rejected).unwrap()
        ));
    }
}

#[tokio::test]
async fn release_download_follows_the_historical_repository_redirect() {
    let server = MockServer::start().await;
    let canonical_path = "/skaft-software/octet/releases/download/v0.7.0/SHA256SUMS";
    Mock::given(method("GET"))
        .and(path(
            "/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
        ))
        .respond_with(
            ResponseTemplate::new(301)
                .insert_header("location", format!("{}{canonical_path}", server.uri())),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(canonical_path))
        .respond_with(ResponseTemplate::new(200).set_body_string("canonical checksums"))
        .expect(1)
        .mount(&server)
        .await;

    let bytes = test_download(
        format!(
            "{}/skaft-software/ygg/releases/download/v0.7.0/SHA256SUMS",
            server.uri()
        ),
        &test_download_client(None),
    )
    .await
    .unwrap();

    assert_eq!(bytes, b"canonical checksums");
    server.verify().await;
}

#[tokio::test]
async fn canonical_release_download_succeeds_without_a_redirect() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(
            "/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string("checksums"))
        .expect(1)
        .mount(&server)
        .await;

    let bytes = test_download(
        format!(
            "{}/skaft-software/octet/releases/download/v0.7.1/SHA256SUMS",
            server.uri()
        ),
        &test_download_client(None),
    )
    .await
    .unwrap();

    assert_eq!(bytes, b"checksums");
    server.verify().await;
}

#[tokio::test]
async fn release_download_retries_a_transient_gateway_failure() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = Arc::clone(&attempts);
    Mock::given(method("GET"))
        .and(path("/transient"))
        .respond_with(move |_: &wiremock::Request| {
            if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(504)
            } else {
                ResponseTemplate::new(200).set_body_string("recovered")
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let bytes = test_download(
        format!("{}/transient", server.uri()),
        &test_download_client(None),
    )
    .await
    .unwrap();

    assert_eq!(bytes, b"recovered");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn release_download_stops_after_the_bounded_attempt_count() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/unavailable"))
        .respond_with(ResponseTemplate::new(503))
        .expect(TEST_RETRY_POLICY.max_attempts as u64)
        .mount(&server)
        .await;

    let error = test_download(
        format!("{}/unavailable", server.uri()),
        &test_download_client(None),
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("503"), "{error:#}");
    server.verify().await;
}

#[tokio::test]
async fn release_download_retries_a_transient_timeout() {
    let server = MockServer::start().await;
    let attempts = Arc::new(AtomicUsize::new(0));
    let response_attempts = Arc::clone(&attempts);
    Mock::given(method("GET"))
        .and(path("/timeout"))
        .respond_with(move |_: &wiremock::Request| {
            if response_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(200)
                    .set_body_string("late")
                    .set_delay(Duration::from_millis(250))
            } else {
                ResponseTemplate::new(200).set_body_string("recovered")
            }
        })
        .expect(2)
        .mount(&server)
        .await;
    let client = test_download_client(Some(Duration::from_millis(50)));

    let bytes = test_download(format!("{}/timeout", server.uri()), &client)
        .await
        .unwrap();

    assert_eq!(bytes, b"recovered");
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    server.verify().await;
}

#[tokio::test]
async fn release_download_does_not_retry_an_untrusted_redirect() {
    let server = MockServer::start().await;
    let rejected_target = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/untrusted-redirect"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/trusted-hop", server.uri())),
        )
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/trusted-hop"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "location",
            rejected_target.uri().replace("127.0.0.1", "localhost"),
        ))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&rejected_target)
        .await;

    for to_file in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("archive");
        let error = test_download_to(
            &format!("{}/untrusted-redirect", server.uri()),
            &test_download_client(None),
            16,
            to_file.then_some(destination.as_path()),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("refusing untrusted release redirect"),
            "{error:#}"
        );
        assert!(!retryable_download_error(&error));
        assert!(!destination.exists());
    }
    assert!(rejected_target
        .received_requests()
        .await
        .unwrap()
        .is_empty());
    rejected_target.verify().await;
    server.verify().await;
}

#[test]
fn release_download_backoff_is_bounded() {
    assert_eq!(DOWNLOAD_RETRY_POLICY.attempts(), 3);
    assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(1), Duration::from_millis(250));
    assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(2), Duration::from_millis(500));
    for retry in [3, 4, 64, usize::MAX] {
        assert_eq!(DOWNLOAD_RETRY_POLICY.backoff(retry), Duration::from_secs(1));
    }
    assert_eq!(TEST_RETRY_POLICY.backoff(2), Duration::ZERO);
    let local_timeout = anyhow::Error::new(io::Error::from(io::ErrorKind::TimedOut))
        .context("local file operation timed out");
    assert!(!retryable_download_error(&local_timeout));
}

#[tokio::test]
async fn release_download_mid_body_timeout_restarts_bytes_and_files() {
    for to_file in [false, true] {
        // A status and a body timeout must use the same three-attempt budget.
        let server = scripted_download_server(vec![
            UNAVAILABLE_DOWNLOAD,
            PARTIAL_DOWNLOAD,
            COMPLETE_DOWNLOAD,
        ])
        .await;
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("archive");
        let client = test_download_client(Some(Duration::from_millis(100)));
        let bytes = test_download_to(
            &server.url,
            &client,
            // The discarded prefix alone fills the limit. A leaked counter,
            // hash, buffer, file offset, or tail would fail this recovery.
            b"old-partial-body".len(),
            to_file.then_some(destination.as_path()),
        )
        .await
        .unwrap();

        assert_eq!(bytes, b"ok");
        assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
        assert!(!server.task.is_finished());
    }
}

#[tokio::test]
async fn release_download_mid_body_timeout_exhausts_one_budget_for_bytes_and_files() {
    for to_file in [false, true] {
        for first_response in [PARTIAL_DOWNLOAD, UNAVAILABLE_DOWNLOAD] {
            let server = scripted_download_server(vec![
                first_response,
                PARTIAL_DOWNLOAD,
                SHORT_PARTIAL_DOWNLOAD,
                COMPLETE_DOWNLOAD,
            ])
            .await;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("archive");
            let client = test_download_client(Some(Duration::from_millis(100)));
            let error = test_download_to(
                &server.url,
                &client,
                b"old-partial-body".len(),
                to_file.then_some(destination.as_path()),
            )
            .await
            .unwrap_err();

            assert!(error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
            assert_eq!(server.attempts.load(Ordering::SeqCst), 3);
            assert!(!server.task.is_finished());
            if to_file {
                // Failure returns no digest and retains only the last attempt,
                // not earlier partial data. The caller owns tempdir cleanup.
                assert_eq!(fs::read(&destination).unwrap(), b"short");
            }
        }
    }
}

#[tokio::test]
async fn release_download_transient_statuses_recover_or_exhaust_bytes_and_files() {
    for status in [408, 429, 500, 503, 504] {
        for to_file in [false, true] {
            for recover in [false, true] {
                let server = MockServer::start().await;
                let attempts = Arc::new(AtomicUsize::new(0));
                let requests = Arc::clone(&attempts);
                Mock::given(method("GET"))
                    .respond_with(move |_: &wiremock::Request| {
                        if requests.fetch_add(1, Ordering::SeqCst) == 1 && recover {
                            ResponseTemplate::new(200).set_body_string("ok")
                        } else {
                            ResponseTemplate::new(status)
                        }
                    })
                    .expect(if recover { 2 } else { 3 })
                    .mount(&server)
                    .await;
                let directory = tempfile::tempdir().unwrap();
                let destination = directory.path().join("archive");
                let result = test_download_to(
                    &server.uri(),
                    &test_download_client(None),
                    16,
                    to_file.then_some(destination.as_path()),
                )
                .await;
                if recover {
                    assert_eq!(result.unwrap(), b"ok");
                } else {
                    let error = result.unwrap_err();
                    assert_eq!(
                        error.downcast_ref::<reqwest::Error>().unwrap().status(),
                        Some(reqwest::StatusCode::from_u16(status).unwrap())
                    );
                    assert!(!destination.exists());
                }
                server.verify().await;
            }
        }
    }
}

#[tokio::test]
async fn release_download_terminal_4xx_are_not_replayed() {
    for status in [400, 401, 403, 404, 410, 422] {
        for to_file in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status))
                .expect(1)
                .mount(&server)
                .await;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("archive");
            let error = test_download_to(
                &server.uri(),
                &test_download_client(None),
                16,
                to_file.then_some(destination.as_path()),
            )
            .await
            .unwrap_err();
            assert!(!retryable_download_error(&error));
            assert_eq!(
                error.downcast_ref::<reqwest::Error>().unwrap().status(),
                Some(reqwest::StatusCode::from_u16(status).unwrap())
            );
            assert!(!destination.exists());
            server.verify().await;
        }
    }
}

#[tokio::test]
async fn release_download_size_and_protocol_failures_are_not_replayed() {
    for to_file in [false, true] {
        for (response, maximum, size_error) in [
            (COMPLETE_DOWNLOAD, 1, true),
            (PARTIAL_DOWNLOAD, 8, true),
            (b"not an HTTP response\r\n\r\n".as_slice(), 16, false),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\nZZ\r\n"
                    .as_slice(),
                16,
                false,
            ),
        ] {
            let server = scripted_download_server(vec![response, COMPLETE_DOWNLOAD]).await;
            let directory = tempfile::tempdir().unwrap();
            let destination = directory.path().join("archive");
            let error = test_download_to(
                &server.url,
                &test_download_client(Some(Duration::from_millis(100))),
                maximum,
                to_file.then_some(destination.as_path()),
            )
            .await
            .unwrap_err();
            assert!(!retryable_download_error(&error), "{error:#}");
            if size_error {
                assert!(format!("{error:#}").contains("byte limit"), "{error:#}");
            } else {
                assert!(!error.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
            }
            assert_eq!(server.attempts.load(Ordering::SeqCst), 1);
            assert!(!server.task.is_finished());
        }
    }
}

#[tokio::test]
async fn release_download_file_creation_errors_are_terminal_and_preserve_existing_files() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("replacement"))
        .expect(2)
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let existing = directory.path().join("existing");
    fs::write(&existing, b"keep me").unwrap();
    let missing_parent = directory.path().join("missing/archive");
    for destination in [&existing, &missing_parent] {
        let error = test_download_to(
            &server.uri(),
            &test_download_client(None),
            16,
            Some(destination),
        )
        .await
        .unwrap_err();
        assert!(format!("{error:#}").contains("cannot create extension download"));
        assert!(!retryable_download_error(&error));
    }
    assert_eq!(fs::read(existing).unwrap(), b"keep me");
    assert!(!missing_parent.exists());
    server.verify().await;
}

#[tokio::test]
async fn release_download_checksum_validation_is_terminal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not a release archive"))
        .expect(1)
        .mount(&server)
        .await;
    let client = test_download_client(None);
    let checksums = test_download(server.uri(), &client).await.unwrap();
    let error =
        checksum_for_asset(std::str::from_utf8(&checksums).unwrap(), "archive").unwrap_err();
    assert!(!retryable_download_error(&error));

    server.verify().await;
}

#[tokio::test]
async fn release_download_rejects_untrusted_initial_urls_before_sending() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("archive");
    for error in [
        download_bytes(&server.uri(), 16).await.unwrap_err(),
        download_file(&server.uri(), &destination, 16)
            .await
            .unwrap_err(),
    ] {
        assert!(format!("{error:#}").contains("refusing untrusted release URL"));
        assert!(!retryable_download_error(&error));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
    assert!(!destination.exists());
}
