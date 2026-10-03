use super::*;
use crate::session::EntryValue;
use serde_json::json;

fn binding(session: &Session) -> SessionLeafBinding {
    SessionLeafBinding {
        activation_epoch: 7,
        owner: ExtensionResourceOwner {
            session_id: session.resource_owner_key(),
            extension_instance_id: "instance-leaf-test".into(),
            process_generation: 3,
        },
        namespace: "octet.test".into(),
        operation_id: "prepare-context:7".into(),
    }
}

fn append(
    producer: &SessionLeafProducer,
    grant: &SessionLeafGrant,
    data: Value,
) -> SessionLeafReceipt {
    producer
        .try_append(grant.id(), grant.binding(), "checkpoint".into(), data)
        .unwrap()
}

fn marker(session: &mut Session) -> EntryId {
    session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap()
}

fn queue_state(consumer: &SessionLeafConsumer) -> (usize, usize, usize) {
    let state = consumer.shared.state.lock().unwrap();
    (state.queue.len(), state.pending.len(), state.bytes)
}

#[tokio::test]
async fn synced_entry_and_head_are_published_before_receipt_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let binding = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, binding.clone()).unwrap();
    let data = json!({"version":1,"revision":2,"projection":"inert private state"});
    let mut receipt = append(&producer, &grant, data.clone());
    assert!(matches!(
        receipt.receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert!(session.head().is_none());
    assert_eq!(file_bytes(&session).unwrap(), 0);
    assert!(consumer.consume_next(&mut session, &binding).unwrap());
    // Publication and complete paired records already exist before ACK is read.
    let head = session.head().unwrap();
    assert_eq!(
        session.extension_entry(&head, "octet.test").unwrap().data,
        data
    );
    let disk = std::fs::read_to_string(&path).unwrap();
    let records: Vec<Value> = disk
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["type"], "entry");
    assert_eq!(records[1]["type"], "head");
    assert_eq!(records[1]["id"], head.0);
    let committed = receipt.wait().await.unwrap();
    assert_eq!(committed.entry_id, head);
    assert_eq!(committed.head, head);
    let successor = committed.successor.unwrap();
    assert_eq!(successor.expected_head(), Some(&head));
    assert_ne!(successor.id(), grant.id());
    assert_eq!(queue_state(&consumer), (0, 0, 0));
    assert!(session.context().unwrap().is_empty());
    let metadata = session.entry(&head).unwrap().metadata.as_ref().unwrap();
    assert!(metadata.public_extension_metadata().is_empty());
    assert_eq!(
        metadata.extension_metadata["octet.test"]
            .provenance
            .process_generation,
        Some(3)
    );
    drop(consumer);
    drop(session); // never open while the authoritative writer is alive
    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(head.clone()));
    assert_eq!(
        reopened.extension_entry(&head, "octet.test").unwrap().data,
        data
    );
}

#[tokio::test]
async fn known_chain_advances_grants_without_reexecuting_hook() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, mut grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let mut previous = None;
    for revision in 0..4 {
        let receipt = append(&producer, &grant, json!({"revision":revision}));
        consumer.consume_next(&mut session, &current).unwrap();
        let commit = receipt.wait().await.unwrap();
        assert_eq!(session.entry(&commit.entry_id).unwrap().parent, previous);
        previous = Some(commit.entry_id);
        grant = commit.successor.unwrap();
    }
    assert_eq!(session.entries().len(), 4);
}

#[tokio::test]
async fn owned_hook_future_can_wait_for_same_driver_append() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    // Models an owned request/snapshot hook future: no Session/Agent borrow.
    let hook = async move {
        let receipt = append(&producer, &grant, json!({"revision":1}));
        receipt.wait().await.unwrap().head
    };
    tokio::pin!(hook);
    let head = loop {
        let ready = consumer.ready();
        tokio::select! {
            result = &mut hook => break result,
            live = ready => {
                assert!(live);
                consumer.consume_next(&mut session, &current).unwrap();
            }
        }
    };
    assert_eq!(session.head(), Some(head));
}

