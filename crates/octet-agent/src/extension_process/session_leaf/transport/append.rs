use super::*;
use crate::session::{
    Entry, EntryId, EntryMetadata, EntryValue, ExtensionEntryMetadata, ExtensionMetadataProvenance,
    Session, MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES,
};

impl ExtensionProcess {
    /// Commit an admitted grant-less append and queue its native counted receipt.
    pub fn append_session_entry_request(
        &self,
        request_id: &ExtensionRequestId,
        generation: u64,
        session: &mut Session,
        entry_type: &str,
        data: Value,
    ) -> Result<Value, (ExtensionRequestFailure, String)> {
        self.append_entry_request(request_id, generation, session, entry_type, data, None)
    }

    pub(crate) fn validate_run_session_append(
        &self,
        id: &ExtensionRequestId,
        generation: u64,
        owner: &str,
    ) -> Result<(), (ExtensionRequestFailure, String)> {
        let connection = read_std_lock(&self.inner.connection).clone();
        let pending = lock_std_mutex(&connection.pending);
        let children = lock_std_mutex(&connection.child_requests);
        let valid = generation == connection.generation
            && children.get(id).is_some_and(|child| {
                pending.get(&child.parent_request_id).is_some_and(|parent| {
                    // Pi starts shortcut handlers without an idle gate, using the
                    // same append service as commands (packages/coding-agent/src/
                    // modes/interactive/interactive-mode.ts:2382-2394).
                    matches!(
                        parent.method.as_str(),
                        methods::COMMAND_EXECUTE | methods::SHORTCUT_EXECUTE
                    ) && parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE
                        && parent
                            .resource_owner
                            .as_ref()
                            .is_some_and(|o| o.session_id == owner)
                })
            });
        if valid {
            Ok(())
        } else {
            Err((
                ExtensionRequestFailure::NotForegroundOwner,
                "active response append requires its live command or shortcut owner".into(),
            ))
        }
    }

    pub(crate) fn append_run_session_entry_request(
        &self,
        id: &ExtensionRequestId,
        generation: u64,
        session: &mut Session,
        entry_type: &str,
        data: Value,
        start: usize,
    ) -> Result<Value, (ExtensionRequestFailure, String)> {
        self.validate_run_session_append(id, generation, &session.resource_owner_key())?;
        self.append_entry_request(id, generation, session, entry_type, data, Some(start))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_entry_request(
        &self,
        request_id: &ExtensionRequestId,
        generation: u64,
        session: &mut Session,
        entry_type: &str,
        data: Value,
        run_start: Option<usize>,
    ) -> Result<Value, (ExtensionRequestFailure, String)> {
        let refused = || {
            (
                ExtensionRequestFailure::NotForegroundOwner,
                "session append parent, owner or generation is unavailable".into(),
            )
        };
        let bounds = || {
            (
                ExtensionRequestFailure::BoundsExceeded,
                "session append receipt exceeds transport bounds".into(),
            )
        };
        let invalid = |detail: String| (ExtensionRequestFailure::InvalidRequest, detail);
        if entry_type.trim().is_empty() || entry_type.chars().any(char::is_control) {
            return Err(invalid(
                "entry_type must be nonempty without control characters".into(),
            ));
        }
        if entry_type.len() > MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES {
            return Err(bounds());
        }
        let envelope = json!({"entry_type":entry_type,"data":data});
        if session_snapshot_bytes(&envelope, MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES).is_err() {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                format!(
                    "entry data exceeds {MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES} stored bytes"
                ),
            ));
        }
        let connection = read_std_lock(&self.inner.connection).clone();
        if generation != connection.generation
            || !{
                let protocol = read_std_lock(&connection.protocol);
                owner_routes_enabled(&protocol) && protocol.supports(APPEND_RECEIPTS)
            }
        {
            return Err(refused());
        }
        let permit = connection.writer.try_reserve().map_err(|_| bounds())?;
        let pending = lock_std_mutex(&connection.pending);
        let owners = lock_std_mutex(&connection.issued_resource_owners);
        let mut children = lock_std_mutex(&connection.child_requests);
        let child = children.get(request_id).ok_or_else(refused)?;
        let parent_id = child.parent_request_id;
        let parent = pending
            .get(&parent_id)
            .filter(|parent| parent.terminal.load(Ordering::Acquire) == REQUEST_ACTIVE)
            .ok_or_else(refused)?;
        let owner = parent.resource_owner.as_ref().ok_or_else(refused)?;
        if connection.closed.load(Ordering::Acquire)
            || connection.draining.load(Ordering::Acquire)
            || child.response_state.state.load(Ordering::Acquire) != CHILD_ACTIVE
            || !owners.contains(owner)
            || owner.session_id != session.resource_owner_key()
            || owner.extension_instance_id != self.extension_instance_id()
            || owner.process_generation != generation
        {
            return Err(refused());
        }
        let mut store = lock_std_mutex(&connection.session_leaf.transport);
        let pin = store.parents.get(&parent_id).ok_or_else(refused)?;
        let session_head = session.head_ref().map(|id| id.0.clone());
        if pin.owner != *owner || pin.preparation.is_some() {
            return Err(refused());
        }
        // Chain only host config writes or the owning Run's known suffix.
        // Branch divergence, foreign writes and older source changes refuse.
        // Pi commits commands during streaming synchronously rather than at idle:
        // packages/coding-agent/src/core/agent-session.ts:2120-2129,3487-3492.
        let gap = append_gap(
            session,
            pin.head.as_deref(),
            session_head.as_deref(),
            run_start,
        )
        .ok_or_else(refused)?;
        let previous_head = pin.head.clone();
        let entry_parent = session_head.clone();
        let previous_revision = pin.revision;
        let namespace = &self.descriptor().manifest.name;
        let candidate = Entry {
            id: EntryId("9".repeat(20)),
            parent: entry_parent.clone().map(EntryId),
            timestamp_unix_ms: Some(u64::MAX),
            value: EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            metadata: Some(EntryMetadata {
                extension_metadata: BTreeMap::from([(
                    namespace.clone(),
                    ExtensionEntryMetadata {
                        public: false,
                        value: envelope,
                        provenance: ExtensionMetadataProvenance {
                            extension: namespace.clone(),
                            process_generation: Some(generation),
                        },
                    },
                )]),
                ..Default::default()
            }),
        };
        let candidate_receipt = receipt::Receipt {
            result: SessionLeafAppendResult {
                entry_id: candidate.id.0.clone(),
                head: candidate.id.0.clone(),
                successor: None,
            },
            entry: visible::VisibleEntry {
                entry: &candidate,
                namespace,
                usage: None,
            },
            previous_revision,
            view_revision: MAX_SAFE,
            previous_head: previous_head.clone(),
            gap: visible_gap(&gap, namespace, session),
        };
        let max_bytes = connection.max_message_bytes();
        if validate_session_snapshot_size(
            &json!({"jsonrpc":"2.0","id":request_id,"result":candidate_receipt}),
            max_bytes.saturating_sub(1),
        )
        .is_err()
        {
            return Err(bounds());
        }
        let view_revision = store.next_revision().map_err(|_| bounds())?;
        let gap_entries: Vec<Entry> = gap.into_iter().cloned().collect();
        let entry_id = session
            .append_extension_entry(namespace, Some(generation), entry_type, data)
            .map_err(|error| {
                invalid(format!("the extension session entry was refused: {error}"))
            })?;
        let entry = session.entry(&entry_id).expect("committed session entry");
        let result = serde_json::to_value(receipt::Receipt {
            result: SessionLeafAppendResult {
                entry_id: entry_id.0.clone(),
                head: entry_id.0.clone(),
                successor: None,
            },
            entry: visible::VisibleEntry {
                entry,
                namespace,
                usage: None,
            },
            previous_revision,
            view_revision,
            previous_head,
            gap: visible_gap(&gap_entries, namespace, session),
        })
        .expect("session entry receipt is JSON");
        let mut line =
            serde_json::to_vec(&json!({"jsonrpc":"2.0","id":request_id,"result":result}))
                .expect("session entry response is JSON");
        line.push(b'\n');
        debug_assert!(line.len() <= max_bytes);
        let pin = store
            .parents
            .get_mut(&parent_id)
            .expect("live session parent");
        pin.revision = view_revision;
        pin.head = Some(entry_id.0);
        let response = Arc::clone(&child.response_state);
        response.state.store(CHILD_SETTLED, Ordering::Release);
        children.remove(request_id);
        response.changed.notify_waiters();
        permit.send(WriterFrame {
            line,
            state: Arc::new(AtomicU8::new(FRAME_QUEUED)),
            completion: None,
            bus_delivery: None,
        });
        let owner = owner.clone();
        drop(store);
        drop(children);
        drop(owners);
        drop(pending);
        if run_start.is_some() && self.supports_feature(EXTENSION_FEATURE_TRANSCRIPT_RENDER) {
            if let Some(entry) = transcript_private_entry(entry, namespace) {
                let _ = self
                    .inner
                    .events
                    .send(ExtensionEvent::TranscriptEntryCommitted {
                        namespace: namespace.clone(),
                        owner: owner.clone(),
                        entry,
                    });
                connection.remote_ui.wake();
            }
        }
        Ok(result)
    }
}

