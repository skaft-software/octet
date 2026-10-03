//! The durable transcript entries extensions contribute to a session.
//!
//! Covers `session/append` returning a real durable entry id, the effective byte
//! and label bounds being reported back to the extension, labelling a known entry
//! (and clearing it), refusal of an unknown label without a write, and the
//! `active_tools` outcome reporting ok or a typed refusal.

use super::*;

#[test]
fn extension_session_append_answers_a_real_durable_entry_id() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let outcome = ExecutableExtensions::apply_extension_entry_append(
        &mut session,
        "octet.todo",
        41,
        "note",
        serde_json::json!({ "text": "remember" }),
    );
    let ExtensionRequestOutcome::Ok(response) = outcome else {
        panic!("append must succeed: {outcome:?}");
    };
    let entry_id = response["entry_id"]
        .as_str()
        .expect("the response must carry the entry id as a string")
        .to_owned();
    assert!(!entry_id.is_empty());
    let entry = session
        .extension_entry(&EntryId(entry_id), "octet.todo")
        .expect("the entry must be durable in its namespace");
    assert_eq!(entry.entry_type, "note");
    assert_eq!(entry.data["text"], "remember");
}

#[test]
fn extension_session_append_and_label_map_the_effective_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    // The wire allows 64 KiB but the durable store retains only the
    // smaller effective cap; the refusal must name the real limit.
    let oversized = serde_json::json!({
        "text": "x".repeat(MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES)
    });
    match ExecutableExtensions::apply_extension_entry_append(
        &mut session,
        "octet.todo",
        7,
        "note",
        oversized,
    ) {
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, message) => {
            assert!(
                message.contains(&MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES.to_string()),
                "{message}"
            );
        }
        other => panic!("expected the effective store bound: {other:?}"),
    }
    let outcome = ExecutableExtensions::apply_extension_entry_append(
        &mut session,
        "octet.todo",
        7,
        &"t".repeat(MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES + 1),
        serde_json::json!({}),
    );
    assert!(matches!(
        outcome,
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, _)
    ));
    let outcome = ExecutableExtensions::apply_extension_entry_append(
        &mut session,
        "octet.todo",
        7,
        "bad\ntype",
        serde_json::json!({}),
    );
    assert!(matches!(
        outcome,
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, _)
    ));
    let outcome = ExecutableExtensions::apply_extension_entry_label(
        &mut session,
        "any-entry",
        &"l".repeat(MAX_EXTENSION_SESSION_LABEL_BYTES + 1),
    );
    assert!(matches!(
        outcome,
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::BoundsExceeded, _)
    ));
}

#[test]
fn extension_session_label_refuses_unknown_entries_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let before = std::fs::read(session.path()).unwrap();
    match ExecutableExtensions::apply_extension_entry_label(
        &mut session,
        "missing-entry",
        "planning",
    ) {
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, message) => {
            assert!(message.contains("missing-entry"), "{message}");
        }
        other => panic!("expected an invalid-request refusal: {other:?}"),
    }
    assert_eq!(std::fs::read(session.path()).unwrap(), before);
}

#[test]
fn active_tools_outcome_reports_ok_and_typed_refusals() {
    assert_eq!(
        ExecutableExtensions::active_tools_outcome(Ok(())),
        ExtensionRequestOutcome::Ok(serde_json::json!({}))
    );
    // The agent refuses unknown or policy-excluded names and never widens
    // the host-policed surface; the wire answer stays a typed
    // `invalid_request` naming the refusal.
    let refused = ExecutableExtensions::active_tools_outcome(Err(
        octet_agent::AgentError::UnknownActiveTools(vec!["nope".to_owned()]),
    ));
    match refused {
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, detail) => {
            assert!(detail.contains("nope"), "{detail}");
            assert!(
                detail.starts_with("the active tool set was refused:"),
                "{detail}"
            );
        }
        other => panic!("expected a typed refusal, got {other:?}"),
    }
}

#[test]
fn extension_session_label_updates_and_clears_a_known_entry() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let ExtensionRequestOutcome::Ok(response) = ExecutableExtensions::apply_extension_entry_append(
        &mut session,
        "octet.todo",
        9,
        "note",
        serde_json::json!({}),
    ) else {
        panic!("append must succeed");
    };
    let entry_id = response["entry_id"].as_str().unwrap().to_owned();
    let id = EntryId(entry_id);
    assert_eq!(
        ExecutableExtensions::apply_extension_entry_label(&mut session, &id.0, "planning"),
        ExtensionRequestOutcome::Ok(serde_json::json!({}))
    );
    assert_eq!(session.entry_label(&id), Some("planning"));
    // Control characters are malformed, not a size overflow, and the
    // previous label survives the refusal.
    assert!(matches!(
        ExecutableExtensions::apply_extension_entry_label(&mut session, &id.0, "bad\nlabel"),
        ExtensionRequestOutcome::Failed(ExtensionRequestFailure::InvalidRequest, _)
    ));
    assert_eq!(session.entry_label(&id), Some("planning"));
    assert_eq!(
        ExecutableExtensions::apply_extension_entry_label(&mut session, &id.0, ""),
        ExtensionRequestOutcome::Ok(serde_json::json!({}))
    );
    assert_eq!(session.entry_label(&id), None);
}
