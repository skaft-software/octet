//! Resolving and launching delegated child sessions.

use super::*;

/// Why a session-owned worker can or cannot be opened as its own interactive
/// session in another process.
///
/// The handle is the opaque, path-free `agent-session:<sha256>` reference that
/// the extension already receives. This verdict is the host-side half: the
/// launchable path is host-only, and nothing here carries a credential, a
/// transcript path, or any session secret.
pub(super) fn launchability(record: &AgentRecord) -> Result<(), String> {
    match &record.status {
        // Unattended mutation fails closed: a worker parked on a decision it
        // no longer has authority for must not be opened for more work.
        DelegatedAgentStatus::AwaitingApproval { .. } => Err(
            "worker is parked at the approval boundary; supplying new authority is required before it can be opened"
                .to_owned(),
        ),
        _ => {
            if record.live_task {
                // One writer per session: a live worker owns this transcript in
                // this process, so another process must not attach to it.
                Err(
                    "a live worker owns this session in the current process; stop or detach it first"
                        .to_owned(),
                )
            } else if !record.session_path.exists() {
                Err("the worker session file is gone".to_owned())
            } else {
                Ok(())
            }
        }
    }
}

/// A session-owned worker that can be handed to another process and opened as
/// its own interactive session.
///
/// The caller keeps the handle string (`reference`) and never receives it back
/// from an extension; `session_path` is host-only and must not be published to
/// an extension, a notice, or a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchableChildSession {
    /// Opaque, path-free, argv-safe handle: `agent-session:<sha256>`.
    pub reference: String,
    /// Host-only transcript path for the launching process.
    pub session_path: PathBuf,
    /// Stable worker identity for diagnostics.
    pub agent_id: String,
    /// Absolute delegation path of the worker.
    pub agent_path: String,
    /// Bounded worker state label at hand-over time.
    pub status: String,
}

/// Strict `agent-session:<sha256>` handle validation.
///
/// The token is deliberately a boring, quotable, argv-safe identifier: the
/// launcher passes it as one `argv` element and rejects shell metacharacters,
/// and nothing in it names a path, a credential, or a session secret.
pub(super) fn validate_launch_reference(reference: &str) -> Result<(), DelegationError> {
    let Some(digest) = reference.strip_prefix("agent-session:") else {
        return Err(DelegationError::Unlaunchable(
            "worker handle must be agent-session:<sha256>".into(),
        ));
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(DelegationError::Unlaunchable(
            "worker handle must carry exactly 64 lowercase hex digits".into(),
        ));
    }
    Ok(())
}

