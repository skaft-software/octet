//! Agent records, the durable fleet roster and the single-owner fleet lease.

use super::*;

#[derive(Clone, Debug)]
pub(crate) struct DelegatedUsageRecord {
    pub(crate) agent_id: String,
    pub(crate) usage: Usage,
    pub(crate) usage_uncertain: bool,
    pub(crate) usage_exposure: Option<UsageUncertaintyBound>,
    pub(crate) cost: Option<Cost>,
    pub(crate) turn_count: u64,
    pub(crate) tool_call_count: u64,
}

/// Bounded host-observed record of one child tool call, retained for
/// owner-scoped inspection in `agent/list`. Arguments are reduced to a
/// bounded single-line summary; results are never captured here because
/// completed tool results are already persisted in the child session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(super) struct ChildToolActivity {
    /// Tool name.
    pub(super) name: String,
    /// Bounded single-line argument summary (`key=value` pairs).
    pub(super) args_summary: String,
    /// Host capture time of the tool start in Unix milliseconds.
    pub(super) started_at_ms: u64,
    /// Host capture time of the tool finish, when observed.
    pub(super) finished_at_ms: Option<u64>,
    /// Whether the call finished as an error result.
    pub(super) error: bool,
    /// Provider-assigned call ID used to match start/finish; never serialized.
    #[serde(skip)]
    pub(super) call_id: String,
}

pub(super) struct AgentRecord {
    pub(super) identity: AgentIdentity,
    pub(super) task_name: String,
    pub(super) display_task_name: Option<String>,
    pub(super) parent_id: String,
    pub(super) session_path: PathBuf,
    pub(super) status: DelegatedAgentStatus,
    pub(super) command_tx: mpsc::Sender<WorkerCommand>,
    pub(super) shutdown: crate::CancellationToken,
    pub(super) interrupt_requested: bool,
    /// Accepted messages stay in the durable queue while a process-local
    /// channel or prompt holds a delivery attempt.
    pub(super) pending_messages: VecDeque<DirectedMessage>,
    pub(super) inflight_message_ids: BTreeSet<String>,
    pub(super) reserved_messages: QueueUsage,
    pub(super) queued_follow_ups: QueueUsage,
    /// Accepted follow-ups remain here until the child session confirms their
    /// durable delivery. The channel is only a process-local wakeup path.
    pub(super) pending_follow_ups: VecDeque<QueuedFollowUp>,
    pub(super) pending_initial_task: Option<QueuedInitialTask>,
    pub(super) mailbox: VecDeque<MailboxMessage>,
    pub(super) mailbox_delivery: Option<MailboxDeliveryPlan>,
    pub(super) resource_owner: Option<String>,
    pub(super) extension_policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) effective_tool_policy: EffectiveToolPolicy,
    pub(super) orchestration_provenance: DelegationOrchestrationProvenance,
    pub(super) extension_principal: Option<String>,
    pub(super) extension_profile: Option<String>,
    pub(super) extension_idempotency_key: Option<String>,
    pub(super) extension_resource_owner: Option<String>,
    pub(super) extension_message_sha256: Option<String>,
    pub(super) extension_requested_policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) extension_fingerprint: Option<String>,
    pub(super) created_at_ms: u64,
    pub(super) started_at_ms: Option<u64>,
    pub(super) completed_at_ms: Option<u64>,
    pub(super) turn_count: u64,
    pub(super) tool_call_count: u64,
    pub(super) active_tools: BTreeMap<String, String>,
    pub(super) recent_tools: VecDeque<ChildToolActivity>,
    pub(super) usage: Usage,
    /// Process-local provisional generation; never part of durable/billable usage.
    pub(super) streamed_output_bytes: u64,
    pub(super) usage_uncertain: bool,
    pub(super) usage_exposure: Option<UsageUncertaintyBound>,
    pub(super) cost: Option<Cost>,
    pub(super) cost_microdollars: Option<u64>,
    pub(super) deadline_at_ms: Option<u64>,
    /// Effective per-run turn ceiling retained for terminal evidence.
    pub(super) turn_limit: Option<u64>,
    /// Session-scoped lifetime marker: `true` once the owning run ended and
    /// the worker left the run that spawned it. Detached workers keep running
    /// while they need no new authority, and park in
    /// [`DelegatedAgentStatus::AwaitingApproval`] when they do.
    pub(super) detached: bool,
    /// Process-local liveness of this record's worker task. `true` while a task
    /// in this process owns the child session; cleared by every exit path of
    /// `run_worker`. It is deliberately not durable: after a restart no task is
    /// live, which is exactly what a launchable handle needs to know.
    pub(super) live_task: bool,
    /// Process-local incarnation; a fleet claim may be reused by many starts.
    pub(super) worker_generation: u64,
    /// Command receiver parked for a detached record restored from the
    /// durable roster. Keeping it alive buffers a later turn's steering or
    /// follow-up until reattachment; it is taken exactly once.
    pub(super) detached_commands: Option<mpsc::Receiver<WorkerCommand>>,
    /// Bounded diagnostic retained when a durable record could not be
    /// reattached (unknown state), so it fails closed visibly.
    pub(super) durable_diagnostic: Option<String>,
    /// Durable claim under which this worker may execute. It is stamped when a
    /// worker is spawned or reattached and checked before any restored worker
    /// starts, so a stale or duplicate session owner cannot run it twice.
    pub(super) claim: Option<DurableFleetClaim>,
}

