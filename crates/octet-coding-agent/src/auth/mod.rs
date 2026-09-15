#![allow(missing_docs)]

//! Provider authentication flows for subscription-backed models.
//!
//! OpenAI Codex ("Sign in with ChatGPT"), GitHub Copilot device OAuth, and
//! custom OpenAI-compatible endpoint credentials. Authentication stays in the
//! product crate behind public credential/provider seams; `octet-ai` is not touched.

pub mod codex;
pub mod copilot;
pub mod custom;

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