#[tokio::test]
async fn cancellation_before_claim_is_zero_write_and_burns_grant() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    let cancel = receipt.cancellation();
    assert_eq!(cancel.cancel(), SessionLeafCancellation::Prevented);
    assert_eq!(
        receipt.wait().await.unwrap_err(),
        SessionLeafError::Cancelled
    );
    assert_eq!(cancel.cancel(), SessionLeafCancellation::Settled);
    assert!(!consumer.consume_next(&mut session, &current).unwrap());
    assert_eq!(file_bytes(&session).unwrap(), 0);
    assert!(session.entries().is_empty());
    assert_eq!(
        producer
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::StaleGrant)
    );
    assert_eq!(queue_state(&consumer), (0, 0, 0));
}

#[tokio::test]
async fn cancellation_after_claim_waits_for_real_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let mut receipt = append(&producer, &grant, json!(1));
    let request = consumer.claim_next(&session, &current).unwrap().unwrap();
    assert_eq!(
        receipt.cancellation().cancel(),
        SessionLeafCancellation::Claimed
    );
    assert!(matches!(
        receipt.receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(queue_state(&consumer).1, 1);
    consumer.commit(&mut session, request).unwrap();
    let commit = receipt.wait().await.unwrap();
    assert_eq!(session.head(), Some(commit.head));
}

#[tokio::test]
async fn revocation_settles_all_queued_and_does_not_lie_about_claimed_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let grant2 = consumer.issue_grant(&session).unwrap();
    let mut claimed = append(&producer, &grant, json!(1));
    let queued = append(&producer, &grant2, json!(2));
    let request = consumer.claim_next(&session, &current).unwrap().unwrap();
    consumer.revoker().revoke();
    assert_eq!(queued.wait().await.unwrap_err(), SessionLeafError::Revoked);
    assert!(matches!(
        claimed.receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    assert_eq!(queue_state(&consumer).1, 1);
    consumer.commit(&mut session, request).unwrap();
    let result = claimed.wait().await.unwrap();
    assert!(result.successor.is_none());
    assert_eq!(session.head(), Some(result.head));
    assert_eq!(queue_state(&consumer), (0, 0, 0));
    assert!(!consumer.ready().await);
    assert_eq!(
        producer
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::Revoked)
    );
}

#[tokio::test]
async fn revocation_before_claim_is_zero_write() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    consumer.revoker().revoke();
    assert_eq!(receipt.wait().await.unwrap_err(), SessionLeafError::Revoked);
    assert!(!consumer.consume_next(&mut session, &current).unwrap());
    assert_eq!(file_bytes(&session).unwrap(), 0);
}

#[tokio::test]
async fn drop_consumer_settles_pending_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (consumer, producer, grant) = SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    let ready = consumer.ready();
    drop(consumer);
    assert!(!ready.await);
    assert_eq!(receipt.wait().await.unwrap_err(), SessionLeafError::Closed);
    assert_eq!(file_bytes(&session).unwrap(), 0);
}

#[tokio::test]
async fn writer_failure_publishes_no_head_and_terminalizes_entire_lane() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let second_grant = consumer.issue_grant(&session).unwrap();
    let first = append(&producer, &grant, json!(1));
    let second = append(&producer, &second_grant, json!(2));
    session.fail_next_append();
    assert_eq!(
        consumer.consume_next(&mut session, &current),
        Err(SessionLeafError::Persistence)
    );
    assert_eq!(
        first.wait().await.unwrap_err(),
        SessionLeafError::Persistence
    );
    assert_eq!(
        second.wait().await.unwrap_err(),
        SessionLeafError::Persistence
    );
    assert_eq!(
        producer
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::Persistence)
    );
    assert!(session.head().is_none());
    assert!(session.entries().is_empty());
    assert_eq!(file_bytes(&session).unwrap(), 0);
    drop(consumer);
    drop(session);
    assert!(Session::open(&path).unwrap().entries().is_empty());
}

