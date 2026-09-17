//! Scoped conditional catalog caching, including actual loopback HTTP framing.
use super::*;
use serde_json::json;
use std::io::Write;

fn refresh<F>(
    path: &std::path::Path,
    credential: &str,
    fetch: F,
) -> anyhow::Result<serde_json::Value>
where
    F: FnOnce(String, http::HeaderMap) -> anyhow::Result<ProviderInventoryResponse>,
{
    refresh_provider_inventory_with(
        path,
        "fixture",
        "https://inventory.invalid/models".into(),
        Default::default(),
        credential,
        fetch,
    )
}

fn seed(path: &std::path::Path) {
    refresh(path, "credential-fingerprint", |_, headers| {
        assert!(!headers.contains_key(http::header::IF_NONE_MATCH));
        Ok(ProviderInventoryResponse::Modified {
            body: json!({"data":[{"id":"last-good"}]}),
            etag: Some("W/\"opaque-v1\"".into()),
        })
    })
    .unwrap();
}

#[test]
fn conditional_inventory_http_200_then_304_keeps_body_and_advances_check() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/models", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        for index in 0..2 {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                socket.read_exact(&mut byte).unwrap();
                request.push(byte[0]);
                assert!(request.len() < 32 * 1024);
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("get /models http/1.1"));
            if index == 0 {
                assert!(!request.contains("if-none-match:"));
                let body = r#"{"data":[{"id":"model-v1"}]}"#;
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: W/\"opaque-v1\"\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
            } else {
                assert!(request.contains("if-none-match: w/\"opaque-v1\"\r\n"));
                write!(
                    socket,
                    "HTTP/1.1 304 Not Modified\r\nETag: \"opaque-v2\"\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    let first = refresh_provider_inventory_with(
        &path,
        "fixture",
        url.clone(),
        Default::default(),
        "fingerprint",
        fetch_provider_inventory,
    )
    .unwrap();
    let mut record = load_provider_inventory_record(&path, "fixture", &url, "fingerprint")
        .unwrap()
        .unwrap();
    assert_eq!(record.etag.as_deref(), Some("W/\"opaque-v1\""));
    record.checked_at = Some(1);
    crate::auth::write_private_atomic(&path, &serde_json::to_vec(&record).unwrap(), ".test-")
        .unwrap();
    let second = refresh_provider_inventory_with(
        &path,
        "fixture",
        url.clone(),
        Default::default(),
        "fingerprint",
        fetch_provider_inventory,
    )
    .unwrap();
    assert_eq!(first, second);
    let record = load_provider_inventory_record(&path, "fixture", &url, "fingerprint")
        .unwrap()
        .unwrap();
    assert_eq!(record.etag.as_deref(), Some("\"opaque-v2\""));
    assert!(record.checked_at.unwrap() > 1);
    server.join().unwrap();
}

#[test]
fn conditional_inventory_does_not_reuse_validators_across_scopes() {
    for (provider, url, fingerprint) in [
        (
            "different-provider",
            "https://inventory.invalid/models",
            "credential-fingerprint",
        ),
        (
            "fixture",
            "https://different.invalid/models",
            "credential-fingerprint",
        ),
        (
            "fixture",
            "https://inventory.invalid/models",
            "different-credential",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        seed(&path);
        let original = std::fs::read(&path).unwrap();
        let result = refresh_provider_inventory_with(
            &path,
            provider,
            url.into(),
            Default::default(),
            fingerprint,
            |_, headers| {
                assert!(!headers.contains_key(http::header::IF_NONE_MATCH));
                Ok(ProviderInventoryResponse::NotModified { etag: None })
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[test]
fn conditional_inventory_errors_preserve_last_good_body_validator_and_check() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    seed(&path);
    let original = std::fs::read(&path).unwrap();
    let result = refresh(&path, "credential-fingerprint", |_, headers| {
        assert_eq!(headers[http::header::IF_NONE_MATCH], "W/\"opaque-v1\"");
        anyhow::bail!("fixture transport failure")
    })
    .unwrap();
    assert_eq!(result["data"][0]["id"], "last-good");
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn conditional_inventory_200_without_etag_clears_previous_validator() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    seed(&path);
    refresh(&path, "credential-fingerprint", |_, headers| {
        assert!(headers.contains_key(http::header::IF_NONE_MATCH));
        Ok(ProviderInventoryResponse::Modified {
            body: json!({"data":[]}),
            etag: None,
        })
    })
    .unwrap();
    let record = load_provider_inventory_record(
        &path,
        "fixture",
        "https://inventory.invalid/models",
        "credential-fingerprint",
    )
    .unwrap()
    .unwrap();
    assert!(record.etag.is_none());
    assert_eq!(record.body.unwrap(), json!({"data":[]}));
}

#[test]
fn conditional_inventory_rejects_unscoped_304_and_bounds_untrusted_validators() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("cache.json");
    assert!(refresh(&path, "credential-fingerprint", |_, _| {
        Ok(ProviderInventoryResponse::NotModified { etag: None })
    })
    .is_err());
    assert!(!path.exists());
    for invalid in [
        "".to_owned(),
        "\"tag\"\r\nAuthorization: stolen".into(),
        "x".repeat(4097),
    ] {
        assert!(inventory_etag(Some(&invalid)).is_none());
    }
    seed(&path);
    let first = std::fs::read(&path).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&first).unwrap();
    legacy.as_object_mut().unwrap().remove("etag");
    legacy.as_object_mut().unwrap().remove("checked_at");
    crate::auth::write_private_atomic(&path, &serde_json::to_vec(&legacy).unwrap(), ".test-")
        .unwrap();
    refresh(&path, "credential-fingerprint", |_, headers| {
        assert!(!headers.contains_key(http::header::IF_NONE_MATCH));
        Ok(ProviderInventoryResponse::Modified {
            body: json!({"data":[]}),
            etag: Some("\"new\"".into()),
        })
    })
    .unwrap();
}
