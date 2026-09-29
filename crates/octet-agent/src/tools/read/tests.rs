//! Unit tests for the `read` tool's offsets, limits and encodings.
//!
//! Separate from `read.rs` so the reader's truncation and line-windowing
//! logic can be read on its own.
use super::*;
use crate::sandbox::SandboxConfig;
use crate::ToolProgressSink;
use serde_json::json;
use std::path::PathBuf;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PNG_BYTES: &[u8] = b"\x89PNG\r\n\x1a\npayload";
const WAV_BYTES: &[u8] = b"RIFF\x04\x00\x00\x00WAVEpayload";
const AAC_BYTES: &[u8] = b"\xff\xf1\x50\x80payload";

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    sandbox: SandboxConfig,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let sandbox = SandboxConfig::new(&workspace);
    Fixture {
        _dir: dir,
        workspace,
        sandbox,
    }
}

fn remote_fixture() -> Fixture {
    let mut fixture = fixture();
    fixture.sandbox.allow_remote_read = true;
    fixture
}

impl Fixture {
    fn ctx(&self) -> ToolContext<'_> {
        ToolContext {
            workspace: &self.workspace,
            sandbox: &self.sandbox,
            execution_scope: "read-test",
            resource_owner: "read-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        }
    }
}

#[test]
fn effect_classification_is_conservative_and_does_not_resolve_local_paths() {
    let mut fixture = fixture();
    assert_eq!(
        ReadTool
            .effect(&json!({"path": "missing.txt"}), &fixture.ctx())
            .unwrap(),
        ToolEffect::WorkspaceRead
    );
    assert_eq!(ReadTool.replay_safety(), ReplaySafety::Safe);
    assert_eq!(ReadTool.concurrency(), ToolConcurrency::Parallel);

    fixture.sandbox.allow_external_paths = true;
    for path in ["missing.txt", "/definitely/not/a/real/octet-effect-path"] {
        assert_eq!(
            ReadTool
                .effect(&json!({"path": path}), &fixture.ctx())
                .unwrap(),
            ToolEffect::HostRead
        );
    }
    fixture.sandbox.allow_remote_read = true;
    assert_eq!(
        ReadTool
            .effect(
                &json!({"path": "https://example.com/media.png"}),
                &fixture.ctx(),
            )
            .unwrap(),
        ToolEffect::Network
    );
}

#[tokio::test]
async fn reads_with_line_numbers_and_hash() {
    let f = fixture();
    std::fs::write(f.workspace.join("a.txt"), "alpha\nbeta\ngamma\n").unwrap();

    let out = ReadTool
        .execute(json!({"path": "a.txt"}), &f.ctx())
        .await
        .unwrap();
    let expected_hash = content_hash(b"alpha\nbeta\ngamma\n");
    assert_eq!(
        out.text,
        format!("a.txt:1-3/3 hash={expected_hash}\n1: alpha\n2: beta\n3: gamma\ntruncated=false")
    );
}

