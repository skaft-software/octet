//! Session authority for host requests.
//!
//! Only this module selects, opens, and seeds session files. All paths are
//! checked against the canonical configured session directory before replay or
//! append, so transport and orchestration cannot bypass that boundary.

use std::path::{Path, PathBuf};

use anyhow::Context as _;
use octet_agent::{EntryValue, Session};
use octet_ai::{AssistantMessage, AssistantPart, Message, UserMessage, UserPart};

use crate::app::bootstrap::SessionSelection;

use super::protocol::{RunRequest, SeedMessage, SeedRole, MAX_ID_BYTES};

pub(crate) fn session_selection(
    session_dir: &Path,
    request: &RunRequest,
) -> anyhow::Result<(SessionSelection, Option<Session>)> {
    octet_agent::secure_fs::create_private_directory_all(session_dir)
        .with_context(|| format!("creating session directory {}", session_dir.display()))?;
    let canonical_dir = session_dir.canonicalize()?;
    if let Some(resume) = request.resume_session.as_deref() {
        let (path, session) = open_confined_session(resume, &canonical_dir, "resume session")?;
        return Ok((SessionSelection::OpenExisting(path), Some(session)));
    }
    let id = request
        .session_id
        .clone()
        .unwrap_or_else(generated_session_id);
    if !valid_session_id(&id) {
        anyhow::bail!("session id is invalid");
    }
    let path = canonical_dir.join(format!("{id}.jsonl"));
    match path.symlink_metadata() {
        Ok(_) => {
            let (path, session) = open_confined_session(&path, &canonical_dir, "session")?;
            Ok((SessionSelection::OpenExisting(path), Some(session)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok((SessionSelection::CreateNew(path), None))
        }
        Err(error) => Err(error).with_context(|| format!("checking session {}", path.display())),
    }
}

fn open_confined_session(
    path: &Path,
    directory: &Path,
    label: &str,
) -> anyhow::Result<(PathBuf, Session)> {
    let canonical = confined_session_file(path, directory, label)?;
    let file = octet_agent::secure_fs::open_regular_file_for_append(&canonical)
        .with_context(|| format!("opening {label} {}", canonical.display()))?;
    let session = Session::open_with_file(canonical.clone(), file)
        .with_context(|| format!("replaying {label} {}", canonical.display()))?;
    Ok((canonical, session))
}

fn confined_session_file(path: &Path, directory: &Path, label: &str) -> anyhow::Result<PathBuf> {
    let link_metadata = path
        .symlink_metadata()
        .with_context(|| format!("{label} {} is unavailable", path.display()))?;
    if link_metadata.file_type().is_symlink() {
        anyhow::bail!("{label} must not be a symbolic link");
    }
    let canonical = path
        .canonicalize()
        .with_context(|| format!("{label} {} is unavailable", path.display()))?;
    if !canonical.starts_with(directory) {
        anyhow::bail!("{label} must stay inside the configured session directory");
    }
    if !canonical
        .metadata()
        .with_context(|| format!("reading {label} metadata"))?
        .is_file()
    {
        anyhow::bail!("{label} must be a regular file");
    }
    Ok(canonical)
}

fn generated_session_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("host-{}-{nanos}", std::process::id())
}

pub(crate) fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(crate) fn seed_history(
    app: &mut crate::app::App,
    history: &[SeedMessage],
) -> anyhow::Result<()> {
    for message in history {
        let message = match message.role {
            SeedRole::User => Message::User(UserMessage {
                content: vec![UserPart::Text(message.text.clone())],
            }),
            SeedRole::Assistant => Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text(message.text.clone())],
                model: app.model.spec.id.clone(),
                protocol: app.model.spec.protocol,
            }),
        };
        app.agent
            .session_mut()
            .append(EntryValue::Message(message))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
