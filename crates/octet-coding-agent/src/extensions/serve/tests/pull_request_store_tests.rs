//! The durable pull-request catalog and the projection built from it.
//! The store is the thing that has to survive a restart without inventing a
//! state, so these cover the evidence it records, the transactions it rolls
//! back, the sessions it refuses to alias, and the ordering guarantee that a
//! projection change is published before the event that announces it.

use super::*;

use super::test_support::*;

#[test]
fn inactive_pull_request_batches_are_bounded_and_rotate_after_failures() {
    let session_ids = (1..=6)
        .map(|number| SessionId::new(format!("pull-request-inactive-{number}")).unwrap())
        .collect::<Vec<_>>();
    let refreshable = session_ids
        .iter()
        .enumerate()
        .map(|(index, session_id)| {
            let mut pull_request =
                stored_pull_request(session_id, (index + 1) as u64, PullRequestState::Ready);
            pull_request.refreshed_at_ms += index as u64;
            pull_request
        })
        .collect::<Vec<_>>();
    let hosted = BTreeSet::from([session_ids[1].clone()]);
    let mut attempted = BTreeSet::new();
    let numbers = |batch: Vec<StoredPullRequest>| {
        batch
            .into_iter()
            .map(|pull_request| pull_request.number)
            .collect::<Vec<_>>()
    };

    assert_eq!(
        numbers(select_inactive_pull_request_batch(
            refreshable.clone(),
            &hosted,
            &mut attempted,
            2,
        )),
        vec![1, 3]
    );
    assert_eq!(
        numbers(select_inactive_pull_request_batch(
            refreshable.clone(),
            &hosted,
            &mut attempted,
            2,
        )),
        vec![4, 5]
    );
    assert_eq!(
        numbers(select_inactive_pull_request_batch(
            refreshable.clone(),
            &hosted,
            &mut attempted,
            2,
        )),
        vec![6]
    );
    assert_eq!(
        numbers(select_inactive_pull_request_batch(
            refreshable,
            &hosted,
            &mut attempted,
            2,
        )),
        vec![1, 3]
    );
}

#[test]
fn github_pull_request_projection_is_structured_and_conservative() {
    for (state, is_draft, expected) in [
        (
            "OPEN",
            true,
            PullRequestObservation::Trackable {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
                state: PullRequestState::InProgress,
            },
        ),
        (
            "OPEN",
            false,
            PullRequestObservation::Trackable {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
                state: PullRequestState::Ready,
            },
        ),
        (
            "MERGED",
            false,
            PullRequestObservation::Trackable {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
                state: PullRequestState::Merged,
            },
        ),
        (
            "CLOSED",
            false,
            PullRequestObservation::Closed {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
            },
        ),
    ] {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "number": 124,
            "url": "https://github.com/skaft-software/ygg/pull/124",
            "state": state,
            "isDraft": is_draft,
        }))
        .unwrap();
        assert_eq!(project_github_pull_request(&bytes), expected);
    }

    for invalid in [
        serde_json::json!({
            "number": 124,
            "url": "https://github.com/skaft-software/ygg/pull/125",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "file:///tmp/pull/124",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "http://github.com/skaft-software/ygg/pull/124",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "https://user:secret@github.com/skaft-software/ygg/pull/124",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "https://github.com/prefix/skaft-software/ygg/pull/124?view=1",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "https://github.com/skaft-software/%79gg/pull/124",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "https://github.com/skaft-software/ygg/pull/0124",
            "state": "OPEN",
            "isDraft": false,
        }),
        serde_json::json!({
            "number": 124,
            "url": "https://github.com/skaft-software/ygg/pull/124",
            "state": "UNKNOWN",
            "isDraft": false,
        }),
    ] {
        assert_eq!(
            project_github_pull_request(&serde_json::to_vec(&invalid).unwrap()),
            PullRequestObservation::Unavailable
        );
    }
    assert_eq!(
        project_github_pull_request(b"not json"),
        PullRequestObservation::Unavailable
    );
}

