#![allow(missing_docs)]

//! Provider authentication flows for subscription-backed models.
//!
//! OpenAI Codex ("Sign in with ChatGPT"), GitHub Copilot device OAuth, the
//! shared [`subscription`] login framework, and custom OpenAI-compatible
//! endpoint credentials. Authentication stays in the product crate behind public
//! credential/provider seams; `octet-ai` is not touched.

pub mod codex;
pub mod copilot;
pub mod custom;
pub(crate) mod kimi;
pub(crate) mod meta;
pub(crate) mod openrouter;
pub(crate) mod subscription;
pub(crate) mod xai;

pub(crate) fn read_bounded_regular(
    path: &std::path::Path,
    limit: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    read_optional(path, limit, false)
}

pub(crate) fn read_bounded_private(
    path: &std::path::Path,
    limit: usize,
) -> anyhow::Result<Option<Vec<u8>>> {
    read_optional(path, limit, true)
}

fn read_optional(
    path: &std::path::Path,
    limit: usize,
    private: bool,
) -> anyhow::Result<Option<Vec<u8>>> {
    let result = if private {
        octet_agent::secure_fs::read_private_file_bounded(path, limit)
    } else {
        octet_agent::secure_fs::read_regular_file_bounded(path, limit)
    };
    match result {
        Ok(bytes) => Ok(Some(bytes)),
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(error) => Err(anyhow::anyhow!("refusing {}: {error}", path.display())),
    }
}

/// Bounded attempts for last-writer-wins model metadata caches.
const MODEL_CACHE_WRITE_ATTEMPTS: u32 = 4;

/// Persist a last-writer-wins model metadata cache with owner-only guarantees.
///
/// Concurrent octet processes (a foreground launch, a background refresh or a
/// delegated child) may publish the same cache at once. The secure writer
/// rejects a target that changed underneath it; for a cache that only means a
/// sibling published fresh metadata. Retry briefly, and if siblings keep
/// winning, their publication supersedes this one. Every other refusal (an
/// insecure object, a non-regular target, I/O) is still returned. Credentials
/// must keep using the strict writer.
pub(crate) fn write_private_model_cache(
    path: &std::path::Path,
    bytes: &[u8],
    limit: usize,
) -> Result<(), octet_agent::secure_fs::SecureFileError> {
    let mut attempt = 1;
    loop {
        match octet_agent::secure_fs::write_private_atomic(path, bytes, limit) {
            Err(octet_agent::secure_fs::SecureFileError::Changed)
                if attempt < MODEL_CACHE_WRITE_ATTEMPTS =>
            {
                std::thread::sleep(std::time::Duration::from_millis(u64::from(attempt)));
                attempt += 1;
            }
            Err(octet_agent::secure_fs::SecureFileError::Changed) => return Ok(()),
            result => return result,
        }
    }
}

/// Atomically persist non-secret authentication-adjacent metadata (for example
/// provider model inventories) under an owner-only directory and file.
pub(crate) fn write_private_atomic(
    path: &std::path::Path,
    bytes: &[u8],
    _temporary_prefix: &str,
) -> anyhow::Result<()> {
    const MAX_PRIVATE_ATOMIC_BYTES: usize = 256 * 1024 * 1024;
    octet_agent::secure_fs::write_private_atomic(path, bytes, MAX_PRIVATE_ATOMIC_BYTES)
        .map_err(anyhow::Error::from)
}

#[cfg(all(test, unix))]
mod model_cache_write_tests {
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn concurrent_model_cache_writers_publish_without_spurious_change_errors() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("models");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = std::sync::Arc::new(dir.join("models.json"));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let writers = (0..8)
            .map(|writer| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    (0..40)
                        .map(|round| {
                            let body = format!("{{\"writer\":{writer},\"round\":{round}}}");
                            super::write_private_model_cache(&path, body.as_bytes(), 4096)
                                .map_err(|error| error.to_string())
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap().unwrap();
        }
        let published = std::fs::read_to_string(&*path).unwrap();
        assert!(published.starts_with("{\"writer\":"), "{published}");
        let mode = std::fs::metadata(&*path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let alias = dir.join("alias.json");
        std::os::unix::fs::symlink(&*path, &alias).unwrap();
        assert!(super::write_private_model_cache(&alias, b"{}", 4096).is_err());
    }
}
