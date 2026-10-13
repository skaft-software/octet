//! Session-file lifecycle and durability: descriptor ownership, append
//! fences, resource limits and torn-tail recovery on reopen.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;

#[test]
fn create_append_reopen_and_reconstruct() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    let e1 = s.append(user("hello")).unwrap();
    let e2 = s.append(assistant("hi there")).unwrap();
    assert_eq!(s.head(), Some(e2.clone()));
    assert_eq!(s.entries()[1].parent, Some(e1.clone()));
    drop(s);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(e2));
    let ctx = reopened.context().unwrap();
    assert_eq!(ctx.len(), 2);
    assert_eq!(text_of(&ctx[0]), "hello");
    assert_eq!(text_of(&ctx[1]), "hi there");
}

#[test]
fn caller_supplied_descriptor_is_not_reopened_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let moved = dir.path().join("authorized.jsonl");

    let mut original = Session::create(&path).unwrap();
    original.append(user("authorized")).unwrap();
    drop(original);
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .open(&path)
        .unwrap();

    std::fs::rename(&path, &moved).unwrap();
    let mut replacement = Session::create(&path).unwrap();
    replacement.append(user("replacement")).unwrap();
    drop(replacement);

    let mut adopted = Session::open_with_file(&path, file).unwrap();
    assert_eq!(text_of(&adopted.context().unwrap()[0]), "authorized");
    adopted.append(assistant("bound descriptor")).unwrap();
    drop(adopted);

    let authorized = Session::open(&moved).unwrap();
    assert_eq!(
        text_of(&authorized.context().unwrap()[1]),
        "bound descriptor"
    );
    let replacement = Session::open(&path).unwrap();
    assert_eq!(replacement.context().unwrap().len(), 1);
    assert_eq!(text_of(&replacement.context().unwrap()[0]), "replacement");
}

#[test]
fn cache_key_is_stable_and_path_scoped() {
    let first_dir = tempfile::tempdir().unwrap();
    let second_dir = tempfile::tempdir().unwrap();
    let first_path = first_dir.path().join("session.jsonl");
    let second_path = second_dir.path().join("session.jsonl");
    let first = Session::create(&first_path).unwrap();
    let first_key = first.cache_key();
    let first_owner = first.resource_owner_key();
    assert_eq!(first_key, first.cache_key());
    assert_eq!(first_owner, first.resource_owner_key());
    assert_eq!(first_owner.len(), "session-".len() + 64);
    drop(first);
    let reopened = Session::open(&first_path).unwrap();
    assert_eq!(first_key, reopened.cache_key());
    assert_eq!(first_owner, reopened.resource_owner_key());
    let other = Session::create(&second_path).unwrap();
    assert_ne!(first_key, other.cache_key());
    assert_ne!(first_owner, other.resource_owner_key());
}

#[test]
fn create_refuses_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    std::fs::write(&path, "").unwrap();
    assert!(matches!(Session::create(&path), Err(SessionError::Io(_))));
}

#[cfg(unix)]
#[test]
fn newly_created_session_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let _session = Session::create(&path).unwrap();
    assert_eq!(
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(windows)]
#[test]
fn newly_created_session_is_owner_only_on_windows() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let session = Session::create(&path).unwrap();
    drop(session);
    // Delegation reconciles child sessions through an owner-only read;
    // a session created the ordinary way must pass that same check.
    crate::secure_fs::open_private_file_for_read(&path).unwrap();
}

#[test]
fn append_seeks_to_the_durable_end_on_writable_descriptors() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let mut session = Session::create_with_file(&path, file).unwrap();

    session.append(user("first")).unwrap();
    session.file.seek(std::io::SeekFrom::Start(0)).unwrap();
    session.append(user("second")).unwrap();
    drop(session);

    let reopened = Session::open_read_only(path).unwrap();
    let context = reopened.context().unwrap();
    assert_eq!(context.len(), 2);
    assert_eq!(text_of(&context[0]), "first");
    assert_eq!(text_of(&context[1]), "second");
}

#[test]
fn stale_handle_cannot_append_a_duplicate_entry_id() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let original = Session::create(&path).unwrap();
    let mut stale = Session::open(&path).unwrap();
    let mut current = original;

    assert_eq!(
        current.append(user("first")).unwrap(),
        EntryId("001".into())
    );
    assert!(matches!(
        stale.append(user("stale")),
        Err(SessionError::ConcurrentModification)
    ));

    drop(stale);
    drop(current);
    let reopened = Session::open(path).unwrap();
    assert_eq!(reopened.entries().len(), 1);
    assert_eq!(reopened.head(), Some(EntryId("001".into())));
}

