use super::*;

#[tokio::test]
async fn pending_messages_observation_tracks_only_steering_and_follow_up_reservations() {
    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    assert!(!control.has_pending_messages());
    control
        .set_steering_mode(QueueDeliveryMode::OneAtATime)
        .await
        .unwrap();
    assert!(!control.has_pending_messages());
    drop(rx.recv().await.unwrap());
    control
        .try_append_custom(crate::session::CustomMessage {
            custom_type: "context".into(),
            content: crate::session::CustomMessageContent::Text("note".into()),
            display: false,
            details: None,
        })
        .unwrap();
    let custom = rx.recv().await.unwrap();
    assert!(!control.has_pending_messages());
    control.finish_now("answer").await.unwrap();
    let finish = rx.recv().await.unwrap();
    assert!(!control.has_pending_messages());
    control.try_follow_up("queued").unwrap();
    assert!(control.has_pending_messages());
    let Control::FollowUp(input) = rx.recv().await.unwrap() else {
        panic!("follow-up")
    };
    let mut pending = Vec::new();
    input.push_pending(&mut pending);
    assert!(
        control.has_pending_messages(),
        "ingress drain is not delivery"
    );
    let payload = pending.pop().unwrap().claim().unwrap();
    assert!(
        control.has_pending_messages(),
        "claim is not durable delivery"
    );
    drop(payload);
    assert!(!control.has_pending_messages());
    drop((custom, finish));
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
}

#[tokio::test]
async fn pending_messages_observation_retract_cancel_and_run_settlement_release() {
    let (control, mut rx) = test_run_control(MAX_PENDING_CONTROL_BYTES);
    let (prepared, receipt) = control.prepare_steer("retract").unwrap();
    assert!(control.has_pending_messages());
    assert!(receipt.try_retract());
    assert!(!control.has_pending_messages());
    drop(prepared);
    for _ in 0..8 {
        control
            .set_steering_mode(QueueDeliveryMode::OneAtATime)
            .await
            .unwrap();
    }
    assert!(!control.has_pending_messages());
    let (prepared, _) = control.prepare_steer("cancel send").unwrap();
    let send = tokio::spawn({
        let control = control.clone();
        async move { control.steer_retractable(prepared).await }
    });
    tokio::task::yield_now().await;
    assert!(control.has_pending_messages());
    send.abort();
    assert!(send.await.unwrap_err().is_cancelled());
    assert!(!control.has_pending_messages());
    while rx.try_recv().is_ok() {}
    control.try_follow_up("settle").unwrap();
    let pending = rx.recv().await.unwrap();
    assert!(control.has_pending_messages());
    *control.admission.lock().unwrap() = false;
    assert!(
        !control.has_pending_messages(),
        "settled runs have no live queue"
    );
    drop(pending);
    assert_eq!(
        control.pending_messages.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
    assert_eq!(
        control.pending_count.available_permits(),
        MAX_PENDING_CONTROL_INPUTS
    );
}
