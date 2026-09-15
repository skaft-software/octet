//! Durable recovery journal for Serve-owned session deletion.
//!
//! The journal is deliberately kept separate from the host and HTTP routing
//! code. It provides the crash-safe primitives used by the host's startup
//! reconciliation and permanent-delete workflow.

use super::*;

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PendingSessionDeletion {
    pub(super) version: u16,
    pub(super) session_id: String,
    pub(super) project_id: String,
    pub(super) trashed_at_ms: u64,
    pub(super) committed: bool,
}

impl PendingSessionDeletion {
    pub(super) fn new(
        session_id: &SessionId,
        project_id: &ProjectId,
        trashed_at_ms: u64,
    ) -> PendingSessionDeletion {
        Self {
            version: SESSION_DELETION_VERSION,
            session_id: session_id.as_str().to_owned(),
            project_id: project_id.as_str().to_owned(),
            trashed_at_ms,
            committed: false,
        }
    }

    pub(super) fn validate(&self) -> bool {
        self.version == SESSION_DELETION_VERSION
            && self.trashed_at_ms > 0
            && SessionId::new(self.session_id.clone()).is_ok()
            && ProjectId::new(self.project_id.clone()).is_ok()
    }
}

pub(super) fn session_deletion_directory(serve_state_dir: &Path) -> anyhow::Result<PathBuf> {
    let directory = serve_state_dir.join(SESSION_DELETION_DIRECTORY);
    match directory.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                anyhow::bail!("session deletion journal must be a real directory");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&directory)?;
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

pub(super) fn pending_session_deletion_path(directory: &Path, session_id: &str) -> PathBuf {
    directory.join(format!("{}.json", stable_hash(session_id.as_bytes())))
}

pub(super) fn write_pending_session_deletion(
    serve_state_dir: &Path,
    deletion: &PendingSessionDeletion,
) -> anyhow::Result<()> {
    if !deletion.validate() {
        anyhow::bail!("invalid pending session deletion");
    }
    let directory = session_deletion_directory(serve_state_dir)?;
    let file_key = stable_hash(deletion.session_id.as_bytes());
    let destination = pending_session_deletion_path(&directory, &deletion.session_id);
    match destination.symlink_metadata() {
        Ok(metadata) => {
            if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
                anyhow::bail!("session deletion journal entry is unsafe");
            }
            let existing = read_pending_session_deletion(&destination, &file_key)?;
            let same_intent = existing.version == deletion.version
                && existing.session_id == deletion.session_id
                && existing.project_id == deletion.project_id
                && existing.trashed_at_ms == deletion.trashed_at_ms;
            if !same_intent {
                anyhow::bail!("session deletion journal intent cannot be replaced");
            }
            if existing.committed && !deletion.committed {
                anyhow::bail!("committed session deletion cannot be downgraded");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let bytes = serde_json::to_vec(deletion)?;
    if bytes.len() as u64 > MAX_SESSION_DELETION_RECORD_BYTES {
        anyhow::bail!("session deletion journal entry is too large");
    }
    let mut random = [0u8; 16];
    getrandom::fill(&mut random)?;
    let temporary = directory.join(format!(".tmp-{}", stable_hash(&random)));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, &destination)?;
        std::fs::File::open(&directory)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

pub(super) fn read_pending_session_deletion(
    path: &Path,
    expected_file_key: &str,
) -> anyhow::Result<PendingSessionDeletion> {
    let metadata = path.symlink_metadata()?;
    if !metadata.file_type().is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > MAX_SESSION_DELETION_RECORD_BYTES
    {
        anyhow::bail!("session deletion journal entry is unsafe");
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    options
        .open(path)?
        .take(MAX_SESSION_DELETION_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SESSION_DELETION_RECORD_BYTES {
        anyhow::bail!("session deletion journal entry is too large");
    }
    let deletion = serde_json::from_slice::<PendingSessionDeletion>(&bytes)?;
    if !deletion.validate() || stable_hash(deletion.session_id.as_bytes()) != expected_file_key {
        anyhow::bail!("session deletion journal entry is invalid");
    }
    Ok(deletion)
}

pub(super) fn load_pending_session_deletions(
    serve_state_dir: &Path,
) -> anyhow::Result<Vec<PendingSessionDeletion>> {
    let directory = session_deletion_directory(serve_state_dir)?;
    let mut deletions = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(file_key) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
            if name.to_string_lossy().starts_with(".tmp-") {
                let _ = std::fs::remove_file(entry.path());
            }
            continue;
        };
        if file_key.len() != 64 || !file_key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            continue;
        }
        deletions.push(read_pending_session_deletion(&entry.path(), file_key)?);
    }
    deletions.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(deletions)
}

pub(super) fn remove_pending_session_deletion(
    serve_state_dir: &Path,
    session_id: &str,
) -> anyhow::Result<()> {
    let directory = session_deletion_directory(serve_state_dir)?;
    let path = pending_session_deletion_path(&directory, session_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(path)?;
            std::fs::File::open(directory)?.sync_all()?;
            Ok(())
        }
        Ok(_) => anyhow::bail!("session deletion journal entry is unsafe"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
