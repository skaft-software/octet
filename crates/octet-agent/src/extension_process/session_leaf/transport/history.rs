//! Borrow the authorized document twice: first count, then serialize after quota.
use super::*;
use serde::ser::{SerializeMap, SerializeSeq};

#[cfg(test)]
thread_local! {
    static SERIALIZED_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct Entries<'a> {
    session: &'a crate::Session,
    namespace: &'a str,
    usage: &'a HashMap<&'a crate::session::EntryId, &'a crate::session::UsageRecord>,
}
impl Serialize for Entries<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.session.entries().len()))?;
        for entry in self.session.entries() {
            #[cfg(test)]
            SERIALIZED_ENTRIES.with(|count| count.set(count.get() + 1));
            seq.serialize_element(&visible::VisibleEntry {
                entry,
                namespace: self.namespace,
                usage: self.usage.get(&entry.id).copied(),
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
    // Rust owns disclosure and revision authority across the process boundary.
    // Reuse bytes only for this exact native handle/mutation and namespace;
    // the invocation still gets its own preparation, revision and reservation.
    if let Some(Some(view)) = store.current.get(&owner) {
        if view.source == Some(session.view_identity())
            && view.namespace.as_deref() == Some(namespace)
        {
            let reservation = store.reserve(
                profile,
                view.bytes.len(),
                view.descriptor.entry_count + view.descriptor.branch_count,
                false,
            )?;
            return Ok(Arc::new(View {
                bytes: Arc::clone(&view.bytes),
                descriptor: Descriptor {
                    transfer_id: String::new(),
                    view_revision: revision,
                    preparation,
                    ..view.descriptor.clone()
                },
                source: view.source,
                namespace: view.namespace.clone(),
                _reservation: reservation,
            }));
        }
    }
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
    let usage = session.assistant_usage_index();
    let mut document = History {
        entries: Entries {
            session,
            namespace,
            usage: &usage,
        },
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
        bytes: Arc::new(encoded),
        descriptor,
        source: Some(session.view_identity()),
        namespace: Some(namespace.to_owned()),
        _reservation: reservation,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{EntryValue, Session};

    #[test]
    fn unchanged_invocation_history_work_is_flat() {
        let mut work = Vec::new();
        for count in [16, 160, 1600] {
            let temp = tempfile::tempdir().unwrap();
            let mut session = Session::create(temp.path().join("history.jsonl")).unwrap();
            for _ in 0..count {
                session
                    .append(EntryValue::Message(octet_ai::Message::User(
                        octet_ai::UserMessage {
                            content: vec![octet_ai::UserPart::Text("fixture".into())],
                        },
                    )))
                    .unwrap();
            }
            let owner = ExtensionResourceOwner {
                session_id: session.resource_owner_key(),
                extension_instance_id: "history-work".into(),
                process_generation: 1,
            };
            let profile = SessionSnapshotProfile::for_frame(1024 * 1024).unwrap();
            let mut store = Store::default();
            let initial = store
                .history(&session, "history-work", owner.clone(), &profile, None, 1)
                .unwrap();
            store
                .current
                .insert(owner.clone(), Some(Arc::clone(&initial)));
            HOST_FACT_BYTES.with(|value| value.set(0));
            let full = host_facts(&initial).unwrap();
            let before = HOST_FACT_BYTES.with(std::cell::Cell::get);
            HOST_FACT_BYTES.with(|value| value.set(0));
            let facts = publication_facts(&initial);
            let after = HOST_FACT_BYTES.with(std::cell::Cell::get);
            assert_eq!(facts["session_leaf_id"], full["session_leaf_id"]);
            assert_eq!(
                facts["session_view_revision"],
                full["session_view_revision"]
            );
            assert_eq!(after, 0);
            eprintln!("{count} entries: publication parsed bytes before={before} after={after}");
            SERIALIZED_ENTRIES.with(|value| value.set(0));
            let prepared = store
                .history(
                    &session,
                    "history-work",
                    owner.clone(),
                    &profile,
                    Some(Preparation {
                        activation_epoch: 3,
                        operation_id: "invocation".into(),
                        tool_generation: 4,
                        head: initial.descriptor.head.clone(),
                    }),
                    2,
                )
                .unwrap();
            let visits = SERIALIZED_ENTRIES.with(std::cell::Cell::get);
            eprintln!("{count} entries: unchanged invocation serialized {visits} entries");
            work.push(visits);
            assert_eq!(prepared.descriptor.view_revision, 2);
            assert!(initial.descriptor.preparation.is_none());
            assert_eq!(prepared.descriptor.sha256, initial.descriptor.sha256);
            assert!(Arc::ptr_eq(&initial.bytes, &prepared.bytes));
            SERIALIZED_ENTRIES.with(|value| value.set(0));
            store
                .history(&session, "other", owner.clone(), &profile, None, 4)
                .unwrap();
            assert!(SERIALIZED_ENTRIES.with(std::cell::Cell::get) > 0);
            // A new mutation cannot reuse the old immutable document.
            session
                .append(EntryValue::Message(octet_ai::Message::User(
                    octet_ai::UserMessage {
                        content: vec![octet_ai::UserPart::Text("new".into())],
                    },
                )))
                .unwrap();
            let appended = store
                .history(&session, "history-work", owner.clone(), &profile, None, 3)
                .unwrap();
            assert_eq!(initial.descriptor.entry_count, count);
            assert_eq!(appended.descriptor.entry_count, count + 1);
            assert_ne!(appended.descriptor.sha256, initial.descriptor.sha256);
            // Namespace changes may disclose a different document, even for the
            // same native handle and owner; they must reconstruct it.
            SERIALIZED_ENTRIES.with(|value| value.set(0));
            store
                .history(&session, "other", owner, &profile, None, 4)
                .unwrap();
            assert!(SERIALIZED_ENTRIES.with(std::cell::Cell::get) > 0);
        }
        assert_eq!(work, [0, 0, 0], "unchanged snapshots revisit old entries");
    }
}
