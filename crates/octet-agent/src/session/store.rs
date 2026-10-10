//! Durable session-file lifecycle: create, open, replay of the append-only
//! log, and the descriptor-bound write fence every append shares.
//!
//! Separate from the entry-log surface in `entries` because opening a
//! session is a different failure mode from appending to one: `open` has
//! to classify a torn final line as recoverable while a completed record
//! with the same bytes is corruption, and it is the only place that
//! enforces the file/record limits.

use super::*;

impl Session {
    /// Creates a new empty session file on disk. Fails if the file exists.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let path = path.into();
        #[cfg(windows)]
        if path.is_absolute() {
            // New session files carry an owner-only ACL from creation,
            // matching the Unix `0o600` behavior below and the production
            // team-file path. Without this, later owner-only reads (for
            // example delegation's child-session reconciliation) fail closed
            // on files inheriting `%TEMP%` ACLs.
            let file = crate::secure_fs::create_regular_file_for_append(&path)
                .map_err(partial_journal_file_error)?;
            return Self::create_with_file(path, file);
        }
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Self::create_with_file(path, file)
    }

    /// Create an empty session through a caller-supplied read/append file
    /// descriptor that was opened with exclusive-create semantics.
    ///
    /// The descriptor must have been opened with exclusive-create semantics
    /// and must still be empty. This lets a host securely create the file
    /// relative to a validated parent directory before handing it here.
    pub fn create_with_file(path: impl Into<PathBuf>, file: File) -> Result<Self, SessionError> {
        if !file.metadata()?.file_type().is_file() {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session descriptor is not a regular file",
            )));
        }
        if file.metadata()?.len() != 0 {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "new session descriptor is not empty",
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let writer = Arc::new(SessionWriter::new(file.try_clone()?, 0, 0, true));
        let invocations = Arc::new(DurableInvocationStore::with_journal(Arc::clone(&writer)));
        let deferred_runs = Arc::new(DeferredRunStore::with_journal(Arc::clone(&writer)));
        Ok(Self {
            header: None,
            path: path.into(),
            file,
            writer,
            invocations,
            invocation_entries: InvocationEntryIndex::default(),
            deferred_runs,
            entries: Vec::new(),
            index: HashMap::new(),
            head: None,
            next_id: 1,
            context_cache: RefCell::new(None),
            responses_replay_cache: RefCell::new(None),
            #[cfg(test)]
            responses_replay_work: std::cell::Cell::new((0, 0)),
            total_cost_microdollars: 0,
            total_cost_picodollars_remainder: 0,
            checkpoints: Vec::new(),
            usage_records: Vec::new(),
            usage_uncertainty_records: Vec::new(),
            usage_uncertainty_bounds: Vec::new(),
            cache_warm_records: Vec::new(),
            entry_labels: BTreeMap::new(),
        })
    }

    /// Opens an existing session, replaying all records and restoring the
    /// head from the last recorded head.
    ///
    /// A torn *final* line (an interrupted write during an unclean exit) is
    /// dropped — and physically truncated from the file, so subsequent
    /// appends start on a fresh line instead of merging into the torn bytes.
    /// A *valid* final record that merely lost its trailing newline is kept,
    /// and the missing newline is written to complete it. Any malformed
    /// record *before* the final line is corruption and is rejected, as are
    /// duplicate entry IDs and references to unknown entries.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        Self::open_impl(path.into(), true)
    }

    /// Open an existing session through a caller-supplied read/append file
    /// descriptor.
    ///
    /// This lets a host bind path authorization and opening into one
    /// descriptor-relative operation. The descriptor must refer to a regular
    /// file and permit reads, locking, permission repair, and durable appends.
    pub fn open_with_file(path: impl Into<PathBuf>, file: File) -> Result<Self, SessionError> {
        Self::open_file_impl_with_limits(
            path.into(),
            file,
            true,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    /// Inspect an existing session without repairing, truncating, appending,
    /// or otherwise changing its bytes. The returned snapshot is intended for
    /// listing and reporting only; mutation methods fail because its file
    /// descriptor is read-only.
    pub fn open_read_only(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        Self::open_impl(path.into(), false)
    }

    /// Inspect an existing session through a caller-supplied read-only file
    /// descriptor without repairing or mutating its bytes.
    ///
    /// This is the descriptor-bound counterpart to [`Self::open_read_only`].
    /// The descriptor must refer to a regular file and permit reads.
    pub fn open_read_only_with_file(
        path: impl Into<PathBuf>,
        file: File,
    ) -> Result<Self, SessionError> {
        Self::open_file_impl_with_limits(
            path.into(),
            file,
            false,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    fn open_impl(path: PathBuf, recover_tail: bool) -> Result<Self, SessionError> {
        Self::open_impl_with_limits(
            path,
            recover_tail,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    pub(super) fn open_impl_with_limits(
        path: PathBuf,
        recover_tail: bool,
        max_file_bytes: u64,
        max_records: usize,
    ) -> Result<Self, SessionError> {
        let mut options = OpenOptions::new();
        options.read(true);
        if recover_tail {
            options.write(true);
            // `append(true)` is Unix-only here: on Windows `std` maps an
            // append handle to `FILE_APPEND_DATA` without `FILE_WRITE_DATA`,
            // so tail repair's `set_len` fails with `ERROR_ACCESS_DENIED`.
            // Writers seek to the end under an exclusive lock before every
            // write, and `GENERIC_WRITE` already carries append-data access,
            // so plain `write(true)` preserves append behavior while also
            // allowing truncation of a torn final record.
            #[cfg(unix)]
            options.append(true);
        }
        let file = options.open(&path)?;
        Self::open_file_impl_with_limits(path, file, recover_tail, max_file_bytes, max_records)
    }

    fn open_file_impl_with_limits(
        path: PathBuf,
        mut file: File,
        recover_tail: bool,
        max_file_bytes: u64,
        max_records: usize,
    ) -> Result<Self, SessionError> {
        if !file.metadata()?.file_type().is_file() {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session descriptor is not a regular file",
            )));
        }
        // Replay and tail handling must observe one stable snapshot. Without
        // this lock, a writer could append after the read but before the
        // observed length is captured, pairing stale IDs with a newer length.
        if recover_tail {
            file.lock_exclusive()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = file.metadata()?.permissions().mode() & 0o777;
                if mode != 0o600 {
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
            }
        } else {
            FileExt::lock_shared(&file)?;
        }
        let file_len = file.metadata()?.len();
        if file_len > max_file_bytes {
            return Err(SessionError::Limit(format!(
                "{} is {file_len} bytes (limit {max_file_bytes})",
                path.display()
            )));
        }
        let mut reader = file.try_clone()?;
        reader.seek(std::io::SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(1024 * 1024, reader);

        let mut header: Option<SessionHeader> = None;
        let mut entries: Vec<Entry> = Vec::new();
        let mut index: HashMap<EntryId, usize> = HashMap::new();
        let mut head: Option<EntryId> = None;
        let mut max_id: u64 = 0;
        let mut total_cost_microdollars: u64 = 0;
        let mut total_cost_picodollars_remainder: u32 = 0;
        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        let mut checkpoint_lines: Vec<usize> = Vec::new();
        let mut usage_records: Vec<UsageRecord> = Vec::new();
        let mut usage_uncertainty_records = Vec::new();
        let mut usage_uncertainty_bounds = Vec::new();
        let mut cache_warm_records: Vec<CacheWarmRecord> = Vec::new();
        let restored_invocations = DurableInvocationStore::new();
        let mut invocation_entries = InvocationEntryIndex::default();
        let restored_deferred_runs = DeferredRunStore::new();
        let mut entry_labels: BTreeMap<EntryId, String> = BTreeMap::new();

        // Byte offset of the end of the last accepted record, so a torn tail
        // can be truncated away below. Only one physical line is buffered at a
        // time; parsed entries remain the authoritative in-memory replay.
        let mut valid_end = 0u64;
        let mut observed_end = 0u64;
        let mut final_record_had_newline = true;
        let mut line_bytes = Vec::new();
        let mut line_no = 0usize;
        let mut persisted_records = 0usize;
        loop {
            line_bytes.clear();
            let read_limit = max_file_bytes
                .saturating_sub(observed_end)
                .saturating_add(1);
            let bytes_read = reader
                .by_ref()
                .take(read_limit)
                .read_until(b'\n', &mut line_bytes)?;
            if bytes_read == 0 {
                break;
            }
            observed_end = observed_end
                .checked_add(u64::try_from(bytes_read).map_err(|_| {
                    SessionError::Limit("session read length does not fit u64".to_owned())
                })?)
                .ok_or_else(|| SessionError::Limit("session read length overflow".to_owned()))?;
            if observed_end > max_file_bytes {
                return Err(SessionError::Limit(format!(
                    "session exceeds the {max_file_bytes}-byte limit while being read"
                )));
            }
            line_no += 1;
            if line_no > max_records {
                return Err(SessionError::Limit(format!(
                    "session has more than {max_records} records"
                )));
            }
            let has_newline = line_bytes.last() == Some(&b'\n');
            let line_bytes = if has_newline {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                line_bytes.as_slice()
            };
            let line = match std::str::from_utf8(line_bytes) {
                Ok(line) => line,
                // A crash may tear the final write in the middle of a UTF-8
                // scalar. Newline-terminated records remain strict UTF-8.
                Err(_) if !has_newline => break,
                Err(error) => {
                    return Err(SessionError::Corrupt {
                        line: line_no,
                        message: format!("invalid UTF-8: {error}"),
                    });
                }
            };
            let record: SessionRecord = match serde_json::from_str(line) {
                Ok(record) => record,
                // `read_until` returns a non-newline-terminated segment only
                // at EOF, so malformed bytes are recoverable only here.
                Err(_) if !has_newline => break,
                Err(error) => {
                    return Err(SessionError::Corrupt {
                        line: line_no,
                        message: error.to_string(),
                    });
                }
            };
            valid_end = observed_end;
            final_record_had_newline = has_newline;
            persisted_records += 1;
            match record {
                SessionRecord::Header { header: value } => {
                    if header.is_some() || !entries.is_empty() || line_no != 1 {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "session header is not the first unique record".into(),
                        });
                    }
                    header = Some(value);
                }
                SessionRecord::ToolInvocation { scope, record } => {
                    restored_invocations
                        .restore(scope, record)
                        .map_err(|error| SessionError::Corrupt {
                            line: line_no,
                            message: error.to_string(),
                        })?;
                }
                SessionRecord::EntryLabel { entry_id, label } => {
                    if !index.contains_key(&entry_id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!(
                                "entry label references unknown entry {:?}",
                                entry_id.0
                            ),
                        });
                    }
                    if !valid_entry_label(&label) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "entry label exceeds its bound or contains control characters"
                                .to_owned(),
                        });
                    }
                    if label.is_empty() {
                        entry_labels.remove(&entry_id);
                    } else {
                        entry_labels.insert(entry_id, label);
                    }
                }
                SessionRecord::DeferredRun { record } => {
                    restored_deferred_runs.restore(record).map_err(|error| {
                        SessionError::Corrupt {
                            line: line_no,
                            message: error.to_string(),
                        }
                    })?;
                }
                SessionRecord::Entry(entry) => {
                    if index.contains_key(&entry.id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!("duplicate entry id {:?}", entry.id.0),
                        });
                    }
                    // Track the maximum numeric ID so we can safely resume
                    // appending even if the in-memory vector diverges from
                    // disk state.
                    if let Ok(n) = entry.id.0.parse::<u64>() {
                        max_id = max_id.max(n);
                    }
                    if let Some(parent) = &entry.parent {
                        if !index.contains_key(parent) {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message: format!(
                                    "entry {:?} references unknown parent {:?}",
                                    entry.id.0, parent.0
                                ),
                            });
                        }
                    }
                    match &entry.value {
                        EntryValue::BranchSummary {
                            summary, details, ..
                        } => {
                            // from_entry is provenance, not a parent reference;
                            // a branch-only fork deliberately omits its source.
                            validate_branch_summary(summary, details).map_err(|error| {
                                SessionError::Corrupt {
                                    line: line_no,
                                    message: error.to_string(),
                                }
                            })?;
                        }
                        EntryValue::Compaction { first_kept, .. } => {
                            if !index.contains_key(first_kept) {
                                return Err(SessionError::Corrupt {
                                    line: line_no,
                                    message: format!(
                                        "compaction {:?} references unknown first_kept {:?}",
                                        entry.id.0, first_kept.0
                                    ),
                                });
                            }
                        }
                        EntryValue::ResponsesTurn {
                            assistant,
                            model,
                            output,
                            ..
                        } => {
                            let valid_assistant = index
                                .get(assistant)
                                .and_then(|position| entries.get(*position))
                                .is_some_and(|candidate| {
                                    matches!(
                                        &candidate.value,
                                        EntryValue::Message(Message::Assistant(message))
                                            if message.protocol == octet_ai::Protocol::OpenAiResponses
                                                && &message.model == model
                                    )
                                });
                            if !valid_assistant
                                || entry.parent.as_ref() != Some(assistant)
                                || output.is_empty()
                            {
                                return Err(SessionError::Corrupt {
                                    line: line_no,
                                    message: format!(
                                        "Responses turn {:?} is not a direct sidecar of assistant {:?}",
                                        entry.id.0, assistant.0
                                    ),
                                });
                            }
                        }
                        EntryValue::ResponsesCompaction {
                            covered_through,
                            output,
                            ..
                        } if !index.contains_key(covered_through)
                            || entry.parent.as_ref() != Some(covered_through)
                            || !output.has_valid_compaction() =>
                        {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message: format!(
                                    "Responses compaction {:?} is not a direct checkpoint of {:?}",
                                    entry.id.0, covered_through.0
                                ),
                            });
                        }
                        _ => {}
                    }
                    let settled = invocation_entries.result_scopes(&entry, &entries, &index);
                    for (scope, _) in &settled {
                        restored_invocations.restore_result(scope);
                    }
                    invocation_entries.record(&entry, &index, &settled);
                    index.insert(entry.id.clone(), entries.len());
                    entries.push(*entry);
                }
                SessionRecord::Head {
                    id,
                    total_cost_microdollars: cost,
                    total_cost_picodollars_remainder: remainder,
                } => {
                    if !index.contains_key(&id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!("head references unknown entry {:?}", id.0),
                        });
                    }
                    head = Some(id);
                    total_cost_microdollars = cost;
                    total_cost_picodollars_remainder = remainder;
                }
                SessionRecord::RootHead {
                    total_cost_microdollars: cost,
                    total_cost_picodollars_remainder: remainder,
                } => {
                    head = None;
                    total_cost_microdollars = cost;
                    total_cost_picodollars_remainder = remainder;
                }
                SessionRecord::Checkpoint {
                    prompt,
                    head: checkpoint_head,
                    usage,
                    run_cost_microdollars,
                } => {
                    let prompt_is_user = index
                        .get(&prompt)
                        .and_then(|position| entries.get(*position))
                        .is_some_and(|entry| {
                            matches!(&entry.value, EntryValue::Message(Message::User(_)))
                        });
                    if !prompt_is_user || !index.contains_key(&checkpoint_head) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "checkpoint references unknown or non-user entries"
                                .to_string(),
                        });
                    }
                    checkpoint_lines.push(line_no);
                    checkpoints.push(Checkpoint {
                        prompt,
                        head: checkpoint_head,
                        usage,
                        run_cost_microdollars,
                    });
                }
                SessionRecord::CacheWarm { record } => {
                    let valid = record.attempt > 0
                        && UsageUncertaintyRecord {
                            endpoint: record.endpoint.clone(),
                            model: record.model.clone(),
                            operation: "cache_warm".into(),
                        }
                        .validate()
                        .is_ok()
                        && match record.state {
                            CacheWarmState::Started => {
                                cache_warm_records
                                    .last()
                                    .map_or(record.attempt == 1, |last| {
                                        last.attempt.checked_add(1) == Some(record.attempt)
                                            && last.state != CacheWarmState::Started
                                    })
                            }
                            _ => cache_warm_records.last().is_some_and(|last| {
                                last.attempt == record.attempt
                                    && last.state == CacheWarmState::Started
                                    && last.endpoint == record.endpoint
                                    && last.model == record.model
                                    && last.anchor == record.anchor
                                    && last.extension_override == record.extension_override
                            }),
                        };
                    if !valid {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "invalid cache-warm lifecycle".into(),
                        });
                    }
                    cache_warm_records.push(record);
                }
                SessionRecord::UsageUncertainty { record, bound } => {
                    record.validate().map_err(|_| SessionError::Corrupt {
                        line: line_no,
                        message: "invalid usage uncertainty identifiers".into(),
                    })?;
                    usage_uncertainty_records.push(record);
                    usage_uncertainty_bounds.push(bound);
                }
                SessionRecord::Usage { record } => {
                    if let UsageRecordKind::AssistantTurn { assistant } = &record.kind {
                        let valid_assistant = index
                            .get(assistant)
                            .and_then(|position| entries.get(*position))
                            .is_some_and(|entry| {
                                matches!(&entry.value, EntryValue::Message(Message::Assistant(_)))
                            });
                        if !valid_assistant {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message:
                                    "usage record references an unknown or non-assistant entry"
                                        .to_string(),
                            });
                        }
                    }
                    if let Some(cost) = record.session_cost_microdollars {
                        total_cost_microdollars = cost;
                        total_cost_picodollars_remainder = record
                            .session_cost_picodollars_remainder
                            .unwrap_or_default();
                    } else {
                        // Usage records written before cumulative session
                        // accounting was introduced have only their request
                        // total. Rebuild that legacy tally while replaying so
                        // reports and limits work for resumed sessions too.
                        let request_cost = record
                            .cost_microdollars
                            .or_else(|| record.cost.map(|cost| cost.total))
                            .unwrap_or_default();
                        total_cost_microdollars =
                            total_cost_microdollars.saturating_add(request_cost);
                    }
                    usage_records.push(record);
                }
            }
        }

        if !checkpoints.is_empty() {
            let (entered, exited) = entry_ancestry_intervals(&entries, &index);
            for (checkpoint, checkpoint_line) in checkpoints.iter().zip(checkpoint_lines) {
                let prompt = index[&checkpoint.prompt];
                let checkpoint_head = index[&checkpoint.head];
                let prompt_is_ancestor = entered[prompt] <= entered[checkpoint_head]
                    && exited[checkpoint_head] <= exited[prompt];
                if !prompt_is_ancestor {
                    return Err(SessionError::Corrupt {
                        line: checkpoint_line,
                        message: "checkpoint prompt is not an ancestor of its head".to_string(),
                    });
                }
            }
        }

        // Validate the ID counter before repairing any tail bytes. A
        // syntactically valid record can still be semantically corrupt, and
        // opening such a file must not normalize or otherwise mutate it before
        // returning the corruption error.
        let next_id = max_id.checked_add(1).ok_or_else(|| SessionError::Corrupt {
            line: line_no,
            message: "numeric entry ID exhausts the u64 ID space".to_owned(),
        })?;

        if recover_tail && valid_end < observed_end {
            // Torn final line: truncate it away so the next append starts on
            // a fresh line rather than merging into the torn bytes (which
            // would corrupt the record for every later reopen).
            file.set_len(valid_end)?;
        }

        if recover_tail && valid_end > 0 && !final_record_had_newline {
            // The final record parsed but lost its newline in an interrupted
            // write; complete the line so the next append cannot merge into it.
            let repaired_len = valid_end.checked_add(1).ok_or_else(|| {
                SessionError::Limit("repaired session file length overflow".to_owned())
            })?;
            if repaired_len > max_file_bytes {
                return Err(SessionError::Limit(format!(
                    "repair would grow session to {repaired_len} bytes (limit {max_file_bytes})"
                )));
            }
            file.seek(std::io::SeekFrom::End(0))?;
            file.write_all(b"\n")?;
        }
        let persisted_len = file.metadata()?.len();
        FileExt::unlock(&file)?;
        let writer = Arc::new(SessionWriter::new(
            file.try_clone()?,
            persisted_len,
            persisted_records,
            recover_tail,
        ));
        let invocations = Arc::new(restored_invocations.attach_journal(Arc::clone(&writer)));
        let deferred_runs = Arc::new(restored_deferred_runs.attach_journal(Arc::clone(&writer)));
        Ok(Self {
            header,
            path,
            file,
            writer,
            invocations,
            invocation_entries,
            deferred_runs,
            entries,
            next_id,
            index,
            head,
            context_cache: RefCell::new(None),
            responses_replay_cache: RefCell::new(None),
            #[cfg(test)]
            responses_replay_work: std::cell::Cell::new((0, 0)),
            total_cost_microdollars,
            total_cost_picodollars_remainder,
            checkpoints,
            usage_records,
            usage_uncertainty_records,
            usage_uncertainty_bounds,
            cache_warm_records,
            entry_labels,
        })
    }

    /// Initialize the actual new file before any entries are appended.
    pub fn initialize_header(
        &mut self,
        cwd: &Path,
        parent_session: Option<String>,
    ) -> Result<(), SessionError> {
        if self.header.is_some() || !self.entries.is_empty() || self.file.metadata()?.len() != 0 {
            return Err(SessionError::Limit(
                "session header requires a new empty session".into(),
            ));
        }
        if parent_session
            .as_ref()
            .is_some_and(|path| path.len() > 4096 || path.chars().any(char::is_control))
        {
            return Err(SessionError::Limit(
                "invalid parent session reference".into(),
            ));
        }
        let header = SessionHeader {
            id: self
                .path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or_else(|| SessionError::Limit("session has no identifier".into()))?
                .to_owned(),
            cwd: cwd.to_owned(),
            timestamp_unix_ms: now_unix_millis(),
            parent_session,
        };
        let mut bytes = Vec::new();
        write_json_line(
            &mut bytes,
            &SessionRecord::Header {
                header: header.clone(),
            },
        )?;
        self.persist(&bytes)?;
        self.header = Some(header);
        Ok(())
    }

    /// Durable creation metadata, absent on historical sessions without it.
    pub fn header(&self) -> Option<&SessionHeader> {
        self.header.as_ref()
    }

    /// The path of the underlying JSONL file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Clones the already-authorized session descriptor for identity-stable
    /// inspection without reopening its path.
    pub(crate) fn try_clone_file(&self) -> std::io::Result<File> {
        self.file.try_clone()
    }

    /// Returns a stable, provider-safe cache-affinity key for this session.
    ///
    /// The key is derived from the full session path, so two sessions with the
    /// same filename in different workspaces cannot share a provider cache.
    /// Reopening the same file preserves the key across process restarts.
    pub fn cache_key(&self) -> String {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = FNV_OFFSET;
        for byte in self.path.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        format!("octet-{hash:016x}")
    }

    /// Returns the durable authorization namespace for extension-owned
    /// resources. Unlike the compact provider cache key, this uses SHA-256 of
    /// the canonical session descriptor path so alias paths converge and the
    /// collision bound is suitable for ownership checks.
    pub fn resource_owner_key(&self) -> String {
        let identity = self
            .path
            .canonicalize()
            .or_else(|_| std::path::absolute(&self.path))
            .unwrap_or_else(|_| self.path.clone());
        let digest = Sha256::digest(identity.to_string_lossy().as_bytes());
        format!("session-{digest:x}")
    }

    /// Append bytes only if this handle still reflects the complete file.
    ///
    /// The length check and write happen under one OS advisory lock. This is
    /// deliberately per-write rather than a lifetime lock: read-only session
    /// listing can still open active sessions, while a second writer fails
    /// before it can reuse stale entry IDs.
    pub(super) fn persist(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.writer.persist(bytes)
    }

    /// Serializes the durable head record naming `id` into `buf`.
    ///
    /// The head record is always written into the *same* buffer as the entry or
    /// usage record it belongs to, so one [`Session::persist`] makes the pair
    /// durable together and a crash can never leave a head that points at an
    /// entry which was not written, or an entry whose head was never advanced.
    /// The picodollar remainder is session-global, so it is read straight off
    /// the session rather than threaded through each call site.
    pub(super) fn write_head_record(
        &self,
        buf: &mut Vec<u8>,
        id: &EntryId,
        total_cost_microdollars: &u64,
    ) -> Result<(), SessionError> {
        write_json_line(
            buf,
            &SessionRecordRef::Head {
                id,
                total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append(&self) {
        self.writer.fail_next_append();
    }

    /// Issues a durable memo/checkpoint capability for one unresolved call in
    /// the current assistant batch. Identity is assistant entry + source index,
    /// so a provider's reused call ID never aliases another invocation.
    pub fn tool_invocation(&self, call_index: usize) -> Result<InvocationHandle, SessionError> {
        self.invocations
            .open(self.invocation_scope(call_index)?)
            .map_err(|e| SessionError::Limit(e.to_string()))
    }

    /// The durable store for suspended/effect-pending deferred runs.
    ///
    /// The store shares this session's descriptor-bound append line, so a
    /// parked leaf, a poll admitted before provider work, and a terminal
    /// tombstone are each one synced session record. It is replaceable state:
    /// the last record for one operation is authoritative on replay, never
    /// model-visible context and never usage accounting.
    pub fn deferred_run_store(&self) -> Arc<DeferredRunStore> {
        Arc::clone(&self.deferred_runs)
    }

    /// Durable state of one suspended deferred run, if any.
    pub fn deferred_run(&self, operation_id: &str) -> Option<DeferredRunRecord> {
        self.deferred_runs.record(operation_id)
    }

    /// Every durable deferred-run record, including terminal tombstones.
    pub fn deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.deferred_runs.records()
    }

    /// Every non-terminal deferred run that may still be resumed.
    pub fn parked_deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.deferred_runs.parked_records()
    }

    /// Recovery refusals may retain existing progress without allocating a
    /// pending-effect slot for a call that will never execute.
    pub(crate) fn invocation_partial_output(
        &self,
        call_index: usize,
    ) -> Result<Option<String>, SessionError> {
        self.invocations
            .partial_output_for_scope(&self.invocation_scope(call_index)?)
            .map_err(|e| SessionError::Limit(e.to_string()))
    }

    /// Reuse an immutable outcome from any branch without issuing a capability
    /// to execute the tool again. This also repairs a result whose head write
    /// was torn after the result entry itself became durable.
    pub(crate) fn persisted_invocation_result(
        &self,
        call_index: usize,
    ) -> Result<Option<(UserMessage, Option<EntryMetadata>)>, SessionError> {
        let scope = self.invocation_scope(call_index)?;
        let Some(&(entry, part)) = self.invocation_entries.results.get(&scope) else {
            return Ok(None);
        };
        let entry = &self.entries[entry];
        let EntryValue::Message(Message::User(user)) = &entry.value else {
            unreachable!("result index contains only user tool results");
        };
        Ok(Some((
            UserMessage {
                content: vec![user.content[part].clone()],
            },
            entry.metadata.clone(),
        )))
    }

    fn invocation_scope(&self, call_index: usize) -> Result<InvocationScope, SessionError> {
        let mut cursor = self.head_ref();
        let mut completed = std::collections::HashSet::new();
        while let Some(id) = cursor {
            let entry = self.entry(id).expect("session ancestry is valid");
            match &entry.value {
                EntryValue::Message(Message::Assistant(assistant)) => {
                    let call = assistant
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            octet_ai::AssistantPart::ToolCall(call) => Some(call),
                            _ => None,
                        })
                        .nth(call_index)
                        .ok_or_else(|| SessionError::Limit("unknown tool invocation".into()))?;
                    if completed.contains(&call.id) {
                        return Err(SessionError::Limit(
                            "tool invocation already settled".into(),
                        ));
                    }
                    return InvocationScope::new(id.0.clone(), call_index.to_string())
                        .map_err(|e| SessionError::Limit(e.to_string()));
                }
                EntryValue::Message(Message::User(user)) => {
                    for part in &user.content {
                        if let UserPart::ToolResult(result) = part {
                            completed.insert(result.tool_call_id.clone());
                        }
                    }
                }
                _ => {}
            }
            cursor = entry.parent.as_ref();
        }
        Err(SessionError::Limit(
            "no pending assistant tool batch".into(),
        ))
    }
}
