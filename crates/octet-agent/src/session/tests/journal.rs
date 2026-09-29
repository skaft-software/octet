//! The bounded partial-assistant frame journal sidecar and its
//! settlement protocol.
//!
//! Part of the `session::tests` suite; the shared builders and
//! `use super::*;` preamble live in `session/tests.rs`.

use super::*;
use crate::session::journal::read_partial_assistant_frames;

fn frame_stream(prefix: &str) -> Vec<octet_ai::AssistantMessageFrame> {
    use octet_ai::AssistantMessageFrame as Frame;
    use octet_ai::StreamEvent;
    let mut encoder = octet_ai::AssistantMessageFrameEncoder::new(
        ModelId("frame-model".to_string()),
        Protocol::AnthropicMessages,
    );
    let events = [
        StreamEvent::Started {
            response_id: Some("resp-1".to_string()),
        },
        StreamEvent::TextStart { index: 0 },
        StreamEvent::TextDelta {
            index: 0,
            delta: prefix.to_string(),
        },
    ];
    let mut frames = Vec::new();
    for event in &events {
        if let Some(frame) = encoder.encode(event).unwrap() {
            frames.push(frame);
        }
    }
    // A terminal event contributes no frame.
    assert!(encoder
        .encode(&StreamEvent::Finished(octet_ai::Response {
            message: AssistantMessage {
                content: vec![AssistantPart::Text(prefix.to_string())],
                model: ModelId("frame-model".to_string()),
                protocol: Protocol::AnthropicMessages,
            },
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            cost: None,
            response_id: Some("resp-1".to_string()),
            responses_output: None,
            deferred: None,
            diagnostics: Vec::new(),
        }))
        .unwrap()
        .is_none());
    let Frame::Start { .. } = &frames[0] else {
        panic!("first frame is the stream start");
    };
    frames
}

#[test]
fn partial_assistant_frames_survive_reopen_and_republish_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let journal_path = {
        let mut session = Session::create(&path).unwrap();
        session.append(user("hi")).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("partial progress") {
            journal.append(&frame).unwrap();
        }
        let journal_path = journal.path().to_path_buf();
        assert!(journal_path.is_file());
        assert!(journal.retained_frames() > 0);
        // Dropping the session (process kill) leaves the journal behind.
        journal_path
    };

    // Restart: a fresh handle sees the partial exactly once.
    let mut reopened = Session::open(&path).unwrap();
    let partial = reopened.take_partial_assistant().unwrap().unwrap();
    assert_eq!(partial.model, ModelId("frame-model".to_string()));
    assert_eq!(partial.protocol, Protocol::AnthropicMessages);
    match partial.content.as_slice() {
        [AssistantPart::Text(text)] => assert_eq!(text, "partial progress"),
        other => panic!("unexpected partial content: {other:?}"),
    }
    // Republish is exactly once and removes the sidecar.
    assert!(reopened.take_partial_assistant().unwrap().is_none());
    assert!(!journal_path.exists());
    // The partial never entered the session log or its context.
    assert!(reopened
        .context()
        .unwrap()
        .iter()
        .all(|message| !matches!(message, Message::Assistant(_))));
}

#[test]
fn settled_assistant_frames_are_not_republished_as_progress() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let journal_path = {
        let mut session = Session::create(&path).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("complete turn") {
            journal.append(&frame).unwrap();
        }
        let journal_path = journal.path().to_path_buf();
        // Terminal settlement removes the partial.
        journal.settle();
        assert!(!journal_path.exists());
        journal_path
    };

    let mut reopened = Session::open(&path).unwrap();
    assert!(reopened.take_partial_assistant().unwrap().is_none());
    assert!(!journal_path.exists());
}

#[test]
fn partial_assistant_journal_never_truncates_an_existing_target() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let mut journal = session.begin_assistant_frame_journal().unwrap();
    for frame in frame_stream("keep this prefix") {
        journal.append(&frame).unwrap();
    }
    let original = std::fs::read(journal.path()).unwrap();
    assert!(session.begin_assistant_frame_journal().is_err());
    assert_eq!(std::fs::read(journal.path()).unwrap(), original);
    drop(journal);
    assert!(session.take_partial_assistant().unwrap().is_some());
    assert!(session.begin_assistant_frame_journal().is_ok());
}