/// Bounded durable snapshot of one session-owned worker.
///
/// Persisted beside the delegation directory so the owning session can
/// reconstruct its fleet after the owning run ends and after a process
/// restart. It never carries process-local handles: on load a record without a
/// live task surfaces as an explicit [`DelegatedAgentStatus::Detached`]
/// diagnostic instead of a silently-forgotten worker.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DurableFleetRecord {
    pub(super) agent_id: String,
    pub(super) agent_path: String,
    pub(super) parent_id: String,
    pub(super) depth: usize,
    pub(super) task_name: String,
    pub(super) display_task_name: Option<String>,
    pub(super) session_path: PathBuf,
    pub(super) status: DelegatedAgentStatus,
    pub(super) detached: bool,
    pub(super) created_at_ms: u64,
    pub(super) started_at_ms: Option<u64>,
    pub(super) completed_at_ms: Option<u64>,
    pub(super) turn_count: u64,
    pub(super) tool_call_count: u64,
    pub(super) usage: Usage,
    pub(super) usage_uncertain: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) usage_exposure: Option<UsageUncertaintyBound>,
    pub(super) cost: Option<Cost>,
    pub(super) cost_microdollars: Option<u64>,
    pub(super) deadline_at_ms: Option<u64>,
    pub(super) turn_limit: Option<u64>,
    pub(super) extension_principal: Option<String>,
    pub(super) extension_profile: Option<String>,
    pub(super) extension_idempotency_key: Option<String>,
    #[serde(default)]
    pub(super) extension_resource_owner: Option<String>,
    #[serde(default)]
    pub(super) extension_message_sha256: Option<String>,
    #[serde(default)]
    pub(super) extension_requested_policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) extension_fingerprint: Option<String>,
    pub(super) extension_policy: Option<ExtensionAgentSessionPolicy>,
    #[serde(default)]
    pub(super) resource_owner: Option<String>,
    #[serde(default)]
    pub(super) durable_diagnostic: Option<String>,
    #[serde(default)]
    pub(super) claim: Option<DurableFleetClaim>,
    #[serde(default)]
    pub(super) pending_messages: VecDeque<DirectedMessage>,
    #[serde(default)]
    pub(super) queued_follow_ups: VecDeque<QueuedFollowUp>,
    #[serde(default)]
    pub(super) pending_initial_task: Option<QueuedInitialTask>,
    #[serde(default)]
    pub(super) mailbox: VecDeque<DurableMailboxMessage>,
    #[serde(default)]
    pub(super) mailbox_delivery: Option<MailboxDeliveryPlan>,
}

impl Default for DurableFleetRecord {
    /// Base for fixtures: an unattached worker with no durable history.
    ///
    /// Fixtures spread this value (`..DurableFleetRecord::default()`) and state
    /// only the fields they mean, so a new durable field is added in exactly one
    /// place and cannot be silently missing from a fixture.
    fn default() -> Self {
        Self {
            agent_id: String::new(),
            agent_path: String::new(),
            parent_id: String::new(),
            depth: 0,
            task_name: String::new(),
            display_task_name: None,
            session_path: PathBuf::new(),
            status: DelegatedAgentStatus::Pending,
            detached: false,
            created_at_ms: 0,
            started_at_ms: None,
            completed_at_ms: None,
            turn_count: 0,
            tool_call_count: 0,
            usage: Usage::default(),
            usage_uncertain: false,
            usage_exposure: None,
            cost: None,
            cost_microdollars: None,
            deadline_at_ms: None,
            turn_limit: None,
            extension_principal: None,
            extension_profile: None,
            extension_idempotency_key: None,
            extension_resource_owner: None,
            extension_message_sha256: None,
            extension_requested_policy: None,
            extension_fingerprint: None,
            extension_policy: None,
            resource_owner: None,
            durable_diagnostic: None,
            claim: None,
            pending_messages: VecDeque::new(),
            queued_follow_ups: VecDeque::new(),
            pending_initial_task: None,
            mailbox: VecDeque::new(),
            mailbox_delivery: None,
        }
    }
}

