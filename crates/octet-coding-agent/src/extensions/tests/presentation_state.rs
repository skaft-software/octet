//! The presentation reducer and what a session transition throws away.
//!
//! The reducer fences every update on revision, generation, and resource owner so
//! a late frame from a replaced extension cannot resurrect a stale view. The
//! second test covers the other half of that contract: moving to a new active
//! session must discard the old session's context and owner presentations.

use super::*;

#[test]
fn semantic_presentation_reducer_fences_revision_generation_and_owner() {
    let owner = octet_agent::extension_process::ExtensionResourceOwner {
        session_id: "owner-a".into(),
        extension_instance_id: "instance-a".into(),
        process_generation: 3,
    };
    assert_eq!(
        admit_presentation_owner(Some(owner.clone()), Some("owner-a")).unwrap(),
        Some("owner-a".into())
    );
    assert!(admit_presentation_owner(Some(owner), Some("owner-b"))
        .unwrap_err()
        .contains("another resource owner"));
    assert_eq!(
        admit_presentation_owner(None, Some("owner-a")).unwrap(),
        None
    );

    let snapshot: ExtensionPresentationSnapshot = serde_json::from_str(include_str!(
        "../../../fixtures/extension-presentation.json"
    ))
    .unwrap();
    snapshot.validate(&["workers".into()]).unwrap();
    let mut presentations = BTreeMap::new();
    assert_eq!(
        reduce_presentation_update(
            &mut presentations,
            "fixture-extension".into(),
            Some(3),
            "instance-a".into(),
            Some("owner-a".into()),
            3,
            snapshot.clone(),
        )
        .unwrap(),
        "1 worker"
    );
    let rendered = format_presentation_views(&presentations.values().cloned().collect::<Vec<_>>());
    assert!(rendered.contains("Reviewing tests"));
    assert!(rendered.contains("https://example.com/docs/extensions"));
    assert!(rendered.contains("session: Worker transcript · session-worker-1"));

    let mut process_scoped = snapshot.clone();
    process_scoped.revision += 1;
    assert!(reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(3),
        "instance-a".into(),
        None,
        3,
        process_scoped,
    )
    .unwrap_err()
    .contains("owner-scoped state is active"));
    assert_eq!(
        presentations["fixture-extension"].resource_owner.as_deref(),
        Some("owner-a")
    );

    assert!(reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(3),
        "instance-a".into(),
        Some("owner-a".into()),
        3,
        snapshot.clone(),
    )
    .unwrap_err()
    .contains("stale semantic presentation revision"));

    let mut restarted = snapshot;
    restarted.revision = 0;
    reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(4),
        "instance-a".into(),
        Some("owner-a".into()),
        4,
        restarted,
    )
    .unwrap();
    assert_eq!(presentations["fixture-extension"].generation, 4);
    assert_eq!(presentations["fixture-extension"].snapshot.revision, 0);

    let owner_reset: ExtensionPresentationSnapshot = serde_json::from_str(include_str!(
        "../../../fixtures/extension-presentation.json"
    ))
    .unwrap();
    reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(4),
        "instance-a".into(),
        Some("owner-b".into()),
        4,
        owner_reset,
    )
    .unwrap();
    assert_eq!(
        presentations["fixture-extension"].resource_owner.as_deref(),
        Some("owner-b")
    );

    let error = reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(4),
        "instance-a".into(),
        Some("owner-b".into()),
        3,
        serde_json::from_str(include_str!(
            "../../../fixtures/extension-presentation.json"
        ))
        .unwrap(),
    )
    .unwrap_err();
    assert!(error.contains("stale generation"));

    let mut replacement: ExtensionPresentationSnapshot = serde_json::from_str(include_str!(
        "../../../fixtures/extension-presentation.json"
    ))
    .unwrap();
    replacement.revision = 0;
    reduce_presentation_update(
        &mut presentations,
        "fixture-extension".into(),
        Some(4),
        "instance-b".into(),
        Some("owner-b".into()),
        4,
        replacement,
    )
    .unwrap();
    assert_eq!(
        presentations["fixture-extension"].extension_instance_id,
        "instance-b"
    );
    assert_eq!(presentations["fixture-extension"].snapshot.revision, 0);
}

#[test]
fn active_session_transition_discards_old_context_and_owner_presentations() {
    let directory = tempfile::tempdir().unwrap();
    let old = Session::create(directory.path().join("old.jsonl")).unwrap();
    let replacement = Session::create(directory.path().join("replacement.jsonl")).unwrap();
    let old_owner = old.resource_owner_key();
    let replacement_owner = replacement.resource_owner_key();
    let sessions = SessionStore::new(&directory.path().join("sessions"), directory.path());
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    let snapshot: ExtensionPresentationSnapshot = serde_json::from_str(include_str!(
        "../../../fixtures/extension-presentation.json"
    ))
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    extensions
        .pending_post_mutation_rescans
        .push_back(PostMutationRescan {
            extension: "fixture-extension".into(),
            mutation_id: "mutation:old".into(),
            kind: PostMutationKind::Resource,
            process_generation: 1,
            generation: 1,
            resource_ids: vec!["resource:old".into()],
        });
    extensions.session_id = Some("old".into());
    extensions.resource_owner = Some(old_owner.clone());
    extensions
        .pending_context
        .try_push(ContextContribution {
            label: "old context".into(),
            content: "must not reach the replacement".into(),
            placement: ContextPlacement::PromptSuffix,
        })
        .unwrap();
    extensions.presentations.insert(
        "old-owner".into(),
        ExtensionPresentationView {
            extension: "fixture-extension".into(),
            generation: 1,
            extension_instance_id: "instance".into(),
            resource_owner: Some(old_owner),
            snapshot: snapshot.clone(),
        },
    );
    extensions.presentations.insert(
        "global".into(),
        ExtensionPresentationView {
            extension: "fixture-extension".into(),
            generation: 1,
            extension_instance_id: "instance".into(),
            resource_owner: None,
            snapshot,
        },
    );

    extensions.transition_active_session(&replacement, &model, &ReasoningConfig::Off, &sessions);

    assert!(extensions.pending_post_mutation_rescans.is_empty());
    assert!(extensions.pending_context.entries.is_empty());
    assert_eq!(extensions.pending_context.retained_bytes, 0);
    assert_eq!(
        extensions.resource_owner.as_deref(),
        Some(replacement_owner.as_str())
    );
    assert!(!extensions.presentations.contains_key("old-owner"));
    assert!(extensions.presentations.contains_key("global"));
}
