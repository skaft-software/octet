//! Delegated (worker) sessions: provenance, export and live inspection.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DelegatedSessionFingerprint {
    pub(super) len: u64,
    pub(super) modified: SystemTime,
    #[cfg(unix)]
    pub(super) device: u64,
    #[cfg(unix)]
    pub(super) inode: u64,
}

impl DelegatedSessionFingerprint {
    pub(super) fn from_metadata(metadata: &std::fs::Metadata) -> Result<Self, ServiceError> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt as _;

        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().map_err(|_| ServiceError::InvalidSeed)?,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }

    pub(super) fn same_file_as(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.device == other.device && self.inode == other.inode
        }
        #[cfg(not(unix))]
        {
            let _ = other;
            false
        }
    }
}

#[derive(Default)]
pub(super) struct DelegatedSessionProvenance {
    pub(super) display_task_name: Option<String>,
    pub(super) parent_session_id: Option<String>,
    pub(super) extension_principal: Option<String>,
    pub(super) extension_resource_owner: Option<String>,
}

pub(super) fn is_lower_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn is_extension_delegation_principal(value: &str) -> bool {
    let Some((name, digest)) = value.split_once("@sha256:") else {
        return false;
    };
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        && is_lower_hex_digest(digest)
}

pub(super) fn is_extension_resource_owner(value: &str) -> bool {
    value
        .strip_prefix("session-")
        .is_some_and(is_lower_hex_digest)
}

pub(super) fn delegated_session_provenance(
    team: &Path,
    child: &Path,
) -> DelegatedSessionProvenance {
    let Ok(file) =
        octet_agent::secure_fs::open_private_file_for_read(&team.join("provenance.jsonl"))
    else {
        return DelegatedSessionProvenance::default();
    };
    let mut reader = BufReader::new(file);
    let Some(expected_reference) = octet_agent::delegated_session_reference(child) else {
        return DelegatedSessionProvenance::default();
    };
    let mut result = DelegatedSessionProvenance::default();
    for _ in 0..MAX_DELEGATION_PROVENANCE_RECORDS {
        let mut line = Vec::new();
        loop {
            let Ok(available) = reader.fill_buf() else {
                return DelegatedSessionProvenance::default();
            };
            if available.is_empty() {
                break;
            }
            let take = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if line.len().saturating_add(take) > MAX_DELEGATION_PROVENANCE_LINE_BYTES {
                return DelegatedSessionProvenance::default();
            }
            line.extend_from_slice(&available[..take]);
            reader.consume(take);
            if line.last() == Some(&b'\n') {
                break;
            }
        }
        if line.is_empty() {
            break;
        }
        let Ok(record) = serde_json::from_slice::<serde_json::Value>(&line) else {
            return DelegatedSessionProvenance::default();
        };
        if record.get("event").and_then(serde_json::Value::as_str) != Some("agent_spawned") {
            continue;
        }
        let matches_child = record
            .get("session_reference")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reference| reference == expected_reference);
        if matches_child {
            result.display_task_name = record
                .get("display_task_name")
                .and_then(serde_json::Value::as_str)
                .filter(|name| {
                    !name.is_empty()
                        && name.len() <= 48
                        && name.bytes().all(|byte| {
                            byte.is_ascii_lowercase()
                                || byte.is_ascii_digit()
                                || matches!(byte, b'_' | b'-')
                        })
                })
                .map(str::to_owned);
            result.parent_session_id = record
                .get("extension_parent_session_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| SessionId::new(*id).is_ok())
                .map(str::to_owned);
            result.extension_principal = record
                .get("extension_principal")
                .and_then(serde_json::Value::as_str)
                .filter(|principal| is_extension_delegation_principal(principal))
                .map(str::to_owned);
            result.extension_resource_owner = record
                .get("extension_resource_owner")
                .and_then(serde_json::Value::as_str)
                .filter(|owner| is_extension_resource_owner(owner))
                .map(str::to_owned);
            break;
        }
    }
    result
}

pub(super) struct DelegatedSessionContext {
    pub(super) project_id: ProjectId,
    pub(super) parent_session_id: SessionId,
    pub(super) config: Config,
    pub(super) session: Session,
    pub(super) meta: SessionMeta,
    pub(super) fingerprint: DelegatedSessionFingerprint,
}