/// Maximum known native entries a parent receipt may chain past its pin.
const MAX_APPEND_GAP: usize = 16;

/// Entries strictly after `pinned` up to `head`, oldest first, when that span
/// is a bounded linear chain of host config or active-Run writes.
fn append_gap<'s>(
    session: &'s Session,
    pinned: Option<&str>,
    head: Option<&str>,
    run_start: Option<usize>,
) -> Option<Vec<&'s Entry>> {
    let mut gap = Vec::new();
    let mut cursor = head;
    while cursor != pinned {
        if gap.len() >= MAX_APPEND_GAP {
            return None;
        }
        let entry = session.entry(&EntryId(cursor?.to_owned()))?;
        let host_config = matches!(entry.value, EntryValue::Config { .. })
            && entry
                .metadata
                .as_ref()
                .is_none_or(|metadata| metadata.extension_metadata.is_empty());
        // The Run owns every write since admission. Permit only its known
        // linear suffix, never an older divergence, checkout or foreign writer.
        let run_owned = run_start.is_some_and(|start| {
            session.entries()[start..]
                .iter()
                .rev()
                .take(MAX_APPEND_GAP)
                .any(|known| known.id == entry.id)
        });
        if !host_config && !run_owned {
            return None;
        }
        gap.push(entry);
        cursor = entry.parent.as_ref().map(|id| id.0.as_str());
    }
    gap.reverse();
    Some(gap)
}

fn visible_gap<'a>(
    gap: &'a [impl std::borrow::Borrow<Entry>],
    namespace: &'a str,
    session: &'a Session,
) -> Vec<visible::VisibleEntry<'a>> {
    gap.iter()
        .map(|entry| visible::VisibleEntry {
            entry: entry.borrow(),
            namespace,
            usage: if matches!(entry.borrow().value, EntryValue::Message(octet_ai::Message::Assistant(_))) {
                session.usage_records()[session.run_appends.as_ref().expect("only Run entries can contain assistant gaps").usage_start..].iter().rev().find(|record| matches!(&record.kind, crate::session::UsageRecordKind::AssistantTurn { assistant } if assistant == &entry.borrow().id))
            } else { None },
        })
        .collect()
}
