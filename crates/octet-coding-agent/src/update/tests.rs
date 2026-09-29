//! Tests for the self-update flow: version comparison, download and replace,
//! and the failure modes that must leave the running binary untouched.
//!
//! Moved out of update.rs so the update sequence stays readable on its own;
//! the suite builds throwaway executable fixtures, which is a different
//! concern from deciding whether an update should be offered.

use super::*;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[cfg(unix)]
#[tokio::test]
async fn installer_download_enforces_its_own_byte_limit() {
    for (size, succeeds) in [(262_144, true), (262_145, false)] {
        let mut command = tokio::process::Command::new("python3");
        command.args([
            "-c",
            &format!("import sys; sys.stdout.buffer.write(b'x' * {size})"),
        ]);
        let result = download_installer_script(
            &mut command,
            "https://example.invalid/installer",
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(result.is_ok(), succeeds);
        if let Ok(bytes) = result {
            assert_eq!(bytes.len(), size);
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn installer_download_deadline_covers_body_and_exit_and_reaps_child() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("pid");
    for script in [
        "printf untrusted_installer_bytes; exec sleep 30",
        "exec 1>&-; exec sleep 30",
    ] {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(format!("printf '%s\\n' $$ > \"$1\"; {script}"))
            .arg("installer-probe")
            .arg(&pid_file);
        let started = std::time::Instant::now();
        let error = download_installer_script(
            &mut command,
            "https://example.invalid/installer",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(!error.to_string().contains("untrusted_installer_bytes"));
        assert!(started.elapsed() < Duration::from_secs(5));
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(!Command::new("kill")
            .arg("-0")
            .arg(pid.trim())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn installed_version_probe_requires_success_and_exact_target_without_exposing_output() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("octet");
    let version = semver::Version::new(0, 7, 5);
    for (script, succeeds) in [
        ("printf 'octet 0.7.5\\n'", true),
        ("printf 'octet 0.7.4\\n'", false),
        ("printf 'octet 0.7.5\\n'; exit 1", false),
        ("printf 'private diagnostic\\n'", false),
        ("dd if=/dev/zero bs=1025 count=1 2>/dev/null", false),
    ] {
        std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = verify_installed_version(&executable, &version).await;
        assert_eq!(result.is_ok(), succeeds);
        if let Err(error) = result {
            assert!(!error.to_string().contains("private diagnostic"));
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn installed_version_probe_has_a_bounded_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("octet");
    std::fs::write(&executable, "#!/bin/sh\nexec sleep 30\n").unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    let error = tokio::time::timeout(
        Duration::from_secs(7),
        verify_installed_version(&executable, &semver::Version::new(0, 7, 5)),
    )
    .await
    .expect("version probe deadline")
    .unwrap_err();
    assert!(error.to_string().contains("timed out"));
}

fn stable_release(tag: &str) -> serde_json::Value {
    serde_json::json!({ "tag_name": tag, "draft": false, "prerelease": false })
}

#[test]
fn startup_only_reports_newer_stable_semver_precedence() {
    for (current, tag, expected) in [
        ("0.7.4", "v0.7.5", Some((0, 7, 5))),
        ("0.7.4", "0.8.0", Some((0, 8, 0))),
        ("0.9.0", "v0.10.0", Some((0, 10, 0))),
        ("0.7.5-rc.1", "v0.7.5", Some((0, 7, 5))),
        ("0.7.4", "v0.7.5+remote.text", Some((0, 7, 5))),
        ("0.7.4", "v0.7.4", None),
        ("0.7.4", "v0.7.3", None),
        ("0.10.0", "v0.9.0", None),
        ("0.7.4", "v0.7.4+remote.text", None),
        ("0.7.4+aaa", "v0.7.4+zzz", None),
        ("0.7.4", "v0.7.5-rc.1", None),
        ("0.7.4", "v1.0.0-alpha", None),
        ("0.7.4", "vv0.7.5", None),
        ("0.7.4", " v0.7.5 ", None),
        ("0.7.4", "v0.07.5", None),
        ("0.7.4", "v0.7", None),
        ("0.7.4", "v0.7.5\ninstall something", None),
        ("0.7.4", "v0.7.5+\u{1b}[31m", None),
        ("0.7.4", "v18446744073709551616.0.0", None),
    ] {
        let body = serde_json::to_vec(&stable_release(tag)).unwrap();
        assert_eq!(
            newer_stable_release(&body, &current.parse().unwrap()),
            expected.map(|(major, minor, patch)| semver::Version::new(major, minor, patch)),
            "current={current}, tag={tag:?}",
        );
    }
}

#[test]
fn startup_rejects_draft_prerelease_and_malformed_metadata() {
    let current = semver::Version::new(0, 7, 4);
    for (field, value) in [
        ("draft", serde_json::json!(true)),
        ("prerelease", serde_json::json!(true)),
        ("draft", serde_json::json!("false")),
        ("prerelease", serde_json::Value::Null),
        ("tag_name", serde_json::json!(75)),
    ] {
        let mut release = stable_release("v0.7.5");
        release[field] = value;
        assert_eq!(
            newer_stable_release(&serde_json::to_vec(&release).unwrap(), &current),
            None,
            "{release}",
        );
    }
    for field in ["draft", "prerelease", "tag_name"] {
        let mut release = stable_release("v0.7.5");
        release.as_object_mut().unwrap().remove(field);
        assert_eq!(
            newer_stable_release(&serde_json::to_vec(&release).unwrap(), &current),
            None,
        );
    }
    for body in [b"not json".as_slice(), b"[]", b"null", b"\xff"] {
        assert_eq!(newer_stable_release(body, &current), None);
    }
}

#[tokio::test]
async fn startup_requests_once_without_credentials_and_returns_only_version_numbers() {
    assert_eq!(
        LATEST_RELEASE_URL,
        "https://api.github.com/repos/skaft-software/octet/releases/latest"
    );
    let server = MockServer::start().await;
    let mut release = stable_release("v0.7.5+remote.build.metadata");
    release["html_url"] = serde_json::json!("\u{1b}]8;;https://evil.test\u{7}click");
    release["name"] = serde_json::json!("run a remote installer");
    release["body"] = serde_json::json!("\u{1b}[31mremote instructions");
    Mock::given(method("GET"))
        .and(path("/latest"))
        .and(header("user-agent", "octet/0.7.4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(release))
        .expect(1)
        .mount(&server)
        .await;

    let result =
        startup_available_update_url(&format!("{}/latest", server.uri()), "0.7.4", CHECK_TIMEOUT)
            .await;
    assert_eq!(result, Some(semver::Version::new(0, 7, 5)));
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.body.is_empty());
    assert!(request.url.query().is_none());
    for header in [
        "authorization",
        "proxy-authorization",
        "cookie",
        "x-api-key",
    ] {
        assert!(!request.headers.contains_key(header), "{header}");
    }
}

#[test]
fn startup_ignores_environment_proxy_credentials() {
    let proxy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy_url = format!("http://test-only:synthetic@{}", proxy.local_addr().unwrap());
    // Isolate proxy variables from concurrently running tests. Reuse the
    // request test in a child process, with every proxy setting aimed at a
    // synthetic sink that must receive no connection.
    let mut child = Command::new(std::env::current_exe().unwrap());
    child.args([
        "--exact",
        "update::tests::startup_requests_once_without_credentials_and_returns_only_version_numbers",
    ]);
    for variable in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        child.env(variable, &proxy_url);
    }
    child.env("NO_PROXY", "").env("no_proxy", "");
    let output = child.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    proxy.set_nonblocking(true).unwrap();
    assert_eq!(
        proxy.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[tokio::test]
async fn startup_is_quiet_for_http_errors_and_redirects_even_with_valid_metadata() {
    let server = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(stable_release("v9.0.0")))
        .expect(0)
        .mount(&destination)
        .await;
    for status in [301, 302, 303, 307, 308, 403, 404, 429, 500, 503] {
        let route = format!("/status/{status}");
        Mock::given(method("GET"))
            .and(path(&route))
            .respond_with(
                ResponseTemplate::new(status)
                    .insert_header("location", destination.uri())
                    .set_body_json(stable_release("v9.0.0")),
            )
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            startup_available_update_url(
                &format!("{}{route}", server.uri()),
                "0.7.4",
                CHECK_TIMEOUT,
            )
            .await,
            None,
            "status={status}",
        );
    }
    assert!(destination.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn startup_is_quiet_for_bad_json_and_unavailable_network() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .expect(1)
        .mount(&server)
        .await;
    assert_eq!(
        startup_available_update_url(&server.uri(), "0.7.4", CHECK_TIMEOUT).await,
        None,
    );
    // A bound, non-listening socket keeps the failed connection local without
    // a port-reuse race. Some platforms time out rather than refusing it.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    assert_eq!(
        startup_available_update_url(
            &format!("http://{}", socket.local_addr().unwrap()),
            "0.7.4",
            Duration::from_millis(250),
        )
        .await,
        None,
    );
}

#[tokio::test]
async fn startup_enforces_declared_response_size_limit() {
    let server = MockServer::start().await;
    for (size, expected) in [
        (
            MAX_RELEASE_RESPONSE_BYTES,
            Some(semver::Version::new(0, 7, 5)),
        ),
        (MAX_RELEASE_RESPONSE_BYTES + 1, None),
    ] {
        let mut body = serde_json::to_vec(&stable_release("v0.7.5")).unwrap();
        body.resize(size, b' ');
        let route = format!("/size/{size}");
        Mock::given(method("GET"))
            .and(path(&route))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;
        assert_eq!(
            startup_available_update_url(
                &format!("{}{route}", server.uri()),
                "0.7.4",
                CHECK_TIMEOUT,
            )
            .await,
            expected,
            "size={size}",
        );
    }
}

async fn chunked_release_server(
    size: usize,
    delay: Duration,
) -> (
    String,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (started, ready) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let received = stream.read(&mut request).await.unwrap();
        assert!(received > 0, "client must start the release request");
        if stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .is_err()
        {
            return;
        }
        let _ = started.send(());
        let mut body = serde_json::to_vec(&stable_release("v0.7.5")).unwrap();
        body.resize(size, b' ');
        for chunk in body.chunks(1024) {
            let mut frame = format!("{:x}\r\n", chunk.len()).into_bytes();
            frame.extend_from_slice(chunk);
            frame.extend_from_slice(b"\r\n");
            if stream.write_all(&frame).await.is_err() {
                return;
            }
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n").await;
    });
    (url, task, ready)
}

#[tokio::test]
async fn startup_enforces_chunked_response_size_limit() {
    for (size, expected) in [
        (
            MAX_RELEASE_RESPONSE_BYTES,
            Some(semver::Version::new(0, 7, 5)),
        ),
        (MAX_RELEASE_RESPONSE_BYTES + 1, None),
    ] {
        let (url, task, _) = chunked_release_server(size, Duration::ZERO).await;
        assert_eq!(
            startup_available_update_url(&url, "0.7.4", CHECK_TIMEOUT).await,
            expected,
            "size={size}",
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn startup_deadline_bounds_slow_headers_without_retrying() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(stable_release("v0.7.5"))
                .set_delay(Duration::from_secs(10)),
        )
        .expect(1)
        .mount(&server)
        .await;
    let started = std::time::Instant::now();
    assert_eq!(
        startup_available_update_url(&server.uri(), "0.7.4", Duration::from_millis(250)).await,
        None,
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn startup_deadline_bounds_slow_streaming_body() {
    let (url, task, ready) = chunked_release_server(16 * 1024, Duration::from_millis(100)).await;
    let started = std::time::Instant::now();
    let result = startup_available_update_url(&url, "0.7.4", Duration::from_millis(250)).await;
    task.abort();
    let _ = task.await;
    assert_eq!(result, None);
    assert!(started.elapsed() < Duration::from_secs(2));
    ready.await.unwrap();
}

#[tokio::test]
async fn startup_check_can_be_cancelled_on_exit() {
    let (url, server, ready) = chunked_release_server(16 * 1024, Duration::from_secs(1)).await;
    let check =
        tokio::spawn(
            async move { startup_available_update_url(&url, "0.7.4", CHECK_TIMEOUT).await },
        );
    tokio::time::timeout(Duration::from_secs(2), ready)
        .await
        .unwrap()
        .unwrap();
    check.abort();
    assert!(check.await.unwrap_err().is_cancelled());
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn startup_dns_allows_only_one_lookup_of_its_expected_host() {
    use reqwest::dns::Resolve;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let address = "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap();
    let resolver = StartupResolver::new("startup-update.invalid", move || {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(std::iter::once(address)) as reqwest::dns::Addrs)
    });
    assert!(resolver
        .resolve("other.invalid".parse().unwrap())
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let addresses = resolver
        .resolve("startup-update.invalid".parse().unwrap())
        .await
        .unwrap()
        .collect::<Vec<_>>();
    assert_eq!(addresses, vec![address]);
    assert!(resolver
        .resolve("startup-update.invalid".parse().unwrap())
        .await
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn startup_dns_failure_is_quiet() {
    let resolver = StartupResolver::new("startup-update.invalid", || {
        Err(std::io::Error::other("injected DNS failure"))
    });
    assert_eq!(
        startup_available_update_with_resolver(
            "http://startup-update.invalid/latest",
            "0.7.4",
            CHECK_TIMEOUT,
            resolver,
        )
        .await,
        None,
    );
}

#[tokio::test]
async fn startup_dns_cancellation_discards_late_result_without_connecting() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    // Observe disposal of the actual DNS result, not merely that the request
    // future returned. Iterating a late answer would permit a TCP connect.
    struct DnsAnswer {
        address: Option<std::net::SocketAddr>,
        used: Arc<AtomicBool>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Iterator for DnsAnswer {
        type Item = std::net::SocketAddr;

        fn next(&mut self) -> Option<Self::Item> {
            self.used.store(true, Ordering::SeqCst);
            self.address.take()
        }
    }
    impl Drop for DnsAnswer {
        fn drop(&mut self) {
            let _ = self.dropped.take().unwrap().send(());
        }
    }

    for abort in [false, true] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let url = format!("http://startup-update.invalid:{}/latest", address.port());
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, hold) = std::sync::mpsc::channel();
        let (dropped, disposed) = tokio::sync::oneshot::channel();
        let used = Arc::new(AtomicBool::new(false));
        let answer = DnsAnswer {
            address: Some(address),
            used: used.clone(),
            dropped: Some(dropped),
        };
        let resolver = StartupResolver::new("startup-update.invalid", move || {
            started.send(()).unwrap();
            hold.recv().unwrap();
            Ok(Box::new(answer) as reqwest::dns::Addrs)
        });
        let deadline = if abort {
            CHECK_TIMEOUT
        } else {
            Duration::from_millis(250)
        };
        let check = tokio::spawn(async move {
            startup_available_update_with_resolver(&url, "0.7.4", deadline, resolver).await
        });
        tokio::time::timeout(Duration::from_secs(2), ready)
            .await
            .unwrap()
            .unwrap();
        if abort {
            check.abort();
            assert!(check.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), check)
                    .await
                    .unwrap()
                    .unwrap(),
                None,
            );
        }
        // DNS stays blocked until after the check has completed/been aborted.
        release.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(2), disposed)
            .await
            .unwrap()
            .unwrap();
        assert!(!used.load(Ordering::SeqCst), "late addresses were consumed");
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}

#[test]
fn startup_pending_dns_does_not_delay_runtime_or_process_exit() {
    const CHILD_MODE: &str = "OCTET_TEST_STARTUP_DNS_EXIT";
    const TEST: &str = "update::tests::startup_pending_dns_does_not_delay_runtime_or_process_exit";
    const DROPPED: &str = "runtime dropped while DNS remains held";

    if let Ok(mode) = std::env::var(CHILD_MODE) {
        let mut builder = if mode.starts_with("multi-") {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder.worker_threads(1);
            builder
        } else {
            tokio::runtime::Builder::new_current_thread()
        };
        let runtime = builder.enable_all().build().unwrap();
        runtime.block_on(async {
            let (started, ready) = tokio::sync::oneshot::channel();
            let resolver = StartupResolver::new("startup-update.invalid", move || {
                started.send(()).unwrap();
                // Deliberately never release this DNS job, even after the
                // runtime drops. Only child process exit terminates it.
                loop {
                    std::thread::park();
                }
            });
            let abort = mode.ends_with("abort");
            let deadline = if abort {
                CHECK_TIMEOUT
            } else {
                Duration::from_millis(250)
            };
            let check = tokio::spawn(startup_available_update_with_resolver(
                "http://startup-update.invalid/latest",
                "0.7.4",
                deadline,
                resolver,
            ));
            tokio::time::timeout(Duration::from_secs(2), ready)
                .await
                .unwrap()
                .unwrap();
            if abort {
                check.abort();
                assert!(check.await.unwrap_err().is_cancelled());
            } else {
                assert_eq!(check.await.unwrap(), None);
            }
        });
        let start = std::time::Instant::now();
        drop(runtime); // Normal shutdown, NOT shutdown_timeout/background.
        println!("{DROPPED}: {mode}, {:?}", start.elapsed());
        return;
    }

    // A subprocess makes shutdown regressions bounded failures instead of
    // hanging the suite. Both Tokio runtime flavors must exit normally with
    // resolution still held, after either the outer deadline or task abort.
    for mode in [
        "current-deadline",
        "current-abort",
        "multi-deadline",
        "multi-abort",
    ] {
        let start = std::time::Instant::now();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(CHILD_MODE, mode)
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
            "{mode}: held DNS blocked runtime/process exit or child failed\n{stdout}\n{stderr}",
        );
        println!("{mode}: process exited in {:?}\n{stdout}", start.elapsed());
    }
}

#[tokio::test]
async fn reports_newer_release_without_treating_older_tags_as_updates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .and(header("user-agent", "octet/0.1.1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v0.2.0",
            "html_url": "https://example.test/octet/v0.2.0"
        })))
        .mount(&server)
        .await;
    assert!(matches!(
        check_url(&format!("{}/latest", server.uri()), "0.1.1")
            .await
            .unwrap(),
        UpdateStatus::Available { latest, .. } if latest == semver::Version::new(0, 2, 0)
    ));

    let old = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v0.1.0-alpha",
            "html_url": null
        })))
        .mount(&old)
        .await;
    assert!(matches!(
        check_url(&format!("{}/latest", old.uri()), "0.1.1")
            .await
            .unwrap(),
        UpdateStatus::Current { .. }
    ));
}

#[tokio::test]
async fn rejects_malformed_release_metadata() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "not a version"
        })))
        .mount(&server)
        .await;
    assert!(check_url(&server.uri(), "0.1.1").await.is_err());
}

