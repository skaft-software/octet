//! Disposable SQLite acceleration for workspace-scoped session discovery.
//!
//! JSONL transcripts remain authoritative. The catalog stores only the bounded
//! title projection and a filesystem fingerprint, so any missing, stale, or
//! unusable row can be rebuilt without changing session data.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::types::Type;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

const CATALOG_DIRECTORY: &str = ".catalog";
const CATALOG_FILE: &str = "sessions-v1.sqlite3";
const CATALOG_SCHEMA_VERSION: i64 = 6;
// Bound SQLite resident pages, not the complete disposable on-disk index.
const CATALOG_CACHE_KIB: i64 = 4 * 1024;
const STATUS_SUMMARY: i64 = 0;
const STATUS_UNREADABLE: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CatalogFingerprint {
    pub(crate) file_size: u64,
    pub(crate) modified_ns: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CachedTranscriptSummary {
    Summary {
        title: Option<String>,
        configured_model: Option<String>,
        configured_reasoning: Option<String>,
        message_count: usize,
    },
    Unreadable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedSession {
    pub(crate) fingerprint: CatalogFingerprint,
    pub(crate) summary: CachedTranscriptSummary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CatalogUpdate {
    pub(crate) id: String,
    pub(crate) fingerprint: CatalogFingerprint,
    pub(crate) summary: CachedTranscriptSummary,
}

/// Which conversation role produced an indexed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IndexedEntryKind {
    User,
    Assistant,
}

impl IndexedEntryKind {
    fn as_status(self) -> i64 {
        match self {
            Self::User => ENTRY_KIND_USER,
            Self::Assistant => ENTRY_KIND_ASSISTANT,
        }
    }

    fn from_status(status: i64) -> Option<Self> {
        match status {
            ENTRY_KIND_USER => Some(Self::User),
            ENTRY_KIND_ASSISTANT => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// One bounded, user-visible transcript entry projection. Only submitted user
/// text and assistant-visible text are retained; reasoning, tool arguments,
/// media and provider metadata are never indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexedEntry {
    pub(crate) entry_id: String,
    pub(crate) kind: IndexedEntryKind,
    pub(crate) text: String,
}

/// One incremental refresh of a single session's entry projection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexedEntryUpdate {
    pub(crate) session_id: String,
    pub(crate) fingerprint: CatalogFingerprint,
    pub(crate) entries: Vec<IndexedEntry>,
}

/// One entry-level search hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IndexedEntryHit {
    pub(crate) session_id: String,
    pub(crate) entry_id: String,
    pub(crate) kind: IndexedEntryKind,
    /// Declaration order of the entry within its session.
    pub(crate) ordinal: usize,
    pub(crate) text: String,
}
/// Which conversation role produced an indexed entry.
const ENTRY_KIND_USER: i64 = 0;
const ENTRY_KIND_ASSISTANT: i64 = 1;
/// Hard bound for indexed searchable text in one entry.
pub(crate) const MAX_INDEXED_ENTRY_CHARS: usize = 512;
/// Hard bound for indexed entries per session.
pub(crate) const MAX_INDEXED_ENTRIES_PER_SESSION: usize = 4_096;
/// Bound the amplification of the bounded text projection into B-tree postings.
/// Overflow sessions keep *all* projected text, but are searched by LIKE instead.
const MAX_POSTINGS_PER_SESSION: usize = 512 * 1024;

pub(crate) struct SessionCatalog {
    connection: Connection,
    #[cfg(test)]
    search_steps: std::cell::Cell<i32>,
}

impl SessionCatalog {
    pub(crate) fn open_recovering(workspace_store: &Path) -> anyhow::Result<Self> {
        match Self::open(workspace_store) {
            Ok(catalog) => Ok(catalog),
            Err(error) if catalog_error_is_rebuildable(&error) => {
                reset_catalog_files(workspace_store)?;
                Self::open(workspace_store)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) fn open(workspace_store: &Path) -> anyhow::Result<Self> {
        if !workspace_store.is_absolute() {
            anyhow::bail!("session catalog path must be absolute");
        }
        let directory = workspace_store.join(CATALOG_DIRECTORY);
        octet_agent::secure_fs::create_private_directory_all(&directory)?;
        // SQLite's NOFOLLOW flag rejects paths containing an intermediate
        // symlink (for example macOS' /var -> /private/var). Resolve the
        // directory only after the secure path walk has created and validated
        // it, then keep symlink following disabled for the database open.
        let path = directory.canonicalize()?.join(CATALOG_FILE);
        prepare_private_database_file(&path)?;

        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(Duration::from_millis(100))?;
        // Check forward compatibility before any persistent PRAGMA. In
        // particular, changing journal_mode would rewrite a future catalog
        // before this binary has established that it understands the schema.
        let schema_version: i64 =
            connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema_version > CATALOG_SCHEMA_VERSION {
            anyhow::bail!(
                "session catalog schema {schema_version} is newer than supported schema {CATALOG_SCHEMA_VERSION}"
            );
        }

        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "temp_store", "FILE")?;
        connection.pragma_update(None, "cache_size", -CATALOG_CACHE_KIB)?;
        connection.pragma_update(None, "mmap_size", 0)?;
        connection.pragma_update(None, "journal_size_limit", 4 * 1024 * 1024)?;
        connection.pragma_update(None, "trusted_schema", false)?;

        if schema_version < CATALOG_SCHEMA_VERSION {
            connection.execute_batch(
                "DROP TABLE IF EXISTS indexed_entry_gram_counts;
                 DROP TABLE IF EXISTS indexed_entry_grams;
                 DROP TABLE IF EXISTS indexed_entries;
                 DROP TABLE IF EXISTS indexed_entry_sessions;
                 DROP TABLE IF EXISTS sessions;
                 CREATE TABLE sessions (
                     id TEXT PRIMARY KEY NOT NULL,
                     file_size INTEGER NOT NULL CHECK (file_size >= 0),
                     modified_ns INTEGER NOT NULL CHECK (modified_ns >= 0),
                     status INTEGER NOT NULL CHECK (status IN (0, 1)),
                     title TEXT,
                     configured_model TEXT,
                     configured_reasoning TEXT,
                     message_count INTEGER NOT NULL CHECK (message_count >= 0)
                 ) WITHOUT ROWID;",
            )?;
        } else {
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (
                     id TEXT PRIMARY KEY NOT NULL,
                     file_size INTEGER NOT NULL CHECK (file_size >= 0),
                     modified_ns INTEGER NOT NULL CHECK (modified_ns >= 0),
                     status INTEGER NOT NULL CHECK (status IN (0, 1)),
                     title TEXT,
                     configured_model TEXT,
                     configured_reasoning TEXT,
                     message_count INTEGER NOT NULL CHECK (message_count >= 0)
                 ) WITHOUT ROWID;",
            )?;
        }

        // Entry-level search projection. It accelerates
        // bounded incremental entry search and is rebuilt from JSONL whenever a
        // session's fingerprint changes; it is still disposable.
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS indexed_entries (
                 session_id TEXT NOT NULL,
                 entry_id TEXT NOT NULL,
                 ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
                 kind INTEGER NOT NULL CHECK (kind IN (0, 1)),
                 text TEXT NOT NULL,
                 PRIMARY KEY (session_id, entry_id)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS indexed_entries_order ON indexed_entries(session_id, ordinal);
             CREATE TABLE IF NOT EXISTS indexed_entry_grams (
                 gram TEXT NOT NULL,
                 session_id TEXT NOT NULL,
                 ordinal INTEGER NOT NULL,
                 entry_id TEXT NOT NULL,
                 PRIMARY KEY (gram, session_id, ordinal)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS indexed_entry_grams_session ON indexed_entry_grams(session_id);
             CREATE TABLE IF NOT EXISTS indexed_entry_sessions (
                 session_id TEXT PRIMARY KEY NOT NULL,
                 file_size INTEGER NOT NULL CHECK (file_size >= 0),
                 modified_ns INTEGER NOT NULL CHECK (modified_ns >= 0),
                 postings_complete INTEGER NOT NULL CHECK (postings_complete IN (0, 1))
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS indexed_entry_fallback ON indexed_entry_sessions(postings_complete, session_id);
             CREATE TABLE IF NOT EXISTS indexed_entry_gram_counts (
                 gram TEXT PRIMARY KEY NOT NULL,
                 postings INTEGER NOT NULL CHECK (postings > 0)
             ) WITHOUT ROWID;
             CREATE TRIGGER IF NOT EXISTS indexed_entry_gram_insert AFTER INSERT ON indexed_entry_grams BEGIN
                 INSERT INTO indexed_entry_gram_counts (gram, postings) VALUES (NEW.gram, 1)
                 ON CONFLICT(gram) DO UPDATE SET postings = postings + 1;
             END;
             CREATE TRIGGER IF NOT EXISTS indexed_entry_gram_delete AFTER DELETE ON indexed_entry_grams BEGIN
                 DELETE FROM indexed_entry_gram_counts WHERE gram = OLD.gram AND postings = 1;
                 UPDATE indexed_entry_gram_counts SET postings = postings - 1 WHERE gram = OLD.gram;
             END;
             CREATE TABLE IF NOT EXISTS catalog_meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value INTEGER NOT NULL
             ) WITHOUT ROWID;
             INSERT OR IGNORE INTO catalog_meta (key, value) VALUES ('entry_revision', 1);
             PRAGMA user_version = 6;",
        )?;

        Ok(Self {
            connection,
            #[cfg(test)]
            search_steps: std::cell::Cell::new(0),
        })
    }

    #[cfg(test)]
    pub(crate) fn load(&self) -> anyhow::Result<HashMap<String, CachedSession>> {
        self.load_selected(None)
    }

    pub(crate) fn session_ids(&self) -> anyhow::Result<HashSet<String>> {
        self.connection
            .prepare("SELECT id FROM sessions")?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()
            .map_err(Into::into)
    }

    pub(crate) fn lookup(&self, id: &str) -> anyhow::Result<Option<CachedSession>> {
        Ok(self.load_selected(Some(id))?.remove(id))
    }

    fn load_selected(&self, id: Option<&str>) -> anyhow::Result<HashMap<String, CachedSession>> {
        let select = "SELECT id, file_size, modified_ns, status, title, configured_model, configured_reasoning, message_count FROM sessions";
        let sql = if id.is_some() {
            format!("{select} WHERE id = ?1")
        } else {
            select.to_owned()
        };
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(rusqlite::params_from_iter(id), |row| {
            let configured_reasoning_type = row.get_ref(6)?.data_type();
            let configured_reasoning = match configured_reasoning_type {
                Type::Null => None,
                Type::Text => Some(row.get::<_, String>(6)?),
                _ => return Ok(None),
            };
            Ok(Some((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                configured_reasoning,
                row.get::<_, i64>(7)?,
            )))
        })?;
        let mut sessions = HashMap::new();
        for row in rows {
            let Some((
                id,
                file_size,
                modified_ns,
                status,
                title,
                configured_model,
                configured_reasoning,
                message_count,
            )) = row?
            else {
                continue;
            };
            let Ok(message_count) = usize::try_from(message_count) else {
                continue;
            };
            let Ok(file_size) = u64::try_from(file_size) else {
                continue;
            };
            let summary = match status {
                STATUS_SUMMARY => CachedTranscriptSummary::Summary {
                    title,
                    configured_model,
                    configured_reasoning,
                    message_count,
                },
                STATUS_UNREADABLE => CachedTranscriptSummary::Unreadable,
                _ => continue,
            };
            sessions.insert(
                id,
                CachedSession {
                    fingerprint: CatalogFingerprint {
                        file_size,
                        modified_ns,
                    },
                    summary,
                },
            );
        }
        Ok(sessions)
    }

    pub(crate) fn apply(
        &mut self,
        updates: &[CatalogUpdate],
        stale_ids: &HashSet<String>,
    ) -> anyhow::Result<()> {
        if updates.is_empty() && stale_ids.is_empty() {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        {
            let mut upsert = transaction.prepare(
                "INSERT INTO sessions (id, file_size, modified_ns, status, title, configured_model, configured_reasoning, message_count)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(id) DO UPDATE SET
                     file_size = excluded.file_size,
                     modified_ns = excluded.modified_ns,
                     status = excluded.status,
                     title = excluded.title,
                     configured_model = excluded.configured_model,
                     configured_reasoning = excluded.configured_reasoning,
                      message_count = excluded.message_count",
            )?;
            for update in updates {
                let file_size = i64::try_from(update.fingerprint.file_size)
                    .map_err(|_| anyhow::anyhow!("session size does not fit SQLite INTEGER"))?;
                let (status, title, configured_model, configured_reasoning, message_count) =
                    match &update.summary {
                        CachedTranscriptSummary::Summary {
                            title,
                            configured_model,
                            configured_reasoning,
                            message_count,
                        } => (
                            STATUS_SUMMARY,
                            title.as_deref(),
                            configured_model.as_deref(),
                            configured_reasoning.as_deref(),
                            i64::try_from(*message_count).map_err(|_| {
                                anyhow::anyhow!("session message count does not fit SQLite INTEGER")
                            })?,
                        ),
                        CachedTranscriptSummary::Unreadable => {
                            (STATUS_UNREADABLE, None, None, None, 0)
                        }
                    };
                upsert.execute(params![
                    update.id,
                    file_size,
                    update.fingerprint.modified_ns,
                    status,
                    title,
                    configured_model,
                    configured_reasoning,
                    message_count
                ])?;
            }
        }
        {
            let mut delete = transaction.prepare("DELETE FROM sessions WHERE id = ?1")?;
            for id in stale_ids {
                delete.execute([id])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Fingerprint recorded for each session's current entry projection. A
    /// session is re-indexed only when its candidate fingerprint differs.
    pub(crate) fn entry_fingerprints(&self) -> anyhow::Result<HashMap<String, CatalogFingerprint>> {
        let mut statement = self
            .connection
            .prepare("SELECT session_id, file_size, modified_ns FROM indexed_entry_sessions")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut fingerprints = HashMap::new();
        for row in rows {
            let (session_id, file_size, modified_ns) = row?;
            let Ok(file_size) = u64::try_from(file_size) else {
                continue;
            };
            fingerprints.insert(
                session_id,
                CatalogFingerprint {
                    file_size,
                    modified_ns,
                },
            );
        }
        Ok(fingerprints)
    }

    /// Apply one bounded incremental entry-index refresh.
    ///
    /// Replaces the entries for every updated session, drops the sessions that
    /// no longer exist, and advances the change revision only when something
    /// actually changed. Returns whether the revision advanced.
    pub(crate) fn apply_entries(
        &mut self,
        updates: &[IndexedEntryUpdate],
        stale_ids: &HashSet<String>,
    ) -> anyhow::Result<bool> {
        self.apply_entries_with_quota(updates, stale_ids, MAX_POSTINGS_PER_SESSION)
    }

    fn apply_entries_with_quota(
        &mut self,
        updates: &[IndexedEntryUpdate],
        stale_ids: &HashSet<String>,
        posting_quota: usize,
    ) -> anyhow::Result<bool> {
        if updates.is_empty() && stale_ids.is_empty() {
            return Ok(false);
        }
        let transaction = self.connection.transaction()?;
        {
            let mut delete_entries =
                transaction.prepare("DELETE FROM indexed_entries WHERE session_id = ?1")?;
            let mut delete_grams =
                transaction.prepare("DELETE FROM indexed_entry_grams WHERE session_id = ?1")?;
            let mut insert_gram = transaction.prepare(
                "INSERT INTO indexed_entry_grams (gram, session_id, ordinal, entry_id) VALUES (?1, ?2, ?3, ?4)",
            )?;
            let mut delete_session =
                transaction.prepare("DELETE FROM indexed_entry_sessions WHERE session_id = ?1")?;
            let mut insert_entry = transaction.prepare(
                "INSERT INTO indexed_entries (session_id, entry_id, ordinal, kind, text) VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            let mut insert_session = transaction.prepare(
                "INSERT INTO indexed_entry_sessions (session_id, file_size, modified_ns, postings_complete) VALUES (?1, ?2, ?3, 1)",
            )?;
            for update in updates {
                delete_entries.execute([&update.session_id])?;
                delete_grams.execute([&update.session_id])?;
                delete_session.execute([&update.session_id])?;
                insert_session.execute(params![
                    update.session_id,
                    i64::try_from(update.fingerprint.file_size).map_err(|_| {
                        anyhow::anyhow!("session size does not fit SQLite INTEGER")
                    })?,
                    update.fingerprint.modified_ns,
                ])?;
                let mut postings = 0;
                let mut postings_complete = true;
                for (ordinal, entry) in update.entries.iter().enumerate() {
                    if postings_complete {
                        let grams = entry_grams(&entry.text);
                        if grams.len() > posting_quota.saturating_sub(postings) {
                            // Never evict searchable text. Remove this session's
                            // partial postings and explicitly use the complete
                            // bounded text projection as its search fallback.
                            delete_grams.execute([&update.session_id])?;
                            transaction.execute(
                                "UPDATE indexed_entry_sessions SET postings_complete = 0 WHERE session_id = ?1",
                                [&update.session_id],
                            )?;
                            postings_complete = false;
                        } else {
                            postings += grams.len();
                            for gram in grams {
                                insert_gram.execute(params![
                                    gram,
                                    update.session_id,
                                    ordinal as i64,
                                    entry.entry_id
                                ])?;
                            }
                        }
                    }
                    insert_entry.execute(params![
                        update.session_id,
                        entry.entry_id,
                        i64::try_from(ordinal)
                            .map_err(|_| anyhow::anyhow!("entry ordinal does not fit INTEGER"))?,
                        entry.kind.as_status(),
                        entry.text,
                    ])?;
                }
            }
            for id in stale_ids {
                delete_entries.execute([id])?;
                delete_grams.execute([id])?;
                delete_session.execute([id])?;
            }
        }
        // Any real change advances the revision so watchers observe it exactly
        // once, even when a session shrank to zero indexed entries.
        transaction.execute(
            "UPDATE catalog_meta SET value = value + 1 WHERE key = 'entry_revision'",
            [],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    /// The current entry-index revision. A watcher that stored a previous value
    /// knows the index changed when the revision advances.
    pub(crate) fn entry_revision(&self) -> anyhow::Result<i64> {
        let revision = self.connection.query_row(
            "SELECT value FROM catalog_meta WHERE key = 'entry_revision'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(revision)
    }

    /// Bounded substring search over the indexed entry projection, ordered by
    /// `(session_id, ordinal)` so results are deterministic.
    pub(crate) fn search_entries(
        &self,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<IndexedEntryHit>> {
        // SQLite LIKE folds ASCII only. Keep its exact substring semantics,
        // including escaped metacharacters and NUL termination, while selecting
        // candidates through postings even for one/two-character queries.
        let grams = query_grams(query);
        // Counts are maintained with the postings transaction, not computed by
        // scanning COUNT(*) for each query gram. A common prefix is not an
        // adequate candidate filter for a rare suffix (especially a miss).
        let mut counts = self
            .connection
            .prepare_cached("SELECT postings FROM indexed_entry_gram_counts WHERE gram = ?1")?;
        let mut selected = "";
        let mut smallest = i64::MAX;
        #[cfg(test)]
        let mut probe_steps = 0;
        for gram in &grams {
            let count: i64 = counts
                .query_row([gram], |row| row.get(0))
                .optional()?
                .unwrap_or(0);
            #[cfg(test)]
            {
                probe_steps += counts.reset_status(rusqlite::StatementStatus::VmStep);
            }
            if count < smallest {
                selected = gram;
                smallest = count;
            }
            if count == 0 {
                break;
            }
        }
        let pattern = format!("%{}%", escape_like(query));
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let sql = if selected.is_empty() {
            "SELECT session_id, entry_id, ordinal, kind, text FROM indexed_entries
             WHERE text LIKE ?1 ESCAPE '\\' AND ?3 = ''
             ORDER BY session_id, ordinal LIMIT ?2"
        } else {
            // Disjoint sources: indexed sessions have postings, overflow
            // sessions have none. SQLite can merge these ordered sources while
            // retaining the same deterministic global order and limit.
            "SELECT g.session_id, e.entry_id, g.ordinal, e.kind, e.text
             FROM indexed_entry_grams g CROSS JOIN indexed_entries e
             ON e.session_id = g.session_id AND e.entry_id = g.entry_id
             WHERE g.gram = ?3 AND e.text LIKE ?1 ESCAPE '\\'
             UNION ALL
             SELECT s.session_id, e.entry_id, e.ordinal, e.kind, e.text
             FROM indexed_entry_sessions s INDEXED BY indexed_entry_fallback CROSS JOIN indexed_entries e INDEXED BY indexed_entries_order ON e.session_id = s.session_id
             WHERE s.postings_complete = 0 AND e.text LIKE ?1 ESCAPE '\\'
             ORDER BY 1, 3 LIMIT ?2"
        };
        let mut statement = self.connection.prepare(sql)?;
        let rows = statement.query_map(params![pattern, limit, selected], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut hits = Vec::new();
        for row in rows {
            let (session_id, entry_id, ordinal, kind, text) = row?;
            let Some(kind) = IndexedEntryKind::from_status(kind) else {
                continue;
            };
            let Ok(ordinal) = usize::try_from(ordinal) else {
                continue;
            };
            hits.push(IndexedEntryHit {
                session_id,
                entry_id,
                kind,
                ordinal,
                text,
            });
        }
        #[cfg(test)]
        self.search_steps
            .set(probe_steps + statement.get_status(rusqlite::StatementStatus::VmStep));
        Ok(hits)
    }

    pub(crate) fn exists(workspace_store: &Path) -> bool {
        Self::path(workspace_store)
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
    }

    pub(crate) fn path(workspace_store: &Path) -> std::path::PathBuf {
        workspace_store.join(CATALOG_DIRECTORY).join(CATALOG_FILE)
    }
}

/// Unique short n-grams bound temporary retention to one projected entry.
/// Posting overflow falls back to the complete bounded text projection.
fn entry_grams(text: &str) -> HashSet<String> {
    let chars: Vec<_> = text.chars().map(|ch| ch.to_ascii_lowercase()).collect();
    let mut grams = HashSet::new();
    for size in 1..=3 {
        for window in chars.windows(size) {
            grams.insert(window.iter().collect());
        }
    }
    grams
}

/// All necessary grams of the longest supported size. LIKE terminates at NUL
/// and folds only ASCII; using Unicode lowercase would lose valid matches.
fn query_grams(query: &str) -> Vec<String> {
    let chars: Vec<_> = query
        .split('\0')
        .next()
        .unwrap_or("")
        .chars()
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    let size = chars.len().min(3);
    if size == 0 {
        return Vec::new();
    }
    let mut grams: Vec<String> = chars
        .windows(size)
        .map(|window| window.iter().collect())
        .collect();
    grams.sort_unstable();
    grams.dedup();
    grams
}

/// Escape LIKE metacharacters so a user query is matched literally.
fn escape_like(query: &str) -> String {
    let mut escaped = String::with_capacity(query.len());
    for ch in query.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn catalog_error_is_rebuildable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        let Some(rusqlite::Error::SqliteFailure(error, _)) =
            cause.downcast_ref::<rusqlite::Error>()
        else {
            return false;
        };
        matches!(
            error.code,
            rusqlite::ErrorCode::DatabaseCorrupt
                | rusqlite::ErrorCode::NotADatabase
                | rusqlite::ErrorCode::Unknown
        )
    })
}

fn reset_catalog_files(workspace_store: &Path) -> anyhow::Result<()> {
    let directory = workspace_store.join(CATALOG_DIRECTORY).canonicalize()?;
    let path = directory.join(CATALOG_FILE);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = path
            .file_name()
            .expect("catalog path always has a filename")
            .to_os_string();
        name.push(suffix);
        octet_agent::secure_fs::remove_regular_file_if_exists(&path.with_file_name(name))?;
    }
    octet_agent::secure_fs::remove_regular_file_if_exists(&path)?;
    Ok(())
}

fn prepare_private_database_file(path: &Path) -> anyhow::Result<()> {
    let file = match octet_agent::secure_fs::open_regular_file_for_append(path) {
        Ok(file) => file,
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            octet_agent::secure_fs::create_regular_file_for_append(path)?
        }
        Err(error) => return Err(error.into()),
    };
    drop(file);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(id: &str, texts: &[&str]) -> IndexedEntryUpdate {
        IndexedEntryUpdate {
            session_id: id.into(),
            fingerprint: CatalogFingerprint {
                file_size: 1,
                modified_ns: 1,
            },
            entries: texts
                .iter()
                .enumerate()
                .map(|(i, text)| IndexedEntry {
                    entry_id: format!("entry-{i}"),
                    kind: IndexedEntryKind::User,
                    text: (*text).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn postings_preserve_like_search_and_invalidate_replacements() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = SessionCatalog::open(temp.path()).unwrap();
        let texts = [
            "Hello WORLD",
            "helLo_%\\tail",
            "éÉ🦀",
            "a",
            "ab",
            "abc",
            "x\0abc",
            "line\nnext",
        ];
        catalog
            .apply_entries(&[update("a", &texts), update("b", &texts)], &HashSet::new())
            .unwrap();
        for query in [
            "",
            "h",
            "HE",
            "HELLO",
            "world",
            "%",
            "_",
            "\\",
            "_%\\",
            "é",
            "É",
            "🦀",
            "a",
            "ab",
            "abc",
            "\0",
            "x\0abc",
            "line\n",
            "not present",
        ] {
            for limit in [0, 1, 100] {
                let mut baseline = catalog.connection.prepare(
                    "SELECT session_id, entry_id FROM indexed_entries WHERE text LIKE ?1 ESCAPE '\\' ORDER BY session_id, ordinal LIMIT ?2"
                ).unwrap();
                let expected = baseline
                    .query_map(
                        params![format!("%{}%", escape_like(query)), limit as i64],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let actual: Vec<_> = catalog
                    .search_entries(query, limit)
                    .unwrap()
                    .into_iter()
                    .map(|hit| (hit.session_id, hit.entry_id))
                    .collect();
                assert_eq!(actual, expected, "query={query:?}, limit={limit}");
            }
        }
        catalog
            .apply_entries(
                &[update("a", &["replacement"])],
                &HashSet::from(["b".into()]),
            )
            .unwrap();
        assert!(catalog.search_entries("hello", 100).unwrap().is_empty());
        assert_eq!(catalog.search_entries("replacement", 100).unwrap().len(), 1);
    }

    #[test]
    fn selective_search_work_does_not_scale_with_unrelated_history() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = SessionCatalog::open(temp.path()).unwrap();
        catalog
            .apply_entries(&[update("target", &["rare ∆x needle"])], &HashSet::new())
            .unwrap();
        for query in ["∆", "∆x", "∆x needle"] {
            assert_eq!(catalog.search_entries(query, 10).unwrap().len(), 1);
        }
        let baseline = catalog.search_steps.get();
        let mut background = update("background", &[]);
        background.entries = (0..4096)
            .map(|i| IndexedEntry {
                entry_id: format!("{i:05}"),
                kind: IndexedEntryKind::User,
                text: format!("ordinary unrelated history number {i}"),
            })
            .collect();
        catalog
            .apply_entries(&[background], &HashSet::new())
            .unwrap();
        for query in ["∆", "∆x", "∆x needle"] {
            assert_eq!(catalog.search_entries(query, 10).unwrap().len(), 1);
            assert!(
                catalog.search_steps.get() <= baseline + 10,
                "{} vs {baseline}",
                catalog.search_steps.get()
            );
        }
    }

    #[test]
    fn common_prefix_misses_choose_a_selective_gram_without_scanning_postings() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = SessionCatalog::open(temp.path()).unwrap();
        catalog
            .apply_entries(&[update("a", &["common prefix ordinary"])], &HashSet::new())
            .unwrap();
        assert!(catalog
            .search_entries("common prefix missing ∆", 10)
            .unwrap()
            .is_empty());
        let baseline = catalog.search_steps.get();
        let mut background = update("b", &[]);
        background.entries = (0..4096)
            .map(|i| IndexedEntry {
                entry_id: format!("{i:05}"),
                kind: IndexedEntryKind::User,
                text: "common prefix ordinary".into(),
            })
            .collect();
        catalog
            .apply_entries(&[background], &HashSet::new())
            .unwrap();
        assert!(catalog
            .search_entries("common prefix missing ∆", 10)
            .unwrap()
            .is_empty());
        assert!(
            catalog.search_steps.get() <= baseline + 10,
            "{} vs {baseline}",
            catalog.search_steps.get()
        );
        assert_eq!(catalog.search_entries("common", 1).unwrap().len(), 1);
        assert!(
            catalog.search_steps.get() < 150,
            "ordered postings must stop at the limit: {}",
            catalog.search_steps.get()
        );
    }

    #[test]
    fn posting_quota_falls_back_without_losing_text_order_or_like_semantics() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = SessionCatalog::open(temp.path()).unwrap();
        let texts = [
            "Hello WORLD",
            "helLo_%\\tail",
            "éÉ🦀",
            "x\0abc",
            "last needle",
        ];
        catalog
            .apply_entries_with_quota(
                &[update("a", &texts), update("c", &texts)],
                &HashSet::new(),
                32,
            )
            .unwrap();
        catalog
            .apply_entries(
                &[update("b", &["Hello world", "last needle"])],
                &HashSet::new(),
            )
            .unwrap();
        let overflow: i64 = catalog
            .connection
            .query_row(
                "SELECT COUNT(*) FROM indexed_entry_sessions WHERE postings_complete = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(overflow, 2);
        let postings: i64 = catalog
            .connection
            .query_row(
                "SELECT COUNT(*) FROM indexed_entry_grams WHERE session_id IN ('a', 'c')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(postings, 0, "overflow uses only bounded projected text");
        for query in [
            "", "h", "HE", "HELLO", "world", "%", "_", "\\", "_%\\", "é", "É", "🦀", "a", "ab",
            "abc", "\0", "x\0abc", "needle", "absent",
        ] {
            for limit in [0, 1, 2, 100] {
                let mut baseline = catalog.connection.prepare("SELECT session_id, entry_id FROM indexed_entries WHERE text LIKE ?1 ESCAPE '\\' ORDER BY session_id, ordinal LIMIT ?2").unwrap();
                let expected = baseline
                    .query_map(
                        params![format!("%{}%", escape_like(query)), limit as i64],
                        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                    )
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let actual: Vec<_> = catalog
                    .search_entries(query, limit)
                    .unwrap()
                    .into_iter()
                    .map(|hit| (hit.session_id, hit.entry_id))
                    .collect();
                assert_eq!(actual, expected, "query={query:?}, limit={limit}");
            }
        }
        // Overflow is replaceable, not permanent eviction or a stale marker.
        catalog
            .apply_entries_with_quota(&[update("a", &["small"])], &HashSet::from(["c".into()]), 32)
            .unwrap();
        let overflow: i64 = catalog
            .connection
            .query_row(
                "SELECT COUNT(*) FROM indexed_entry_sessions WHERE postings_complete = 0",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(overflow, 0);
        let inaccurate: i64 = catalog.connection.query_row("SELECT COUNT(*) FROM indexed_entry_gram_counts c WHERE c.postings != (SELECT COUNT(*) FROM indexed_entry_grams g WHERE g.gram = c.gram)", [], |row| row.get(0)).unwrap();
        assert_eq!(inaccurate, 0);
        assert_eq!(
            catalog.search_entries("small", 10).unwrap()[0].session_id,
            "a"
        );
    }

    #[test]
    fn catalogs_beyond_the_old_size_cliff_remain_searchable_with_bounded_cache() {
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = SessionCatalog::open(temp.path()).unwrap();
        catalog
            .apply_entries(
                &[update("oldest", &["still discoverable"])],
                &HashSet::new(),
            )
            .unwrap();
        // Grow a real SQLite file, not malformed trailing bytes. A formerly
        // valid >64 MiB catalog used to become unusable on the next open.
        catalog.connection.execute_batch("CREATE TABLE padding (data BLOB); INSERT INTO padding VALUES (zeroblob(65 * 1024 * 1024)); PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        assert!(SessionCatalog::path(temp.path()).metadata().unwrap().len() > 64 * 1024 * 1024);
        drop(catalog);
        let catalog = SessionCatalog::open_recovering(temp.path()).unwrap();
        assert_eq!(
            catalog.search_entries("discoverable", 10).unwrap()[0].session_id,
            "oldest"
        );
        let cache: i64 = catalog
            .connection
            .pragma_query_value(None, "cache_size", |row| row.get(0))
            .unwrap();
        assert_eq!(cache, -CATALOG_CACHE_KIB);
    }

    #[test]
    fn non_text_reasoning_rows_are_ignored_for_transcript_rebuild() {
        let temp = tempfile::tempdir().unwrap();
        let workspace_store = temp.path().join("sessions");
        std::fs::create_dir(&workspace_store).unwrap();
        let catalog = SessionCatalog::open(&workspace_store).unwrap();
        catalog
            .connection
            .execute_batch(
                "DROP TABLE sessions;
                 CREATE TABLE sessions (
                     id TEXT PRIMARY KEY NOT NULL,
                     file_size INTEGER NOT NULL,
                     modified_ns INTEGER NOT NULL,
                     status INTEGER NOT NULL,
                     title TEXT,
                     configured_model TEXT,
                     configured_reasoning,
                     message_count INTEGER NOT NULL
                 ) WITHOUT ROWID;",
            )
            .unwrap();
        catalog
            .connection
            .execute(
                "INSERT INTO sessions (id, file_size, modified_ns, status, title, configured_model, configured_reasoning, message_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, TRUE, ?7)",
                params!["invalid-reasoning", 1_i64, 2_i64, STATUS_SUMMARY, "title", "model", 3_i64],
            )
            .unwrap();
        catalog
            .connection
            .execute(
                "INSERT INTO sessions (id, file_size, modified_ns, status, title, configured_model, configured_reasoning, message_count) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params!["valid-reasoning", 1_i64, 2_i64, STATUS_SUMMARY, "title", "model", "high", 3_i64],
            )
            .unwrap();

        let loaded = catalog.load().unwrap();

        assert!(!loaded.contains_key("invalid-reasoning"));
        assert!(matches!(
            loaded.get("valid-reasoning").map(|entry| &entry.summary),
            Some(CachedTranscriptSummary::Summary {
                configured_reasoning: Some(reasoning),
                ..
            }) if reasoning == "high"
        ));
    }
}
