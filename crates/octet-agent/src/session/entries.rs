//! The entry log: append, checkout, fork, checkpoints, and read access to
//! entries and their labels.
//!
//! Separate from `store` because these operations are about the shape of
//! the entry tree — which entry is the parent, which branch is active —
//! rather than about the bytes underneath it.

use super::context::append_context_message;
use super::*;

impl Session {
    /// Appends an entry (parented on the current head) and records the new
    /// head. Writes two JSONL records — the entry, then a head record — in a
    /// single synced write to the append-only file.
    pub fn append(&mut self, value: EntryValue) -> Result<EntryId, SessionError> {
        self.append_with_metadata(value, None)
    }

    /// Append a durable, non-model-visible terminal marker for a frontend run.
    ///
    /// The marker uses the long-standing configuration entry envelope for
    /// backwards-compatible replay. Its typed outcome lives in presentation
    /// metadata and therefore never enters provider-visible context.
    pub fn append_run_outcome(
        &mut self,
        outcome: SessionRunOutcome,
    ) -> Result<EntryId, SessionError> {
        self.append_with_metadata(
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(EntryMetadata {
                run_outcome: Some(outcome),
                ..EntryMetadata::default()
            }),
        )
    }

    /// Appends an entry with stable semantic presentation metadata.
    ///
    /// Metadata is intentionally kept outside [`EntryValue`] so model-visible
    /// conversation messages remain provider-independent and legacy readers can
    /// continue to ignore presentation details.
    pub fn append_with_metadata(
        &mut self,
        value: EntryValue,
        metadata: Option<EntryMetadata>,
    ) -> Result<EntryId, SessionError> {
        match &value {
            EntryValue::ResponsesTurn {
                assistant,
                model,
                output,
                ..
            } => {
                let valid_assistant = self.head.as_ref() == Some(assistant)
                    && self.entry(assistant).is_some_and(|entry| {
                        matches!(
                            &entry.value,
                            EntryValue::Message(Message::Assistant(message))
                                if message.protocol == octet_ai::Protocol::OpenAiResponses
                                    && &message.model == model
                        )
                    });
                if !valid_assistant || output.is_empty() {
                    return Err(SessionError::InvalidResponsesSidecar(format!(
                        "Responses turn is not a direct sidecar of a Responses assistant from model {}",
                        model.0
                    )));
                }
            }
            EntryValue::ResponsesCompaction {
                covered_through,
                output,
                ..
            } if self.head.as_ref() != Some(covered_through) || !output.has_valid_compaction() => {
                return Err(SessionError::InvalidResponsesSidecar(format!(
                    "Responses compaction is not a direct checkpoint of {:?}",
                    covered_through.0
                )));
            }
            _ => {}
        }
        let id = EntryId(format!("{:03}", self.next_id));
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| SessionError::Limit("session entry ID space is exhausted".to_owned()))?;
        let accepts_tool_output_details = matches!(
            &value,
            EntryValue::Message(Message::User(message))
                if message
                    .content
                    .iter()
                    .filter(|part| matches!(part, UserPart::ToolResult(_)))
                    .count()
                    == 1
        );
        let metadata = metadata
            .map(|mut metadata| {
                if !accepts_tool_output_details {
                    metadata.tool_output = None;
                }
                metadata
            })
            .and_then(EntryMetadata::sanitized);
        let entry = Entry {
            id: id.clone(),
            parent: self.head.clone(),
            metadata,
            timestamp_unix_ms: Some(now_unix_millis()),
            value,
        };
        let mut buf = Vec::with_capacity(256);
        write_json_line(&mut buf, &SessionRecordRef::Entry(&entry))?;
        self.write_head_record(&mut buf, &id, &self.total_cost_microdollars)?;
        // The immutable paired result doubles as the invocation tombstone.
        // Hold the store fence across its synced append so late memos cannot
        // revive state; replay performs the same cleanup from this entry.
        let settled = self
            .invocation_entries
            .result_scopes(&entry, &self.entries, &self.index);
        let scopes = settled
            .iter()
            .map(|(scope, _)| scope.clone())
            .collect::<Vec<_>>();
        self.invocations
            .commit_results(&scopes, || self.writer.persist(&buf))?;
        self.invocation_entries
            .record(&entry, &self.index, &settled);