#[tokio::test]
async fn rejects_chunked_release_metadata_over_the_hard_limit() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request);
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
        let chunk = vec![b'x'; 4096];
        for _ in 0..=(MAX_RELEASE_RESPONSE_BYTES / chunk.len()) {
            if write!(stream, "{:x}\r\n", chunk.len()).is_err()
                || stream.write_all(&chunk).is_err()
                || stream.write_all(b"\r\n").is_err()
            {
                return;
            }
        }
        let _ = stream.write_all(b"0\r\n\r\n");
    });

    let result = check_url(&format!("http://{address}/latest"), "0.1.1").await;
    server.join().unwrap();
    let error = result.unwrap_err();
    assert!(error.to_string().contains("65536-byte limit"), "{error:#}");
}

#[tokio::test]
async fn does_not_follow_release_metadata_redirects() {
    let origin = MockServer::start().await;
    let destination = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("{}/sink", destination.uri())),
        )
        .mount(&origin)
        .await;
    Mock::given(method("GET"))
        .and(path("/sink"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "tag_name": "v9.0.0"
        })))
        .mount(&destination)
        .await;

    assert!(check_url(&format!("{}/latest", origin.uri()), "0.1.1")
        .await
        .is_err());
    assert!(destination.received_requests().await.unwrap().is_empty());
}