#[test]
fn append_preflights_the_file_limit_without_partial_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    let near_limit = MAX_SESSION_FILE_BYTES - 1;
    session.file.set_len(near_limit).unwrap();
    session.writer.state.lock().unwrap().len = near_limit;
    let before_next_id = session.next_id;
    let before_records = session.writer.state.lock().unwrap().records;

    let error = session.append(user("must not be written")).unwrap_err();

    assert!(matches!(error, SessionError::Limit(_)), "{error}");
    assert_eq!(session.file.metadata().unwrap().len(), near_limit);
    assert_eq!(session.writer.state.lock().unwrap().len, near_limit);
    assert_eq!(session.writer.state.lock().unwrap().records, before_records);
    assert_eq!(session.next_id, before_next_id);
    assert!(session.entries.is_empty());
    assert!(session.index.is_empty());
    assert!(session.head.is_none());
}

#[test]
fn append_preflights_the_record_limit_without_partial_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.writer.state.lock().unwrap().records = MAX_SESSION_RECORDS - 1;
    let before_next_id = session.next_id;

    let error = session
        .append(user("two records would exceed the limit"))
        .unwrap_err();

    assert!(matches!(error, SessionError::Limit(_)), "{error}");
    assert_eq!(session.file.metadata().unwrap().len(), 0);
    assert_eq!(session.writer.state.lock().unwrap().len, 0);
    assert_eq!(
        session.writer.state.lock().unwrap().records,
        MAX_SESSION_RECORDS - 1
    );
    assert_eq!(session.next_id, before_next_id);
    assert!(session.entries.is_empty());
    assert!(session.index.is_empty());
    assert!(session.head.is_none());
}

#[test]
fn missing_newline_repair_never_grows_past_the_file_limit() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.append(user("valid unterminated record")).unwrap();
    drop(session);

    let mut unterminated = std::fs::read(&path).unwrap();
    assert_eq!(unterminated.pop(), Some(b'\n'));
    std::fs::write(&path, &unterminated).unwrap();
    let exact_limit = u64::try_from(unterminated.len()).unwrap();

    let error =
        Session::open_impl_with_limits(path.clone(), true, exact_limit, MAX_SESSION_RECORDS)
            .unwrap_err();
    assert!(matches!(error, SessionError::Limit(_)), "{error}");
    assert!(error.to_string().contains("repair would grow session"));
    assert_eq!(
        std::fs::read(&path).unwrap(),
        unterminated,
        "a rejected repair must leave the source bytes untouched"
    );

    let repaired =
        Session::open_impl_with_limits(path.clone(), true, exact_limit + 1, MAX_SESSION_RECORDS)
            .unwrap();
    drop(repaired);
    let repaired = std::fs::read(path).unwrap();
    assert_eq!(repaired.len() as u64, exact_limit + 1);
    assert_eq!(repaired.last(), Some(&b'\n'));
}

#[test]
fn maximum_numeric_entry_id_is_corruption_not_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);
    let mut session = Session::create(&path).unwrap();
    session.append(user("ordinary")).unwrap();
    drop(session);
    let original = std::fs::read_to_string(&path).unwrap();
    let mut corrupt = original
        .replace("\"001\"", &format!("\"{}\"", u64::MAX))
        .into_bytes();
    assert_eq!(corrupt.pop(), Some(b'\n'));
    std::fs::write(&path, &corrupt).unwrap();

    let opened = std::panic::catch_unwind(|| Session::open(&path));
    assert!(opened.is_ok(), "opening a corrupt ID must never unwind");
    let error = opened.unwrap().unwrap_err();
    assert!(matches!(error, SessionError::Corrupt { .. }), "{error}");
    assert!(error.to_string().contains("exhausts the u64 ID space"));
    assert_eq!(
        std::fs::read(path).unwrap(),
        corrupt,
        "semantic corruption must be rejected before tail repair mutates bytes"
    );
}

#[test]
fn durable_head_survives_reopen_after_checkout() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    let e1 = s.append(user("one")).unwrap();
    let _e2 = s.append(assistant("two")).unwrap();
    s.checkout(e1.clone()).unwrap();
    drop(s);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(e1));
    let ctx = reopened.context().unwrap();
    assert_eq!(ctx.len(), 1);
    assert_eq!(text_of(&ctx[0]), "one");
}