        self.index.insert(id.clone(), self.entries.len());
        self.entries.push(entry);
        self.head = Some(id.clone());
        self.next_id = next_id;

        let cache = self.context_cache.get_mut();
        match &self.entries.last().expect("just appended").value {
            EntryValue::Message(message) => {
                if let Some(messages) = cache {
                    append_context_message(messages, message);
                }
            }
            EntryValue::ResponsesReasoning { .. }
            | EntryValue::ResponsesSteering { .. }
            | EntryValue::Config { .. }
            | EntryValue::PromptTemplateSelected { .. }
            | EntryValue::ResponsesTurn { .. }
            | EntryValue::ResponsesCompaction { .. } => {}
            EntryValue::Compaction { .. }
            | EntryValue::SkillActivated { .. }
            | EntryValue::SkillResourceRead { .. }
            | EntryValue::SkillDeactivated { .. } => *cache = None,
        }
        Ok(id)
    }

    /// Appends one durable, non-model-visible extension-owned entry.
    ///
    /// The payload is retained exactly like [`Session::append_run_outcome`]:
    /// the entry uses the long-standing non-context configuration marker and
    /// the typed data lives in entry metadata, so no provider projection can
    /// observe it and older readers can still replay the record. The entry's
    /// `extension_metadata` carries the host-attested provenance envelope for
    /// `namespace`, mirroring the persistence-metadata hook path, and the
    /// payload itself is stored in that namespace's bounded value slot.
    ///
    /// Returns the new entry ID, which resolves again after reopening the
    /// session from disk (see [`Session::extension_entry`]). An invalid
    /// namespace, entry type, or payload is refused with a typed error before
    /// anything is written; nothing is truncated or silently dropped.
    pub fn append_extension_entry(
        &mut self,
        namespace: &str,
        process_generation: Option<u64>,
        entry_type: &str,
        data: serde_json::Value,
    ) -> Result<EntryId, SessionError> {
        if !is_valid_extension_metadata_namespace(namespace) {
            return Err(SessionError::Limit(format!(
                "invalid extension metadata namespace {namespace:?}"
            )));
        }
        let payload = ExtensionEntry {
            entry_type: entry_type.to_owned(),
            data,
        };
        let Some(_data_bytes) = valid_extension_entry_payload(&payload) else {
            return Err(SessionError::Limit(format!(
                "extension entry type must be 1..={MAX_EXTENSION_ENTRY_TYPE_BYTES} non-control bytes and its data must be an inert JSON value within {MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES} encoded bytes"
            )));
        };
        let mut extension_metadata = BTreeMap::new();
        extension_metadata.insert(
            namespace.to_owned(),
            ExtensionEntryMetadata {
                // The append protocol carries no public flag, so extension
                // payloads stay private: exports and frontend projections must
                // not surface extension-owned data implicitly.
                public: false,
                value: payload.to_value(),
                provenance: ExtensionMetadataProvenance {
                    extension: namespace.to_owned(),
                    process_generation,
                },
            },
        );
        self.append_with_metadata(
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(EntryMetadata {
                extension_metadata,
                ..EntryMetadata::default()
            }),
        )
    }

    /// Changes the head to an existing entry and appends a head record (same
    /// persistence semantics as [`Session::append`]). Future appends fork a
    /// new branch from this point.
    pub fn checkout(&mut self, id: EntryId) -> Result<(), SessionError> {
        if !self.index.contains_key(&id) {
            return Err(SessionError::UnknownEntry(id));
        }
        let mut buf = Vec::with_capacity(64);
        self.write_head_record(&mut buf, &id, &self.total_cost_microdollars)?;
        self.persist(&buf)?;
        self.head = Some(id);
        *self.context_cache.get_mut() = None;
        *self.responses_replay_cache.get_mut() = None;
        Ok(())
    }

    /// Durably selects the empty pre-entry boundary. Future appends create a
    /// new root branch; all existing roots and descendants remain preserved.
    pub fn checkout_root(&mut self) -> Result<(), SessionError> {
        let mut buf = Vec::with_capacity(64);
        write_json_line(
            &mut buf,
            &SessionRecordRef::RootHead {
                total_cost_microdollars: &self.total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        self.persist(&buf)?;
        self.head = None;
        *self.context_cache.get_mut() = None;
        *self.responses_replay_cache.get_mut() = None;
        Ok(())
    }

    /// Copies exactly one committed ancestor chain into a new session file.
    ///
    /// Entry IDs and semantic sidecar references are preserved, but sibling
    /// branches, usage telemetry, and later checkpoints are deliberately not
    /// copied. When the selected chain crosses a compaction boundary, entries
    /// older than that boundary's `first_kept` are omitted: the fork replays
    /// from the compaction summary exactly like the source session, so the
    /// replaced history is never copied again. The boundary's root-side entry
    /// keeps a source-side parent that was not copied, so it is detached and
    /// re-rooted in the destination.
    ///
    /// A `None` checkpoint copies no entries at all: the destination is an
    /// empty session (for forking "before" a root message).
    ///
    /// The destination is created atomically enough to remain absent on
    /// every validation/write failure.
    pub fn fork_to(
        &self,
        path: impl Into<PathBuf>,
        checkpoint: Option<&EntryId>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        // A compacted fork still needs the original host pin and all retained
        // update positions. Keep ancestry for reasoning-aware branches; context
        // reconstruction continues to honor the compaction marker.
        let mut probe = checkpoint;
        let mut preserve_reasoning_ancestry = false;
        while let Some(id) = probe {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            preserve_reasoning_ancestry |=
                matches!(entry.value, EntryValue::ResponsesReasoning { .. });
            probe = entry.parent.as_ref();
        }
        let mut newest_first = Vec::<&Entry>::new();
        let mut stop_at: Option<&EntryId> = None;
        let mut cursor = checkpoint;
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            newest_first.push(entry);
            if stop_at == Some(id) && !preserve_reasoning_ancestry {
                break;
            }
            if let EntryValue::Compaction { first_kept, .. } = &entry.value {
                stop_at = Some(first_kept);
            }
            cursor = entry.parent.as_ref();
        }
        newest_first.reverse();

        let mut destination = Session::create(path.clone())?;
        let result = (|| {
            let mut bytes = Vec::new();
            let mut remaining = newest_first.into_iter();
            if let Some(first) = remaining.next() {
                if first.parent.is_some() {
                    // The chain starts inside a compacted span: detach the
                    // root-side entry from its source-side parent, which was
                    // deliberately not copied.
                    let mut detached = (*first).clone();
                    detached.parent = None;
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(&detached))?;
                } else {
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(first))?;
                }
                for entry in remaining {
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(entry))?;
                }
            }
            if let Some(id) = checkpoint {
                write_json_line(
                    &mut bytes,
                    &SessionRecordRef::Head {
                        id,
                        total_cost_microdollars: &0,
                        total_cost_picodollars_remainder: &0,
                    },
                )?;
            }
            destination.persist(&bytes)?;
            drop(destination);
            Session::open(&path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    /// Persist a restore point for a completed prompt without changing the
    /// current head or model-visible context.
    ///
    /// The prompt must be a user-message ancestor of the current head. The
    /// returned record can later be restored with [`Self::restore_checkpoint`].
    pub fn checkpoint(&mut self, prompt: EntryId) -> Result<Checkpoint, SessionError> {
        self.checkpoint_with_telemetry(prompt, None, None)
    }

    /// Persist a completed-prompt restore point together with exact aggregate
    /// usage and current-run cost for UI/status rehydration.
    ///
    /// `run_cost_microdollars` is `Some(0)` for explicitly zero-priced models
    /// and `None` when pricing was unavailable.
    pub fn checkpoint_with_telemetry(
        &mut self,
        prompt: EntryId,
        usage: Option<Usage>,
        run_cost_microdollars: Option<u64>,
    ) -> Result<Checkpoint, SessionError> {
        let head = self.head.clone().ok_or(SessionError::EmptySession)?;
        let prompt_is_user = self
            .entry(&prompt)
            .is_some_and(|entry| matches!(&entry.value, EntryValue::Message(Message::User(_))));
        if !prompt_is_user {
            return Err(SessionError::UnknownEntry(prompt));
        }
        if !self.is_ancestor_of_head(&prompt) {
            return Err(SessionError::NotAncestor(prompt));
        }

        let checkpoint = Checkpoint {
            prompt,
            head,
            usage,
            run_cost_microdollars,
        };
        let mut buffer = Vec::with_capacity(192);
        write_json_line(
            &mut buffer,
            &SessionRecordRef::Checkpoint {
                prompt: &checkpoint.prompt,
                head: &checkpoint.head,
                usage: &checkpoint.usage,
                run_cost_microdollars: &checkpoint.run_cost_microdollars,
            },
        )?;
        self.persist(&buffer)?;
        self.checkpoints.push(checkpoint.clone());
        Ok(checkpoint)
    }

    /// Durable completed-prompt restore points in append order, across all
    /// preserved branches.
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }
}