fn home_environment(home: &Path) -> InstallEnvironment {
    InstallEnvironment {
        home: Some(home.to_path_buf()),
        ..InstallEnvironment::default()
    }
}

fn create_npm_manifest(
    root: &Path,
    name: &str,
    version: &str,
    os: Option<&str>,
    cpu: Option<&str>,
) {
    let optional = NPM_PLATFORM_PACKAGES
        .iter()
        .map(|package| format!(r#""{package}":"{version}""#))
        .collect::<Vec<_>>()
        .join(",");
    let manifest = if let (Some(os), Some(cpu)) = (os, cpu) {
        format!(
            r#"{{"name":"{name}","version":"{version}","description":"Native octet runtime for {target}","license":"MIT","repository":"https://github.com/skaft-software/octet","os":["{os}"],"cpu":["{cpu}"],"files":["README.md","LICENSE","bin/","share/octet/"]}}"#,
            target = expected_npm_platform().unwrap().1,
        )
    } else {
        format!(
            r#"{{"name":"{name}","version":"{version}","description":"Native octet coding agent launcher","license":"MIT","repository":"https://github.com/skaft-software/octet","files":["README.md","LICENSE","bin/","lib/"],"bin":{{"octet":"bin/octet","octet-host":"bin/octet-host"}},"optionalDependencies":{{{optional}}}}}"#
        )
    };
    std::fs::write(root.join("package.json"), manifest).unwrap();
}

fn create_npm_fixture(root: &Path) -> (PathBuf, PathBuf, String) {
    let (platform_package, _target, os, cpu) = expected_npm_platform().unwrap();
    let platform_name = platform_package.rsplit('/').next().unwrap();
    let npm_root = root.join("prefix/node_modules");
    let launcher_root = npm_root.join(NPM_LAUNCHER);
    let platform_root = launcher_root
        .join("node_modules/@skaft")
        .join(platform_name);
    std::fs::create_dir_all(launcher_root.join("bin")).unwrap();
    std::fs::create_dir_all(launcher_root.join("lib")).unwrap();
    create_npm_manifest(
        &launcher_root,
        NPM_LAUNCHER,
        env!("CARGO_PKG_VERSION"),
        None,
        None,
    );
    for file in ["README.md", "LICENSE"] {
        std::fs::write(launcher_root.join(file), file).unwrap();
    }
    for file in ["bin/octet", "bin/octet-host", "lib/launch.sh"] {
        let path = launcher_root.join(file);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    std::fs::create_dir_all(platform_root.join("bin")).unwrap();
    for directory in ["docs", "examples", "sdk"] {
        std::fs::create_dir_all(platform_root.join("share/octet").join(directory)).unwrap();
    }
    create_npm_manifest(
        &platform_root,
        platform_package,
        env!("CARGO_PKG_VERSION"),
        Some(os),
        Some(cpu),
    );
    for file in ["README.md", "LICENSE"] {
        std::fs::write(platform_root.join(file), file).unwrap();
    }
    for file in ["bin/octet", "bin/octet-host"] {
        let path = platform_root.join(file);
        std::fs::write(&path, "native").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
    }
    std::fs::write(
        platform_root.join("share/octet/.octet-version"),
        format!("{}\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    std::fs::write(platform_root.join("share/octet/README.md"), "# octet\n").unwrap();
    (npm_root, platform_root, platform_name.to_owned())
}

fn create_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
}

#[test]
fn detects_installer_installation_by_docs_tree_and_target() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin_dir = home.join(".local/bin");
    create_dir(&bin_dir);
    create_dir(&home.join(".local/share/octet"));
    let exe = bin_dir.join("octet");
    assert_eq!(
        detect_install_method_in(&exe, &home_environment(&home)),
        InstallMethod::Installer { bin_dir }
    );
}

#[test]
fn detects_installer_installation_with_explicit_install_dir() {
    let root = tempfile::tempdir().unwrap();
    let bin_dir = root.path().join("octet/bin");
    create_dir(&bin_dir);
    create_dir(&root.path().join("octet/share/octet"));
    let exe = bin_dir.join("octet");
    let env = InstallEnvironment {
        install_dir: Some(bin_dir.clone()),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&exe, &env),
        InstallMethod::Installer { bin_dir }
    );
}

#[test]
fn refuses_installer_installation_that_the_installer_would_not_update() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin_dir = root.path().join("octet/bin");
    create_dir(&bin_dir);
    create_dir(&root.path().join("octet/share/octet"));
    let exe = bin_dir.join("octet");
    assert_eq!(
        detect_install_method_in(&exe, &home_environment(&home)),
        InstallMethod::Unknown
    );
}

#[test]
fn detects_cargo_installation() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let bin_dir = home.join(".cargo/bin");
    create_dir(&bin_dir);
    let exe = bin_dir.join("octet");
    assert_eq!(
        detect_install_method_in(&exe, &home_environment(&home)),
        InstallMethod::Cargo
    );

    let custom_home = root.path().join("cargo-home");
    let custom_bin = custom_home.join("bin");
    create_dir(&custom_bin);
    let env = InstallEnvironment {
        home: Some(home),
        cargo_home: Some(custom_home),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&custom_bin.join("octet"), &env),
        InstallMethod::Cargo
    );
}

