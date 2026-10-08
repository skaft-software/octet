//! Borrow the authorized document twice: first count, then serialize after quota.
use super::*;
use serde::ser::{SerializeMap, SerializeSeq};

struct Entries<'a> {
    session: &'a crate::Session,
    namespace: &'a str,
}
impl Serialize for Entries<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.session.entries().len()))?;
        for entry in self.session.entries() {
            seq.serialize_element(&visible::VisibleEntry {
                entry,
                namespace: self.namespace,
            })?;
        }
        seq.end()
    }
}
struct Branch<'a> {
    session: &'a crate::Session,
    ordered: Option<&'a [&'a crate::session::EntryId]>,
}
impl Serialize for Branch<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        if let Some(ordered) = self.ordered {
            for id in ordered {
                seq.serialize_element(id)?;
            }
        } else {
            // Reversal changes order but not the exact compact encoded length.
            let mut cursor = self.session.head_ref();
            while let Some(id) = cursor {
                seq.serialize_element(id)?;
                cursor = self
                    .session
                    .entry(id)
                    .expect("native ancestry")
                    .parent
                    .as_ref();
            }
        }
        seq.end()
    }
}
struct History<'a> {
    entries: Entries<'a>,
    branch_ids: Branch<'a>,
}
impl Serialize for History<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let session = self.entries.session;
        let mut map = serializer.serialize_map(Some(6))?;
        map.serialize_entry("entries", &self.entries)?;
        map.serialize_entry("branch_ids", &self.branch_ids)?;
        map.serialize_entry("head", &session.head_ref())?;
        map.serialize_entry("file", session.path())?;
        if let Some(header) = session.header() {
            map.serialize_entry("header", header)?;
        } else {
            map.serialize_entry("header", &serde_json::Map::<String, Value>::new())?;
        }
        map.serialize_entry("labels", session.entry_labels())?;
        map.end()
    }
}

pub(super) fn encode(
    store: &mut Store,
    session: &crate::Session,
    namespace: &str,
    owner: ExtensionResourceOwner,
    profile: &SessionSnapshotProfile,
    preparation: Option<Preparation>,
    revision: u64,
) -> Result<Arc<View>, ExtensionRuntimeError> {
    let mut branch_count = 0usize;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        branch_count += 1;
        cursor = session.entry(id).expect("native ancestry").parent.as_ref();
    }
    let entry_count = session.entries().len();
    if entry_count > profile.view_entries || branch_count > profile.view_entries {
        return Err(unavailable());
    }
    for entry in session.entries() {
        visible::validate_metadata(entry, namespace)?;
    }
    let mut document = History {
        entries: Entries { session, namespace },
        branch_ids: Branch {
            session,
            ordered: None,
        },
    };
    let bytes = session_snapshot_bytes(&document, profile.snapshot_bytes)?;
    let reservation = store.reserve(profile, bytes, entry_count + branch_count, false)?;
    // All auxiliary vectors and encoded bytes are allocated only after admission.
    let mut branch = Vec::with_capacity(branch_count);
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        branch.push(id);
        cursor = session.entry(id).expect("native ancestry").parent.as_ref();
    }
    branch.reverse();
    document.branch_ids.ordered = Some(&branch);
    let mut encoded = Vec::with_capacity(bytes);
    serde_json::to_writer(&mut encoded, &document).map_err(|_| unavailable())?;
    debug_assert_eq!(encoded.len(), bytes);
    let descriptor = Descriptor {
        transfer_id: String::new(),
        kind: "history".into(),
        owner,
        view_revision: revision,
        head: session.head_ref().map(|id| id.0.clone()),
        bytes,
        sha256: hex_digest(&encoded),
        entry_count,
        branch_count,
        preparation,
    };
    Ok(Arc::new(View {
        bytes: encoded,
        descriptor,
        _reservation: reservation,
    }))
}
