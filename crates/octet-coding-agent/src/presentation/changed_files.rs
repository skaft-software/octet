#![allow(missing_docs)]

//! Workspace snapshots and conservative changed-file evidence.
//!
//! A display candidate is not a change by itself. A path is projected only
//! when it is inside the workspace and its post-tool content is validated
//! against the live workspace snapshot.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// A bounded identity for one regular workspace file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceFileSnapshot {
    pub display_path: String,
    pub content_hash: String,
    pub byte_len: u64,
}

/// A snapshot of regular files below a workspace root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSnapshot {
    root: PathBuf,
    files: BTreeMap<String, WorkspaceFileSnapshot>,
}

impl WorkspaceSnapshot {
    /// Capture all regular, non-symlink files below `root`.
    pub fn capture(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root is not a directory",
            ));
        }
        let mut files = BTreeMap::new();
        capture_directory(&root, &root, &mut files)?;
        Ok(Self { root, files })
    }

    /// Create an empty snapshot rooted at an existing workspace directory.
    /// This is useful when callers already have a bounded list of paths and
    /// want to project a newly-created file against an empty baseline.
    pub fn empty(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "workspace root is not a directory",
            ));
        }
        Ok(Self {
            root,
            files: BTreeMap::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn files(&self) -> &BTreeMap<String, WorkspaceFileSnapshot> {
        &self.files
    }

    pub fn file(&self, path: &str) -> Option<&WorkspaceFileSnapshot> {
        self.files.get(path)
    }

    pub fn contains(&self, path: &str) -> bool {
        self.files.contains_key(path)
    }

    /// Read one regular file through the workspace boundary.
    pub fn read_file(&self, requested: &str) -> Option<WorkspaceFileSnapshot> {
        let relative = relative_workspace_path(&self.root, requested)?;
        let path = self.root.join(&relative);
        let metadata = path.symlink_metadata().ok()?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return None;
        }
        let canonical = path.canonicalize().ok()?;
        if canonical == self.root || !canonical.starts_with(&self.root) {
            return None;
        }
        let bytes = fs::read(&canonical).ok()?;
        Some(WorkspaceFileSnapshot {
            display_path: relative.display().to_string().replace('\\', "/"),
            content_hash: content_hash(&bytes),
            byte_len: bytes.len() as u64,
        })
    }

    /// Return the path as a stable workspace-relative display value.
    pub fn display_path(&self, requested: &str) -> Option<String> {
        relative_workspace_path(&self.root, requested)
            .map(|path| path.display().to_string().replace('\\', "/"))
    }
}

/// A raw mutation argument plus optional hash evidence captured at tool
/// completion. The raw path is retained so validation can resolve it against
/// the actual workspace roots rather than a presentation-normalized string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFileCandidate {
    pub path: String,
    pub reported_hash: Option<String>,
}

pub(crate) fn project_candidates<'a, I>(
    before: Option<&WorkspaceSnapshot>,
    after: Option<&WorkspaceSnapshot>,
    candidates: I,
) -> std::collections::BTreeSet<String>
where
    I: IntoIterator<Item = &'a ChangedFileCandidate>,
{
    let (Some(before), Some(after)) = (before, after) else {
        return std::collections::BTreeSet::new();
    };
    candidates
        .into_iter()
        .filter_map(|candidate| {
            validate_changed_file_evidence(
                before,
                after,
                &candidate.path,
                candidate.reported_hash.as_deref(),
            )
        })
        .collect()
}

/// A set of validated workspace-relative changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangedFileProjection {
    paths: std::collections::BTreeSet<String>,
}

impl ChangedFileProjection {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn paths(&self) -> &std::collections::BTreeSet<String> {
        &self.paths
    }

    pub fn into_paths(self) -> std::collections::BTreeSet<String> {
        self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    fn insert(&mut self, path: String) {
        self.paths.insert(path);
    }
}

/// Compare only the supplied mutation candidates. This keeps a tool's
/// accounting local and prevents unrelated concurrent workspace changes from
/// being attributed to it.
pub fn project_changed_files<'a, I>(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    candidates: I,
) -> ChangedFileProjection
where
    I: IntoIterator<Item = &'a str>,
{
    let mut projection = ChangedFileProjection::new();
    for candidate in candidates {
        if let Some(path) = validated_changed_path(before, after, candidate, None) {
            projection.insert(path);
        }
    }
    projection
}