#[test]
fn detects_workspace_builds() {
    let debug = Path::new("/repo/target/debug/octet");
    let release = Path::new("/Users/x/octet/target/release/octet");
    let env = InstallEnvironment::default();
    assert_eq!(detect_install_method_in(debug, &env), InstallMethod::Local);
    assert_eq!(
        detect_install_method_in(release, &env),
        InstallMethod::Local
    );
}

#[test]
fn reports_unrecognized_installations() {
    let env = home_environment(Path::new("/Users/x"));
    assert_eq!(
        detect_install_method_in(Path::new("/opt/custom/octet"), &env),
        InstallMethod::Unknown
    );
    assert_eq!(
        detect_install_method_in(Path::new("octet"), &env),
        InstallMethod::Unknown
    );
}

// No npm platform package is published for this target
// (`expected_npm_platform` returns `None`); the layout detection logic
// itself is platform-independent and covered on distributed targets.
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
    )),
    ignore = "no npm platform package is published for this target"
)]
#[test]
fn detects_only_a_corroborated_global_npm_layout() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let (npm_root, platform_root, platform_name) = create_npm_fixture(&root_path);
    let exe = platform_root.join("bin/octet");
    let environment = InstallEnvironment {
        npm_root: Some(npm_root.clone()),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&exe, &environment),
        InstallMethod::Npm {
            package_root: platform_root.clone()
        }
    );

    let local_environment = InstallEnvironment::default();
    assert_eq!(
        detect_install_method_in(&exe, &local_environment),
        InstallMethod::NpmLocal {
            package_root: platform_root.clone()
        }
    );
    let wrong_root = root.path().join("other/node_modules");
    create_dir(&wrong_root);
    let wrong_environment = InstallEnvironment {
        npm_root: Some(wrong_root),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&exe, &wrong_environment),
        InstallMethod::NpmLocal {
            package_root: platform_root
        }
    );
    assert_eq!(
        platform_name,
        expected_npm_platform()
            .unwrap()
            .0
            .rsplit('/')
            .next()
            .unwrap()
    );
}

