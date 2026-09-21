//! Disposable, persistent ID/offset acceleration for the authoritative ledger.
//!
//! The ledger's exclusive lock also serializes index access across processes.
//! A cache snapshot is published only *after* ledger fsync; an interrupted or
//! failed index commit therefore causes a rebuild, never a duplicate append.
//! Descriptor identity/size/mtime/ctime changes invalidate the snapshot, including
//! replacement, truncation and observable in-place edits. Unexpected changes rebuild rather than assuming
//! an unchanged prefix. Non-Unix platforms conservatively use the streaming path
//! until an equally strong change fingerprint is available.

use super::*;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Transaction};
use std::fs::File;
use std::io::{Seek, SeekFrom};

const INDEX_FILE: &str = "ephemeral-index-v1.sqlite3";

#[cfg(test)]
thread_local! { static RECORDS_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

pub(super) fn append(directory: &Path, record: &EphemeralAccountingRecord) -> anyhow::Result<()> {
    octet_agent::secure_fs::create_private_directory_all(directory)?;
    let path = directory.join(EPHEMERAL_ACCOUNTING_FILE);
    let mut line = serde_json::to_vec(record)?;
    if line.len() > MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES {
        anyhow::bail!(
            "ephemeral accounting record is {} bytes (limit {MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES})",
            line.len()
        );
    }
    line.push(b'\n');
    let mut file = open_or_create(&path)?;
    fs2::FileExt::lock_exclusive(&file)?;
    if file.metadata()?.len() > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
        anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
    }
    repair_tail(&mut file)?;

    // Failure of a disposable accelerator cannot waive accounting validation or
    // prevent a durable append. Stream/rebuild on cache misses/errors, retaining
    // at most one bounded ledger record, never the complete ledger.
    let mut index = Index::open_recovering(directory).ok();
    let cached = index.as_mut().and_then(|index| {
        index.ensure_current(&mut file).ok()?;
        index.lookup(record.accounting_id.as_deref()).ok()
    });
    let existing = match cached {
        Some(location) => location,
        None => {
            index = None;
            scan(&mut file, None, record.accounting_id.as_deref())?
        }
    };
    if let Some((offset, bytes)) = existing {
        let mut prior = read_at(&mut file, offset, bytes)?;
        prior.retain_accounting_uncertainty();
        if prior.accounting_id != record.accounting_id
            || serde_json::to_value(&prior)? != serde_json::to_value(record)?
        {
            anyhow::bail!("ephemeral accounting recovery key has conflicting data");
        }
        // A previous complete append may have failed fsync before acknowledgement.
        file.sync_all()?;
        return Ok(());
    }
    let offset = file.metadata()?.len();
    if offset + line.len() as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
        anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
    }
    file.write_all(&line)?;
    file.sync_all()?;
    if let Some(index) = index.as_mut() {
        // Cache failure after ledger durability is harmless. The old snapshot
        // no longer matches, so the next invocation must reconstruct its IDs.
        let _ = index.record(&file, record.accounting_id.as_deref(), offset, line.len());
    }
    Ok(())
}

fn open_or_create(path: &Path) -> anyhow::Result<File> {
    match octet_agent::secure_fs::open_regular_file_for_append(path) {
        Ok(file) => Ok(file),
        Err(octet_agent::secure_fs::SecureFileError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            match octet_agent::secure_fs::create_regular_file_for_append(path) {
                Ok(file) => Ok(file),
                // Another process may have created the ledger before we lock it.
                Err(octet_agent::secure_fs::SecureFileError::Io(error))
                    if error.kind() == std::io::ErrorKind::AlreadyExists =>
                {
                    Ok(octet_agent::secure_fs::open_regular_file_for_append(path)?)
                }
                Err(error) => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

fn repair_tail(file: &mut File) -> anyhow::Result<()> {
    let len = file.metadata()?.len();
    if len == 0 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0];
    file.read_exact(&mut last)?;
    if last[0] == b'\n' {
        return Ok(());
    }
    let start = len.saturating_sub(MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES as u64 + 1);
    file.seek(SeekFrom::Start(start))?;
    let mut tail = Vec::new();
    (&mut *file).take(len - start).read_to_end(&mut tail)?;
    let offset = tail
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |i| i + 1);
    if offset == 0 && start != 0 {
        anyhow::bail!("ephemeral accounting tail exceeds its record byte limit");
    }
    if serde_json::from_slice::<EphemeralAccountingRecord>(&tail[offset..]).is_ok() {
        if len == MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
        }
        file.write_all(b"\n")?;
    } else {
        file.set_len(start + offset as u64)?;
    }
    // Cache snapshots must never acknowledge unsynced repair.
    file.sync_all()?;
    Ok(())
}

fn parse_record(bytes: &[u8]) -> anyhow::Result<EphemeralAccountingRecord> {
    #[cfg(test)]
    RECORDS_READ.with(|count| count.set(count.get() + 1));
    Ok(serde_json::from_slice(bytes)?)
}

fn read_at(
    file: &mut File,
    offset: u64,
    bytes: usize,
) -> anyhow::Result<EphemeralAccountingRecord> {
    if bytes > MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES + 1
        || offset + bytes as u64 > file.metadata()?.len()
    {
        anyhow::bail!("invalid ephemeral accounting index offset");
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut line = vec![0; bytes];
    file.read_exact(&mut line)?;
    parse_record(&line)
}

type Location = (u64, usize);

fn scan(
    file: &mut File,
    transaction: Option<&Transaction<'_>>,
    wanted: Option<&str>,
) -> anyhow::Result<Option<Location>> {
    file.seek(SeekFrom::Start(0))?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut line = Vec::new();
    let mut offset = 0;
    let mut found = None;
    loop {
        line.clear();
        let bytes = reader
            .by_ref()
            .take(MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES as u64 + 2)
            .read_until(b'\n', &mut line)?;
        if bytes == 0 {
            break;
        }
        if bytes > MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES + 1 || line.last() != Some(&b'\n') {
            anyhow::bail!(
                "ephemeral accounting record exceeds its byte limit or has an unrepaired tail"
            );
        }
        if offset + bytes as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
        }
        if line != b"\n" {
            let record = parse_record(&line)?;
            if let Some(id) = record.accounting_id.as_deref() {
                if Some(id) == wanted && found.is_none() {
                    found = Some((offset, bytes));
                }
                if let Some(transaction) = transaction {
                    transaction.execute(
                        "INSERT OR IGNORE INTO ids (id, offset, bytes) VALUES (?1, ?2, ?3)",
                        params![id, offset as i64, bytes as i64],
                    )?;
                }
            }
        }
        offset += bytes as u64;
    }
    Ok(found)
}

fn fingerprint(file: &File) -> anyhow::Result<Option<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(Some(format!(
            "{}:{}:{}:{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec()
        )))
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        Ok(None)
    }
}