#[test]
fn pull_request_store_persists_evidence_and_rejects_cross_session_aliases() {
    let directory = tempfile::tempdir().unwrap();
    let first = SessionId::new("pull-request-first").unwrap();
    let second = SessionId::new("pull-request-second").unwrap();
    let mut store = PullRequestStore::open(directory.path()).unwrap();
    store
        .replace(
            &first,
            Some(stored_pull_request(&first, 124, PullRequestState::Ready)),
        )
        .unwrap();
    assert_eq!(
        store.summary(&first),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
    let mut aliased = stored_pull_request(&second, 124, PullRequestState::Merged);
    aliased.url = "https://GITHUB.com/SKAFT-SOFTWARE/YGG/pull/124".into();
    assert!(store.replace(&second, Some(aliased)).is_err());
    let mut port_aliased = stored_pull_request(&second, 124, PullRequestState::Merged);
    port_aliased.url = "https://github.com:443/skaft-software/ygg/pull/124".into();
    assert!(store.replace(&second, Some(port_aliased)).is_err());
    drop(store);

    let mut reopened = PullRequestStore::open(directory.path()).unwrap();
    assert_eq!(
        reopened.summary(&first),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
    assert_eq!(reopened.summary(&second), None);
    reopened.replace(&first, None).unwrap();
    assert_eq!(
        PullRequestStore::open(directory.path())
            .unwrap()
            .summary(&first),
        None
    );
}

#[test]
fn permanently_deleted_sessions_reject_late_pull_request_discovery() {
    let directory = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("pull-request-deleted-race").unwrap();
    let mut store = PullRequestStore::open(directory.path()).unwrap();

    store.delete_session(&session_id).unwrap();
    assert!(apply_pull_request_observation(
        &mut store,
        &session_id,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        },
        20,
    )
    .is_err());
    assert_eq!(store.summary(&session_id), None);
    assert!(store.take_catalog_changes().is_empty());
}

#[cfg(unix)]
#[test]
fn pull_request_store_fails_closed_on_unsafe_or_ambiguous_evidence() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let symlink_directory = directory.path().join("symlink");
    std::fs::create_dir(&symlink_directory).unwrap();
    let target = symlink_directory.join("target.json");
    std::fs::write(&target, b"{}").unwrap();
    symlink(&target, symlink_directory.join(PULL_REQUEST_STORE_FILE)).unwrap();
    assert!(PullRequestStore::open(&symlink_directory).is_err());

    let oversized_directory = directory.path().join("oversized");
    std::fs::create_dir(&oversized_directory).unwrap();
    let oversized =
        std::fs::File::create(oversized_directory.join(PULL_REQUEST_STORE_FILE)).unwrap();
    oversized.set_len(MAX_PULL_REQUEST_STORE_BYTES + 1).unwrap();
    assert!(PullRequestStore::open(&oversized_directory).is_err());

    let duplicate_directory = directory.path().join("duplicate");
    std::fs::create_dir(&duplicate_directory).unwrap();
    let first = SessionId::new("pull-request-duplicate-first").unwrap();
    let second = SessionId::new("pull-request-duplicate-second").unwrap();
    let first_record = stored_pull_request(&first, 124, PullRequestState::Ready);
    let mut second_record = first_record.clone();
    second_record.session_id = second.as_str().to_owned();
    second_record.url = "https://GITHUB.com/SKAFT-SOFTWARE/YGG/pull/124".into();
    let duplicate_catalog = StoredPullRequestCatalog {
        version: PULL_REQUEST_STORE_VERSION,
        records: BTreeMap::from([
            (first.as_str().to_owned(), first_record),
            (second.as_str().to_owned(), second_record),
        ]),
    };
    std::fs::write(
        duplicate_directory.join(PULL_REQUEST_STORE_FILE),
        serde_json::to_vec(&duplicate_catalog).unwrap(),
    )
    .unwrap();
    assert!(PullRequestStore::open(&duplicate_directory).is_err());

    let duplicate_key_directory = directory.path().join("duplicate-key");
    std::fs::create_dir(&duplicate_key_directory).unwrap();
    let session_id = SessionId::new("pull-request-duplicate-key").unwrap();
    let record = serde_json::to_string(&stored_pull_request(
        &session_id,
        125,
        PullRequestState::Ready,
    ))
    .unwrap();
    std::fs::write(
        duplicate_key_directory.join(PULL_REQUEST_STORE_FILE),
        format!(
            r#"{{"version":1,"records":{{"{session_id}":{record},"{session_id}":{record}}}}}"#,
            session_id = session_id.as_str(),
        ),
    )
    .unwrap();
    assert!(PullRequestStore::open(&duplicate_key_directory).is_err());
}

#[test]
fn pull_request_store_transactions_roll_back_records_and_catalog_changes() {
    let directory = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("pull-request-transaction").unwrap();
    let mut store = PullRequestStore::open(directory.path()).unwrap();
    store
        .replace(
            &session_id,
            Some(stored_pull_request(
                &session_id,
                124,
                PullRequestState::Ready,
            )),
        )
        .unwrap();
    assert_eq!(
        store.take_catalog_changes(),
        BTreeSet::from([session_id.clone()])
    );

    let update_error: anyhow::Result<()> = store.transaction(|store| {
        store.replace_unpersisted(
            &session_id,
            Some(stored_pull_request(
                &session_id,
                124,
                PullRequestState::Merged,
            )),
        )?;
        anyhow::bail!("injected update failure")
    });
    assert!(update_error.is_err());
    assert_eq!(
        store.summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
    assert!(store.take_catalog_changes().is_empty());

    let persisted_path = store.path.clone();
    store.path = directory.path().join("unreplaceable-directory");
    std::fs::create_dir(&store.path).unwrap();
    assert!(apply_pull_request_observation(
        &mut store,
        &session_id,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Merged,
        },
        20,
    )
    .is_err());
    assert_eq!(
        store.summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
    assert!(store.take_catalog_changes().is_empty());
    store.path = persisted_path;
    assert_eq!(
        PullRequestStore::open(directory.path())
            .unwrap()
            .summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Ready,
        })
    );
}

