use super::*;
use crate::session::{
    Entry, EntryId, EntryMetadata, EntryValue, ExtensionEntryMetadata, ExtensionMetadataProvenance,
    Session, MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES,
};

impl ExtensionProcess {
    /// Commit an admitted grant-less append and queue its native counted receipt.
    ///
    /// Requires `session_append_receipts_v1` and a live, owner-matched parent
    /// pinned to the session's current head. Writer capacity and receipt bounds
    /// are checked before the durable write; cancellation is fenced through ACK
    /// enqueue. Success has already settled the child: callers must not respond
    /// again. Failure does not enqueue a reply; callers supply the typed refusal.
    pub fn append_session_entry_request(
        &self,
        request_id: &ExtensionRequestId,
        generation: u64,
        session: &mut Session,
        entry_type: &str,
        data: Value,
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
        let previous_head = session.head_ref().map(|id| id.0.clone());
        if pin.owner != *owner || pin.preparation.is_some() || pin.head != previous_head {
            return Err(refused());
        }
        let previous_revision = pin.revision;
        let namespace = &self.descriptor().manifest.name;
        let candidate = Entry {
            id: EntryId("9".repeat(20)),
            parent: previous_head.clone().map(EntryId),
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
        Ok(result)
    }
}
