//! The whole `SessionStore` operation surface: how a workspace-scoped store is
//! created, how it enumerates and inspects its candidate sessions, how it reads
//! and writes per-session metadata, how it stages and commits deletions, and how
//! it hands a session path to the durable accounting ledger.
//!
//! These are the methods that act *on* a store instance, as opposed to the free
//! functions beside them that parse a transcript or classify a delegated handle.
//! They are one block because they share a single invariant - every mutation
//! goes through the same lock discipline and the same durable path selection - so
//! splitting them by verb would scatter that invariant across files without
//! making any of it easier to read.

use super::*;

impl SessionStore {
    /// Create a store rooted at `<session_dir>/<workspace-key>`.
    pub fn new(session_dir: &Path, workspace: &Path) -> Self {
        Self {
            dir: session_dir.join(workspace_key(workspace)),
            root: session_dir.to_path_buf(),
            workspace: Some(workspace.to_path_buf()),
        }
    }

    /// Create a store for an already-known workspace directory, recovering the
    /// workspace path from its `.workspace` marker when present.
    ///
    /// Used for cross-workspace browsing and mutation (the workspace-key hash
    /// is one-way, so the marker is the only way to learn which workspace a
    /// directory belongs to). Older binaries ignore the marker file.
    pub fn for_directory(dir: &Path, root: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            root: root.to_path_buf(),
            workspace: Self::read_workspace_marker(dir),
        }
    }

    /// The workspace-scoped session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The shared sessions root containing every workspace directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical workspace path, when this store knows it.
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Write the workspace path marker so future (and other) processes can
    /// display and scope sessions from this directory.
    pub fn write_workspace_marker(&self) -> anyhow::Result<()> {
        let Some(workspace) = self.workspace.as_ref() else {
            anyhow::bail!("store has no workspace to record");
        };
        // The private writer creates missing directories owned by the current
        // user. A plain `create_dir_all` here would let an elevated Windows
        // process create the session directory owned by the Administrators
        // group, which the private checks for the marker and session refuse.
        let marker = self.dir.join(WORKSPACE_MARKER);
        let bytes = format!("{}\n", workspace.display());
        // Only a validated owner-only, no-follow read may skip publication.
        // Missing, oversized, or insecure markers take the existing atomic
        // writer path, which repairs or rejects them as before.
        if octet_agent::secure_fs::read_private_file_bounded(&marker, bytes.len())
            .is_ok_and(|current| current == bytes.as_bytes())
        {
            return Ok(());
        }
        crate::auth::write_private_atomic(&marker, bytes.as_bytes(), ".workspace-")?;
        Ok(())
    }

    /// Read the workspace path marker from a workspace directory, if present.
    pub(crate) fn read_workspace_marker(dir: &Path) -> Option<PathBuf> {
        let bytes = std::fs::read(dir.join(WORKSPACE_MARKER)).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let path = text.lines().next()?.trim().to_owned();
        if path.is_empty() {
            return None;
        }
        Some(PathBuf::from(path))
    }

    /// List sessions across every workspace under the shared root, newest
    /// first. Each row carries its workspace path when the store marker is
    /// readable; otherwise `workspace` is `None`.
    #[allow(dead_code)]
    pub fn list_all(&self) -> Vec<SessionMeta> {
        let mut all = Vec::new();
        for entry in std::fs::read_dir(&self.root).into_iter().flatten() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let is_dir = match entry.file_type() {
                Ok(file_type) => file_type.is_dir(),
                Err(_) => false,
            };
            if !is_dir {
                continue;
            }
            let dir = entry.path();
            let store = if dir == self.dir {
                self.clone()
            } else {
                Self::for_directory(&dir, &self.root)
            };
            all.extend(store.list());
        }
        all.sort_by_key(|meta| std::cmp::Reverse(meta.modified));
        all
    }

    /// Allocate a new JSONL path. The caller supplies a timestamp for testability.
    pub fn new_path(&self, stamp: &str) -> PathBuf {
        let suffix = NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed);
        self.dir.join(format!("{stamp}-{suffix:04x}.jsonl"))
    }

    /// Bounded incremental entry search over this workspace's sessions.
    ///
    /// Only transcripts whose fingerprint changed since the last search are
    /// re-read; unchanged sessions are served from the disposable catalog, so a
    /// repeat search does not re-read transcript bytes. Reconciliation still
    /// enumerates/stats files: directory mtime cannot detect existing-file edits.
    pub fn search_entries(&self, query: &str, limit: usize) -> anyhow::Result<EntrySearchOutcome> {
        self.search_entries_with(query, limit, index_session_entries)
    }

    /// Inspect the entry-index revision while testing batch reconciliation.
    #[cfg(test)]
    pub fn entry_index_revision(&self) -> anyhow::Result<i64> {
        let catalog = SessionCatalog::open_recovering(&self.dir)?;
        catalog.entry_revision()
    }

    pub(crate) fn search_entries_with<F>(
        &self,
        query: &str,
        limit: usize,
        extractor: F,
    ) -> anyhow::Result<EntrySearchOutcome>
    where
        F: Fn(&Path) -> anyhow::Result<Vec<IndexedEntry>>,
    {
        const BATCH_SESSIONS: usize = 32;
        const BATCH_BYTES: usize = 8 * 1024 * 1024;
        let mut catalog = SessionCatalog::open_recovering(&self.dir)?;
        let mut indexed = catalog.entry_fingerprints()?;
        let mut scanned_sessions = 0;
        let mut index_changed = false;
        let mut updates = Vec::new();
        let mut removals = HashSet::new();
        let mut pending_bytes = 0;
        // A search has no recency-order requirement. Stream directory entries,
        // removing seen IDs from the old map rather than building/sorting an
        // additional workspace-sized candidate list and current-ID set.
        // Do not cache directory mtimes: appends/in-place edits do not change it.
        for candidate in self.unsorted_candidates() {
            let Some(id) = candidate
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let prior = indexed.remove(&id);
            let Some(fingerprint) = catalog_fingerprint(&candidate) else {
                if prior.is_some() {
                    removals.insert(id);
                }
                if updates.len() + removals.len() >= BATCH_SESSIONS {
                    index_changed |= catalog.apply_entries(&updates, &removals)?;
                    updates.clear();
                    removals.clear();
                    pending_bytes = 0;
                }
                continue;
            };
            if prior == Some(fingerprint) {
                continue;
            }
            match extractor(&candidate.path) {
                Ok(entries) => {
                    let bytes = entries
                        .iter()
                        .map(|entry| entry.text.len() + entry.entry_id.len())
                        .sum::<usize>();
                    if !updates.is_empty() && pending_bytes + bytes > BATCH_BYTES {
                        index_changed |= catalog.apply_entries(&updates, &removals)?;
                        updates.clear();
                        removals.clear();
                        pending_bytes = 0;
                    }
                    updates.push(IndexedEntryUpdate {
                        session_id: id,
                        fingerprint,
                        entries,
                    });
                    pending_bytes += bytes;
                    scanned_sessions += 1;
                }
                Err(_) => {
                    // Changed/unreadable transcripts must never retain old hits.
                    if prior.is_some() {
                        removals.insert(id);
                    }
                }
            }
            if updates.len() + removals.len() >= BATCH_SESSIONS || pending_bytes >= BATCH_BYTES {
                index_changed |= catalog.apply_entries(&updates, &removals)?;
                updates.clear();
                removals.clear();
                pending_bytes = 0;
            }
        }
        for id in indexed.into_keys() {
            removals.insert(id);
            if updates.len() + removals.len() >= BATCH_SESSIONS {
                index_changed |= catalog.apply_entries(&updates, &removals)?;
                updates.clear();
                removals.clear();
            }
        }
        index_changed |= catalog.apply_entries(&updates, &removals)?;
        let revision = catalog.entry_revision()?;
        let hits = catalog
            .search_entries(query, limit)?
            .into_iter()
            .map(|hit| EntrySearchHit {
                session_id: hit.session_id,
                entry_id: hit.entry_id,
                kind: match hit.kind {
                    IndexedEntryKind::User => EntryKind::User,
                    IndexedEntryKind::Assistant => EntryKind::Assistant,
                },
                text: hit.text,
            })
            .collect();
        Ok(EntrySearchOutcome {
            hits,
            index_changed,
            revision,
            scanned_sessions,
        })
    }

    /// Persist only the durable accounting for one ephemeral transcript.
    ///
    /// Reads the run's usage and unknown-usage records plus its cumulative cost
    /// and appends them to the workspace's accounting ledger. The conversation
    /// itself is never copied.
    #[cfg(test)]
    pub fn record_ephemeral_accounting(
        &self,
        transcript: &Path,
    ) -> anyhow::Result<EphemeralAccountingRecord> {
        let record = read_ephemeral_accounting(transcript)?;
        self.append_ephemeral_accounting(&record)?;
        Ok(record)
    }

    pub(super) fn append_ephemeral_accounting(
        &self,
        record: &EphemeralAccountingRecord,
    ) -> anyhow::Result<()> {
        let mut record = record.clone();
        record.retain_accounting_uncertainty();
        accounting_index::append(&self.dir.join(EPHEMERAL_ACCOUNTING_DIRECTORY), &record)
    }

    /// Aggregate durable accounting for every ephemeral run in this workspace.
    ///
    /// `has_uncertain_usage` is fail-closed: while any recorded run exposed
    /// unknown usage or absent exact pricing, the workspace total remains uncertain.
    pub fn ephemeral_accounting_summary(&self) -> anyhow::Result<EphemeralAccountingSummary> {
        let path = self
            .dir
            .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
            .join(EPHEMERAL_ACCOUNTING_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(EphemeralAccountingSummary::default())
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!(
                "ephemeral accounting ledger is {} bytes (limit {MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES})",
                bytes.len()
            );
        }
        let mut summary = EphemeralAccountingSummary::default();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(mut record) = serde_json::from_str::<EphemeralAccountingRecord>(line) else {
                continue;
            };
            record.retain_accounting_uncertainty();
            summary.runs += 1;
            summary.total_cost_microdollars = summary
                .total_cost_microdollars
                .saturating_add(record.session_cost_microdollars);
            summary.has_uncertain_usage |= record.has_uncertain_usage;
            summary.usage_records += record.usage_records.len();
            summary.uncertainty_records += record.usage_uncertainty_records.len();
            for usage in &record.usage_records {
                summary.input_tokens = summary
                    .input_tokens
                    .saturating_add(usage.usage.input_tokens);
                summary.output_tokens = summary
                    .output_tokens
                    .saturating_add(usage.usage.output_tokens);
            }
        }
        Ok(summary)
    }

    pub(super) fn unsorted_candidates(&self) -> impl Iterator<Item = SessionCandidate> {
        std::fs::read_dir(&self.dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                Some(SessionCandidate {
                    path,
                    modified,
                    file_size: metadata.len(),
                })
            })
    }

    pub(super) fn candidates(&self) -> Vec<SessionCandidate> {
        let mut candidates = self.unsorted_candidates().collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.modified));
        candidates
    }

    /// Lists safe regular JSONL filename stems without parsing transcript content.
    #[allow(dead_code)]
    pub(crate) fn session_file_ids(&self) -> Vec<String> {
        self.candidates()
            .into_iter()
            .filter_map(|candidate| {
                let id = candidate.path.file_stem()?.to_str()?.to_owned();
                session_id_is_valid(&id).then_some(id)
            })
            .collect()
    }

    /// Sort named, already-authorized session IDs by transcript mtime without
    /// enumerating or parsing other workspace sessions.
    #[allow(dead_code)]
    pub(crate) fn session_ids_newest_first<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        let mut candidates = ids
            .into_iter()
            .filter_map(|id| {
                self.candidate_by_id(id)
                    .ok()
                    .map(|candidate| (id.to_owned(), candidate.modified))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
        candidates.into_iter().map(|(id, _)| id).collect()
    }

    fn candidate_by_id(&self, id: &str) -> anyhow::Result<SessionCandidate> {
        let path = self.path_by_id(id)?;
        let metadata = path.symlink_metadata().map_err(|error| {
            anyhow::anyhow!("session {id:?} could not be inspected after lookup: {error}")
        })?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("session {id:?} is not a regular file");
        }
        Ok(SessionCandidate {
            path,
            modified: metadata.modified()?,
            file_size: metadata.len(),
        })
    }

    fn meta_from_parts(
        &self,
        candidate: SessionCandidate,
        id: String,
        fallback_title: String,
        metadata: SessionUserMetadata,
        message_count: usize,
    ) -> SessionMeta {
        let title = metadata
            .name
            .clone()
            .unwrap_or_else(|| fallback_title.clone());
        let modified = self
            .metadata_path(&id)
            .ok()
            .and_then(|path| path.symlink_metadata().ok())
            .filter(|metadata| metadata.file_type().is_file())
            .and_then(|metadata| metadata.modified().ok())
            .map_or(candidate.modified, |metadata_modified| {
                std::cmp::max(candidate.modified, metadata_modified)
            });
        SessionMeta {
            id,
            path: candidate.path,
            title,
            name: metadata.name,
            tags: metadata.tags,
            pinned: metadata.pinned,
            archived: metadata.archived,
            trashed_at_ms: metadata.trashed_at_ms,
            purge_after_ms: metadata.purge_after_ms,
            forked_from_session_id: metadata.forked_from_session_id,
            forked_from_entry_id: metadata.forked_from_entry_id,
            message_count,
            modified,
            workspace: self.workspace.clone(),
        }
    }

    fn inspect_candidate(
        &self,
        candidate: SessionCandidate,
        retain_usage_records: bool,
    ) -> anyhow::Result<SessionCatalogInspection> {
        let id = candidate
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|id| session_id_is_valid(id))
            .ok_or_else(|| anyhow::anyhow!("session has an invalid filename"))?
            .to_owned();
        let transcript = if retain_usage_records {
            summarize_session(&candidate.path)?
        } else {
            summarize_catalog_session(&candidate.path)?
        };
        let metadata = transcript
            .title
            .is_some()
            .then(|| self.load_metadata(&id))
            .transpose()?
            .unwrap_or_default();
        let meta = transcript.title.map(|title| {
            self.meta_from_parts(candidate, id, title, metadata, transcript.message_count)
        });
        Ok(SessionCatalogInspection {
            catalog: SessionCatalogEntry {
                meta,
                configured_model: transcript.configured_model,
                configured_reasoning: transcript.configured_reasoning,
            },
            usage_records: transcript.usage_records,
            usage_uncertainty_records: transcript.usage_uncertainty_records,
        })
    }

    /// Inspect one named transcript without enumerating or parsing unrelated
    /// sessions. The bounded scan validates its graph and torn tail before
    /// returning catalog and usage projections.
    #[allow(dead_code)]
    pub(crate) fn inspect_by_id(&self, id: &str) -> anyhow::Result<SessionCatalogInspection> {
        self.inspect_candidate(self.candidate_by_id(id)?, true)
    }

    fn catalog_entry_from_cached_summary(
        &self,
        candidate: SessionCandidate,
        id: String,
        summary: CachedTranscriptSummary,
    ) -> anyhow::Result<SessionCatalogEntry> {
        let CachedTranscriptSummary::Summary {
            title,
            configured_model,
            configured_reasoning,
            message_count,
        } = summary
        else {
            anyhow::bail!("session {id:?} is unreadable");
        };
        let metadata = title
            .as_ref()
            .map(|_| self.load_metadata(&id))
            .transpose()?
            .unwrap_or_default();
        Ok(SessionCatalogEntry {
            meta: title
                .map(|title| self.meta_from_parts(candidate, id, title, metadata, message_count)),
            configured_model,
            configured_reasoning,
        })
    }

    /// Load catalog metadata for one named transcript without scanning the
    /// workspace catalog.
    #[allow(dead_code)]
    pub(crate) fn catalog_by_id(&self, id: &str) -> anyhow::Result<SessionCatalogEntry> {
        self.catalog_by_ids([id])?
            .into_iter()
            .next()
            .map(|(_, entry)| entry)
            .ok_or_else(|| anyhow::anyhow!("session {id:?} is unavailable"))
    }

    /// Load catalog metadata for several already-authorized transcripts while
    /// opening the disposable catalog and reading its cached rows only once.
    ///
    /// The requested IDs are never expanded into a workspace-wide listing. A
    /// missing, invalid, or unreadable ID is omitted just as a failed targeted
    /// [`Self::catalog_by_id`] lookup is omitted by Serve callers.
    #[allow(dead_code)]
    pub(crate) fn catalog_by_ids<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> anyhow::Result<Vec<(String, SessionCatalogEntry)>> {
        let mut catalog = SessionCatalog::open_recovering(&self.dir)?;
        let mut updates = Vec::new();
        let mut entries = Vec::new();

        for id in ids {
            let Ok(candidate) = self.candidate_by_id(id) else {
                continue;
            };
            let fingerprint = catalog_fingerprint(&candidate);
            let cached = catalog.lookup(id)?;
            if let Some(summary) = fingerprint.and_then(|fingerprint| {
                cached
                    .as_ref()
                    .filter(|cached| cached.fingerprint == fingerprint)
                    .map(|cached| cached.summary.clone())
            }) {
                if let Ok(entry) =
                    self.catalog_entry_from_cached_summary(candidate, id.to_owned(), summary)
                {
                    entries.push((id.to_owned(), entry));
                }
                continue;
            }

            let Ok(inspection) = self.inspect_candidate(candidate, false) else {
                continue;
            };
            if let Some(fingerprint) = fingerprint {
                let summary = CachedTranscriptSummary::Summary {
                    title: inspection
                        .catalog
                        .meta
                        .as_ref()
                        .map(|meta| meta.title.clone()),
                    configured_model: inspection.catalog.configured_model.clone(),
                    configured_reasoning: inspection.catalog.configured_reasoning.clone(),
                    message_count: inspection
                        .catalog
                        .meta
                        .as_ref()
                        .map_or(0, |meta| meta.message_count),
                };
                updates.push(CatalogUpdate {
                    id: id.to_owned(),
                    fingerprint,
                    summary,
                });
            }
            entries.push((id.to_owned(), inspection.catalog));
        }

        catalog.apply(&updates, &HashSet::new())?;
        Ok(entries)
    }

    /// Build catalog metadata from the already authorized, fully replayed
    /// session rather than reopening its pathname.
    #[allow(dead_code)]
    pub(crate) fn meta_for_open_session(
        &self,
        id: &str,
        session: &Session,
    ) -> anyhow::Result<Option<SessionMeta>> {
        let candidate = self.candidate_by_id(id)?;
        if absolute_read_path(session.path())? != absolute_read_path(&candidate.path)? {
            anyhow::bail!("opened session does not match requested session id {id:?}");
        }
        let Some(title) = active_branch_catalog_title(session) else {
            return Ok(None);
        };
        Ok(Some(self.meta_from_parts(
            candidate,
            id.to_owned(),
            title,
            self.load_metadata(id)?,
            active_branch_message_count(session),
        )))
    }

    /// Refresh the disposable title projection from an already replayed session.
    /// This keeps a normally closed session warm without reopening its JSONL.
    pub(crate) fn refresh_catalog_for_open_session(&self, session: &Session) -> anyhow::Result<()> {
        let id = session
            .path()
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("opened session path has no UTF-8 filename stem"))?;
        let candidate = self.candidate_by_id(id)?;
        if absolute_read_path(session.path())? != absolute_read_path(&candidate.path)? {
            anyhow::bail!("opened session does not belong to this workspace store");
        }
        let fingerprint = catalog_fingerprint(&candidate)
            .ok_or_else(|| anyhow::anyhow!("session fingerprint is outside catalog bounds"))?;
        let (configured_model, configured_reasoning) = active_branch_catalog_config(session);
        let update = CatalogUpdate {
            id: id.to_owned(),
            fingerprint,
            summary: CachedTranscriptSummary::Summary {
                title: active_branch_catalog_title(session),
                configured_model,
                configured_reasoning,
                message_count: active_branch_message_count(session),
            },
        };
        let mut catalog = SessionCatalog::open_recovering(&self.dir)?;
        catalog.apply(&[update], &HashSet::new())
    }

    /// Remove one disposable row after its authoritative transcript is removed.
    pub(crate) fn remove_catalog_entry(&self, id: &str) -> anyhow::Result<()> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        if !SessionCatalog::exists(&self.dir) {
            return Ok(());
        }
        let mut catalog = SessionCatalog::open_recovering(&self.dir)?;
        catalog.apply(&[], &HashSet::from([id.to_owned()]))
    }

    /// Load one session's validated catalog metadata without scanning unrelated
    /// transcripts.
    #[cfg(test)]
    pub(crate) fn get_by_id(&self, id: &str) -> anyhow::Result<Option<SessionMeta>> {
        Ok(self.catalog_by_id(id)?.meta)
    }

    fn meta_from_cached_summary(
        &self,
        candidate: SessionCandidate,
        id: String,
        summary: CachedTranscriptSummary,
    ) -> Option<SessionMeta> {
        let (fallback_title, message_count) = match summary {
            CachedTranscriptSummary::Summary {
                title,
                message_count,
                ..
            } => (title?, message_count),
            CachedTranscriptSummary::Unreadable => ("(unreadable session)".to_owned(), 0),
        };
        let metadata = self.load_metadata(&id).unwrap_or_default();
        Some(self.meta_from_parts(candidate, id, fallback_title, metadata, message_count))
    }

    pub(super) fn discover_with_summarizer<F>(
        &self,
        candidates: Vec<SessionCandidate>,
        first_only: bool,
        summarizer: F,
    ) -> Vec<SessionMeta>
    where
        F: Fn(&Path) -> anyhow::Result<TranscriptSummary>,
    {
        let mut catalog = SessionCatalog::open_recovering(&self.dir).ok();
        // Latest only needs a valid newest entry. Catalog garbage collection
        // belongs to full listing, not the first-paint resume selector.
        if !first_only {
            let current_ids = candidates
                .iter()
                .filter_map(|candidate| {
                    candidate
                        .path
                        .file_stem()
                        .and_then(|value| value.to_str())
                        .map(str::to_owned)
                })
                .collect::<HashSet<_>>();
            if let Some(catalog) = &mut catalog {
                if let Ok(cached_ids) = catalog.session_ids() {
                    let stale_ids = cached_ids.difference(&current_ids).cloned().collect();
                    let _ = catalog.apply(&[], &stale_ids);
                }
            }
        }
        let mut updates = Vec::new();
        let mut discovered = Vec::new();
        // A latest lookup normally inspects one candidate. Heap construction
        // is linear, and only rejected newer transcripts pay for another pop;
        // listing retains its stable newest-first sort and full projection.
        let order: Box<dyn Iterator<Item = usize>> = if first_only {
            let mut heap = BinaryHeap::from(
                candidates
                    .iter()
                    .enumerate()
                    .map(|(index, candidate)| (candidate.modified, std::cmp::Reverse(index), index))
                    .collect::<Vec<_>>(),
            );
            Box::new(std::iter::from_fn(move || {
                heap.pop().map(|(_, _, index)| index)
            }))
        } else {
            Box::new(0..candidates.len())
        };
        let mut candidates = candidates.into_iter().map(Some).collect::<Vec<_>>();
        for index in order {
            let candidate = candidates[index]
                .take()
                .expect("each candidate visited once");
            let Some(id) = candidate
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let fingerprint = catalog_fingerprint(&candidate);
            let cached = catalog
                .as_ref()
                .and_then(|catalog| catalog.lookup(&id).ok().flatten());
            let summary = fingerprint
                .and_then(|fingerprint| {
                    cached
                        .as_ref()
                        .filter(|cached| cached.fingerprint == fingerprint)
                })
                .map(|cached| cached.summary.clone())
                .unwrap_or_else(|| match summarizer(&candidate.path) {
                    Ok(transcript) => {
                        let summary = CachedTranscriptSummary::Summary {
                            title: transcript.title,
                            configured_model: transcript.configured_model,
                            configured_reasoning: transcript.configured_reasoning,
                            message_count: transcript.message_count,
                        };
                        if let Some(fingerprint) = fingerprint.filter(|_| catalog.is_some()) {
                            updates.push(CatalogUpdate {
                                id: id.clone(),
                                fingerprint,
                                summary: summary.clone(),
                            });
                        }
                        summary
                    }
                    // I/O failures can be transient, so unreadable projections
                    // are shown but deliberately not retained in the catalog.
                    Err(_) => CachedTranscriptSummary::Unreadable,
                });
            if updates.len() >= 32 {
                if let Some(catalog) = &mut catalog {
                    let _ = catalog.apply(&updates, &HashSet::new());
                }
                updates.clear();
            }
            if let Some(meta) = self.meta_from_cached_summary(candidate, id, summary) {
                discovered.push(meta);
                if first_only {
                    break;
                }
            }
        }

        if let Some(mut catalog) = catalog {
            let _ = catalog.apply(&updates, &HashSet::new());
        }
        discovered
    }

    /// List sessions newest-first by filesystem modification time.
    pub fn list(&self) -> Vec<SessionMeta> {
        let candidates = self.candidates();
        if candidates.is_empty() && !SessionCatalog::exists(&self.dir) {
            return Vec::new();
        }
        self.discover_with_summarizer(candidates, false, summarize_catalog_session)
    }

    /// Return the newest session or an actionable error when none exists.
    pub fn latest(&self) -> anyhow::Result<SessionMeta> {
        let candidates = self.unsorted_candidates().collect::<Vec<_>>();
        if candidates.is_empty() && !SessionCatalog::exists(&self.dir) {
            anyhow::bail!("no sessions for this workspace yet");
        }
        self.discover_with_summarizer(candidates, true, summarize_catalog_session)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no sessions for this workspace yet"))
    }

    /// Reports whether the canonical transcript currently exists.
    ///
    /// A non-regular entry is an error, not absence. Permanent-deletion
    /// recovery uses this distinction so it never crosses the irreversible
    /// boundary merely because an existing transcript could not be validated.
    #[allow(dead_code)]
    pub fn session_file_exists(&self, id: &str) -> anyhow::Result<bool> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        let path = self.dir.join(format!("{id}.jsonl"));
        match path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => Ok(true),
            Ok(_) => anyhow::bail!("session {id:?} is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(anyhow::anyhow!(
                "session {id:?} could not be inspected: {error}"
            )),
        }
    }

    /// Resolve a filename stem without enumerating or parsing unrelated sessions.
    ///
    /// A session-owned worker handle (`agent-session:<sha256>`) is resolved
    /// through this session's durable delegation roster instead of a flat
    /// `<session-dir>/<id>.jsonl` join, so `octet --resume <handle>` can open a
    /// detached delegated child as its own interactive session. Every
    /// non-launchable handle fails closed with a typed, bounded
    /// [`DelegatedHandleRefusal`]; an ordinary session id keeps exactly its
    /// previous resolution path.
    pub fn path_by_id(&self, id: &str) -> anyhow::Result<PathBuf> {
        if id.starts_with(DELEGATED_SESSION_HANDLE_PREFIX) {
            return self.path_for_delegated_handle(id);
        }
        if !self.session_file_exists(id)? {
            anyhow::bail!("session {id:?} was not found");
        }
        Ok(self.dir.join(format!("{id}.jsonl")))
    }

    /// Resolve one launchable session-owned worker handle to its transcript.
    ///
    /// The handle is the only reference an extension ever receives for a
    /// session-owned delegated child (`octet_agent::delegated_session_reference`),
    /// and it is deliberately path-free, argv-safe, and credential-free. It is
    /// resolved through `octet_agent::delegation::resolve_launchable_child_session`,
    /// which needs no live agent:
    ///
    /// 1. the token is validated *before any filesystem work*, so a shell
    ///    metacharacter, a control byte, or a path component can never reach a
    ///    path join;
    /// 2. the owning session's durable roster is read, and a parked
    ///    (`awaiting_approval`) worker, an unknown handle, a missing roster, and
    ///    a vanished transcript each refuse with their own bounded reason;
    /// 3. a worker the roster still records as live (`pending`/`running`) is
    ///    refused here too: process-local liveness cannot be read from the
    ///    roster, but a live record means another process owns this transcript,
    ///    and one session has one writer;
    /// 4. the resolved path is confined to this store's private delegation
    ///    directory, so a forged roster entry cannot escape it.
    pub fn path_for_delegated_handle(&self, handle: &str) -> anyhow::Result<PathBuf> {
        if delegated_handle_digest(handle).is_none() {
            return Err(DelegatedHandleRefusal::MalformedHandle.into());
        }
        let delegation_directory = self.dir.join(DELEGATION_DIRECTORY);
        let resolved = octet_agent::delegation::resolve_launchable_child_session(
            &delegation_directory,
            handle,
        )
        .map_err(|error| anyhow::Error::from(classify_delegated_handle_error(&error)))?;
        if let Some(status) = live_worker_state(&resolved.status) {
            return Err(DelegatedHandleRefusal::LiveInOwningProcess { status }.into());
        }
        confine_delegated_session_path(&delegation_directory, &resolved.session_path)?;
        Ok(resolved.session_path)
    }

    pub(super) fn metadata_dir(&self) -> PathBuf {
        self.dir.join(".metadata")
    }

    pub(super) fn metadata_path(&self, id: &str) -> anyhow::Result<PathBuf> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        Ok(self.metadata_dir().join(format!("{id}.json")))
    }

    /// Read optional user-owned session catalog metadata.
    pub fn load_metadata(&self, id: &str) -> anyhow::Result<SessionUserMetadata> {
        let path = self.metadata_path(id)?;
        let metadata_dir = self.metadata_dir();
        match metadata_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => anyhow::bail!(
                "session metadata directory is not a real directory: {}",
                metadata_dir.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionUserMetadata::default());
            }
            Err(error) => return Err(error.into()),
        }

        let bytes = match crate::auth::read_bounded_private(&path, MAX_SESSION_METADATA_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(SessionUserMetadata::default()),
            Err(error) => anyhow::bail!("cannot read session metadata {}: {error}", path.display()),
        };
        let parsed: SessionUserMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!("invalid session metadata {}: {error}", path.display())
        })?;
        let metadata = SessionUserMetadata {
            name: parsed
                .name
                .as_deref()
                .map(sanitize_session_name)
                .transpose()?
                .flatten(),
            tags: sanitize_session_tags(&parsed.tags)?,
            pinned: parsed.pinned,
            archived: parsed.archived,
            trashed_at_ms: parsed.trashed_at_ms,
            purge_after_ms: parsed.purge_after_ms,
            forked_from_session_id: parsed.forked_from_session_id,
            forked_from_entry_id: parsed.forked_from_entry_id,
        };
        validate_session_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Atomically replace user-owned catalog metadata. The target session must exist.
    pub fn save_metadata(&self, id: &str, metadata: &SessionUserMetadata) -> anyhow::Result<()> {
        self.path_by_id(id)?;
        let metadata = SessionUserMetadata {
            name: metadata
                .name
                .as_deref()
                .map(sanitize_session_name)
                .transpose()?
                .flatten(),
            tags: sanitize_session_tags(&metadata.tags)?,
            pinned: metadata.pinned,
            archived: metadata.archived,
            trashed_at_ms: metadata.trashed_at_ms,
            purge_after_ms: metadata.purge_after_ms,
            forked_from_session_id: metadata.forked_from_session_id.clone(),
            forked_from_entry_id: metadata.forked_from_entry_id.clone(),
        };
        validate_session_metadata(&metadata)?;
        let bytes = serde_json::to_vec_pretty(&metadata)?;
        if bytes.len() > MAX_SESSION_METADATA_BYTES {
            anyhow::bail!("session metadata exceeds {MAX_SESSION_METADATA_BYTES} bytes");
        }
        crate::auth::write_private_atomic(&self.metadata_path(id)?, &bytes, ".session-metadata-")
    }

    pub fn rename(&self, id: &str, name: &str) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.name = sanitize_session_name(name)?;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    pub fn set_tags(&self, id: &str, tags: Vec<String>) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.tags = sanitize_session_tags(&tags)?;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[allow(dead_code)]
    pub fn set_pinned(&self, id: &str, pinned: bool) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.pinned = pinned;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[allow(dead_code)]
    pub fn set_archived(&self, id: &str, archived: bool) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.archived = archived;
        metadata.trashed_at_ms = None;
        metadata.purge_after_ms = None;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[allow(dead_code)]
    pub fn set_lifecycle(
        &self,
        id: &str,
        lifecycle: SessionStorageLifecycle,
        changed_at_ms: u64,
    ) -> anyhow::Result<SessionUserMetadata> {
        if changed_at_ms == 0 {
            anyhow::bail!("session lifecycle timestamp must be positive");
        }
        let mut metadata = self.load_metadata(id)?;
        match lifecycle {
            SessionStorageLifecycle::Active => {
                metadata.archived = false;
                metadata.trashed_at_ms = None;
                metadata.purge_after_ms = None;
            }
            SessionStorageLifecycle::Archived => {
                metadata.archived = true;
                metadata.trashed_at_ms = None;
                metadata.purge_after_ms = None;
            }
            SessionStorageLifecycle::Trash => {
                metadata.archived = true;
                metadata.pinned = false;
                if metadata.trashed_at_ms.is_none() {
                    metadata.trashed_at_ms = Some(changed_at_ms);
                    metadata.purge_after_ms = changed_at_ms.checked_add(SESSION_TRASH_RETENTION_MS);
                }
                if metadata.purge_after_ms.is_none() {
                    anyhow::bail!("session trash retention timestamp overflow");
                }
            }
        }
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[allow(dead_code)]
    pub fn set_fork_provenance(
        &self,
        id: &str,
        source_session_id: &str,
        source_entry_id: &str,
    ) -> anyhow::Result<SessionUserMetadata> {
        if !session_id_is_valid(source_session_id)
            || source_entry_id.is_empty()
            || source_entry_id.len() > 256
            || source_entry_id.chars().any(char::is_control)
        {
            anyhow::bail!("invalid session fork provenance");
        }
        let mut metadata = self.load_metadata(id)?;
        metadata.forked_from_session_id = Some(source_session_id.to_owned());
        metadata.forked_from_entry_id = Some(source_entry_id.to_owned());
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[allow(dead_code)]
    pub fn delete_permanently(&self, id: &str, expected_trashed_at_ms: u64) -> anyhow::Result<()> {
        let metadata = self.load_metadata(id)?;
        if metadata.trashed_at_ms != Some(expected_trashed_at_ms) {
            anyhow::bail!("session trash confirmation is stale");
        }
        let session_path = self.path_by_id(id)?;
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) => return Err(error.into()),
        }
        let suffix = NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed);
        let staged_session = self.dir.join(format!(".delete-{id}-{suffix:016x}"));
        let staged_metadata = self
            .metadata_dir()
            .join(format!(".delete-{id}-{suffix:016x}"));

        std::fs::rename(&session_path, &staged_session)?;
        if !staged_session
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            let _ = std::fs::rename(&staged_session, &session_path);
            anyhow::bail!("staged session transcript is not a regular file");
        }
        if let Err(error) = std::fs::rename(&metadata_path, &staged_metadata) {
            let _ = std::fs::rename(&staged_session, &session_path);
            return Err(error.into());
        }
        if !staged_metadata
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            let _ = std::fs::rename(&staged_metadata, &metadata_path);
            let _ = std::fs::rename(&staged_session, &session_path);
            anyhow::bail!("staged session metadata is not a regular file");
        }
        if let Err(error) = std::fs::remove_file(&staged_session) {
            let _ = std::fs::rename(&staged_metadata, &metadata_path);
            let _ = std::fs::rename(&staged_session, &session_path);
            return Err(error.into());
        }
        std::fs::remove_file(&staged_metadata)?;
        self.finish_permanent_delete(id)
    }

    /// Rolls back an interrupted permanent deletion while the canonical
    /// transcript still exists.
    ///
    /// The intent journal is written before the transcript rename. If a crash
    /// occurs before the irreversible transcript-removal boundary, metadata may
    /// already have been staged. This restores that metadata and removes only
    /// deletion staging files, making pre-commit recovery idempotent.
    #[allow(dead_code)]
    pub fn rollback_permanent_delete(&self, id: &str) -> anyhow::Result<()> {
        self.path_by_id(id)?;
        let metadata_dir = self.metadata_dir();
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let staged = staged_deletion_files(&metadata_dir, id)?;
                let [staged_metadata] = staged.as_slice() else {
                    anyhow::bail!("interrupted session metadata cannot be restored");
                };
                std::fs::rename(staged_metadata, &metadata_path)?;
                sync_directory(&metadata_dir)?;
            }
            Err(error) => return Err(error.into()),
        }

        remove_staged_deletion_files(&self.dir, id)?;
        remove_staged_deletion_files(&metadata_dir, id)?;
        sync_directory(&self.dir)?;
        sync_directory(&metadata_dir)?;
        Ok(())
    }

    /// Finishes an already-confirmed permanent deletion after interruption.
    ///
    /// This idempotently removes both canonical files and transaction staging
    /// files. Callers must establish the destructive confirmation boundary
    /// before invoking it.
    #[allow(dead_code)]
    pub fn finish_permanent_delete(&self, id: &str) -> anyhow::Result<()> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session ID");
        }
        remove_regular_file_if_exists(&self.dir.join(format!("{id}.jsonl")))?;
        remove_regular_file_if_exists(&self.metadata_path(id)?)?;
        remove_staged_deletion_files(&self.dir, id)?;
        let metadata_dir = self.metadata_dir();
        remove_staged_deletion_files(&metadata_dir, id)?;
        sync_directory(&self.dir)?;
        sync_directory(&metadata_dir)?;
        let _ = self.remove_catalog_entry(id);
        Ok(())
    }

    /// Removes a just-created session and sidecar during a higher-level
    /// transaction rollback. This is intentionally not a user-facing delete
    /// path and must only be used before the new session is acknowledged.
    #[allow(dead_code)]
    pub fn discard_unacknowledged(&self, id: &str) -> anyhow::Result<()> {
        let session_path = self.path_by_id(id)?;
        std::fs::remove_file(session_path)?;
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(metadata_path)?;
            }
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let _ = self.remove_catalog_entry(id);
        Ok(())
    }
}