#[tokio::test]
async fn local_image_and_audio_reads_return_structured_media() {
    let f = fixture();
    std::fs::write(f.workspace.join("capture.bin"), PNG_BYTES).unwrap();
    std::fs::write(f.workspace.join("memo.wav"), WAV_BYTES).unwrap();

    let image = ReadTool
        .execute(json!({"path": "capture.bin"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(image.media_kinds(), &[crate::ToolOutputMediaKind::Image]);
    assert_eq!(image.media().len(), 1);
    assert!(image.text.contains("read=vision"), "{}", image.text);
    assert!(!image.text.contains("payload"), "{}", image.text);

    let audio = ReadTool
        .execute(json!({"path": "memo.wav"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(audio.media_kinds(), &[crate::ToolOutputMediaKind::Audio]);
    assert_eq!(audio.media().len(), 1);
    assert!(audio.text.contains("read=audio"), "{}", audio.text);
    assert!(!audio.text.contains("payload"), "{}", audio.text);
}

#[tokio::test]
async fn recognized_audio_format_is_preserved_for_protocol_lowering() {
    let f = fixture();
    std::fs::write(f.workspace.join("clip.aac"), AAC_BYTES).unwrap();

    let audio = ReadTool
        .execute(json!({"path": "clip.aac"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(audio.media_kinds(), &[crate::ToolOutputMediaKind::Audio]);
    assert!(audio.text.contains("media=audio/aac"), "{}", audio.text);
    let Media::Audio(audio) = &audio.media()[0] else {
        panic!("expected audio media");
    };
    assert_eq!(audio.format, AudioFormat::Aac);
}

#[tokio::test]
async fn local_media_extension_must_match_magic_bytes() {
    let f = fixture();
    std::fs::write(f.workspace.join("not-really.png"), b"<html>nope</html>").unwrap();
    let error = ReadTool
        .execute(json!({"path": "not-really.png"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(error.message.contains("does not match"), "{error}");
}

#[tokio::test]
async fn local_media_size_is_rejected_before_buffering() {
    let f = fixture();
    let path = f.workspace.join("oversized.png");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES as u64 + 1)
        .unwrap();
    let error = ReadTool
        .execute(json!({"path": "oversized.png"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(error.message.contains("too large"), "{error}");
}

#[tokio::test]
async fn extensionless_media_uses_its_content_cap_before_buffering() {
    let f = fixture();
    let path = f.workspace.join("oversized.bin");
    std::fs::write(&path, PNG_BYTES).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES as u64 + 1)
        .unwrap();

    let error = ReadTool
        .execute(json!({"path": "oversized.bin"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(
        error.message.contains(&format!("limit {MAX_IMAGE_BYTES}")),
        "{error}"
    );
}

#[tokio::test]
async fn workspace_file_url_uses_the_same_local_media_pipeline() {
    let f = fixture();
    let path = f.workspace.join("screen shot.png");
    std::fs::write(&path, PNG_BYTES).unwrap();
    let url = reqwest::Url::from_file_path(&path).unwrap();

    let output = ReadTool
        .execute(json!({"path": url.as_str()}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(output.media_kinds(), &[crate::ToolOutputMediaKind::Image]);
}

#[tokio::test]
async fn workspace_mode_rejects_external_file_urls() {
    let f = fixture();
    let outside = tempfile::NamedTempFile::new().unwrap();
    let url = reqwest::Url::from_file_path(outside.path()).unwrap();
    let error = ReadTool
        .execute(json!({"path": url.as_str()}), &f.ctx())
        .await
        .unwrap_err();
    assert!(
        error.message.contains("absolute paths are not allowed"),
        "{error}"
    );
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::WorkspaceConfinement)
    );
}

#[tokio::test]
async fn remote_reads_are_default_off_and_rejected_before_network_access() {
    let server = MockServer::start().await;
    let f = fixture();

    let error = ReadTool
        .execute(
            json!({"path": format!("{}/capture?local-secret", server.uri())}),
            &f.ctx(),
        )
        .await
        .unwrap_err();

    assert!(
        error.message.contains("remote URL reads are disabled"),
        "{error}"
    );
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::RemoteReadDisabled)
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn remote_read_cancellation_interrupts_header_waits() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(PNG_BYTES)
                .set_delay(Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let f = remote_fixture();
    let cancellation = crate::CancellationToken::default();
    let cancel = cancellation.clone();
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &f.sandbox,
        execution_scope: "read-test",
        resource_owner: "read-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation,
    };
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(25)).await;
        cancel.cancel();
    });

    let started = std::time::Instant::now();
    let error = ReadTool
        .execute(json!({"path": format!("{}/slow.png", server.uri())}), &ctx)
        .await
        .unwrap_err();
    assert!(error.message.contains("cancelled"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn remote_read_retries_transient_http_failures_with_a_bounded_policy() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/transient.png"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    let mut f = remote_fixture();
    f.sandbox.remote_read_retry = crate::sandbox::RemoteReadRetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
    };

    let error = ReadTool
        .execute(
            json!({"path": format!("{}/transient.png", server.uri())}),
            &f.ctx(),
        )
        .await
        .unwrap_err();

    assert!(error.message.contains("HTTP 503"), "{error}");
    server.verify().await;
}

#[tokio::test]
async fn remote_link_requires_matching_mime_and_magic_and_redacts_query() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/capture"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(PNG_BYTES),
        )
        .mount(&server)
        .await;
    let f = remote_fixture();
    let url = format!("{}/capture?token=secret", server.uri());

    let output = ReadTool
        .execute(json!({"path": url}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(output.media_kinds(), &[crate::ToolOutputMediaKind::Image]);
    assert!(output.text.contains("?…"), "{}", output.text);
    assert!(!output.text.contains("secret"), "{}", output.text);
}

#[tokio::test]
async fn remote_link_rejects_mime_magic_mismatch() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/fake.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(WAV_BYTES),
        )
        .mount(&server)
        .await;
    let f = remote_fixture();
    let error = ReadTool
        .execute(
            json!({"path": format!("{}/fake.png", server.uri())}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("does not match"), "{error}");
}

#[tokio::test]
async fn remote_link_requires_a_supported_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/capture"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG_BYTES))
        .mount(&server)
        .await;
    let f = remote_fixture();
    let error = ReadTool
        .execute(
            json!({"path": format!("{}/capture", server.uri())}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("missing Content-Type"), "{error}");
}

#[tokio::test]
async fn remote_link_rejects_private_https_targets_before_connecting() {
    let f = remote_fixture();
    let error = ReadTool
        .execute(
            json!({"path": "https://169.254.169.254/latest/meta-data"}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("non-public"), "{error}");
}

#[test]
fn transition_ipv6_cannot_embed_a_nonpublic_ipv4_target() {
    for address in [
        "64:ff9b::a9fe:a9fe",
        "64:ff9b:1::808:808",
        "2002:a9fe:a9fe::",
        "2001:0000::1",
    ] {
        let address = address.parse::<std::net::IpAddr>().unwrap();
        assert!(
            !is_public_remote_ip(address),
            "accepted transition address {address}"
        );
    }
    assert!(is_public_remote_ip(
        "64:ff9b::808:808".parse::<std::net::IpAddr>().unwrap()
    ));
}

#[test]
fn reserved_address_space_is_not_treated_as_public() {
    for address in [
        "192.88.99.2",
        "100::1",
        "2001:2::1",
        "3fff::1",
        "5f00::1",
        "4000::1",
    ] {
        let address = address.parse::<std::net::IpAddr>().unwrap();
        assert!(!is_public_remote_ip(address), "accepted {address}");
    }
    assert!(is_public_remote_ip(
        "2606:4700:4700::1111".parse::<std::net::IpAddr>().unwrap()
    ));
}

#[tokio::test]
async fn public_ipv6_literal_is_normalized_without_dns_or_brackets() {
    let url = reqwest::Url::parse("https://[2606:4700:4700::1111]/image.png").unwrap();
    let (host, literal, addresses) = validated_remote_endpoint(&url, "public IPv6 literal")
        .await
        .unwrap();
    let expected = "2606:4700:4700::1111".parse::<std::net::IpAddr>().unwrap();
    assert_eq!(host, "2606:4700:4700::1111");
    assert_eq!(literal, Some(expected));
    assert_eq!(addresses, vec![std::net::SocketAddr::new(expected, 443)]);
}

#[tokio::test]
async fn trailing_dot_loopback_url_uses_the_normalized_pinned_host() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/capture.png"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_bytes(PNG_BYTES),
        )
        .expect(1)
        .mount(&server)
        .await;
    let port = reqwest::Url::parse(&server.uri()).unwrap().port().unwrap();
    let f = remote_fixture();
    let output = ReadTool
        .execute(
            json!({"path": format!("http://127.0.0.1.:{port}/capture.png")}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(output.text.contains("read=vision"), "{}", output.text);
    assert!(output.text.contains("http://127.0.0.1:"), "{}", output.text);
    assert!(!output.text.contains("127.0.0.1.:"), "{}", output.text);
}

#[tokio::test]
async fn offset_and_limit_report_continuation() {
    let f = fixture();
    let content: String = (1..=10).map(|i| format!("line{i}\n")).collect();
    std::fs::write(f.workspace.join("b.txt"), &content).unwrap();

    let out = ReadTool
        .execute(json!({"path": "b.txt", "offset": 3, "limit": 4}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.starts_with("b.txt:3-6/10 hash="), "{}", out.text);
    assert!(out.text.contains("3: line3\n"));
    assert!(out.text.contains("6: line6\n"));
    assert!(!out.text.contains("7: line7"));
    assert!(out.text.ends_with("next_offset=7 truncated=false"));
}

#[tokio::test]
async fn extreme_limit_is_safely_clamped_to_the_file() {
    let f = fixture();
    std::fs::write(f.workspace.join("bounded.txt"), "one\ntwo\n").unwrap();

    let out = ReadTool
        .execute(
            json!({"path": "bounded.txt", "offset": 2, "limit": usize::MAX}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(out.text.contains("bounded.txt:2-2/2"), "{}", out.text);
    assert!(out.text.contains("2: two"), "{}", out.text);
}

#[tokio::test]
async fn byte_budget_truncates_with_marker() {
    let f = fixture();
    let content: String = (1..=2000).map(|i| format!("line number {i}\n")).collect();
    std::fs::write(f.workspace.join("big.txt"), &content).unwrap();
    let mut sandbox = f.sandbox.clone();
    sandbox.max_output_bytes = 2048;
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "read-test",
        resource_owner: "read-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
    };

    let out = ReadTool
        .execute(json!({"path": "big.txt"}), &ctx)
        .await
        .unwrap();
    assert!(out.text.len() < 4096);
    assert!(out.text.contains("truncated=true"), "{}", out.text);
    assert!(out.text.contains("next_offset="), "{}", out.text);
}

#[tokio::test]
async fn directory_missing_and_escaping_paths_fail() {
    let f = fixture();
    std::fs::create_dir(f.workspace.join("sub")).unwrap();

    let err = ReadTool
        .execute(json!({"path": "sub"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("directory"), "{err}");

    let err = ReadTool
        .execute(json!({"path": "missing.txt"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("missing.txt"), "{err}");

    let err = ReadTool
        .execute(json!({"path": "../outside.txt"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains(".."), "{err}");
}

#[tokio::test]
async fn trusted_local_mode_reads_an_absolute_path() {
    let f = fixture();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(outside.path(), "outside\n").unwrap();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_external_paths = true;
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "read-test",
        resource_owner: "read-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
    };

    let out = ReadTool
        .execute(json!({"path": outside.path().to_string_lossy()}), &ctx)
        .await
        .unwrap();
    assert!(out.text.contains("1: outside"), "{}", out.text);
}

#[tokio::test]
async fn offset_beyond_end_is_an_error() {
    let f = fixture();
    std::fs::write(f.workspace.join("s.txt"), "only\n").unwrap();
    let err = ReadTool
        .execute(json!({"path": "s.txt", "offset": 5}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("beyond the end"), "{err}");
}

#[tokio::test]
async fn empty_file_reads_cleanly() {
    let f = fixture();
    std::fs::write(f.workspace.join("e.txt"), "").unwrap();
    let out = ReadTool
        .execute(json!({"path": "e.txt"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("e.txt:0-0/0 hash="), "{}", out.text);
    assert!(out.text.contains("(empty file)"));
}

#[tokio::test]
async fn invalid_args_are_a_tool_error() {
    let f = fixture();
    let err = ReadTool
        .execute(json!({"offset": 1}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("invalid arguments"), "{err}");
}
