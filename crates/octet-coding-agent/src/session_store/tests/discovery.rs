//! Candidate discovery and the warm catalog: which sessions a store lists, how
//! the newest resumable one is chosen, how a Responses-API steering or
//! reasoning record is classified rather than mistaken for a user prompt, how a
//! warm catalog avoids transcript scans while metadata stays live, and how a
//! fingerprint decides that only changed transcripts need a rescan.
//!
//! Separate from the repair tests because discovery asserts what a listing
//! returns, while repair asserts what happens when the cache underneath it is
//! wrong.

use super::*;

#[test]
fn latest_projection_visits_only_the_newest_resumable_candidate() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    for index in 0..32 {
        let path = store.dir().join(format!("session-{index:02}.jsonl"));
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("resumable".into())],
            })))
            .unwrap();
    }
    let candidates = store.unsorted_candidates().collect::<Vec<_>>();
    let newest = candidates
        .iter()
        .map(|candidate| candidate.modified)
        .max()
        .unwrap();
    let visits = std::cell::Cell::new(0);
    let result = store.discover_with_summarizer(candidates, true, |path| {
        visits.set(visits.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(visits.get(), 1);
    assert_eq!(result[0].modified, newest);
    assert_eq!(store.latest().unwrap().path, result[0].path);
}

#[test]
fn latest_returns_newest_by_mtime() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let older_path = store.dir().join("2026-01-01T00-00-00Z-aaaa.jsonl");
    let mut older = Session::create(&older_path).unwrap();
    older
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("older".into())],
        })))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(15));
    let newer_path = store.dir().join("2026-02-02T00-00-00Z-bbbb.jsonl");
    let mut newer = Session::create(&newer_path).unwrap();
    newer
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("newer".into())],
        })))
        .unwrap();
    assert_eq!(store.latest().unwrap().path, newer_path);
}

#[test]
fn responses_steering_metadata_is_not_a_catalog_prompt() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("steering.jsonl");
    let mut session = Session::create(&path).unwrap();
    let input = octet_ai::UserMessage {
        content: vec![UserPart::Text("steered instruction".into())],
    };
    session
        .append(EntryValue::ResponsesSteering {
            endpoint: EndpointId("fixture".into()),
            model: ModelId("fixture".into()),
            operation: "fixture-operation".into(),
            local_id: 1,
            input: Some(input.clone()),
            state: None,
            completed: None,
        })
        .unwrap();
    assert!(summarize_session(&path).unwrap().title.is_none());
    session
        .append(EntryValue::Message(Message::User(input)))
        .unwrap();
    assert_eq!(
        summarize_session(&path).unwrap().title.as_deref(),
        Some("steered instruction")
    );
}

#[test]
fn responses_reasoning_catalog_uses_durable_effective_choice() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("reasoning.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Config {
            model: Some("fixture".into()),
            reasoning: Some("low".into()),
            reasoning_mode: None,
        })
        .unwrap();
    let baseline = octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::Low);
    for update in [
        None,
        Some(octet_ai::ResponsesConfigurationUpdate {
            reasoning: octet_ai::ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        }),
    ] {
        session
            .append(EntryValue::ResponsesReasoning {
                endpoint: EndpointId("fixture".into()),
                model: ModelId("fixture".into()),
                baseline: baseline.clone(),
                update,
            })
            .unwrap();
    }
    assert_eq!(
        active_branch_catalog_config(&session),
        (Some("fixture".into()), Some("high".into()))
    );
    let summary = summarize_session(&path).unwrap();
    assert_eq!(summary.configured_model.as_deref(), Some("fixture"));
    assert_eq!(summary.configured_reasoning.as_deref(), Some("high"));
    assert!(
        summary.title.is_none(),
        "internal control is not a user prompt"
    );
}

#[test]
fn latest_skips_a_newer_config_only_session() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let older_path = store.dir().join("conversation.jsonl");
    let mut older = Session::create(&older_path).unwrap();
    older
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("resumable".into())],
        })))
        .unwrap();
    drop(older);
    std::thread::sleep(std::time::Duration::from_millis(15));
    let newer_path = store.dir().join("config-only.jsonl");
    let mut newer = Session::create(&newer_path).unwrap();
    newer
        .append(EntryValue::Config {
            model: Some("model".into()),
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();

    let latest = store.latest().unwrap();
    assert_eq!(latest.path, older_path);
    assert_eq!(latest.title, "resumable");
}

#[test]
fn warm_catalog_avoids_transcript_scans_and_keeps_metadata_live() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("warm.jsonl");
    let mut session = Session::create(path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("warm title".into())],
        })))
        .unwrap();
    drop(session);

    let scans = std::cell::Cell::new(0);
    let first = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 1);
    assert_eq!(first[0].title, "warm title");
    assert!(SessionCatalog::path(store.dir()).is_file());

    scans.set(0);
    let second = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 0);
    assert_eq!(second[0].title, "warm title");

    store.rename("warm", "Renamed without replay").unwrap();
    let renamed = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 0);
    assert_eq!(renamed[0].title, "Renamed without replay");
}

#[test]
fn catalog_fingerprint_rescans_only_changed_transcripts() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let path = store.dir().join("changed.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("old".into())],
        })))
        .unwrap();
    drop(session);
    assert_eq!(store.list()[0].title, "old");

    let replacement_path = store.dir().join("replacement.tmp");
    let mut replacement = Session::create(&replacement_path).unwrap();
    replacement
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("a distinct replacement title".into())],
        })))
        .unwrap();
    drop(replacement);
    std::fs::copy(&replacement_path, &path).unwrap();
    std::fs::remove_file(replacement_path).unwrap();

    let scans = std::cell::Cell::new(0);
    let changed = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 1);
    assert_eq!(changed[0].title, "a distinct replacement title");

    scans.set(0);
    let warm = store.discover_with_summarizer(store.candidates(), false, |path| {
        scans.set(scans.get() + 1);
        summarize_catalog_session(path)
    });
    assert_eq!(scans.get(), 0);
    assert_eq!(warm[0].title, "a distinct replacement title");
}
