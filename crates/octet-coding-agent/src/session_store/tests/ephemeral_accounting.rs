//! The durable accounting ledger for runs whose transcript is never written to
//! disk: an ephemeral run is accounted for with usage and uncertainty carried
//! across every session it touched, survives a torn or legacy append, repairs
//! itself on retry, and never leaks a prompt body into the accounting root.
//!
//! Separate from the store's own tests because this is the one part of
//! `SessionStore` whose durability lives in a different file tree (the
//! `.accounting` ledger and its SQLite index) rather than in the session
//! directory, and because it is the only boundary with a cross-process lock.

use super::*;

/// One transcript worth of usage plus an unknown-usage exposure, appended
/// through the session's own durable path.
fn write_ephemeral_transcript(path: &Path, uncertain: bool) -> PathBuf {
    let mut session = Session::create(path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("ephemeral prompt".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("ephemeral answer".into())],
                model: ModelId("custom/model".into()),
                protocol: Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    session
        .record_terminal_gate_usage(
            EndpointId("custom".into()),
            ModelId("probe".into()),
            octet_ai::Usage {
                input_tokens: 40,
                output_tokens: 10,
                total_tokens: 50,
                ..octet_ai::Usage::default()
            },
            Some(octet_ai::Cost {
                total: 7,
                ..octet_ai::Cost::default()
            }),
            Some(true),
        )
        .unwrap();
    if uncertain {
        // The operation id the Codex above-272K policy exports.
        session
            .record_usage_uncertainty(
                EndpointId("custom".into()),
                ModelId("probe".into()),
                crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION,
            )
            .unwrap();
    }
    drop(session);
    path.to_path_buf()
}

#[test]
fn ephemeral_accounting_keeps_usage_and_uncertainty_without_the_transcript() {
    let transcript_root = tempfile::tempdir().unwrap();
    let accounting_root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(accounting_root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();

    let transcript = transcript_root
        .path()
        .join(workspace_key(workspace.path()))
        .join("ephemeral.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    write_ephemeral_transcript(&transcript, true);

    let record = store.record_ephemeral_accounting(&transcript).unwrap();
    assert_eq!(record.usage_records.len(), 1);
    assert_eq!(record.usage_records[0].usage.input_tokens, 40);
    assert_eq!(record.usage_uncertainty_records.len(), 1);
    assert_eq!(
        record.usage_uncertainty_records[0].operation,
        crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION
    );
    assert!(record.has_uncertain_usage, "unknown usage must survive");

    // The conversation itself is never copied into the durable ledger.
    let ledger = store
        .dir()
        .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
        .join(EPHEMERAL_ACCOUNTING_FILE);
    let bytes = std::fs::read_to_string(&ledger).unwrap();
    assert!(!bytes.contains("ephemeral prompt"), "{bytes}");
    assert!(!bytes.contains("ephemeral answer"), "{bytes}");

    // A second run accumulates, and uncertainty stays fail-closed across the
    // whole workspace ledger.
    let clean = transcript_root
        .path()
        .join(workspace_key(workspace.path()))
        .join("ephemeral-two.jsonl");
    write_ephemeral_transcript(&clean, false);
    let second = store.record_ephemeral_accounting(&clean).unwrap();
    assert!(!second.has_uncertain_usage);

    let summary = store.ephemeral_accounting_summary().unwrap();
    assert_eq!(summary.runs, 2);
    assert!(
        summary.has_uncertain_usage,
        "one uncertain run keeps the total uncertain"
    );

    // The transcript can now be discarded: accounting still answers.
    std::fs::remove_file(&transcript).unwrap();
    std::fs::remove_file(&clean).unwrap();
    let after = store.ephemeral_accounting_summary().unwrap();
    assert_eq!(after.runs, 2);
    assert!(after.has_uncertain_usage);
}

#[test]
fn unpriced_ephemeral_receipts_survive_legacy_ledger_and_recovery_flags() {
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let workspace = root_path.join("workspace");
    let transcript_root = root_path.join("transcripts");
    let accounting_root = root_path.join("durable");
    let store = SessionStore::new(&accounting_root, &workspace);
    let directory = transcript_root.join(workspace_key(&workspace));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("unpriced.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .record_compaction_usage(
            EndpointId("fixture".into()),
            ModelId("fixture".into()),
            octet_ai::Usage {
                input_tokens: 4,
                output_tokens: 2,
                total_tokens: 6,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert!(session.has_unpriced_usage());
    assert!(!session.has_uncertain_usage());
    drop(session);
    let mut record = read_ephemeral_accounting(&path).unwrap();
    assert!(record.has_uncertain_usage);
    assert!(record.usage_uncertainty_records.is_empty());
    record.accounting_id = Some(workspace_key(&transcript_root));
    store.append_ephemeral_accounting(&record).unwrap();
    // Persist the pre-unpriced-reporting flag to exercise old accounting-only
    // ledgers and recovery after the transcript itself is discarded.
    record.has_uncertain_usage = false;
    let legacy = serde_json::to_vec(&record).unwrap();
    let ledger = store
        .dir()
        .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
        .join(EPHEMERAL_ACCOUNTING_FILE);
    let mut line = legacy.clone();
    line.push(b'\n');
    std::fs::write(&ledger, &line).unwrap();
    octet_agent::secure_fs::write_private_atomic(
        &transcript_root.join(EPHEMERAL_ACCOUNTING_RECOVERY),
        &legacy,
        MAX_SESSION_FILE_BYTES,
    )
    .unwrap();
    let summary = store.ephemeral_accounting_summary().unwrap();
    assert!(summary.has_uncertain_usage);
    assert_eq!(summary.uncertainty_records, 0);
    let mut run = EphemeralRun {
        transcript_root: transcript_root.clone(),
        accounting_session_dir: accounting_root,
        workspace,
        pending: None,
    };
    let recovered = finish_ephemeral_run_state(&mut run).unwrap().unwrap();
    assert!(recovered.has_uncertain_usage);
    assert!(!transcript_root.exists());
    let summary = store.ephemeral_accounting_summary().unwrap();
    assert_eq!(
        summary.runs, 1,
        "normalizing the flag must not duplicate a recovery receipt"
    );
    assert_eq!(summary.input_tokens, 4);
    assert_eq!(summary.output_tokens, 2);
    assert!(summary.has_uncertain_usage);
    assert_eq!(
        std::fs::read(ledger).unwrap(),
        line,
        "historical receipts are not rewritten"
    );
}

#[test]
fn ephemeral_finish_accounts_for_all_sessions_including_an_empty_newest_session() {
    for second_has_usage in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let transcript_root = root.path().join("transcripts");
        let store = SessionStore::new(&root.path().join("durable"), &workspace);
        let directory = transcript_root.join(workspace_key(&workspace));
        std::fs::create_dir_all(&directory).unwrap();
        write_ephemeral_transcript(&directory.join("first.jsonl"), true);
        if second_has_usage {
            write_ephemeral_transcript(&directory.join("second.jsonl"), false);
        } else {
            Session::create(directory.join("second.jsonl")).unwrap();
        }
        let mut run = EphemeralRun {
            transcript_root: transcript_root.clone(),
            accounting_session_dir: root.path().join("durable"),
            workspace,
            pending: None,
        };
        let record = finish_ephemeral_run_state(&mut run).unwrap().unwrap();
        let count = if second_has_usage { 2 } else { 1 };
        assert_eq!(record.usage_records.len(), count);
        assert_eq!(record.session_cost_microdollars, 7 * count as u64);
        assert!(record.has_uncertain_usage);
        assert_eq!(record.usage_uncertainty_records.len(), 1);
        assert!(!transcript_root.exists());
        let summary = store.ephemeral_accounting_summary().unwrap();
        assert_eq!(
            summary.runs, 1,
            "one invocation, not one record per RPC session"
        );
        assert_eq!(summary.input_tokens, 40 * count as u64);
        assert_eq!(summary.usage_records, count);
    }
}

#[test]
fn ephemeral_append_failure_keeps_private_accounting_only_and_retries_once() {
    let _exclusive_ephemeral = EPHEMERAL_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let transcript_root = root.path().join("transcripts");
    let accounting_root = root.path().join("durable");
    let store = SessionStore::new(&accounting_root, &workspace);
    let directory = transcript_root.join(workspace_key(&workspace));
    std::fs::create_dir_all(&directory).unwrap();
    write_ephemeral_transcript(&directory.join("first.jsonl"), true);
    write_ephemeral_transcript(&directory.join("second.jsonl"), false);
    // A directory in place of the ledger fails deterministically, even as root.
    let ledger = store
        .dir()
        .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
        .join(EPHEMERAL_ACCOUNTING_FILE);
    std::fs::create_dir_all(&ledger).unwrap();
    begin_ephemeral_run(
        transcript_root.clone(),
        accounting_root.clone(),
        workspace.clone(),
    );
    let error = finish_ephemeral_run().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("accounting-only recovery retained"),
        "{error:#}"
    );
    assert!(
        error.chain().count() > 1,
        "original append failure must be retained"
    );
    assert!(
        !directory.exists(),
        "no conversation survives failed accounting"
    );
    let recovery = transcript_root.join(EPHEMERAL_ACCOUNTING_RECOVERY);
    let bytes =
        octet_agent::secure_fs::read_private_file_bounded(&recovery, MAX_SESSION_FILE_BYTES)
            .unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    assert!(!text.contains("ephemeral prompt"));
    assert!(!text.contains("ephemeral answer"));
    let record: EphemeralAccountingRecord = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(record.usage_records.len(), 2);
    assert!(record.has_uncertain_usage);
    // Retrying before repair reports the original failure again, never a no-op.
    assert!(finish_ephemeral_run().is_err());
    std::fs::remove_dir(&ledger).unwrap();
    // Simulate a complete but unacknowledged append (e.g. sync failure), then
    // process-state loss. Disk-only recovery must not double-count that append.
    store.append_ephemeral_accounting(&record).unwrap();
    begin_ephemeral_run(transcript_root.clone(), accounting_root, workspace);
    let recovered = finish_ephemeral_run().unwrap().unwrap();
    assert_eq!(recovered.usage_records.len(), 2);
    assert!(!transcript_root.exists());
    assert!(finish_ephemeral_run().unwrap().is_none());
    let summary = store.ephemeral_accounting_summary().unwrap();
    assert_eq!(summary.runs, 1);
    assert_eq!(summary.usage_records, 2);
    assert_eq!(summary.total_cost_microdollars, 14);
    assert!(summary.has_uncertain_usage);
}

#[test]
fn ephemeral_accounting_retry_repairs_a_torn_append() {
    let root = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), root.path());
    let transcript = root.path().join("source.jsonl");
    write_ephemeral_transcript(&transcript, true);
    let mut record = read_ephemeral_accounting(&transcript).unwrap();
    record.accounting_id = Some("retry-key".into());
    let directory = store.dir().join(EPHEMERAL_ACCOUNTING_DIRECTORY);
    std::fs::create_dir_all(&directory).unwrap();
    let ledger = directory.join(EPHEMERAL_ACCOUNTING_FILE);
    let bytes = serde_json::to_vec(&record).unwrap();
    std::fs::write(&ledger, &bytes[..bytes.len() / 2]).unwrap();
    store.append_ephemeral_accounting(&record).unwrap();
    store.append_ephemeral_accounting(&record).unwrap();
    assert_eq!(store.ephemeral_accounting_summary().unwrap().runs, 1);
    assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 1);
}