#[tokio::test]
async fn failure_after_claim_still_waits_and_never_retries_uncertain_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    let request = consumer.claim_next(&session, &current).unwrap().unwrap();
    assert_eq!(
        receipt.cancellation().cancel(),
        SessionLeafCancellation::Claimed
    );
    // External partial bytes model the existing descriptor fence's uncertainty.
    // No second Session is opened; the original writer must refuse and close.
    let mut external = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    external.write_all(b"{\"type\":").unwrap();
    external.sync_data().unwrap();
    drop(external);
    assert_eq!(
        consumer.commit(&mut session, request),
        Err(SessionLeafError::Persistence)
    );
    assert_eq!(
        receipt.wait().await.unwrap_err(),
        SessionLeafError::Persistence
    );
    assert!(session.head().is_none());
    assert!(session.entries().is_empty());
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"type\":");
    assert_eq!(
        consumer.issue_grant(&session).err(),
        Some(SessionLeafError::Persistence)
    );
}

#[tokio::test]
async fn count_bound_refuses_without_dropping_or_consuming_refused_grant() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, first) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let mut grants = vec![first];
    for _ in 0..MAX_SESSION_LEAF_REQUESTS {
        grants.push(consumer.issue_grant(&session).unwrap());
    }
    let mut receipts = Vec::new();
    for grant in &grants[..MAX_SESSION_LEAF_REQUESTS] {
        receipts.push(append(&producer, grant, json!(1)));
    }
    let last = grants.last().unwrap();
    assert_eq!(
        producer
            .try_append(last.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::Full)
    );
    assert_eq!(queue_state(&consumer).0, MAX_SESSION_LEAF_REQUESTS);
    assert_eq!(
        receipts.remove(0).cancellation().cancel(),
        SessionLeafCancellation::Prevented
    );
    let retry_admission = append(&producer, last, json!(1));
    assert_eq!(queue_state(&consumer).0, MAX_SESSION_LEAF_REQUESTS);
    consumer.revoker().revoke();
    for receipt in receipts {
        assert_eq!(receipt.wait().await.unwrap_err(), SessionLeafError::Revoked);
    }
    assert_eq!(
        retry_admission.wait().await.unwrap_err(),
        SessionLeafError::Revoked
    );
    assert_eq!(queue_state(&consumer), (0, 0, 0));
}

#[tokio::test]
async fn aggregate_bytes_and_unused_grants_are_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, first) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let mut grants = vec![first];
    for _ in 1..MAX_SESSION_LEAF_GRANTS {
        grants.push(consumer.issue_grant(&session).unwrap());
    }
    assert_eq!(
        consumer.issue_grant(&session).err(),
        Some(SessionLeafError::Full)
    );
    let data = json!("x".repeat(16_000));
    let mut receipts = Vec::new();
    for grant in &grants[..4] {
        receipts.push(append(&producer, grant, data.clone()));
    }
    assert!(queue_state(&consumer).2 < MAX_SESSION_LEAF_BYTES);
    assert_eq!(
        producer
            .try_append(grants[4].id(), &current, "checkpoint".into(), data)
            .err(),
        Some(SessionLeafError::Full)
    );
    assert_eq!(queue_state(&consumer).0, 4);
    consumer.revoker().revoke();
    for receipt in receipts {
        assert_eq!(receipt.wait().await.unwrap_err(), SessionLeafError::Revoked);
    }
}

#[tokio::test]
async fn duplicate_foreign_namespace_generation_and_operation_cannot_admit() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let current = binding(&session);
    let (consumer, producer, grant) = SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let (_other, foreign, _) = SessionLeafConsumer::new(&session, current.clone()).unwrap();
    assert_eq!(
        foreign
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::StaleGrant)
    );
    for field in 0..6 {
        let mut changed = current.clone();
        match field {
            0 => changed.activation_epoch += 1,
            1 => changed.owner.session_id = "foreign".into(),
            2 => changed.owner.extension_instance_id = "foreign".into(),
            3 => changed.owner.process_generation += 1,
            4 => changed.namespace = "other.namespace".into(),
            _ => changed.operation_id = "other-operation".into(),
        }
        assert_eq!(
            producer
                .try_append(grant.id(), &changed, "checkpoint".into(), json!(1))
                .err(),
            Some(SessionLeafError::StaleBinding)
        );
    }
    let receipt = append(&producer, &grant, json!(1));
    assert_eq!(
        producer
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::StaleGrant)
    );
    consumer.revoker().revoke();
    assert_eq!(receipt.wait().await.unwrap_err(), SessionLeafError::Revoked);
}

