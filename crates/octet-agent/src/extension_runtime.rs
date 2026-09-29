//! Governed process-fleet ownership for executable extensions.
//!
//! A catalog is static and content-bound: loading it never launches an
//! executable. A manager owns the resulting process fleet, while each product
//! session receives an [`ExtensionSessionBinding`] that may attach eligible
//! runtimes. Shared processes require all of canonical workspace, explicit
//! trust domain, explicit manifest sharing policy, and content digest to match.
//!
//! This file is only the crate-facing surface. The implementation is split
//! into three siblings so each concern can be read on its own:
//!
//! - [`catalog`] owns the static catalog, the content digest that makes an
//!   entry shareable, and the bounded source scan behind both.
//! - [`governance`] owns the budget, usage, status, and error vocabulary the
//!   fleet is measured and reported in. It contains no process logic.
//! - [`manager`] owns the durable process fleet and the per-session bindings
//!   that attach to it, including the tests that exercise both.
//!
//! The split exists because the three concerns change for different reasons:
//! a catalog change is a content-identity change, a governance change is a
//! limits/reporting change, and a manager change is a lifecycle change. Keeping
//! them in one file made every one of those reviews read the whole fleet.

use std::path::{Path, PathBuf};
use std::sync::{Mutex as StdMutex, RwLock as StdRwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

mod catalog;
mod governance;
mod manager;

pub use catalog::{
    ExtensionContentDigest, ExtensionDigestWork, ExtensionRuntimeCatalog,
    ExtensionRuntimeCatalogDiagnostic, ExtensionRuntimeCatalogEntry,
};
pub use governance::{
    ExtensionManagedRuntimeState, ExtensionResourceExhausted, ExtensionRuntimeActivation,
    ExtensionRuntimeActivationOutcome, ExtensionRuntimeBudget, ExtensionRuntimeFailure,
    ExtensionRuntimeLease, ExtensionRuntimeManagerError, ExtensionRuntimeProvenance,
    ExtensionRuntimeResource, ExtensionRuntimeStatus, ExtensionRuntimeUsage,
};
pub use manager::{ExtensionRuntimeManager, ExtensionSessionBinding};

const ESTIMATED_PROCESS_FDS: usize = 4;
const SUPERVISOR_POLL: Duration = Duration::from_millis(100);
fn lock<T>(value: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn read<T>(value: &StdRwLock<T>) -> std::sync::RwLockReadGuard<'_, T> {
    value
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write<T>(value: &StdRwLock<T>) -> std::sync::RwLockWriteGuard<'_, T> {
    value
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn sha256_hex(domain: &[u8], bytes: impl AsRef<[u8]>) -> String {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(bytes.as_ref());
    format!("{:x}", digest.finalize())
}

/// Error while canonicalizing a workspace or defining an explicit trust domain.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionRuntimeDomainError {
    /// The workspace could not be canonicalized into an existing directory.
    #[error("extension runtime workspace is unavailable")]
    WorkspaceUnavailable,
    /// A trust-domain label was empty or contained unsupported characters.
    #[error("extension runtime trust domain is invalid")]
    InvalidTrustDomain,
    /// A persisted content identity was not a lowercase SHA-256 digest.
    #[error("extension runtime content digest is invalid")]
    InvalidContentDigest,
}

/// Canonical workspace identity used by one extension runtime domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CanonicalWorkspace {
    path: PathBuf,
    digest: String,
}

impl CanonicalWorkspace {
    /// Resolves an existing workspace directory once for domain ownership.
    pub fn new(path: impl AsRef<Path>) -> Result<Self, ExtensionRuntimeDomainError> {
        let path = path
            .as_ref()
            .canonicalize()
            .map_err(|_| ExtensionRuntimeDomainError::WorkspaceUnavailable)?;
        if !path.is_dir() {
            return Err(ExtensionRuntimeDomainError::WorkspaceUnavailable);
        }
        let digest = sha256_hex(
            b"octet-extension-workspace-v1\0",
            path.to_string_lossy().as_bytes(),
        );
        Ok(Self { path, digest })
    }

