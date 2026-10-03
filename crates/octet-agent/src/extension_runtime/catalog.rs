//! The static, content-bound extension catalog.
//!
//! Building a catalog never launches an executable. It resolves each already
//! discovered descriptor to a manifest-selected entry plus a SHA-256 content
//! digest over the manifest bytes, the entrypoint metadata, the entrypoint
//! source, and — for Python entrypoints — every local `.py` file below the
//! entrypoint. An entry that cannot be verified stays in the catalog as a
//! diagnostic, and its digest is deliberately computed in a separate domain
//! tag so it can never collide with a verified digest.
//!
//! This is separate from [`super::manager`] because the catalog is pure data
//! with a hard source bound and no process, task, or session concept at all.
//! It is the security boundary that decides *what may be shared*; the manager
//! only consumes the answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{sha256_hex, ExtensionRuntimeDomainError};
use crate::extension_process::{
    DiscoveredExtension, ExtensionLifecycleProfile, ExtensionRuntimeSharing, ExtensionTrust,
};
use crate::secure_fs::read_regular_file_bounded;
const MAX_CATALOG_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CATALOG_PACKAGE_FILES: usize = 256;
const MAX_CATALOG_PACKAGE_ENTRIES: usize = 1024;
const MAX_CATALOG_PACKAGE_DEPTH: usize = 8;
const MAX_CATALOG_PACKAGE_BYTES: usize = 16 * 1024 * 1024;

/// SHA-256 content identity used for explicit runtime sharing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ExtensionContentDigest(String);

impl ExtensionContentDigest {
    /// Parses an externally stored lowercase SHA-256 digest.
    pub fn parse(value: impl Into<String>) -> Result<Self, ExtensionRuntimeDomainError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ExtensionRuntimeDomainError::InvalidContentDigest);
        }
        Ok(Self(value))
    }

    /// Returns the lowercase digest text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Non-fatal static-catalog diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionRuntimeCatalogDiagnostic {
    /// Manifest-selected extension name, when it was parseable.
    pub extension: String,
    /// Bounded path-free diagnostic category.
    pub message: String,
}

/// One static catalog entry. Constructing this value never launches a process.
#[derive(Clone, Debug)]
pub struct ExtensionRuntimeCatalogEntry {
    /// The validated selected manifest and explicit activation policy.
    pub descriptor: DiscoveredExtension,
    /// Digest of the manifest, local entrypoint, and bounded local Python packages.
    pub content_digest: ExtensionContentDigest,
    /// Whether the entrypoint content was directly verified. Workspace sharing
    /// requires this to be true; isolated legacy execution remains compatible
    /// with PATH-resolved commands.
    pub source_verified: bool,
}

impl ExtensionRuntimeCatalogEntry {
    /// Returns the selected lifecycle profile.
    pub fn lifecycle(&self) -> ExtensionLifecycleProfile {
        self.descriptor.manifest.runtime.lifecycle
    }

    /// Returns the explicit sharing selection.
    pub fn sharing(&self) -> ExtensionRuntimeSharing {
        self.descriptor.manifest.runtime.sharing
    }

    pub(super) fn current_digest(&self) -> Result<(ExtensionContentDigest, bool), String> {
        catalog_content_digest(&self.descriptor, &mut ExtensionDigestWork::default())
    }
}

/// Bounded source work performed while constructing one catalog. Admission and
/// post-handshake source rechecks remain separate security boundaries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtensionDigestWork {
    /// Source files whose bytes entered the catalog hash.
    pub files: usize,
    /// Source bytes whose content entered the catalog hash.
    pub bytes: usize,
    /// Discovered entries skipped because they cannot activate.
    pub inactive: usize,
}

/// Static content-bound catalog used by a runtime manager.
#[derive(Clone, Debug, Default)]
pub struct ExtensionRuntimeCatalog {
    entries: BTreeMap<String, ExtensionRuntimeCatalogEntry>,
    diagnostics: Vec<ExtensionRuntimeCatalogDiagnostic>,
    digest_work: ExtensionDigestWork,
}

