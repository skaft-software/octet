//! Conversation topology and branch projection.
//!
//! This boundary owns durable entry relationships, bounded branch deltas, and
//! attachment identity recovery used while exposing a conversation through
//! Serve. It intentionally does not own run execution or session selection.

use super::*;

pub(super) fn branch_graph(session: &Session) -> Result<SessionBranchGraph, ServiceError> {
    let all_entries = session.entries();
    let head = session.head();
    let mut selected_indices = (all_entries
        .len()
        .saturating_sub(MAX_PROJECTED_BRANCH_ENTRIES)
        ..all_entries.len())
        .collect::<Vec<_>>();
    if let Some(head) = head.as_ref() {
        let head_index = all_entries
            .iter()
            .position(|entry| &entry.id == head)
            .ok_or(ServiceError::InvalidSeed)?;
        if !selected_indices.contains(&head_index) {
            if selected_indices.len() == MAX_PROJECTED_BRANCH_ENTRIES {
                selected_indices.remove(0);
            }
            selected_indices.push(head_index);
            selected_indices.sort_unstable();
        }
    }
    Ok(SessionBranchGraph {
        head: head
            .map(|head| DurableEntryId::new(head.0))
            .transpose()
            .map_err(|_| ServiceError::InvalidSeed)?,
        entries: selected_indices
            .iter()
            .map(|index| project_branch_entry(&all_entries[*index]))
            .collect::<Result<_, _>>()?,
        truncated: selected_indices.len() < all_entries.len(),
    })
}

pub(super) fn branch_delta_events(
    session: &Session,
    start: usize,
) -> Result<Vec<TimestampedEvent>, ServiceError> {
    let entries = session.entries();
    if start > entries.len() {
        return Err(ServiceError::InvalidSeed);
    }
    let mut events = entries[start..]
        .chunks(MAX_BRANCH_DELTA_ENTRIES)
        .map(|chunk| {
            Ok(event(EventPayload::SessionBranchEntriesAppended {
                entries: chunk
                    .iter()
                    .map(project_branch_entry)
                    .collect::<Result<_, _>>()?,
            }))
        })
        .collect::<Result<Vec<_>, ServiceError>>()?;
    let durable_entry_id = session
        .head()
        .map(|head| DurableEntryId::new(head.0))
        .transpose()
        .map_err(|_| ServiceError::Internal)?;
    events.push(event(EventPayload::SessionDurableHeadChanged {
        durable_entry_id,
    }));
    Ok(events)
}

pub(super) fn project_branch_entry(entry: &Entry) -> Result<SessionBranchEntry, ServiceError> {
    let kind = if is_local_synthetic_assistant(entry) {
        SessionBranchEntryKind::Internal
    } else {
        match &entry.value {
            EntryValue::Message(Message::User(_)) => SessionBranchEntryKind::UserMessage,
            EntryValue::Message(Message::Assistant(_)) => SessionBranchEntryKind::AssistantMessage,
            EntryValue::Compaction { .. } => SessionBranchEntryKind::Compaction,
            _ => SessionBranchEntryKind::Internal,
        }
    };
    Ok(SessionBranchEntry {
        entry_id: DurableEntryId::new(entry.id.0.clone()).map_err(|_| ServiceError::InvalidSeed)?,
        parent_entry_id: entry
            .parent
            .as_ref()
            .map(|parent| DurableEntryId::new(parent.0.clone()))
            .transpose()
            .map_err(|_| ServiceError::InvalidSeed)?,
        checkoutable: kind != SessionBranchEntryKind::Internal,
        kind,
        label: branch_entry_label(entry),
    })
}

