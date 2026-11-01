use super::*;
use crate::extension_process::tests::trusted_descriptor;
use crate::session::{EntryValue, Session};

fn append_message(session: &mut Session, text: &str) {
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(text.into())],
            },
        )))
        .unwrap();
}

#[test]
fn reload_history_rebind_charges_new_generation_without_copying_authority() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    append_message(&mut session, "AUTHENTIC_RELOAD_HISTORY");
    let owner = ExtensionResourceOwner {
        session_id: session.resource_owner_key().into(),
        extension_instance_id: "reload-history".into(),
        process_generation: 1,
    };
    let profile = SessionSnapshotProfile::for_frame(1024 * 1024).unwrap();
    let mut previous = Store::default();
    let revision = previous.next_revision().unwrap();
    let view = previous
        .history(
            &session,
            "reload-history",
            owner.clone(),
            &profile,
            Some(Preparation {
                activation_epoch: 7,
                operation_id: "old-operation".into(),
                tool_generation: 9,
                head: session.head_ref().map(|id| id.0.clone()),
            }),
            revision,
        )
        .unwrap();
    previous
        .current
        .insert(owner.clone(), Some(Arc::clone(&view)));
    let old_handle = previous.transfer(7, Arc::clone(&view), &profile).unwrap();
    previous.parents.insert(
        7,
        ParentView {
            owner: owner.clone(),
            preparation: view.descriptor.preparation.clone(),
            revision,
            head: view.descriptor.head.clone(),
        },
    );
    previous
        .projection_begin(
            wire::ProjectionBegin {
                parent_request_id: 7,
                bytes: 1,
                sha256: "0".repeat(64),
            },
            &owner,
            &profile,
        )
        .unwrap();
    let old_retained = previous.retained();
    let mut replacement = Store::default();
    replacement
        .rebind_current(&previous, &owner, 2, &profile)
        .unwrap();
    let mut next_owner = owner.clone();
    next_owner.process_generation = 2;
    let rebound = Arc::clone(replacement.current[&next_owner].as_ref().unwrap());
    assert_eq!(rebound.bytes, view.bytes);
    assert_eq!(rebound.descriptor.owner, next_owner);
    assert_eq!(rebound.descriptor.head, view.descriptor.head);
    assert_eq!(rebound.descriptor.sha256, view.descriptor.sha256);
    assert_eq!(rebound.descriptor.view_revision, revision);
    assert!(rebound.descriptor.transfer_id.is_empty());
    assert!(rebound.descriptor.preparation.is_none());
    assert!(!Arc::ptr_eq(&rebound, &view));
    assert_eq!(replacement.retained(), old_retained);
    assert_eq!(previous.retained(), old_retained);
    assert!(replacement.transfers.is_empty());
    assert!(replacement.parents.is_empty());
    assert!(replacement.projections.is_empty());
    assert!(!replacement.current.contains_key(&owner));
    assert!(replacement
        .read(
            wire::Read {
                parent_request_id: 7,
                transfer_id: old_handle.transfer_id,
                offset: 0,
                max_bytes: 1,
            },
            &next_owner,
            &profile,
        )
        .is_err());
    let next = replacement
        .transfer(8, Arc::clone(&rebound), &profile)
        .unwrap();
    let read = replacement
        .read(
            wire::Read {
                parent_request_id: 8,
                transfer_id: next.transfer_id,
                offset: 0,
                max_bytes: profile.chunk_bytes,
            },
            &next_owner,
            &profile,
        )
        .unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(read["data"].as_str().unwrap())
        .unwrap();
    assert_eq!(decoded, view.bytes);
    assert!(replacement.next_revision().unwrap() > revision);
    replacement.clear();
    drop(rebound);
    assert_eq!(replacement.retained(), (0, 0, 0));
    assert_eq!(previous.retained(), old_retained);
}

