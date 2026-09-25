//! Copilot-only storage using the coding host's descriptor-bound private I/O.

use std::fmt;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use octet_agent::secure_fs::{self, SecureFileError};
use serde::{Deserialize, Serialize};

const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

/// Default private store. No GitHub, Codex, editor, or legacy store is imported.
pub fn default_path() -> Result<PathBuf> {
    let home = dirs::home_dir()
        .filter(|home| home.is_absolute())
        .ok_or_else(|| anyhow!("GitHub Copilot credential home is unavailable"))?;
    Ok(home.join(".octet").join("credentials").join("copilot.json"))
}

/// A single owner-private GitHub OAuth credential, separate from inference tokens.
#[derive(Clone)]
pub struct CredentialStore {
    path: PathBuf,
}

impl fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialStore")
            .field("path", &"<private>")
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    version: u8,
    github_token: String,
}

// Never derive Debug or expose the serialized secret-bearing snapshot.
pub(super) struct Snapshot {
    pub(super) bytes: Option<Vec<u8>>,
}

impl Snapshot {
    pub(super) fn token(&self) -> Result<Option<String>> {
        let Some(bytes) = self.bytes.as_deref() else {
            return Ok(None);
        };
        let credential: Credential = serde_json::from_slice(bytes)
            .map_err(|_| anyhow!("GitHub Copilot credential is invalid; sign in again"))?;
        if credential.version != 1 || !valid_token(&credential.github_token) {
            return Err(anyhow!(
                "GitHub Copilot credential is invalid; sign in again"
            ));
        }
        Ok(Some(credential.github_token))
    }
}

pub(super) fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 4096 && token.bytes().all(|byte| byte.is_ascii_graphic())
}

impl CredentialStore {
    /// Select an absolute private file; all components are checked by secure_fs.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub(super) fn snapshot(&self) -> Result<Snapshot> {
        let bytes = match secure_fs::read_private_file_bounded(&self.path, MAX_CREDENTIAL_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(SecureFileError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => {
                return Err(anyhow!(
                    "GitHub Copilot private credential could not be read"
                ))
            }
        };
        Ok(Snapshot { bytes })
    }

    /// Check local credential readiness without constructing a network client.
    pub fn is_configured(&self) -> Result<bool> {
        Ok(self.snapshot()?.token()?.is_some())
    }

    /// Save a fixture credential through the same private, conditional write path.
    #[cfg(test)]
    pub fn save(&self, github_token: &str) -> Result<()> {
        self.save_if_unchanged(&self.snapshot()?, github_token)
    }

    pub(super) fn save_if_unchanged(&self, snapshot: &Snapshot, github_token: &str) -> Result<()> {
        if !valid_token(github_token) {
            return Err(anyhow!("GitHub Copilot credential is invalid"));
        }
        let bytes = serde_json::to_vec(&Credential {
            version: 1,
            github_token: github_token.to_owned(),
        })
        .map_err(|_| anyhow!("GitHub Copilot credential could not be encoded"))?;
        secure_fs::write_private_atomic_if_unchanged(
            &self.path,
            snapshot.bytes.as_deref(),
            &bytes,
            MAX_CREDENTIAL_BYTES,
        )
        .map_err(|_| anyhow!("GitHub Copilot private credential changed or could not be saved"))
    }

    /// Delete exactly this private credential. Malformed JSON is still removable;
    /// symlinks, hard links, insecure files and concurrent replacements are not.
    /// No cache, directory, other provider, or third-party credential is removed.
    pub fn delete(&self) -> Result<()> {
        let Some(bytes) = self.snapshot()?.bytes else {
            return Ok(());
        };
        secure_fs::remove_private_file_if_unchanged(&self.path, &bytes, MAX_CREDENTIAL_BYTES)
            .map_err(|_| {
                anyhow!("GitHub Copilot private credential changed or could not be removed")
            })
    }
}