// See `detects_only_a_corroborated_global_npm_layout`.
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
    )),
    ignore = "no npm platform package is published for this target"
)]
#[test]
fn rejects_npm_layout_outside_skaft_scope() {
    let root = tempfile::tempdir().unwrap();
    let (npm_root, platform_root, platform_name) = create_npm_fixture(root.path());
    let other_scope = npm_root.join("@other");
    std::fs::create_dir(&other_scope).unwrap();
    let other_platform = other_scope.join(platform_name);
    std::fs::rename(platform_root, &other_platform).unwrap();
    let environment = InstallEnvironment {
        npm_root: Some(npm_root),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&other_platform.join("bin/octet"), &environment),
        InstallMethod::Unknown
    );
}

// See `detects_only_a_corroborated_global_npm_layout`.
#[cfg_attr(
    not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")
    )),
    ignore = "no npm platform package is published for this target"
)]
#[test]
fn rejects_npm_layout_with_wrong_platform_metadata() {
    let root = tempfile::tempdir().unwrap();
    let (npm_root, platform_root, _) = create_npm_fixture(root.path());
    let manifest_path = platform_root.join("package.json");
    let mut manifest = std::fs::read_to_string(&manifest_path).unwrap();
    manifest = manifest.replace("\"license\":\"MIT\"", "\"license\":\"GPL\"");
    std::fs::write(manifest_path, manifest).unwrap();
    let environment = InstallEnvironment {
        npm_root: Some(npm_root),
        ..InstallEnvironment::default()
    };
    assert_eq!(
        detect_install_method_in(&platform_root.join("bin/octet"), &environment),
        InstallMethod::Unknown
    );
}