impl ExtensionRuntimeCatalog {
    /// Builds a static catalog from already discovered descriptors.
    ///
    /// This reads only bounded regular files needed for digesting and never
    /// executes an extension. Invalid source fingerprints remain inspectable
    /// diagnostics instead of making unrelated entries disappear.
    pub fn from_descriptors(descriptors: impl IntoIterator<Item = DiscoveredExtension>) -> Self {
        let mut catalog = Self::default();
        for descriptor in descriptors {
            let name = descriptor.manifest.name.clone();
            if catalog.entries.contains_key(&name) {
                catalog.diagnostics.push(ExtensionRuntimeCatalogDiagnostic {
                    extension: name,
                    message: "duplicate selected extension name".into(),
                });
                continue;
            }
            // Status still retains inactive descriptors, but no content identity
            // can authorize their launch. An enable/trust/policy change builds a
            // new catalog and verifies the source before any activation.
            if !descriptor.activation.enabled
                || descriptor.activation.trust != ExtensionTrust::Trusted
            {
                catalog.digest_work.inactive += 1;
                let encoded = serde_json::to_vec(&descriptor.manifest).unwrap_or_default();
                catalog.entries.insert(
                    name,
                    ExtensionRuntimeCatalogEntry {
                        descriptor,
                        content_digest: ExtensionContentDigest(sha256_hex(
                            b"octet-extension-inactive-catalog-v1\0",
                            encoded,
                        )),
                        source_verified: false,
                    },
                );
                continue;
            }
            let (content_digest, source_verified) =
                match catalog_content_digest(&descriptor, &mut catalog.digest_work) {
                    Ok(value) => value,
                    Err(message) => {
                        catalog.diagnostics.push(ExtensionRuntimeCatalogDiagnostic {
                            extension: name.clone(),
                            message,
                        });
                        // Keep isolated legacy behavior available while preventing
                        // unverified source sharing. The fallback remains stable for
                        // the parsed manifest and cannot collide with verified
                        // content because it has a separate domain tag.
                        let encoded = serde_json::to_vec(&descriptor.manifest).unwrap_or_default();
                        (
                            ExtensionContentDigest(sha256_hex(
                                b"octet-extension-unverified-catalog-v1\0",
                                encoded,
                            )),
                            false,
                        )
                    }
                };
            catalog.entries.insert(
                name,
                ExtensionRuntimeCatalogEntry {
                    descriptor,
                    content_digest,
                    source_verified,
                },
            );
        }
        catalog
    }

    /// Returns bounded catalog hashing work for opt-in startup attribution.
    pub fn digest_work(&self) -> &ExtensionDigestWork {
        &self.digest_work
    }

    /// Returns an entry by selected manifest name.
    pub fn get(&self, name: &str) -> Option<&ExtensionRuntimeCatalogEntry> {
        self.entries.get(name)
    }

    /// Returns all entries in deterministic selected-name order.
    pub fn entries(&self) -> impl Iterator<Item = &ExtensionRuntimeCatalogEntry> {
        self.entries.values()
    }

    /// Returns non-fatal source/deduplication diagnostics.
    pub fn diagnostics(&self) -> &[ExtensionRuntimeCatalogDiagnostic] {
        &self.diagnostics
    }
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_owned())
    } else {
        std::path::absolute(path).map_err(|_| "source path cannot be resolved".into())
    }
}