#[test]
fn partial_assistant_journal_bounds_reads_and_discards_torn_utf8_tail() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let mut journal = session.begin_assistant_frame_journal().unwrap();
    for frame in frame_stream("valid prefix") {
        journal.append(&frame).unwrap();
    }
    journal.file.write_all(b"{\"torn\":\"\xff").unwrap();
    drop(journal);
    assert!(session.take_partial_assistant().unwrap().is_some());

    let journal = session.begin_assistant_frame_journal().unwrap();
    journal
        .file
        .set_len((MAX_PARTIAL_FRAME_JOURNAL_BYTES + 1) as u64)
        .unwrap();
    drop(journal);
    assert!(matches!(
        session.take_partial_assistant(),
        Err(SessionError::Limit(_))
    ));
    assert!(session.partial_assistant_frames_path().unwrap().exists());

    let frame = octet_ai::AssistantMessageFrame::TextDelta {
        index: 0,
        delta: "x".into(),
    };
    let mut line = serde_json::to_vec(&frame).unwrap();
    // A JSON value without its newline is still an uncommitted tail.
    assert!(read_partial_assistant_frames(&line).unwrap().is_empty());
    line.push(b'\n');
    let bytes = line.repeat(MAX_PARTIAL_FRAME_JOURNAL_FRAMES + 1);
    assert!(bytes.len() < MAX_PARTIAL_FRAME_JOURNAL_BYTES);
    assert!(matches!(
        read_partial_assistant_frames(&bytes),
        Err(SessionError::Limit(_))
    ));
}

#[cfg(unix)]
#[test]
fn partial_assistant_journal_rejects_symlinks_hardlinks_and_special_files() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{symlink, PermissionsExt};
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let path = session.partial_assistant_frames_path().unwrap();
    let target = directory.path().join("target");
    std::fs::write(&target, b"do not overwrite").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&target, &path).unwrap();
    assert!(session.begin_assistant_frame_journal().is_err());
    assert!(session.take_partial_assistant().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do not overwrite");
    std::fs::remove_file(&path).unwrap();
    std::fs::hard_link(&target, &path).unwrap();
    assert!(session.begin_assistant_frame_journal().is_err());
    assert!(session.take_partial_assistant().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"do not overwrite");
    std::fs::remove_file(&path).unwrap();
    let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a valid NUL-terminated pathname, with a valid mode.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(session.begin_assistant_frame_journal().is_err());
    assert!(session.take_partial_assistant().is_err());
}

#[test]
fn partial_assistant_journal_settlement_preserves_a_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let mut journal = session.begin_assistant_frame_journal().unwrap();
    for frame in frame_stream("original") {
        journal.append(&frame).unwrap();
    }
    let path = journal.path().to_owned();
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    let mut replacement = session.begin_assistant_frame_journal().unwrap();
    for frame in frame_stream("replacement") {
        replacement.append(&frame).unwrap();
    }
    let bytes = std::fs::read(&path).unwrap();
    journal.settle();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    replacement.settle();
    assert!(!path.exists());
}

#[test]
fn partial_frame_journal_is_bounded_and_does_not_grow_without_limit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let mut journal = session.begin_assistant_frame_journal().unwrap();
    let mut frame = octet_ai::AssistantMessageFrame::TextDelta {
        index: 0,
        delta: "x".repeat(MAX_PARTIAL_FRAME_JOURNAL_BYTES / 4 + 1),
    };
    let mut accepted = 0usize;
    for _ in 0..64 {
        journal.append(&frame).unwrap();
        accepted += 1;
    }
    assert!(journal.is_bounded(), "journal must stop at its byte bound");
    assert!(
        journal.retained_bytes() <= MAX_PARTIAL_FRAME_JOURNAL_BYTES,
        "retained journal bytes stay bounded"
    );
    let bytes_before = journal.retained_bytes();
    frame = octet_ai::AssistantMessageFrame::TextDelta {
        index: 0,
        delta: "y".to_string(),
    };
    journal.append(&frame).unwrap();
    assert_eq!(journal.retained_bytes(), bytes_before);
    let path = journal.path().to_path_buf();
    drop(journal);
    assert!(accepted > 1);
    assert!(std::fs::metadata(&path).unwrap().len() as usize <= MAX_PARTIAL_FRAME_JOURNAL_BYTES);
}