#[tokio::test]
async fn consumption_revalidates_each_binding_fence_without_writes() {
    for field in 0..6 {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let current = binding(&session);
        let (mut consumer, producer, grant) =
            SessionLeafConsumer::new(&session, current.clone()).unwrap();
        let receipt = append(&producer, &grant, json!(1));
        let mut changed = current.clone();
        match field {
            0 => changed.activation_epoch += 1,
            1 => changed.owner.session_id = "foreign".into(),
            2 => changed.owner.extension_instance_id = "foreign".into(),
            3 => changed.owner.process_generation += 1,
            4 => changed.namespace = "other.namespace".into(),
            _ => changed.operation_id = "other-operation".into(),
        }
        assert_eq!(
            consumer.consume_next(&mut session, &changed),
            Err(SessionLeafError::StaleBinding)
        );
        assert_eq!(
            receipt.wait().await.unwrap_err(),
            SessionLeafError::StaleBinding
        );
        assert_eq!(file_bytes(&session).unwrap(), 0);
        assert_eq!(
            consumer.issue_grant(&session).err(),
            Some(SessionLeafError::StaleBinding)
        );
    }
}

#[tokio::test]
async fn head_and_head_aba_are_stale_even_if_final_head_matches() {
    for aba in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
        let a = marker(&mut session);
        let b = marker(&mut session);
        session.checkout(a.clone()).unwrap();
        let current = binding(&session);
        let (mut consumer, producer, grant) =
            SessionLeafConsumer::new(&session, current.clone()).unwrap();
        let receipt = append(&producer, &grant, json!(1));
        session.checkout(b).unwrap();
        if aba {
            session.checkout(a).unwrap();
        }
        let before = file_bytes(&session).unwrap();
        assert_eq!(
            consumer.consume_next(&mut session, &current),
            Err(SessionLeafError::StaleHead)
        );
        assert_eq!(
            receipt.wait().await.unwrap_err(),
            SessionLeafError::StaleHead
        );
        assert_eq!(file_bytes(&session).unwrap(), before);
        assert_eq!(session.entries().len(), 2);
    }
}

#[tokio::test]
async fn reopened_same_path_and_foreign_session_cannot_be_retargeted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.jsonl");
    let session = Session::create(&path).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    drop(session);
    let mut reopened = Session::open(&path).unwrap();
    assert_eq!(
        consumer.consume_next(&mut reopened, &current),
        Err(SessionLeafError::StaleSession)
    );
    assert_eq!(
        receipt.wait().await.unwrap_err(),
        SessionLeafError::StaleSession
    );
    assert_eq!(file_bytes(&reopened).unwrap(), 0);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&reopened, current.clone()).unwrap();
    let receipt = append(&producer, &grant, json!(1));
    let mut foreign = Session::create(dir.path().join("b.jsonl")).unwrap();
    assert_eq!(
        consumer.consume_next(&mut foreign, &current),
        Err(SessionLeafError::StaleSession)
    );
    assert_eq!(
        receipt.wait().await.unwrap_err(),
        SessionLeafError::StaleSession
    );
    assert_eq!(file_bytes(&foreign).unwrap(), 0);
}

#[tokio::test]
async fn lost_reply_does_not_uncommit_or_allow_replay() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    drop(append(&producer, &grant, json!(1)));
    consumer.consume_next(&mut session, &current).unwrap();
    let head = session.head().unwrap();
    assert_eq!(
        producer
            .try_append(grant.id(), &current, "checkpoint".into(), json!(1))
            .err(),
        Some(SessionLeafError::StaleGrant)
    );
    assert_eq!(session.entries().len(), 1);
    drop(consumer);
    drop(session);
    assert_eq!(Session::open(&path).unwrap().head(), Some(head));
}

