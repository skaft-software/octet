//! Borrowed namespace filtering: count before allocating history representations.
use super::*;
use crate::session::{Entry, EntryMetadata};
use serde::ser::{SerializeMap, SerializeStruct};

pub(super) struct VisibleEntry<'a> {
    pub(super) entry: &'a Entry,
    pub(super) namespace: &'a str,
}

struct VisibleMetadata<'a> {
    metadata: &'a EntryMetadata,
    namespace: &'a str,
}

struct VisibleNamespaces<'a> {
    metadata: &'a EntryMetadata,
    namespace: &'a str,
}

impl Serialize for VisibleNamespaces<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (owner, value) in &self.metadata.extension_metadata {
            if value.public || owner == self.namespace {
                map.serialize_entry(owner, value)?;
            }
        }
        map.end()
    }
}

impl Serialize for VisibleMetadata<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let metadata = self.metadata;
        let mut map = serializer.serialize_map(None)?;
        macro_rules! optional {
            ($($field:ident),* $(,)?) => { $(
                if let Some(value) = &metadata.$field {
                    map.serialize_entry(stringify!($field), value)?;
                }
            )* };
        }
        optional!(
            custom_message,
            native_steering,
            prompt_model,
            prompt_model_source,
            prompt_color,
            display_text,
            run_outcome,
            tool_output,
            tool_composition,
            replay_safe_tool_calls,
            tool_started_unix_ms,
            tool_finished_unix_ms
        );
        if metadata.local_synthetic_assistant {
            map.serialize_entry("local_synthetic_assistant", &true)?;
        }
        if metadata
            .extension_metadata
            .iter()
            .any(|(owner, value)| value.public || owner == self.namespace)
        {
            map.serialize_entry(
                "extension_metadata",
                &VisibleNamespaces {
                    metadata,
                    namespace: self.namespace,
                },
            )?;
        }
        map.end()
    }
}

impl Serialize for VisibleEntry<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let entry = self.entry;
        let mut value = serializer.serialize_struct("Entry", 5)?;
        value.serialize_field("id", &entry.id)?;
        value.serialize_field("parent", &entry.parent)?;
        if let Some(metadata) = &entry.metadata {
            value.serialize_field(
                "metadata",
                &VisibleMetadata {
                    metadata,
                    namespace: self.namespace,
                },
            )?;
        }
        if let Some(timestamp) = entry.timestamp_unix_ms {
            value.serialize_field("timestamp_unix_ms", &timestamp)?;
        }
        value.serialize_field("value", &entry.value)?;
        value.end()
    }
}

pub(super) fn validate_metadata(
    entry: &Entry,
    namespace: &str,
) -> Result<(), ExtensionRuntimeError> {
    let Some(metadata) = &entry.metadata else {
        return Ok(());
    };
    let mut total = 0usize;
    let mut namespaces = 0usize;
    for (owner, value) in &metadata.extension_metadata {
        if !value.public && owner != namespace {
            continue;
        }
        namespaces += 1;
        if namespaces > crate::session::MAX_EXTENSION_ENTRY_METADATA_NAMESPACES {
            return Err(ExtensionRuntimeError::Protocol(
                "session metadata exceeds namespace limit".into(),
            ));
        }
        let bytes = session_snapshot_bytes(
            &value.value,
            crate::session::MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES,
        )
        .map_err(|_| {
            ExtensionRuntimeError::Protocol("session metadata exceeds durable bounds".into())
        })?;
        total = total.saturating_add(bytes);
        if total > crate::session::MAX_EXTENSION_ENTRY_METADATA_BYTES {
            return Err(ExtensionRuntimeError::Protocol(
                "session metadata exceeds durable bounds".into(),
            ));
        }
    }
    Ok(())
}

pub(super) fn clone_entry(entry: &Entry, namespace: &str) -> Result<Entry, ExtensionRuntimeError> {
    validate_metadata(entry, namespace)?;
    // Never clone the source metadata map and then retain: excluded private
    // values can be arbitrarily large and must remain borrowed throughout.
    let metadata = entry.metadata.as_ref().map(|m| EntryMetadata {
        custom_message: m.custom_message.clone(),
        native_steering: m.native_steering.clone(),
        prompt_model: m.prompt_model.clone(),
        prompt_model_source: m.prompt_model_source.clone(),
        prompt_color: m.prompt_color.clone(),
        display_text: m.display_text.clone(),
        run_outcome: m.run_outcome.clone(),
        tool_output: m.tool_output.clone(),
        tool_composition: m.tool_composition.clone(),
        replay_safe_tool_calls: m.replay_safe_tool_calls.clone(),
        tool_started_unix_ms: m.tool_started_unix_ms,
        tool_finished_unix_ms: m.tool_finished_unix_ms,
        local_synthetic_assistant: m.local_synthetic_assistant,
        extension_metadata: m
            .extension_metadata
            .iter()
            .filter(|(owner, value)| value.public || owner.as_str() == namespace)
            .map(|(owner, value)| (owner.clone(), value.clone()))
            .collect(),
    });
    Ok(Entry {
        id: entry.id.clone(),
        parent: entry.parent.clone(),
        metadata,
        timestamp_unix_ms: entry.timestamp_unix_ms,
        value: entry.value.clone(),
    })
}

/// Count the exact legacy complete snapshot before entry/branch clones, including
/// duplicated branch bodies and all non-entry visible facts. Traversal borrows
/// native entries; reversing order does not change JSON byte counts.
pub(super) fn legacy_snapshot_bytes(
    session: &crate::Session,
    namespace: &str,
    limit: usize,
) -> Result<usize, ExtensionRuntimeError> {
    #[derive(Serialize)]
    struct Empty<'a> {
        session_entries: &'a [Entry],
        session_branch: &'a [Entry],
        session_leaf_id: Option<crate::session::EntryId>,
        session_file: &'a std::path::Path,
        session_header: Option<&'a crate::session::SessionHeader>,
        session_labels: &'a BTreeMap<crate::session::EntryId, String>,
    }
    let mut total = session_snapshot_bytes(
        &Empty {
            session_entries: &[],
            session_branch: &[],
            session_leaf_id: session.head(),
            session_file: session.path(),
            session_header: session.header(),
            session_labels: session.entry_labels(),
        },
        limit,
    )?;
    let mut add = |entry: &Entry, comma: bool| -> Result<(), ExtensionRuntimeError> {
        validate_metadata(entry, namespace)?;
        total = total.saturating_add(usize::from(comma));
        let bytes = session_snapshot_bytes(
            &VisibleEntry { entry, namespace },
            limit.saturating_sub(total),
        )?;
        total = total.saturating_add(bytes);
        Ok(())
    };
    for (index, entry) in session.entries().iter().enumerate() {
        add(entry, index != 0)?;
    }
    let mut cursor = session.head();
    let mut count = 0;
    while let Some(id) = cursor {
        let entry = session.entry(&id).ok_or_else(|| {
            ExtensionRuntimeError::Protocol("session snapshot unavailable".into())
        })?;
        add(entry, count != 0)?;
        count += 1;
        cursor = entry.parent.clone();
    }
    if total > limit {
        return Err(ExtensionRuntimeError::Protocol(
            "session snapshot exceeds wire bound; no entries truncated".into(),
        ));
    }
    Ok(total)
}