#[test]
fn incomplete_trailing_record_is_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    let e1 = s.append(user("kept")).unwrap();
    drop(s);

    // Simulate a torn write: a partial JSON record with no newline.
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(br#"{"type":"entry","id":"002","paren"#)
        .unwrap();
    drop(f);

    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.entries().len(), 1);
    assert_eq!(reopened.head(), Some(e1));
    // The session remains appendable after recovery.
    let e2 = reopened.append(assistant("next")).unwrap();
    assert_eq!(reopened.head(), Some(e2.clone()));
    drop(reopened);

    // Regression: recovery must truncate the torn bytes, so the
    // post-recovery append starts a fresh line — a second reopen must not
    // see a merged/corrupt record.
    let reopened_again = Session::open(&path).unwrap();
    assert_eq!(reopened_again.entries().len(), 2);
    assert_eq!(reopened_again.head(), Some(e2));
    assert!(!std::fs::read_to_string(&path).unwrap().contains("paren\""));
}

#[test]
fn invalid_utf8_in_an_unterminated_final_record_is_recovered() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut session = Session::create(&path).unwrap();
    let durable_head = session.append(user("kept")).unwrap();
    drop(session);
    let durable_len = std::fs::metadata(&path).unwrap().len();

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"text\":\"").unwrap();
    file.write_all(&[0xf0, 0x9f]).unwrap();
    drop(file);
    let torn_bytes = std::fs::read(&path).unwrap();

    let read_only = Session::open_read_only(&path).unwrap();
    assert_eq!(read_only.head(), Some(durable_head.clone()));
    drop(read_only);
    assert_eq!(
        std::fs::read(&path).unwrap(),
        torn_bytes,
        "read-only inspection must not repair the tail"
    );

    let recovered = Session::open(&path).unwrap();
    assert_eq!(recovered.head(), Some(durable_head));
    drop(recovered);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_len);
}

#[test]
fn invalid_utf8_in_a_newline_terminated_record_is_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut session = Session::create(&path).unwrap();
    session.append(user("kept")).unwrap();
    drop(session);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(&[0xff, b'\n']).unwrap();
    drop(file);
    let original = std::fs::read(&path).unwrap();

    let error = Session::open(&path).unwrap_err();
    assert!(
        matches!(error, SessionError::Corrupt { line: 3, .. }),
        "{error}"
    );
    assert!(error.to_string().contains("invalid UTF-8"), "{error}");
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[test]
fn malformed_newline_terminated_final_record_is_corruption() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut session = Session::create(&path).unwrap();
    session.append(user("kept")).unwrap();
    drop(session);

    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    file.write_all(b"{\"type\":\"entry\"\n").unwrap();
    drop(file);
    let original = std::fs::read(&path).unwrap();

    let read_only_error = Session::open_read_only(&path).unwrap_err();
    assert!(
        matches!(read_only_error, SessionError::Corrupt { line: 3, .. }),
        "{read_only_error}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);

    let recovery_error = Session::open(&path).unwrap_err();
    assert!(
        matches!(recovery_error, SessionError::Corrupt { line: 3, .. }),
        "{recovery_error}"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "completed corrupt records must never be truncated as torn tails"
    );
}

#[test]
fn valid_final_record_without_trailing_newline_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    s.append(user("one")).unwrap();
    let e2 = s.append(assistant("two")).unwrap();
    drop(s);

    // Simulate losing only the final newline of an otherwise complete
    // write: the record is valid and must be kept, not discarded.
    let content = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, content.strip_suffix('\n').unwrap()).unwrap();

    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.entries().len(), 2);
    assert_eq!(reopened.head(), Some(e2));

    // And the completed newline keeps subsequent appends line-separated.
    let e3 = reopened.append(user("three")).unwrap();
    drop(reopened);
    let reopened_again = Session::open(&path).unwrap();
    assert_eq!(reopened_again.entries().len(), 3);
    assert_eq!(reopened_again.head(), Some(e3));
}

#[test]
fn corruption_before_the_trailing_record_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = temp_path(&dir);

    let mut s = Session::create(&path).unwrap();
    s.append(user("a")).unwrap();
    s.append(assistant("b")).unwrap();
    drop(s);

    let content = std::fs::read_to_string(&path).unwrap();
    let mut lines: Vec<String> = content.lines().map(String::from).collect();
    // Corrupt a completed (non-final) record.
    lines[1] = lines[1][..lines[1].len() / 2].to_string();
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let err = Session::open(&path).unwrap_err();
    assert!(
        matches!(err, SessionError::Corrupt { line: 2, .. }),
        "{err}"
    );
}