#[test]
fn reload_history_rebind_preserves_unavailability_and_refuses_new_generation_quota() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    append_message(&mut session, "AUTHENTIC_RELOAD_HISTORY");
    let owner = ExtensionResourceOwner {
        session_id: session.resource_owner_key().into(),
        extension_instance_id: "reload-history".into(),
        process_generation: 1,
    };
    let mut profile = SessionSnapshotProfile::for_frame(1024 * 1024).unwrap();
    let mut previous = Store::default();
    let view = previous
        .history(&session, "reload-history", owner.clone(), &profile, None, 1)
        .unwrap();
    previous.current.insert(owner.clone(), Some(view));
    profile.generation_bytes = 1;
    let mut replacement = Store::default();
    assert!(replacement
        .rebind_current(&previous, &owner, 2, &profile)
        .is_err());
    let mut next_owner = owner.clone();
    next_owner.process_generation = 2;
    assert!(matches!(replacement.current.get(&next_owner), Some(None)));
    assert_eq!(replacement.retained(), (0, 0, 0));
    previous.current.insert(owner.clone(), None);
    replacement
        .rebind_current(&previous, &owner, 3, &profile)
        .unwrap();
    next_owner.process_generation = 3;
    assert!(matches!(replacement.current.get(&next_owner), Some(None)));
    assert_eq!(replacement.retained(), (0, 0, 0));
    previous.current.remove(&owner);
    assert!(replacement
        .rebind_current(&previous, &owner, 4, &profile)
        .is_err());
    next_owner.process_generation = 4;
    assert!(matches!(replacement.current.get(&next_owner), Some(None)));
    assert_eq!(replacement.retained(), (0, 0, 0));
}

#[tokio::test]
async fn reload_session_start_reads_authentic_nonempty_history_before_frontend_refresh() {
    let temp = tempfile::tempdir().unwrap();
    let factory = temp.path().join("reload-history.mjs");
    std::fs::write(&factory, r#"import fs from 'node:fs';
export default pi => {
  pi.on('session_start', (event, ctx) => {
    const entries = ctx.sessionManager.getEntries();
    const branch = ctx.sessionManager.getBranch();
    fs.appendFileSync(`${process.cwd()}/reload-history.jsonl`, JSON.stringify({reason:event.reason, entries, branch, leaf:ctx.sessionManager.getLeafId()}) + '\n');
  });
};"#).unwrap();
    let manifest_path =
        crate::extension_process::pi_fixture::capture(temp.path(), &[factory]).await;
    let manifest = ExtensionManifest::load(&manifest_path).unwrap();
    let mut config = ExtensionRuntimeConfig::new(temp.path());
    config.supervise = false;
    config.request_timeout = Duration::from_secs(10);
    config.host_state.has_ui = Some(false);
    let process = ExtensionProcess::start(
        trusted_descriptor(manifest_path.parent().unwrap(), manifest),
        config,
    )
    .await
    .unwrap();
    let mut session = Session::create(temp.path().join("session.jsonl")).unwrap();
    append_message(&mut session, "AUTHENTIC_RELOAD_FIRST");
    let state = process.current_context().host;
    for start in [true, false] {
        if !start {
            append_message(&mut session, "AUTHENTIC_RELOAD_SECOND");
        }
        process
            .set_host_state_with_session(state.clone(), &session)
            .unwrap();
        let context = process.current_context_for_resource_owner(session.resource_owner_key());
        process
            .wait_for_session_snapshot_prepared(
                context.resource_owner.as_ref().unwrap(),
                Duration::from_secs(5),
                &CancellationToken::default(),
            )
            .await
            .unwrap();
        if start {
            process
                .start_session_hook_binding(session.resource_owner_key())
                .await
                .unwrap();
        } else {
            let generation = context.resource_owner.unwrap().process_generation;
            assert!(process.reload().await.unwrap().generation > generation);
        }
    }
    let observed: Vec<Value> = std::fs::read_to_string(temp.path().join("reload-history.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(observed.len(), 2);
    for (index, event) in observed.iter().enumerate() {
        assert_eq!(
            event["reason"],
            if index == 0 { "startup" } else { "reload" }
        );
        assert_eq!(event["entries"].as_array().unwrap().len(), index + 1);
        assert_eq!(event["branch"].as_array().unwrap().len(), index + 1);
        assert_eq!(event["leaf"], json!(session.entries()[index].id));
        assert_eq!(
            event["entries"][index]["id"],
            json!(session.entries()[index].id)
        );
        assert!(event.to_string().contains("AUTHENTIC_RELOAD_FIRST"));
    }
    assert!(observed[1].to_string().contains("AUTHENTIC_RELOAD_SECOND"));
    assert!(process.shutdown().await);
}