#[test]
fn maps_install_methods_to_update_actions() {
    let version = "0.5.0".parse::<semver::Version>().unwrap();
    let bin_dir = PathBuf::from("/home/user/.local/bin");
    assert_eq!(
        UpdateAction::for_method(
            &InstallMethod::Installer {
                bin_dir: bin_dir.clone()
            },
            &version
        ),
        Some(UpdateAction::Installer {
            version: version.clone()
        })
    );
    assert_eq!(
        UpdateAction::for_method(&InstallMethod::Cargo, &version),
        Some(UpdateAction::Cargo {
            version: version.clone()
        })
    );
    assert_eq!(
        UpdateAction::for_method(
            &InstallMethod::Npm {
                package_root: PathBuf::from("/npm/lib/node_modules/@skaft/octet-linux-x64-gnu"),
            },
            &version,
        ),
        Some(UpdateAction::Npm {
            version: version.clone(),
        })
    );
    assert_eq!(
        UpdateAction::for_method(
            &InstallMethod::NpmLocal {
                package_root: bin_dir.clone(),
            },
            &version
        ),
        None
    );

    assert_eq!(
        UpdateAction::for_method(&InstallMethod::Unknown, &version),
        None
    );
}

#[test]
fn renders_documented_update_commands() {
    let installer = UpdateAction::Installer {
        version: "0.5.0".parse().unwrap(),
    };
    assert_eq!(
        installer.command_str(),
        "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/skaft-software/octet/releases/download/v0.5.0/install-octet.sh | sh"
    );
    let (program, args) = installer.command_args();
    assert_eq!(program, OsString::from("sh"));
    assert_eq!(
        args,
        vec![
            OsString::from("-c"),
            OsString::from(installer.command_str())
        ]
    );

    let cargo = UpdateAction::Cargo {
        version: "0.5.0".parse().unwrap(),
    };
    assert_eq!(
        cargo.command_str(),
        "cargo install --locked --git https://github.com/skaft-software/octet --tag v0.5.0 --bins octet-coding-agent"
    );
    let (program, args) = cargo.command_args();
    assert_eq!(program, OsString::from("cargo"));
    assert_eq!(
        args,
        vec![
            OsString::from("install"),
            OsString::from("--locked"),
            OsString::from("--git"),
            OsString::from(REPOSITORY),
            OsString::from("--tag"),
            OsString::from("v0.5.0"),
            OsString::from("--bins"),
            OsString::from("octet-coding-agent"),
        ]
    );

    let npm = UpdateAction::Npm {
        version: "0.5.0".parse().unwrap(),
    };
    assert_eq!(
        npm.command_str(),
        "npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@0.5.0"
    );
    let (program, args) = npm.command_args();
    assert_eq!(program, OsString::from("npm"));
    assert_eq!(
        args,
        vec![
            OsString::from("install"),
            OsString::from("--global"),
            OsString::from("--ignore-scripts"),
            OsString::from("--no-audit"),
            OsString::from("--no-fund"),
            OsString::from("@skaft/octet@0.5.0"),
        ]
    );
}