pub(super) fn export_session_bytes(
    sessions: &SessionStore,
    session_id: &SessionId,
    serve_state_dir: &Path,
    max_bytes: usize,
) -> Result<bytes::Bytes, ServiceError> {
    sessions
        .path_by_id(session_id.as_str())
        .map_err(|_| ServiceError::NotFound)?;
    let serve_state_dir = serve_state_dir
        .canonicalize()
        .map_err(|_| ServiceError::Internal)?;
    let temporary = tempfile::Builder::new()
        .prefix(".session-export-")
        .tempdir_in(&serve_state_dir)
        .map_err(|_| ServiceError::Internal)?;
    let destination = temporary.path().join("session.json");
    let report = crate::session_commands::export_portable(
        sessions,
        session_id.as_str(),
        Some(destination),
        temporary.path(),
        false,
        false,
    )
    .map_err(|_| ServiceError::Internal)?;
    if report.included_secrets {
        return Err(ServiceError::Internal);
    }
    let bytes =
        match octet_agent::secure_fs::read_regular_file_bounded(&report.destination, max_bytes) {
            Ok(bytes) => bytes,
            Err(octet_agent::secure_fs::SecureFileError::TooLarge { .. }) => {
                return Err(ServiceError::PayloadTooLarge);
            }
            Err(_) => return Err(ServiceError::Internal),
        };
    Ok(bytes::Bytes::from(bytes))
}

pub(super) fn export_delegated_session_bytes(
    path: &Path,
    fingerprint: DelegatedSessionFingerprint,
    session_id: &SessionId,
    workspace: &Path,
    serve_state_dir: &Path,
    max_bytes: usize,
) -> Result<bytes::Bytes, ServiceError> {
    let source = octet_agent::secure_fs::open_private_file_for_read(path)
        .map_err(|_| ServiceError::CorruptResource)?;
    fs2::FileExt::lock_shared(&source).map_err(|_| ServiceError::CorruptResource)?;
    let snapshot = (|| {
        let current = DelegatedSessionFingerprint::from_metadata(
            &source
                .metadata()
                .map_err(|_| ServiceError::CorruptResource)?,
        )?;
        if !fingerprint.same_file_as(&current) {
            return Err(ServiceError::CorruptResource);
        }
        if current.len > max_bytes as u64 {
            return Err(ServiceError::PayloadTooLarge);
        }
        let mut transcript = Vec::with_capacity(current.len as usize);
        let mut reader = (&source).take(max_bytes as u64 + 1);
        reader
            .read_to_end(&mut transcript)
            .map_err(|_| ServiceError::CorruptResource)?;
        let after = DelegatedSessionFingerprint::from_metadata(
            &source
                .metadata()
                .map_err(|_| ServiceError::CorruptResource)?,
        )?;
        if !current.same_file_as(&after)
            || current.len != after.len
            || transcript.len() as u64 != current.len
        {
            return Err(ServiceError::CorruptResource);
        }
        if transcript.len() > max_bytes {
            return Err(ServiceError::PayloadTooLarge);
        }
        Ok(transcript)
    })();
    let unlocked = fs2::FileExt::unlock(&source);
    if unlocked.is_err() {
        return Err(ServiceError::CorruptResource);
    }
    let transcript = snapshot?;

    let temporary = tempfile::Builder::new()
        .prefix(".delegated-session-export-")
        .tempdir_in(serve_state_dir)
        .map_err(|_| ServiceError::Internal)?;
    let sessions = SessionStore::new(temporary.path(), workspace);
    octet_agent::secure_fs::create_private_directory_all(sessions.dir())
        .map_err(|_| ServiceError::Internal)?;
    // This is an already-authorized, read-only snapshot, not a launchable
    // worker. A delegated handle makes SessionStore consult the durable roster,
    // which intentionally does not exist in this temporary export store.
    let copied_id = SessionId::new("delegated-export").map_err(|_| ServiceError::Internal)?;
    let copied_path = sessions.dir().join(format!("{}.jsonl", copied_id.as_str()));
    let mut copied = octet_agent::secure_fs::create_regular_file_for_append(&copied_path)
        .map_err(|_| ServiceError::Internal)?;
    copied
        .write_all(&transcript)
        .and_then(|()| copied.sync_all())
        .map_err(|_| ServiceError::Internal)?;
    drop(copied);
    let exported = export_session_bytes(&sessions, &copied_id, serve_state_dir, max_bytes)?;
    let mut package: serde_json::Value =
        serde_json::from_slice(&exported).map_err(|_| ServiceError::Internal)?;
    // Restore only the host-generated, path-free identity after the ordinary
    // portable exporter has validated and redacted the entire snapshot.
    package["source_id"] = serde_json::Value::String(session_id.as_str().to_owned());
    let bytes = serde_json::to_vec_pretty(&package).map_err(|_| ServiceError::Internal)?;
    if bytes.len() > max_bytes {
        return Err(ServiceError::PayloadTooLarge);
    }
    Ok(bytes::Bytes::from(bytes))
}

#[derive(Clone)]
pub(super) struct DelegatedInspectionRefresh {
    pub(super) path: PathBuf,
    pub(super) workspace: PathBuf,
    pub(super) project_id: ProjectId,
    pub(super) model: ModelSelection,
    pub(super) generation: u64,
    pub(super) meta: SessionMeta,
}