/// Versioned durable fleet file written to the session-scoped delegation
/// directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DurableFleet {
    pub(super) version: u32,
    pub(super) root_session: PathBuf,
    pub(super) records: Vec<DurableFleetRecord>,
    #[serde(default = "default_mailbox_delivery_id")]
    pub(super) next_mailbox_delivery: u64,
    #[serde(default)]
    pub(super) root_mailbox: VecDeque<DurableMailboxMessage>,
    #[serde(default)]
    pub(super) root_mailbox_delivery: Option<MailboxDeliveryPlan>,
}

pub(super) fn default_mailbox_delivery_id() -> u64 {
    1
}

/// Fits every admitted fleet (at most 256 agents), including JSON's worst-case
/// six-byte escaping of both bounded text fields (status and diagnostic), plus
/// bounded identities, paths and policy metadata. Writers and readers share
/// this aggregate bound; admitted terminal summaries must not poison a fleet.
pub(super) const MAX_FLEET_ROSTER_BYTES: usize =
    256 * (12 * MAX_PROVENANCE_TEXT_BYTES + 64 * 1024) + 64 * 1024;

pub(super) const ROSTER_PROJECTION_BYTES: usize = 256 * 1024;

/// The roster is an index into the durable child sessions, not another copy
/// of every answer. Preserve full live/provenance output; only its restart
/// projection shares the remaining roster byte budget. Both native and extension
/// workers accumulate Completed/LimitReached text solely from TurnFinished, which
/// the agent emits only after append_assistant_turn_with_metadata succeeds.
/// Failure and approval diagnostics are metadata, never output-budget candidates.
pub(super) fn encode_durable_fleet(mut fleet: DurableFleet) -> io::Result<Vec<u8>> {
    let mut texts = Vec::new();
    for (index, record) in fleet.records.iter_mut().enumerate() {
        if let Some(text) = roster_status_text(&mut record.status) {
            if !text.is_empty() {
                texts.push((index, std::mem::take(text)));
            }
        }
    }
    let base_bytes = serde_json::to_vec(&fleet).map_err(io::Error::other)?.len();
    let available = ROSTER_PROJECTION_BYTES
        .saturating_sub(base_bytes)
        .max(texts.len() * json_text_bytes(ROSTER_OUTPUT_SUFFIX));
    let requested: usize = texts.iter().map(|(_, text)| json_text_bytes(text)).sum();
    let budget = available / texts.len().max(1);
    // Metadata still fails closed. Never turn a status into an unmarked empty
    // answer just to make an overfull roster fit.
    if base_bytes.saturating_add(available) > MAX_FLEET_ROSTER_BYTES
        || (requested > available && budget < json_text_bytes(ROSTER_OUTPUT_SUFFIX))
    {
        return Err(io::Error::other(
            "durable fleet metadata exceeded its bounded size",
        ));
    }
    for (index, text) in texts {
        *roster_status_text(&mut fleet.records[index].status).expect("saved status text") =
            if requested <= available {
                text
            } else {
                bound_roster_text(&text, budget)
            };
    }
    let encoded = serde_json::to_vec(&fleet).map_err(io::Error::other)?;
    debug_assert!(encoded.len() <= MAX_FLEET_ROSTER_BYTES);
    Ok(encoded)
}

pub(super) const ROSTER_OUTPUT_SUFFIX: &str =
    "\n...[truncated in fleet roster; inspect child session]";

pub(super) fn roster_status_text(status: &mut DelegatedAgentStatus) -> Option<&mut String> {
    match status {
        DelegatedAgentStatus::Completed { output }
        | DelegatedAgentStatus::LimitReached { output, .. } => Some(output),
        _ => None,
    }
}

// serde_json's escaped string content size (excluding the two quotes). A raw
// UTF-8 budget alone is insufficient: control bytes expand by up to six times.
pub(super) fn json_char_bytes(ch: char) -> usize {
    match ch {
        '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
        '\u{0}'..='\u{1f}' => 6,
        _ => ch.len_utf8(),
    }
}