#[tokio::test]
async fn multiline_private_clm_checkpoint_roundtrips_as_escaped_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let mut session = Session::create(&path).unwrap();
    let data = json!({"version":1,"checkpoint":{"projectedMessages":[{
        "role":"user","content":[{"type":"text","text":"first line\nsecond line\r\n\tindent"}]
    }]}});
    let current = binding(&session);
    let (mut consumer, producer, grant) =
        SessionLeafConsumer::new(&session, current.clone()).unwrap();
    let receipt = append(&producer, &grant, data.clone());
    consumer.consume_next(&mut session, &current).unwrap();
    let head = receipt.wait().await.unwrap().head;
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(raw.lines().count(), 2);
    assert!(raw.contains("first line\\nsecond line\\r\\n\\tindent"));
    assert!(!raw.contains('\r'));
    assert!(!raw.contains('\t'));
    assert!(session
        .entry(&head)
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .public_extension_metadata()
        .is_empty());
    drop(consumer);
    drop(session);
    let reopened = Session::open(&path).unwrap();
    assert_eq!(
        reopened.extension_entry(&head, "octet.test").unwrap().data,
        data
    );
    assert!(reopened.context().unwrap().is_empty());
}

#[test]
fn private_multiline_correction_preserves_public_key_type_namespace_and_control_refusals() {
    use crate::session::{EntryMetadata, ExtensionEntryMetadata, ExtensionMetadataProvenance};
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    for data in [
        json!({"text":"nul\u{0}"}),
        json!({"text":"escape\u{1b}[31m"}),
        json!({"key\n":1}),
        json!({"key\r":1}),
        json!({"key\t":1}),
    ] {
        assert_eq!(
            payload_bytes("checkpoint", &data),
            Err(SessionLeafError::InvalidPayload)
        );
        assert!(session
            .append_extension_entry("octet.test", Some(3), "checkpoint", data)
            .is_err());
    }
    for (namespace, kind) in [
        ("octet.test\n", "checkpoint"),
        ("octet.test", "type\n"),
        ("octet.test", "type\r"),
        ("octet.test", "type\t"),
    ] {
        assert!(session
            .append_extension_entry(namespace, Some(3), kind, json!(1))
            .is_err());
    }
    assert_eq!(file_bytes(&session).unwrap(), 0);
    let value = json!({"text":"line1\nline2\r\n\tindent"});
    for public in [true, false] {
        let metadata = EntryMetadata {
            extension_metadata: [(
                "octet.test".into(),
                ExtensionEntryMetadata {
                    public,
                    value: value.clone(),
                    provenance: ExtensionMetadataProvenance {
                        extension: "octet.test".into(),
                        process_generation: Some(3),
                    },
                },
            )]
            .into(),
            ..EntryMetadata::default()
        };
        let head = session
            .append_with_metadata(
                EntryValue::Config {
                    model: None,
                    reasoning: None,
                    reasoning_mode: None,
                },
                Some(metadata),
            )
            .unwrap();
        assert_eq!(session.entry(&head).unwrap().metadata.is_some(), !public);
    }
}

#[test]
fn payload_bounds_match_authoritative_append_and_never_silently_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let mut session = Session::create(dir.path().join("session.jsonl")).unwrap();
    let mut deep = json!(0);
    for _ in 0..16 {
        deep = json!([deep]);
    }
    let cases = [
        ("", json!(0)),
        (
            "checkpoint",
            json!("x".repeat(MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES)),
        ),
        ("checkpoint", json!({"key\u{1b}":1})),
        ("checkpoint", deep),
        ("checkpoint", json!((0..256).collect::<Vec<_>>())),
        ("checkpoint", json!({"text":"nul\u{0}"})),
    ];
    for (entry_type, data) in cases {
        assert_eq!(
            payload_bytes(entry_type, &data),
            Err(SessionLeafError::InvalidPayload)
        );
        assert!(session
            .append_extension_entry("octet.test", Some(3), entry_type, data)
            .is_err());
        assert_eq!(file_bytes(&session).unwrap(), 0);
    }
}
