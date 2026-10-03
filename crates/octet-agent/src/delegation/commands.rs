//! Messages, spawn and follow-up requests, worker commands and the provenance journal.

use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct DirectedMessage {
    /// Cryptographically random host delivery identity, persisted until the
    /// child session records this exact envelope.
    #[serde(default)]
    pub(super) delivery_id: String,
    pub(super) from: String,
    pub(super) message: String,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct MailboxMessage {
    pub(super) kind: &'static str,
    pub(super) from: String,
    pub(super) task_name: Option<String>,
    pub(super) message: String,
    #[serde(skip)]
    pub(super) evictable: bool,
    #[serde(skip)]
    pub(super) continued: bool,
    #[serde(skip)]
    pub(super) leased: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DurableMailboxMessage {
    pub(super) kind: String,
    pub(super) from: String,
    pub(super) task_name: Option<String>,
    pub(super) message: String,
    pub(super) evictable: bool,
    pub(super) continued: bool,
    pub(super) leased: bool,
}

impl From<&MailboxMessage> for DurableMailboxMessage {
    fn from(message: &MailboxMessage) -> Self {
        Self {
            kind: message.kind.into(),
            from: message.from.clone(),
            task_name: message.task_name.clone(),
            message: message.message.clone(),
            evictable: message.evictable,
            continued: message.continued,
            leased: message.leased,
        }
    }
}

impl From<DurableMailboxMessage> for MailboxMessage {
    fn from(message: DurableMailboxMessage) -> Self {
        // Mailbox kinds are host-generated constants. Unknown persisted values
        // are rendered as a bounded diagnostic rather than becoming a trusted
        // static string.
        let kind = match message.kind.as_str() {
            "message" => "message",
            "task_status" => "task_status",
            _ => "diagnostic",
        };
        Self {
            kind,
            from: message.from,
            task_name: message.task_name,
            message: message.message,
            evictable: message.evictable,
            continued: message.continued,
            leased: message.leased,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) struct MailboxDeliveryPlan {
    pub(super) id: u64,
    pub(super) complete_messages: usize,
    pub(super) partial_bytes: usize,
    pub(super) touched_messages: usize,
}

pub(super) struct WaitOutput {
    pub(super) value: Value,
    pub(super) delivery_id: Option<u64>,
}

pub(super) struct SpawnRequest {
    pub(super) task_name: String,
    pub(super) display_task_name: Option<String>,
    pub(super) message: String,
    pub(super) extension_policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) extension_provenance: Option<ExtensionSpawnProvenance>,
}

pub(super) struct ExtensionSpawnProvenance {
    pub(super) parent_session_id: String,
    pub(super) principal: String,
    pub(super) resource_owner: String,
    pub(super) profile: Option<String>,
    pub(super) idempotency_key: String,
    pub(super) fingerprint: Option<String>,
}

pub(super) struct FollowUpRequest {
    pub(super) target: String,
    pub(super) message: String,
}

pub(super) struct WorkerCommand {
    pub(super) kind: WorkerCommandKind,
}

pub(super) struct WorkerStartup {
    pub(super) generation: u64,
    pub(super) identity: AgentIdentity,
    pub(super) session: Session,
    pub(super) commands: mpsc::Receiver<WorkerCommand>,
    pub(super) shutdown: crate::CancellationToken,
    pub(super) initial_permit: OwnedSemaphorePermit,
    pub(super) extension_policy: Option<ExtensionAgentSessionPolicy>,
    pub(super) deadline: Option<tokio::time::Instant>,
    pub(super) deadline_ms: Option<u64>,
}

/// Clears a record's process-local liveness flag when its worker task ends.
pub(super) struct WorkerLiveness {
    pub(super) manager: Arc<DelegationManager>,
    pub(super) id: String,
    pub(super) generation: u64,
}

/// One record this manager is about to start from its durable snapshot.
pub(super) struct ReattachPlan {
    pub(super) id: String,
    pub(super) session: Session,
    // A transcript with no undelivered task is settled without using a slot.
    pub(super) initial_permit: Option<OwnedSemaphorePermit>,
    pub(super) claim: DurableFleetClaim,
}

impl WorkerLiveness {
    pub(super) fn new(manager: &Arc<DelegationManager>, id: String, generation: u64) -> Self {
        Self {
            manager: Arc::clone(manager),
            id,
            generation,
        }
    }
}

impl Drop for WorkerLiveness {
    fn drop(&mut self) {
        self.manager.mark_worker_stopped(&self.id, self.generation);
    }
}

pub(super) struct ChildRunContext<'a> {
    pub(super) queued_delivery_ids: BTreeSet<String>,
    pub(super) identity: &'a AgentIdentity,
    pub(super) commands: &'a mut mpsc::Receiver<WorkerCommand>,
    pub(super) shutdown: &'a crate::CancellationToken,
    pub(super) extension_policy: Option<&'a ExtensionAgentSessionPolicy>,
    pub(super) deadline: Option<tokio::time::Instant>,
}

impl WorkerCommand {
    pub(super) fn message(message: DirectedMessage) -> Self {
        Self {
            kind: WorkerCommandKind::Message(message),
        }
    }

    pub(super) fn follow_up() -> Self {
        Self {
            kind: WorkerCommandKind::FollowUp,
        }
    }

    pub(super) fn shutdown() -> Self {
        Self {
            kind: WorkerCommandKind::Shutdown,
        }
    }
}

pub(super) enum WorkerCommandKind {
    Message(DirectedMessage),
    FollowUp,
    Shutdown,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub(super) enum ProvenanceEvent<'a> {
    TeamStarted {
        timestamp_ms: u128,
        root_session: &'a Path,
        limits: &'a DelegationLimits,
        mode: &'static str,
    },
    AgentSpawned {
        timestamp_ms: u128,
        agent_id: &'a str,
        agent_path: &'a str,
        parent_id: &'a str,
        task_name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        display_task_name: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_parent_session_id: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_principal: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_resource_owner: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_profile: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_idempotency_key: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        extension_fingerprint: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        task: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<&'a Path>,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_reference: Option<&'a str>,
        effective_tool_policy: &'a EffectiveToolPolicy,
        orchestration_provenance: &'a DelegationOrchestrationProvenance,
    },
    AgentStatus {
        timestamp_ms: u128,
        agent_id: &'a str,
        status: &'a DelegatedAgentStatus,
    },
    Message {
        timestamp_ms: u128,
        from: &'a str,
        to: &'a str,
        kind: &'a str,
        message: &'a str,
    },
    InterruptRequested {
        timestamp_ms: u128,
        from: &'a str,
        to: &'a str,
    },
    /// Explicit session-scoped detachment boundary: the owning run ended but
    /// these workers survive it. This replaces run-scoped retirement as the
    /// default, keeping the end-of-run boundary visible instead of a silent
    /// vanish.
    RunDetached {
        timestamp_ms: u128,
        agent_ids: Vec<String>,
    },
    /// Explicit session-scoped reattachment boundary: a later owning run
    /// resumed these workers from the durable record.
    RunReattached {
        timestamp_ms: u128,
        agent_ids: Vec<String>,
    },
    /// A worker parked at the approval boundary was rediscovered by
    /// reattachment and deliberately not resumed.
    ReattachParked {
        timestamp_ms: u128,
        agent_id: &'a str,
        reason: &'a str,
    },
    /// A worker could not be reattached, with the bounded reason. The record
    /// keeps its durable state and the reason instead of disappearing.
    ReattachRefused {
        timestamp_ms: u128,
        agent_id: &'a str,
        reason: &'a str,
    },
    TeamShutdown {
        timestamp_ms: u128,
    },
}

pub(super) struct ProvenanceJournal {
    pub(super) file: Mutex<File>,
}

impl ProvenanceJournal {
    pub(super) fn create(directory: &secure_fs::PrivateDirectory) -> Result<Self, SecureFileError> {
        let path = directory.path().join("provenance.jsonl");
        let file = directory.create_regular_file_for_append(&path)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }

    pub(super) fn append(&self, event: &ProvenanceEvent<'_>) -> io::Result<()> {
        let encoded = serde_json::to_vec(event).map_err(io::Error::other)?;
        self.append_encoded(&encoded)
    }

    pub(super) fn append_encoded(&self, encoded: &[u8]) -> io::Result<()> {
        let mut file = self
            .file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        file.write_all(encoded)?;
        file.write_all(b"\n")?;
        file.sync_data()
    }
}