pub(super) fn json_text_bytes(text: &str) -> usize {
    text.chars().map(json_char_bytes).sum()
}

pub(super) fn bound_roster_text(text: &str, budget: usize) -> String {
    if json_text_bytes(text) <= budget {
        return text.to_owned();
    }
    let mut remaining = budget - json_text_bytes(ROSTER_OUTPUT_SUFFIX);
    let mut end = 0;
    for ch in text.chars() {
        let size = json_char_bytes(ch);
        if size > remaining {
            break;
        }
        remaining -= size;
        end += ch.len_utf8();
    }
    format!("{}{ROSTER_OUTPUT_SUFFIX}", &text[..end])
}

// Version 2 pins per-worker routes; v1 readers must not inherit a wrong parent route.
pub(super) const FLEET_ROSTER_VERSION: u32 = 2;

pub(super) const FLEET_ROSTER_FILE: &str = "fleet.json";

/// Versioned durable claim that fences execution of one session's fleet to a
/// single live owner. A larger or malformed file fails closed instead of being
/// partially trusted.
pub(super) const FLEET_LEASE_VERSION: u32 = 1;

pub(super) const FLEET_LEASE_LIMIT: usize = 16 * 1024;

/// Bounded durable claim naming the owner that may execute a session's
/// workers.
///
/// `generation` is monotonic per durable store: every successful acquisition
/// takes the next generation, so a stale owner holding an older generation is
/// fenced even if it still has the roster in memory. `instance` is unique per
/// owning manager, so a duplicate session open cannot prove the current claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct DurableFleetClaim {
    pub(super) generation: u64,
    pub(super) instance: String,
}

/// On-disk claim written beside the roster by the manager that owns the
/// session's durable fleet.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DurableFleetLease {
    pub(super) version: u32,
    pub(super) root_session: PathBuf,
    pub(super) generation: u64,
    pub(super) instance: String,
    pub(super) claimed_at_ms: u64,
}

/// Exclusive owner of one session's durable fleet.
///
/// The claim is fenced twice: an owner-only advisory lock proves no *other live
/// process* owns the fleet, and the durable generation/instance pair proves
/// this manager still holds the claim it wrote. Both are required before a
/// restored worker may be started, so a stale process or a duplicate session
/// open fails closed instead of running the same worker twice.
pub(super) struct FleetLease {
    pub(super) file: std::fs::File,
    pub(super) lock_path: PathBuf,
    pub(super) lock_identity: secure_fs::PrivateLockIdentity,
    pub(super) claim_path: PathBuf,
    pub(super) claim: DurableFleetClaim,
    pub(super) root_session: PathBuf,
}

impl std::fmt::Debug for FleetLease {
    /// The lease owns an open lock file and a private lock identity, neither of
    /// which belongs in diagnostics. A manual impl keeps `unwrap_err`/`expect`
    /// usable on `Result<FleetLease, String>` without printing them.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FleetLease")
            .field("lock_path", &self.lock_path)
            .field("claim_path", &self.claim_path)
            .field("root_session", &self.root_session)
            .field("claim", &self.claim)
            .finish_non_exhaustive()
    }
}

/// Manager-local instance token. Unique per manager, ASCII, bounded.
pub(super) fn fleet_instance_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{:x}-{:x}-{:x}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed),
        timestamp_ms()
    )
}