#[test]
fn pull_request_observations_retain_unavailable_evidence_and_emit_state_changes() {
    let directory = tempfile::tempdir().unwrap();
    let session_id = SessionId::new("pull-request-observation").unwrap();
    let mut store = PullRequestStore::open(directory.path()).unwrap();
    let ready = PullRequestObservation::Trackable {
        number: 124,
        url: "https://github.com/skaft-software/ygg/pull/124".into(),
        state: PullRequestState::Ready,
    };
    assert_eq!(
        apply_pull_request_observation(&mut store, &session_id, ready.clone(), 10).unwrap(),
        Some(Some(PullRequestSummary {
            state: PullRequestState::Ready,
        }))
    );
    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Unavailable,
            20,
        )
        .unwrap(),
        None
    );
    assert_eq!(store.get(&session_id).unwrap().refreshed_at_ms, 10);
    assert_eq!(
        apply_pull_request_observation(&mut store, &session_id, ready, 30).unwrap(),
        None
    );
    assert_eq!(store.get(&session_id).unwrap().refreshed_at_ms, 30);
    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Trackable {
                number: 125,
                url: "https://github.com/skaft-software/ygg/pull/125".into(),
                state: PullRequestState::Ready,
            },
            35,
        )
        .unwrap(),
        None
    );
    assert_eq!(store.get(&session_id).unwrap().number, 124);
    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Closed {
                number: 125,
                url: "https://github.com/skaft-software/ygg/pull/125".into(),
            },
            36,
        )
        .unwrap(),
        None
    );
    assert_eq!(store.get(&session_id).unwrap().number, 124);
    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Closed {
                number: 124,
                url: "https://GITHUB.com:443/SKAFT-SOFTWARE/YGG/pull/124".into(),
            },
            37,
        )
        .unwrap(),
        Some(None)
    );
    assert_eq!(store.get(&session_id), None);
    apply_pull_request_observation(
        &mut store,
        &session_id,
        PullRequestObservation::Trackable {
            number: 124,
            url: "https://github.com/skaft-software/ygg/pull/124".into(),
            state: PullRequestState::Ready,
        },
        37,
    )
    .unwrap();

    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Trackable {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
                state: PullRequestState::Merged,
            },
            40,
        )
        .unwrap(),
        Some(Some(PullRequestSummary {
            state: PullRequestState::Merged,
        }))
    );
    assert_eq!(
        apply_pull_request_observation(
            &mut store,
            &session_id,
            PullRequestObservation::Closed {
                number: 124,
                url: "https://github.com/skaft-software/ygg/pull/124".into(),
            },
            50,
        )
        .unwrap(),
        None
    );
    assert_eq!(
        store.summary(&session_id),
        Some(PullRequestSummary {
            state: PullRequestState::Merged,
        })
    );
}

#[tokio::test]
async fn pull_request_projection_leads_event_delivery_for_replacements() {
    let directory = tempfile::tempdir().unwrap();
    let plan = pull_request_worker_plan(directory.path(), "pull-request-event-order");
    let refresh_plan = PullRequestRefreshPlan::from(&plan);
    let projection = Arc::clone(&refresh_plan.projection);
    let (events, mut received) = mpsc::channel(1);
    events
        .send(event(EventPayload::SessionPullRequestChanged {
            pull_request: None,
        }))
        .await
        .unwrap();

    let publisher = tokio::spawn(async move {
        publish_pull_request_projection(
            &refresh_plan,
            &events,
            Some(PullRequestSummary {
                state: PullRequestState::Ready,
            }),
        )
        .await
    });

    tokio::time::timeout(std::time::Duration::from_millis(250), async {
        loop {
            if *projection.lock().unwrap()
                == Some(PullRequestSummary {
                    state: PullRequestState::Ready,
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("projection should advance while event delivery is backpressured");
    assert!(!publisher.is_finished());

    let _ = received.recv().await.unwrap();
    publisher.await.unwrap().unwrap();
    assert!(matches!(
        received.recv().await.unwrap().payload,
        EventPayload::SessionPullRequestChanged {
            pull_request: Some(PullRequestSummary {
                state: PullRequestState::Ready,
            })
        }
    ));
}