/// Resolves the session-owned launchable handle for one worker from the
/// root-scoped durable roster (`<session directory>/fleet-<owner hash>.json`).
///
/// This is the host-side primitive a *separate* process needs in order to open
/// a session-owned worker as its own interactive session: it needs no live
/// agent, only the owning session directory. It fails closed - an unknown
/// handle, a parked worker, or a vanished transcript is an explicit, bounded
/// refusal, never a fabricated launch. Liveness is process-local, so a worker
/// that is still live in another process is caught by the child session's own
/// open-time lock rather than by this roster read.
pub fn resolve_launchable_child_session(
    session_directory: &Path,
    reference: &str,
) -> Result<LaunchableChildSession, DelegationError> {
    validate_launch_reference(reference)?;
    let unavailable = |message: String| DelegationError::Unlaunchable(message);
    let directory = std::fs::read_dir(session_directory).map_err(|error| {
        unavailable(format!(
            "no session-owned delegation roster in this session: {error}"
        ))
    })?;
    let mut paths = Vec::new();
    // A workspace contains private team directories as well as per-root files.
    // Bound inventory work without loading other roots' transcript bodies.
    for (count, entry) in directory.enumerate() {
        if count >= 100_000 {
            return Err(unavailable(
                "delegation directory inventory limit exceeded".into(),
            ));
        }
        let entry = entry.map_err(|error| unavailable(error.to_string()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let scoped = name
            .strip_prefix("fleet-")
            .and_then(|name| name.strip_suffix(".json"))
            .is_some_and(|digest| {
                digest.len() == 16 && digest.bytes().all(|b| b.is_ascii_hexdigit())
            });
        if scoped || name == FLEET_ROSTER_FILE {
            paths.push(entry.path());
        }
    }
    paths.sort();
    if paths.is_empty() {
        return Err(unavailable(
            "no session-owned delegation roster in this session".into(),
        ));
    }
    let mut found = None;
    for path in paths {
        // Other roots' damaged or obsolete snapshots cannot make a valid
        // worker unavailable. Invalid files confer no launch authority.
        let Ok(bytes) = secure_fs::read_private_file_bounded(&path, MAX_FLEET_ROSTER_BYTES) else {
            continue;
        };
        let Ok(fleet) = serde_json::from_slice::<DurableFleet>(&bytes) else {
            continue;
        };
        if !matches!(fleet.version, 1 | FLEET_ROSTER_VERSION) {
            continue;
        }
        let scoped_path = fleet_roster_path(session_directory, &fleet.root_session);
        if path
            .file_name()
            .is_some_and(|name| name == FLEET_ROSTER_FILE)
        {
            // Never let an old shared snapshot shadow a migrated owner's state.
            if scoped_path.exists() {
                continue;
            }
        } else if path != scoped_path {
            continue;
        }
        if let Some(record) = fleet.records.into_iter().find(|record| {
            delegated_session_reference(&record.session_path).as_deref() == Some(reference)
        }) {
            found = Some(record);
            break;
        }
    }
    let record = found.ok_or_else(|| unavailable("unknown worker handle".into()))?;
    if let DelegatedAgentStatus::AwaitingApproval { .. } = record.status {
        return Err(DelegationError::Unlaunchable(
            "worker is parked at the approval boundary; supplying new authority is required before it can be opened"
                .into(),
        ));
    }
    if !record.session_path.exists() {
        return Err(DelegationError::Unlaunchable(
            "the worker session file is gone".into(),
        ));
    }
    Ok(LaunchableChildSession {
        reference: reference.to_owned(),
        session_path: record.session_path,
        agent_id: record.agent_id,
        agent_path: record.agent_path,
        status: record.status.label().to_owned(),
    })
}

/// Opaque, session-owned delegation handle for host-side callers.
///
/// It owns the durable worker records of one session and resolves a worker's
/// launchable interactive handle with the process-local liveness the durable
/// roster cannot carry. Extensions never see this type: they receive only the
/// opaque `agent-session:<sha256>` token plus the `launchable` /
/// `launch_blocked` verdict on each `agent/list` row.
#[derive(Clone)]
pub struct SessionDelegationHandle {
    pub(super) manager: Arc<DelegationManager>,
}

impl SessionDelegationHandle {
    /// Resolves a worker's launchable handle, failing closed on a parked
    /// worker, a live in-process worker, or a vanished transcript.
    pub fn launchable_child_session(
        &self,
        reference: &str,
    ) -> Result<LaunchableChildSession, DelegationError> {
        self.manager.launchable_child_session(reference)
    }

    /// The opaque handle of one worker, when it has a durable transcript.
    pub fn reference_for_agent(&self, agent_id: &str) -> Option<String> {
        let state = self
            .manager
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state
            .records
            .get(agent_id)
            .and_then(|record| delegated_session_reference(&record.session_path))
    }

    /// Session directory that owns the durable roster.
    pub fn session_directory(&self) -> &Path {
        &self.manager.config.session_directory
    }
}

impl std::fmt::Debug for SessionDelegationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionDelegationHandle")
            .field("session_directory", &self.manager.config.session_directory)
            .finish()
    }
}

/// Bounded, actionable diagnostic for a spawn that reuses a live
/// session-scoped worker name.
///
/// The name is never recycled silently: the caller is told which worker holds
/// it, what state that worker is in, and which collaboration tool resumes or
/// stops it.
pub(super) fn existing_task_name_error(record: &AgentRecord) -> String {
    let name = record
        .display_task_name
        .as_deref()
        .unwrap_or(record.task_name.as_str());
    let parent = record
        .identity
        .path
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                ROOT_AGENT_PATH
            } else {
                parent
            }
        })
        .unwrap_or(ROOT_AGENT_PATH);
    let state = record.status.label();
    let id = &record.identity.id;
    match &record.status {
        DelegatedAgentStatus::Detached | DelegatedAgentStatus::AwaitingApproval { .. } => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); its owning \
             session reattaches it on a later turn, and a free concurrency slot is required"
        ),
        DelegatedAgentStatus::Pending | DelegatedAgentStatus::Running => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); steer it \
             with send_message or followup_task"
        ),
        _ => format!(
            "task name already exists under {parent}: {name} is {state} (agent {id}); send more \
             work with followup_task, or stop it with interrupt_agent"
        ),
    }
}