struct Index {
    connection: Connection,
}
impl Index {
    fn open_recovering(directory: &Path) -> anyhow::Result<Self> {
        match Self::open(directory) {
            Ok(index) => Ok(index),
            Err(error) if error.chain().any(|cause| matches!(cause.downcast_ref::<rusqlite::Error>(), Some(rusqlite::Error::SqliteFailure(error, _)) if matches!(error.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))) => {
                let path = directory.join(INDEX_FILE);
                octet_agent::secure_fs::remove_regular_file_if_exists(&path.with_file_name(format!("{INDEX_FILE}-journal")))?;
                octet_agent::secure_fs::remove_regular_file_if_exists(&path)?;
                Self::open(directory)
            }
            Err(error) => Err(error),
        }
    }

    fn open(directory: &Path) -> anyhow::Result<Self> {
        let path = directory.canonicalize()?.join(INDEX_FILE);
        drop(open_or_create(&path)?);
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )?;
        connection.busy_timeout(std::time::Duration::from_millis(100))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > 1 {
            anyhow::bail!("unsupported ephemeral accounting index version");
        }
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "cache_size", -1024)?;
        connection.pragma_update(None, "mmap_size", 0)?;
        connection.pragma_update(None, "temp_store", "FILE")?;
        connection.pragma_update(None, "trusted_schema", false)?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS ids (id TEXT PRIMARY KEY NOT NULL, offset INTEGER NOT NULL CHECK(offset >= 0), bytes INTEGER NOT NULL CHECK(bytes > 0)) WITHOUT ROWID;
            CREATE TABLE IF NOT EXISTS snapshot (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), fingerprint TEXT NOT NULL);
            PRAGMA user_version = 1;")?;
        Ok(Self { connection })
    }

    fn ensure_current(&mut self, file: &mut File) -> anyhow::Result<()> {
        let actual = fingerprint(file)?;
        let saved: Option<String> = self
            .connection
            .query_row(
                "SELECT fingerprint FROM snapshot WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if actual.is_some() && actual == saved {
            return Ok(());
        }
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM ids", [])?;
        scan(file, Some(&transaction), None)?;
        file.sync_all()?;
        // Capture after any repair and ledger fsync. If the process dies before
        // this transaction commits, no advanced snapshot is visible.
        save_snapshot(&transaction, file)?;
        transaction.commit()?;
        Ok(())
    }

    fn lookup(&self, id: Option<&str>) -> anyhow::Result<Option<Location>> {
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(self
            .connection
            .query_row("SELECT offset, bytes FROM ids WHERE id = ?1", [id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?)
    }

    fn record(
        &mut self,
        file: &File,
        id: Option<&str>,
        offset: u64,
        bytes: usize,
    ) -> anyhow::Result<()> {
        let transaction = self.connection.transaction()?;
        if let Some(id) = id {
            transaction.execute(
                "INSERT INTO ids (id, offset, bytes) VALUES (?1, ?2, ?3)",
                params![id, offset as i64, bytes as i64],
            )?;
        }
        save_snapshot(&transaction, file)?;
        transaction.commit()?;
        Ok(())
    }
}

fn save_snapshot(transaction: &Transaction<'_>, file: &File) -> anyhow::Result<()> {
    transaction.execute("INSERT INTO snapshot (singleton, fingerprint) VALUES (1, ?1) ON CONFLICT(singleton) DO UPDATE SET fingerprint = excluded.fingerprint", [fingerprint(file)?.unwrap_or_default()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(id: usize) -> EphemeralAccountingRecord {
        EphemeralAccountingRecord {
            accounting_id: Some(format!("id-{id}")),
            recorded_at_unix_ms: id as u64,
            session_cost_microdollars: id as u64,
            has_uncertain_usage: false,
            usage_records: Vec::new(),
            usage_uncertainty_records: Vec::new(),
        }
    }

    #[test]
    #[cfg(unix)]
    fn reopened_warm_index_never_replays_ledger_history() {
        let temp = tempfile::tempdir().unwrap();
        for id in 0..256 {
            append(temp.path(), &record(id)).unwrap();
        }
        RECORDS_READ.with(|count| count.set(0));
        // append() closes/reopens SQLite on *every* invocation; no process cache.
        append(temp.path(), &record(256)).unwrap();
        assert_eq!(RECORDS_READ.with(|count| count.get()), 0);
        append(temp.path(), &record(17)).unwrap();
        assert_eq!(RECORDS_READ.with(|count| count.get()), 1);
        let mut conflict = record(17);
        conflict.session_cost_microdollars += 1;
        assert!(append(temp.path(), &conflict)
            .unwrap_err()
            .to_string()
            .contains("conflicting"));
        assert_eq!(RECORDS_READ.with(|count| count.get()), 2);
    }

    #[test]
    fn unacknowledged_append_torn_tail_and_corrupt_index_recover() {
        let temp = tempfile::tempdir().unwrap();
        append(temp.path(), &record(1)).unwrap();
        let path = temp.path().join(EPHEMERAL_ACCOUNTING_FILE);
        // Durable/unacknowledged ledger ahead of its SQLite snapshot.
        let mut file = open_or_create(&path).unwrap();
        file.write_all(&serde_json::to_vec(&record(2)).unwrap())
            .unwrap();
        file.sync_all().unwrap();
        drop(file);
        append(temp.path(), &record(2)).unwrap();
        let mut file = open_or_create(&path).unwrap();
        file.write_all(b"{\"accounting_id\":\"torn").unwrap();
        drop(file);
        append(temp.path(), &record(3)).unwrap();
        std::fs::write(temp.path().join(INDEX_FILE), b"broken cache").unwrap();
        append(temp.path(), &record(2)).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 3);
    }

    #[test]
    fn same_length_in_place_edit_invalidates_the_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        append(temp.path(), &record(1)).unwrap();
        let path = temp.path().join(EPHEMERAL_ACCOUNTING_FILE);
        let mut line = serde_json::to_vec(&record(2)).unwrap();
        line.push(b'\n');
        std::fs::write(&path, line).unwrap();
        // Deterministic even on filesystems with coarse clock granularity.
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(UNIX_EPOCH + std::time::Duration::from_secs(123))
            .unwrap();
        append(temp.path(), &record(2)).unwrap();
        append(temp.path(), &record(1)).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(), 2);
    }

    #[test]
    fn unavailable_index_keeps_the_ledger_authoritative() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join(INDEX_FILE)).unwrap();
        append(temp.path(), &record(1)).unwrap();
        append(temp.path(), &record(1)).unwrap();
        let mut conflict = record(1);
        conflict.session_cost_microdollars = 99;
        assert!(append(temp.path(), &conflict)
            .unwrap_err()
            .to_string()
            .contains("conflicting"));
        assert_eq!(
            std::fs::read_to_string(temp.path().join(EPHEMERAL_ACCOUNTING_FILE))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn independent_processes_share_one_locked_index() {
        let temp = tempfile::tempdir().unwrap();
        let test = format!(
            "{}::subprocess_append",
            module_path!().split_once("::").unwrap().1
        );
        let mut children = Vec::new();
        for _ in 0..2 {
            children.push(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--exact")
                    .arg(&test)
                    .env("OCTET_ACCOUNTING_INDEX_TEST_DIRECTORY", temp.path())
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }
        assert_eq!(
            std::fs::read_to_string(temp.path().join(EPHEMERAL_ACCOUNTING_FILE))
                .unwrap()
                .lines()
                .count(),
            4
        );
    }

    #[test]
    fn subprocess_append() {
        let Some(directory) = std::env::var_os("OCTET_ACCOUNTING_INDEX_TEST_DIRECTORY") else {
            return;
        };
        for id in 0..4 {
            append(Path::new(&directory), &record(id)).unwrap();
        }
    }

    #[test]
    fn concurrent_retries_append_once() {
        let temp = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    append(temp.path(), &record(1)).unwrap();
                });
            }
        });
        assert_eq!(
            std::fs::read_to_string(temp.path().join(EPHEMERAL_ACCOUNTING_FILE))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}