impl DelegatedInspectionRefresh {
    pub(super) fn load(
        &self,
        previous: DelegatedSessionFingerprint,
    ) -> Result<Option<(DelegatedSessionFingerprint, SessionSeed)>, ServiceError> {
        let file = octet_agent::secure_fs::open_private_file_for_read(&self.path)
            .map_err(|_| ServiceError::CorruptResource)?;
        let metadata = file.metadata().map_err(|_| ServiceError::CorruptResource)?;
        let fingerprint = DelegatedSessionFingerprint::from_metadata(&metadata)?;
        if !previous.same_file_as(&fingerprint) {
            return Err(ServiceError::CorruptResource);
        }
        if previous == fingerprint {
            return Ok(None);
        }
        let session = Session::open_read_only_with_file(self.path.clone(), file)
            .map_err(|_| ServiceError::CorruptResource)?;
        let mut meta = self.meta.clone();
        meta.modified = fingerprint.modified;
        let session_id = SessionId::new(meta.id.clone()).map_err(|_| ServiceError::InvalidSeed)?;
        let mut seed = seed_from_session(
            &session,
            session_id,
            SessionSeedOptions {
                workspace: &self.workspace,
                project_id: Some(self.project_id.clone()),
                model: self.model.clone(),
                authority: AuthorityProfile::ReadOnly,
                generation: self.generation,
                meta: Some(meta),
                attachment_store: None,
                resource_store: None,
            },
        )?;
        seed.summary.live_state = SessionLiveState::Locked;
        seed.summary.owner = ActorOwnerState::ExternallyLocked;
        seed.snapshot.live_state = SessionLiveState::Locked;
        Ok(Some((fingerprint, seed)))
    }
}

pub(super) const MAX_DELEGATED_INSPECTION_EVENTS: usize = 256;

pub(super) fn delegated_inspection_events(
    previous: &SessionSeed,
    next: &SessionSeed,
) -> Option<VecDeque<TimestampedEvent>> {
    let timestamp = now_ms();
    let mut payloads = Vec::new();
    let known_branches = previous
        .snapshot
        .branches
        .entries
        .iter()
        .map(|entry| entry.entry_id.clone())
        .collect::<BTreeSet<_>>();
    let appended = next
        .snapshot
        .branches
        .entries
        .iter()
        .filter(|entry| !known_branches.contains(&entry.entry_id))
        .cloned()
        .collect::<Vec<_>>();
    for entries in appended.chunks(MAX_BRANCH_DELTA_ENTRIES) {
        payloads.push(EventPayload::SessionBranchEntriesAppended {
            entries: entries.to_vec(),
        });
    }
    if previous.snapshot.durable_head != next.snapshot.durable_head {
        payloads.push(EventPayload::SessionDurableHeadChanged {
            durable_entry_id: next.snapshot.durable_head.clone(),
        });
    }

    let previous_items = previous
        .snapshot
        .items
        .iter()
        .map(|item| (&item.id, item))
        .collect::<BTreeMap<_, _>>();
    for item in &next.snapshot.items {
        if previous_items
            .get(&item.id)
            .is_none_or(|previous| *previous != item)
        {
            payloads.push(EventPayload::ItemCommitted { item: item.clone() });
        }
    }
    let previous_sources = previous
        .snapshot
        .sources
        .iter()
        .map(|source| (&source.id, source))
        .collect::<BTreeMap<_, _>>();
    for source in &next.snapshot.sources {
        if previous_sources
            .get(&source.id)
            .is_none_or(|previous| *previous != source)
        {
            payloads.push(EventPayload::SourceUpserted {
                source: source.clone(),
            });
        }
    }
    let previous_artifacts = previous
        .snapshot
        .artifacts
        .iter()
        .map(|artifact| (&artifact.id, artifact))
        .collect::<BTreeMap<_, _>>();
    for artifact in &next.snapshot.artifacts {
        if previous_artifacts
            .get(&artifact.id)
            .is_none_or(|previous| *previous != artifact)
        {
            payloads.push(EventPayload::ArtifactUpserted {
                artifact: artifact.clone(),
            });
        }
    }
    if payloads.len() > MAX_DELEGATED_INSPECTION_EVENTS {
        return None;
    }
    Some(
        payloads
            .into_iter()
            .map(|payload| TimestampedEvent::new(timestamp, payload))
            .collect(),
    )
}

pub(super) struct DelegatedInspection {
    pub(super) refresh: DelegatedInspectionRefresh,
    pub(super) fingerprint: DelegatedSessionFingerprint,
    pub(super) projection: SessionSeed,
}