impl Session {
    /// Returns the current head entry ID (`None` for an empty session).
    pub fn head(&self) -> Option<EntryId> {
        self.head.clone()
    }

    /// Borrows the current head entry ID without allocating.
    pub fn head_ref(&self) -> Option<&EntryId> {
        self.head.as_ref()
    }

    /// Returns all entries in insertion order, across all branches.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns the entry with the given ID.
    pub fn entry(&self, id: &EntryId) -> Option<&Entry> {
        self.index.get(id).map(|&i| &self.entries[i])
    }

    /// Durably sets or clears the label of an existing entry.
    ///
    /// Labels are a replaceable mutation of an immutable JSONL entry: a new
    /// label record is appended and the last record for one entry wins on
    /// replay. An empty `label` clears the entry's label. Unknown entry IDs,
    /// labels longer than [`MAX_ENTRY_LABEL_BYTES`], and control characters are
    /// refused with a typed error and leave the session state unchanged.
    pub fn set_entry_label(&mut self, id: &EntryId, label: &str) -> Result<(), SessionError> {
        if !self.index.contains_key(id) {
            return Err(SessionError::UnknownEntry(id.clone()));
        }
        if !valid_entry_label(label) {
            return Err(SessionError::Limit(format!(
                "entry label must be at most {MAX_ENTRY_LABEL_BYTES} bytes without control characters"
            )));
        }
        let mut buf = Vec::with_capacity(64 + label.len());
        write_json_line(
            &mut buf,
            &SessionRecordRef::EntryLabel {
                entry_id: id,
                label,
            },
        )?;
        self.persist(&buf)?;
        if label.is_empty() {
            self.entry_labels.remove(id);
        } else {
            self.entry_labels.insert(id.clone(), label.to_owned());
        }
        Ok(())
    }

    /// The durable label of `id`, if one is currently set.
    pub fn entry_label(&self, id: &EntryId) -> Option<&str> {
        self.entry_labels.get(id).map(String::as_str)
    }

    /// Every durable entry label, keyed by entry ID.
    ///
    /// At most one label exists per entry, so the map never outgrows the
    /// session's entries.
    pub fn entry_labels(&self) -> &BTreeMap<EntryId, String> {
        &self.entry_labels
    }

    /// The durable extension-owned payload appended for `id` by `namespace`.
    ///
    /// Returns a detached decoded payload, or `None` when the entry has no
    /// extension value in that namespace (or the value was written by another
    /// host path that does not use the entry envelope).
    pub fn extension_entry(&self, id: &EntryId, namespace: &str) -> Option<ExtensionEntry> {
        let metadata = self.entry(id)?.metadata.as_ref()?;
        ExtensionEntry::from_value(&metadata.extension_metadata.get(namespace)?.value)
    }
}