/// Validate one mutation candidate against both workspace snapshots and an
/// optional provider/host-reported post-write hash.
///
/// A malformed reported hash is rejected rather than normalized. If no hash
/// was reported, the actual before/after snapshot difference is still the
/// evidence; this supports older write tools while keeping the path and bytes
/// workspace-bound.
pub fn validate_changed_file_evidence(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    candidate: &str,
    reported_hash: Option<&str>,
) -> Option<String> {
    validated_changed_path(before, after, candidate, reported_hash)
}

fn validated_changed_path(
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
    candidate: &str,
    reported_hash: Option<&str>,
) -> Option<String> {
    let before_path = relative_workspace_path(before.root(), candidate)?;
    let after_path = relative_workspace_path(after.root(), candidate)?;
    if before_path != after_path {
        return None;
    }
    let path = before_path.display().to_string().replace('\\', "/");
    let before_hash = before.file(&path).map(|file| file.content_hash.as_str());
    let after_file = after.file(&path);
    let after_hash = after_file.map(|file| file.content_hash.as_str());
    if before_hash == after_hash {
        return None;
    }

    if let Some(reported_hash) = reported_hash {
        if !is_valid_sha256_hex(reported_hash) || Some(reported_hash) != after_hash {
            return None;
        }
    }
    Some(path)
}

/// Return the lowercase SHA-256 digest of file/content bytes.
pub fn content_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Hash evidence is intentionally strict: exactly 64 lowercase hexadecimal
/// characters, never an uppercase or abbreviated digest.
pub fn is_valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Extract a trusted post-tool hash only when the output contains exactly one
/// valid lowercase SHA-256 token. A malformed or ambiguous report is not
/// evidence: callers must not accept a valid token hidden beside another
/// malformed or duplicate token.
pub fn trusted_output_hash(text: &str) -> Option<String> {
    reported_output_hash(text).filter(|hash| is_valid_sha256_hex(hash))
}

/// Extract one trusted post-tool hash token. If hash evidence is present but
/// malformed, or if more than one hash token is reported, return an invalid
/// marker so callers cannot accidentally accept a partial/ambiguous report.
pub fn reported_output_hash(text: &str) -> Option<String> {
    let hashes = text
        .split_ascii_whitespace()
        .filter_map(|token| token.strip_prefix("hash="))
        .collect::<Vec<_>>();
    match hashes.as_slice() {
        [] => None,
        [hash] if is_valid_sha256_hex(hash) => Some((*hash).to_owned()),
        [hash] => Some((*hash).to_owned()),
        _ => Some(String::new()),
    }
}

pub fn output_contains_hash_token(text: &str) -> bool {
    text.split_ascii_whitespace()
        .any(|token| token.starts_with("hash="))
}

fn capture_directory(
    root: &Path,
    directory: &Path,
    files: &mut BTreeMap<String, WorkspaceFileSnapshot>,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            capture_directory(root, &path, files)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let canonical = path.canonicalize()?;
        if canonical == root || !canonical.starts_with(root) {
            continue;
        }
        let bytes = fs::read(&canonical)?;
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "file escaped workspace"))?;
        let display_path = relative.display().to_string().replace('\\', "/");
        files.insert(
            display_path.clone(),
            WorkspaceFileSnapshot {
                display_path,
                content_hash: content_hash(&bytes),
                byte_len: bytes.len() as u64,
            },
        );
    }
    Ok(())
}

fn relative_workspace_path(root: &Path, requested: &str) -> Option<PathBuf> {
    if requested.trim().is_empty() || requested.contains("://") || requested.starts_with("file:") {
        return None;
    }
    let source = Path::new(requested);
    let relative = if source.is_absolute() {
        source.strip_prefix(root).ok()?.to_path_buf()
    } else {
        source.to_path_buf()
    };
    let mut result = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => result.push(value),
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!result.as_os_str().is_empty()).then_some(result)
}