/// Lease lock and claim paths, scoped to one root session so two different
/// sessions sharing a workspace delegation directory never contend.
pub(super) fn fleet_lease_paths(
    session_directory: &Path,
    root_session: &Path,
) -> (PathBuf, PathBuf) {
    let mut hasher = Sha256::new();
    hasher.update(root_session.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let stem = format!("fleet-{}", &digest[..16]);
    (
        session_directory.join(format!("{stem}.lock")),
        session_directory.join(format!("{stem}.lease")),
    )
}

pub(super) fn fleet_roster_path(session_directory: &Path, root_session: &Path) -> PathBuf {
    // Use exactly the lease's owner identity, not the shared workspace directory.
    fleet_lease_paths(session_directory, root_session)
        .0
        .with_extension("json")
}

pub(super) fn read_fleet_lease(claim_path: &Path) -> Option<DurableFleetLease> {
    let bytes = secure_fs::read_private_file_bounded(claim_path, FLEET_LEASE_LIMIT).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn lease_contention(error: &io::Error) -> bool {
    // `fs2` reports contention with the platform's lock error (EWOULDBLOCK on
    // Unix, ERROR_LOCK_VIOLATION on Windows), so compare against its own
    // contended error instead of assuming one `ErrorKind`.
    let contended = fs2::lock_contended_error();
    error.raw_os_error() == contended.raw_os_error()
        || matches!(error.kind(), io::ErrorKind::WouldBlock)
}

impl FleetLease {
    pub(super) fn try_acquire(
        session_directory: &Path,
        root_session: &Path,
    ) -> Result<Self, String> {
        let (lock_path, claim_path) = fleet_lease_paths(session_directory, root_session);
        let file = secure_fs::open_private_lock_file(&lock_path)
            .map_err(|error| format!("session fleet lease lock is unavailable: {error}"))?;
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => {}
            Err(error) if lease_contention(&error) => {
                return Err(match read_fleet_lease(&claim_path) {
                    Some(holder) => format!(
                        "another live session owner holds the durable fleet lease (instance {}, generation {})",
                        holder.instance, holder.generation
                    ),
                    None => "another live session owner holds the durable fleet lease".to_owned(),
                });
            }
            Err(error) => return Err(format!("session fleet lease lock is unavailable: {error}")),
        }
        let lock_identity = secure_fs::validate_private_lock_after_acquire(&lock_path, &file)
            .map_err(|error| format!("session fleet lease lock failed validation: {error}"))?;
        let existing = secure_fs::read_private_file_bounded(&claim_path, FLEET_LEASE_LIMIT).ok();
        let previous = existing
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<DurableFleetLease>(bytes).ok());
        let generation = previous
            .as_ref()
            .map(|previous| previous.generation.saturating_add(1))
            .unwrap_or(1);
        let instance = fleet_instance_token();
        let durable = DurableFleetLease {
            version: FLEET_LEASE_VERSION,
            root_session: root_session.to_path_buf(),
            generation,
            instance: instance.clone(),
            claimed_at_ms: u64::try_from(timestamp_ms()).unwrap_or(u64::MAX),
        };
        let encoded = serde_json::to_vec(&durable)
            .map_err(|error| format!("session fleet lease could not be encoded: {error}"))?;
        secure_fs::write_private_atomic_if_unchanged(
            &claim_path,
            existing.as_deref(),
            &encoded,
            FLEET_LEASE_LIMIT,
        )
        .map_err(|error| format!("session fleet lease could not be claimed: {error}"))?;
        Ok(Self {
            file,
            lock_path,
            lock_identity,
            claim_path,
            claim: DurableFleetClaim {
                generation,
                instance,
            },
            root_session: root_session.to_path_buf(),
        })
    }

    pub(super) fn claim(&self) -> DurableFleetClaim {
        self.claim.clone()
    }

    /// Re-proves the claim immediately before starting restored work. Any
    /// mismatch or unreadable claim fails closed rather than being trusted.
    pub(super) fn is_current(&self) -> Result<(), String> {
        let bytes = secure_fs::read_private_file_bounded(&self.claim_path, FLEET_LEASE_LIMIT)
            .map_err(|error| format!("session fleet lease could not be verified: {error}"))?;
        let current: DurableFleetLease = serde_json::from_slice(&bytes)
            .map_err(|error| format!("session fleet lease could not be verified: {error}"))?;
        if current.version != FLEET_LEASE_VERSION || current.root_session != self.root_session {
            return Err("session fleet lease was replaced by another durable owner".into());
        }
        if current.generation != self.claim.generation || current.instance != self.claim.instance {
            return Err(format!(
                "session fleet lease moved to a newer owner (instance {}, generation {})",
                current.instance, current.generation
            ));
        }
        Ok(())
    }
}

impl Drop for FleetLease {
    fn drop(&mut self) {
        let revalidated = secure_fs::revalidate_private_lock_before_release(
            &self.lock_path,
            &self.file,
            &self.lock_identity,
        );
        let unlocked = fs2::FileExt::unlock(&self.file);
        let _ = (revalidated, unlocked);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct QueueUsage {
    pub(super) messages: usize,
    pub(super) bytes: usize,
}

impl QueueUsage {
    pub(super) fn add(&mut self, bytes: usize) {
        self.messages = self.messages.saturating_add(1);
        self.bytes = self.bytes.saturating_add(bytes);
    }

    pub(super) fn add_usage(&mut self, other: Self) {
        self.messages = self.messages.saturating_add(other.messages);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    pub(super) fn remove(&mut self, usage: Self) {
        self.messages = self.messages.saturating_sub(usage.messages);
        self.bytes = self.bytes.saturating_sub(usage.bytes);
    }
}