// Conservatively bind Python's local import surface, including vendored and
// namespace packages. Scan every local .py below the entrypoint rather than
// guessing which conditional/dynamic imports will execute at runtime.
fn python_package_sources(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn visit(
        directory: &Path,
        depth: usize,
        sources: &mut Vec<PathBuf>,
        entries: &mut usize,
    ) -> Result<(), String> {
        if depth > MAX_CATALOG_PACKAGE_DEPTH {
            return Err("extension package depth exceeds source bound".into());
        }
        let listing = std::fs::read_dir(directory)
            .map_err(|_| "extension package directory cannot be verified".to_owned())?;
        for entry in listing {
            let entry =
                entry.map_err(|_| "extension package directory cannot be verified".to_owned())?;
            *entries += 1;
            if *entries > MAX_CATALOG_PACKAGE_ENTRIES {
                return Err("extension package entries exceed source bound".into());
            }
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|_| "extension package entry cannot be verified".to_owned())?;
            if path.extension().is_some_and(|extension| extension == "py") {
                // Keep symlink/special candidates in the list so secure reads
                // reject them rather than silently omitting imported modules.
                sources.push(path);
                if sources.len() > MAX_CATALOG_PACKAGE_FILES {
                    return Err("extension package files exceed source bound".into());
                }
            } else if file_type.is_dir() {
                visit(&path, depth + 1, sources, entries)?;
            } else if file_type.is_symlink() {
                // A package directory may itself be a symlink. We cannot
                // safely enumerate its imported modules without following it.
                return Err("extension package directory cannot be verified".into());
            }
        }
        Ok(())
    }

    let mut sources = Vec::new();
    let mut entries = 0;
    visit(root, 0, &mut sources, &mut entries)?;
    sources.sort();
    Ok(sources)
}

fn catalog_content_digest(
    descriptor: &DiscoveredExtension,
    work: &mut ExtensionDigestWork,
) -> Result<(ExtensionContentDigest, bool), String> {
    let manifest_path = absolute_path(&descriptor.manifest_path)?;
    let manifest = read_regular_file_bounded(&manifest_path, 64 * 1024)
        .map_err(|_| "manifest content cannot be verified".to_owned())?;
    work.files += 1;
    work.bytes += manifest.len();
    let mut hasher = Sha256::new();
    hasher.update(b"octet-extension-runtime-content-v1\0manifest\0");
    hasher.update(&manifest);
    hasher.update(b"\0entrypoint\0");
    let entrypoint = serde_json::to_vec(&descriptor.manifest.entrypoint)
        .map_err(|_| "entrypoint metadata cannot be encoded".to_owned())?;
    hasher.update(entrypoint);

    let configured = PathBuf::from(&descriptor.manifest.entrypoint.command);
    let local = if configured.is_absolute() {
        Some(configured)
    } else {
        descriptor
            .manifest_path
            .parent()
            .map(|directory| directory.join(configured))
    };
    let Some(local) = local else {
        return Err("entrypoint source cannot be located".into());
    };
    let local = absolute_path(&local)?;
    match read_regular_file_bounded(&local, MAX_CATALOG_SOURCE_BYTES) {
        Ok(bytes) => {
            work.files += 1;
            work.bytes += bytes.len();
            hasher.update(b"\0source\0");
            let python_entrypoint = local.extension().is_some_and(|extension| extension == "py")
                || bytes
                    .split(|byte| *byte == b'\n')
                    .next()
                    .is_some_and(|line| {
                        line.starts_with(b"#!") && line.windows(6).any(|part| part == b"python")
                    });
            hasher.update(bytes);
            if python_entrypoint {
                let root = local
                    .parent()
                    .ok_or("extension package root cannot be located")?;
                let mut total = 0usize;
                for source in python_package_sources(root)? {
                    let relative = source
                        .strip_prefix(root)
                        .map_err(|_| "extension package path cannot be verified".to_owned())?;
                    let remaining = MAX_CATALOG_PACKAGE_BYTES.saturating_sub(total);
                    let content = read_regular_file_bounded(&source, remaining)
                        .map_err(|_| "extension package source cannot be verified".to_owned())?;
                    total += content.len();
                    work.files += 1;
                    work.bytes += content.len();
                    hasher.update(b"\0package\0");
                    hasher.update(relative.to_string_lossy().as_bytes());
                    hasher.update(b"\0");
                    hasher.update(content);
                }
            }
            Ok((
                ExtensionContentDigest(format!("{:x}", hasher.finalize())),
                true,
            ))
        }
        Err(_) => {
            // A PATH-resolved executable is a compatible legacy launch form,
            // but it is intentionally not eligible for content-digested
            // sharing because the manager cannot bind its bytes safely here.
            hasher.update(b"\0unverified-source\0");
            Ok((
                ExtensionContentDigest(format!("{:x}", hasher.finalize())),
                false,
            ))
        }
    }
}