pub(super) fn branch_entry_label(entry: &Entry) -> String {
    if is_local_synthetic_assistant(entry) {
        return "Internal session state".into();
    }
    let candidate = match &entry.value {
        EntryValue::Message(Message::User(message)) => entry
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.display_text.as_deref())
            .or_else(|| {
                message.content.iter().find_map(|part| match part {
                    UserPart::Text(text) => Some(text.as_str()),
                    UserPart::Media(_) | UserPart::ToolResult(_) => None,
                })
            })
            .unwrap_or("User input"),
        EntryValue::Message(Message::Assistant(message)) => message
            .content
            .iter()
            .find_map(|part| match part {
                AssistantPart::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .unwrap_or("Assistant response"),
        EntryValue::Config { .. } => "Internal session state",
        EntryValue::Compaction { .. } => "Compaction",
        EntryValue::ResponsesTurn { .. }
        | EntryValue::ResponsesCompaction { .. }
        | EntryValue::PromptTemplateSelected { .. }
        | EntryValue::SkillActivated { .. }
        | EntryValue::SkillResourceRead { .. }
        | EntryValue::SkillDeactivated { .. } => "Internal session state",
    };
    let first_line = candidate.lines().find(|line| !line.trim().is_empty());
    bounded_single_line_text(first_line.unwrap_or("Session entry"), 256)
}

pub(super) fn attachment_refs_for_entry(
    entry: &Entry,
    attachment_store: Option<&AttachmentStore>,
    session_id: &SessionId,
    pending: &mut VecDeque<Vec<AttachmentRef>>,
) -> Result<Vec<AttachmentRef>, ServiceError> {
    let fingerprints = entry_image_fingerprints(entry)?;
    if fingerprints.is_empty() {
        return Ok(Vec::new());
    }
    let Some(store) = attachment_store else {
        return Ok(Vec::new());
    };
    if let Some(references) = store
        .refs_for_entry(session_id, &entry.id.0)
        .map_err(attachment_service_error)?
    {
        if references_match_fingerprints(store, &references, &fingerprints)? {
            return Ok(references);
        }
        return Err(ServiceError::Internal);
    }
    if let Some(references) = pending.front() {
        if references_match_fingerprints(store, references, &fingerprints)? {
            store
                .associate(session_id, &entry.id.0, references)
                .map_err(attachment_service_error)?;
            return Ok(pending.pop_front().unwrap_or_default());
        }
    }
    store
        .recover_association(session_id, &entry.id.0, &fingerprints)
        .map_err(attachment_service_error)
        .map(|references| references.unwrap_or_default())
}

pub(super) fn entry_image_fingerprints(
    entry: &Entry,
) -> Result<Vec<AttachmentFingerprint>, ServiceError> {
    let EntryValue::Message(Message::User(message)) = &entry.value else {
        return Ok(Vec::new());
    };
    message
        .content
        .iter()
        .filter_map(|part| match part {
            UserPart::Media(Media::Image(image)) => Some(image),
            _ => None,
        })
        .filter_map(|image| match &image.source {
            ImageSource::Inline(bytes) => Some((image, bytes)),
            _ => None,
        })
        .map(|(image, bytes)| {
            let media_type = image
                .media_type
                .as_ref()
                .ok_or(ServiceError::InvalidSeed)?
                .essence_str()
                .to_owned();
            Ok(AttachmentFingerprint {
                media_type,
                byte_len: bytes.len() as u64,
                sha256: stable_hash(bytes),
            })
        })
        .collect()
}

pub(super) fn references_match_fingerprints(
    store: &AttachmentStore,
    references: &[AttachmentRef],
    fingerprints: &[AttachmentFingerprint],
) -> Result<bool, ServiceError> {
    if references.len() != fingerprints.len() {
        return Ok(false);
    }
    let resolved = store
        .resolve_many(references)
        .map_err(attachment_service_error)?;
    Ok(resolved
        .iter()
        .zip(fingerprints)
        .all(|(attachment, fingerprint)| {
            attachment.reference.media_type == fingerprint.media_type
                && attachment.reference.byte_len == fingerprint.byte_len
                && attachment.sha256 == fingerprint.sha256
        }))
}