    /// Returns the canonical local path used as the child working directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns a path-free stable fingerprint for diagnostics and provenance.
    pub fn fingerprint(&self) -> &str {
        &self.digest
    }
}

/// Explicit trust partition for a runtime domain.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ExtensionTrustDomain {
    fingerprint: String,
}

impl ExtensionTrustDomain {
    /// Creates an explicit trust partition from a bounded stable label.
    ///
    /// The original label is deliberately not retained or exposed by runtime
    /// status, so diagnostics never need to reveal a Serve principal or other
    /// trust-routing input.
    pub fn new(label: impl AsRef<str>) -> Result<Self, ExtensionRuntimeDomainError> {
        let label = label.as_ref();
        if label.is_empty()
            || label.len() > 128
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ExtensionRuntimeDomainError::InvalidTrustDomain);
        }
        Ok(Self {
            fingerprint: sha256_hex(b"octet-extension-trust-domain-v1\0", label),
        })
    }

    /// Returns the ordinary local-host trust partition.
    pub fn ordinary() -> Self {
        // This literal is validated above and cannot fail.
        Self::new("ordinary").expect("ordinary trust domain is valid")
    }

    /// Returns the path-free trust partition fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}

/// Host family owning a runtime domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRuntimeHostKind {
    /// The terminal, print, RPC, or native local host.
    Ordinary,
    /// A Serve host. It always uses a separately supplied trust partition.
    Serve,
}

/// Canonical workspace plus explicit trust partition for one manager.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExtensionRuntimeDomain {
    workspace: CanonicalWorkspace,
    trust_domain: ExtensionTrustDomain,
    host_kind: ExtensionRuntimeHostKind,
    fingerprint: String,
}

impl ExtensionRuntimeDomain {
    /// Creates the ordinary local-host domain for an existing workspace.
    pub fn ordinary(workspace: impl AsRef<Path>) -> Result<Self, ExtensionRuntimeDomainError> {
        Self::new(
            CanonicalWorkspace::new(workspace)?,
            ExtensionTrustDomain::ordinary(),
            ExtensionRuntimeHostKind::Ordinary,
        )
    }

    /// Creates a Serve domain. Callers must supply the project/session trust
    /// partition explicitly; Serve and ordinary hosts never share by accident.
    pub fn serve(
        workspace: impl AsRef<Path>,
        trust_domain: ExtensionTrustDomain,
    ) -> Result<Self, ExtensionRuntimeDomainError> {
        Self::new(
            CanonicalWorkspace::new(workspace)?,
            trust_domain,
            ExtensionRuntimeHostKind::Serve,
        )
    }

    /// Creates a domain from already canonical workspace identity and an
    /// explicit trust partition.
    pub fn new(
        workspace: CanonicalWorkspace,
        trust_domain: ExtensionTrustDomain,
        host_kind: ExtensionRuntimeHostKind,
    ) -> Result<Self, ExtensionRuntimeDomainError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(workspace.fingerprint().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(trust_domain.fingerprint().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(match host_kind {
            ExtensionRuntimeHostKind::Ordinary => b"ordinary",
            ExtensionRuntimeHostKind::Serve => b"serve",
        });
        Ok(Self {
            workspace,
            trust_domain,
            host_kind,
            fingerprint: sha256_hex(b"octet-extension-runtime-domain-v1\0", bytes),
        })
    }

    /// Returns the canonical workspace identity.
    pub fn workspace(&self) -> &CanonicalWorkspace {
        &self.workspace
    }

    /// Returns the explicit trust-domain identity.
    pub fn trust_domain(&self) -> &ExtensionTrustDomain {
        &self.trust_domain
    }

    /// Returns whether this is an ordinary or Serve runtime partition.
    pub fn host_kind(&self) -> ExtensionRuntimeHostKind {
        self.host_kind
    }

    /// Returns the complete path-free domain fingerprint.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
}
