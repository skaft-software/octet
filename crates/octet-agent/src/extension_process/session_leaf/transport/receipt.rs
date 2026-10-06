//! Preclaim complete receipt bound, including the namespace-filtered durable entry.
use super::*;
use crate::session::{
    Entry, EntryId, EntryMetadata, EntryValue, ExtensionEntryMetadata, ExtensionMetadataProvenance,
};

#[derive(Serialize)]
pub(in crate::extension_process) struct Receipt<'a> {
    #[serde(flatten)]
    pub result: SessionLeafAppendResult,
    pub entry: visible::VisibleEntry<'a>,
    pub previous_revision: u64,
    pub view_revision: u64,
    pub previous_head: Option<String>,
}

pub(in crate::extension_process) fn fits(
    request: &LeafWireAppend,
    bound: &BoundLeaf,
    max_bytes: usize,
) -> bool {
    let entry_id = "9".repeat(20);
    let namespace = &bound.producer.binding.namespace;
    let current = lock_std_mutex(&bound.current_grant).clone();
    let Some(current) = current else {
        return false;
    };
    let mut successor = current.clone();
    successor.grant_id = "f".repeat(64);
    successor.expected_head = Some(entry_id.clone());
    let entry = Entry {
        id: EntryId(entry_id.clone()),
        parent: current.expected_head.as_ref().map(|s| EntryId(s.clone())),
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
                    value: json!({"entry_type":request.entry_type,"data":request.data}),
                    provenance: ExtensionMetadataProvenance {
                        extension: namespace.clone(),
                        process_generation: Some(current.owner.process_generation),
                    },
                },
            )]),
            ..Default::default()
        }),
    };
    let receipt = Receipt {
        result: SessionLeafAppendResult {
            entry_id: entry_id.clone(),
            head: entry_id,
            successor: Some(successor),
        },
        entry: visible::VisibleEntry {
            entry: &entry,
            namespace,
        },
        previous_revision: MAX_SAFE,
        view_revision: MAX_SAFE,
        previous_head: current.expected_head,
    };
    // Maximum legal child ID, not merely the present ID. All escaped UTF-8
    // record fields are counted by the same serializer used for the real reply.
    validate_session_snapshot_size(
        &json!({"jsonrpc":"2.0","id":"\u{0001}".repeat(256),"result":receipt}),
        max_bytes.saturating_sub(1),
    )
    .is_ok()
}
