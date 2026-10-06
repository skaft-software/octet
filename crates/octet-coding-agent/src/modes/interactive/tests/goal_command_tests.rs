//! `/goal` mutations applied mid-run and at idle, and the fail-closed cases when the
//! durable goal store is unreachable or rejects.
//! Separate because goal commands are the one control family that never queues.

use super::*;

use super::support::*;

/// A real durable goal store, its driver, and an inspection that addresses
/// it exactly as `App` does: same store, same shared driver state, same
/// session key. The caller keeps the store and driver to read back the
/// mutation and to prove driver coherence.
fn goal_fixture(
    dir: &Path,
    session_id: &str,
) -> (
    Arc<octet_agent::DurableGoalStore>,
    octet_agent::GoalDriver,
    ActiveRunInspection,
) {
    let store =
        Arc::new(octet_agent::DurableGoalStore::open(dir).expect("durable goal store fixture"));
    let driver = octet_agent::GoalDriver::new(store.clone(), session_id);
    let inspection = test_run_inspection_with_goal(dir, store.clone(), driver.clone(), session_id);
    (store, driver, inspection)
}

#[test]
fn goal_commands_have_no_queued_form() {
    let mut queue = VecDeque::new();
    for command in [
        GoalCommand::Help,
        GoalCommand::Status,
        GoalCommand::Set("an objective".into()),
        GoalCommand::Pause,
        GoalCommand::Resume,
        GoalCommand::Clear,
    ] {
        let error = queue_command(Command::Goal(command), &mut queue)
            .expect_err("goal commands are never queued");
        assert!(
            error.to_string().contains("never queued"),
            "the refusal names the defect: {error}"
        );
    }
    assert!(queue.is_empty(), "no goal action was deferred: {queue:?}");
    assert!(matches!(
        commands::parse("/goal close the parity row"),
        Command::Goal(GoalCommand::Set(objective)) if objective == "close the parity row"
    ));
}

#[tokio::test]
async fn active_goal_objective_mutates_the_durable_store_without_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, driver, inspection) = goal_fixture(dir.path(), "goal-session");
    let mut shell = InteractiveShell::test_shell();
    let (queue, quit, deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Set("close the goal-parity row".into())),
        &inspection,
    )
    .await;
    assert!(queue.is_empty(), "mid-run /goal must not queue: {queue:?}");
    assert!(!quit);
    let goal = store
        .get("goal-session")
        .expect("the fixture store stays readable")
        .expect("the durable goal exists immediately");
    assert_eq!(goal.objective, "close the goal-parity row");
    assert_eq!(goal.status, octet_agent::GoalStatus::Active);
    assert_eq!(goal.turns_used, 0, "setting an objective reserves no turn");
    assert!(
        deadline.is_some(),
        "an active objective arms the same deadline an idle /goal arms"
    );
    let painted = shell.debug_snapshot();
    assert!(painted.contains("goal set"), "{painted}");
    // Driver coherence: the shared driver waits on the new objective exactly
    // as it would after an idle `/goal`, and nothing has been reserved.
    assert!(matches!(
        driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
        Ok(octet_agent::GoalDecision::Wait { .. })
    ));
    assert!(goal.turns_used == 0);
}

#[tokio::test]
async fn active_goal_commands_apply_while_the_run_is_still_streaming() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (server, mut started, release) = HeldApi::start(text_turn()).await;
    let (_workspace, mut agent) =
        scripted_agent_for_route(scripted_model(&server.uri), octet_ai::AiClient::new());
    let dir = tempfile::tempdir().unwrap();
    let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
    store
        .set("goal-session", "keep the stream honest", None)
        .expect("durable goal fixture");

    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let (sender, receiver) = tokio::sync::mpsc::channel(64);
    // The objective change is typed first: it mutates no overlay, so the
    // status report typed second still receives its Enter.
    for command in ["/goal set streamed objective", "/goal status"] {
        for character in command.chars() {
            sender
                .send(Ok(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))))
                .await
                .unwrap();
        }
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .await
            .unwrap();
    }
    // Keep the sender alive so EOF cannot abort the held run.
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut made_tool_call = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let mut goal_deadline = None;
    // The run borrows the shell, its queue, and the deadline for the whole
    // held turn, so the mid-run observations happen inside this block and
    // every post-settlement assertion happens after its borrows end.
    let ended = {
        let drive = drive_active_run(
            &mut run,
            &control,
            &mut shell,
            &mut input,
            &mut ticker,
            &mut pending,
            &mut quit,
            None,
            None,
            &mut extensions,
            &mut made_tool_call,
            &inspection,
            &mut goal_deadline,
        );
        tokio::pin!(drive);
        tokio::select! {
            result = &mut drive => {
                panic!("the held run settled before the provider request started: {result:?}")
            }
            result = &mut started => result.expect("the held provider request starts"),
        }

        // The provider response is still withheld, so a store mutation
        // observed here was applied to the durable goal mid-run, while the
        // stream was live.
        let budget = Instant::now() + Duration::from_secs(2);
        while store.get("goal-session").unwrap().unwrap().objective != "streamed objective" {
            assert!(
                Instant::now() < budget,
                "the mid-run objective was never applied while kept alive"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(10), &mut drive)
                    .await
                    .is_err(),
                "the run settled before the mid-run /goal was applied"
            );
        }
        let streamed = store.get("goal-session").unwrap().unwrap();
        assert_eq!(streamed.objective, "streamed objective");
        assert_eq!(streamed.status, octet_agent::GoalStatus::Active);
        assert_eq!(
            streamed.turns_used, 0,
            "the mid-run objective reserved no continuation turn"
        );

        release.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(5), &mut drive)
            .await
            .expect("the released run settles")
            .unwrap()
    };
    assert_eq!(ended, HostRunOutcome::Completed);
    drop(run);
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        goal_deadline.is_some(),
        "the mid-run objective armed the continuation deadline"
    );
    assert!(
        pending.is_empty(),
        "neither goal command was queued to the idle boundary: {pending:?}"
    );
    assert!(!quit);
    assert!(
        shell.has_overlay(),
        "the /goal status report opened mid-run and stayed open"
    );
    let painted = shell.debug_snapshot();
    assert!(painted.contains("goal set"), "{painted}");
    assert!(
        painted.contains("streamed objective"),
        "the notice names the objective applied mid-run: {painted}"
    );
}

#[tokio::test]
async fn active_goal_status_reports_immediately_without_queueing() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
    store
        .set("goal-session", "keep the stream honest", None)
        .expect("durable goal fixture");
    let mut shell = InteractiveShell::test_shell();
    let (queue, quit, deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Status),
        &inspection,
    )
    .await;
    assert!(queue.is_empty(), "status is never queued: {queue:?}");
    assert!(!quit);
    assert!(deadline.is_none(), "status arms no deadline");
    assert!(
        shell.has_overlay(),
        "the active-run status reports in the frame immediately"
    );
    let status = inspection
        .goal_access()
        .expect("addressable fixture")
        .status_text()
        .expect("the report reads the durable store");
    assert!(
        status.contains("Active goal: keep the stream honest"),
        "the open report carries the durable goal: {status}"
    );
    assert_eq!(
        store.get("goal-session").unwrap().unwrap().objective,
        "keep the stream honest",
        "status does not mutate the goal"
    );
}

#[tokio::test]
async fn active_goal_pause_resume_and_clear_are_coherent_mid_run() {
    let dir = tempfile::tempdir().unwrap();
    let (store, driver, inspection) = goal_fixture(dir.path(), "goal-session");
    store
        .set("goal-session", "stay coherent", None)
        .expect("durable goal fixture");
    let mut shell = InteractiveShell::test_shell();

    let (queue, _, paused_deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Pause),
        &inspection,
    )
    .await;
    assert!(queue.is_empty(), "pause is never queued: {queue:?}");
    assert_eq!(
        store.get("goal-session").unwrap().unwrap().status,
        octet_agent::GoalStatus::Paused
    );
    assert!(
        paused_deadline.is_none(),
        "a paused goal disarms the continuation deadline"
    );
    assert_eq!(
        driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
        Ok(octet_agent::GoalDecision::Paused),
        "a paused goal cannot continue the run that paused it"
    );
    assert!(
        driver.fire_continuation().unwrap().is_none(),
        "a paused goal reserves no continuation turn"
    );

    let (queue, _, resumed_deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Resume),
        &inspection,
    )
    .await;
    assert!(queue.is_empty(), "resume is never queued: {queue:?}");
    let resumed = store.get("goal-session").unwrap().unwrap();
    assert_eq!(resumed.status, octet_agent::GoalStatus::Active);
    assert_eq!(
        resumed.objective, "stay coherent",
        "resume keeps the objective"
    );
    assert!(resumed_deadline.is_some(), "resume re-arms the deadline");
    assert!(matches!(
        driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
        Ok(octet_agent::GoalDecision::Wait { .. })
    ));

    let (queue, _, cleared_deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Clear),
        &inspection,
    )
    .await;
    assert!(queue.is_empty(), "clear is never queued: {queue:?}");
    assert!(store.get("goal-session").unwrap().is_none());
    assert!(
        cleared_deadline.is_none(),
        "a cleared goal disarms the continuation deadline"
    );
    assert_eq!(
        driver.turn_settled(octet_agent::GoalTurnSource::User, "", false),
        Ok(octet_agent::GoalDecision::Inactive)
    );
    assert!(
        driver.fire_continuation().unwrap().is_none(),
        "a cleared goal reserves no continuation turn"
    );
    let painted = shell.debug_snapshot();
    for expected in ["goal paused", "goal resumed", "goal cleared"] {
        assert!(
            painted.contains(expected),
            "{expected:?} missing: {painted}"
        );
    }
    assert!(paused_deadline.is_none());
}

#[tokio::test]
async fn unaddressable_active_goal_fails_closed_and_is_never_queued() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(octet_agent::DurableGoalStore::open(dir.path()).expect("store fixture"));
    let driver = octet_agent::GoalDriver::new(store.clone(), "goal-session");
    assert!(matches!(
        GoalAccess::from_parts(store, driver, String::new()),
        Err(ActiveGoalError::UnaddressableSession)
    ));
    let mut shell = InteractiveShell::test_shell();
    for command in [
        GoalCommand::Status,
        GoalCommand::Set("never queued".into()),
        GoalCommand::Pause,
        GoalCommand::Resume,
        GoalCommand::Clear,
    ] {
        let (queue, quit, deadline) = run_active_command_observing_deadline(
            &mut shell,
            Command::Goal(command),
            test_run_inspection(),
        )
        .await;
        assert!(queue.is_empty(), "the refusal is never queued: {queue:?}");
        assert!(!quit);
        assert!(deadline.is_none(), "no deadline can be armed");
        let error = shell
            .debug_error()
            .expect("the typed reason is rendered, not a silent no-op");
        assert!(error.starts_with("/goal failed:"), "{error}");
        assert!(error.contains("no durable goal identity"), "{error}");
    }
}

#[tokio::test]
async fn a_reachable_goal_store_that_rejects_still_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (store, _driver, inspection) = goal_fixture(dir.path(), "goal-session");
    let mut shell = InteractiveShell::test_shell();
    let oversized = "x".repeat(octet_agent::MAX_GOAL_OBJECTIVE_BYTES + 1);
    let (queue, _, deadline) = run_active_command_observing_deadline(
        &mut shell,
        Command::Goal(GoalCommand::Set(oversized)),
        &inspection,
    )
    .await;
    assert!(
        queue.is_empty(),
        "a rejected goal is never queued: {queue:?}"
    );
    assert!(deadline.is_none(), "a rejected goal arms no deadline");
    assert!(
        store.get("goal-session").unwrap().is_none(),
        "the store rejected the objective and stayed empty"
    );
    let error = shell.debug_error().expect("the store error is rendered");
    assert!(error.starts_with("unable to set goal:"), "{error}");
    assert!(error.contains("invalid goal objective"), "{error}");
}

/// `/goal` at idle applies the same durable mutations through the same
/// producer the idle dispatcher's arm calls, on a real bootstrap-built App
/// with a real durable store and session key.
#[tokio::test]
async fn idle_goal_commands_still_apply_the_same_durable_mutations() {
    let (_workspace, app) = crate::compaction::tests::app_for_estimate();
    let store = app.goal_store.clone();
    let session_id = app.goal_session_id.clone();
    assert!(
        !session_id.is_empty(),
        "the idle fixture carries a durable goal identity"
    );
    let mut shell = InteractiveShell::test_shell();
    let mut goal_deadline = None;

    apply_idle_goal_command(
        &app,
        &mut shell,
        GoalCommand::Set("idle objective".into()),
        &mut goal_deadline,
    )
    .expect("idle /goal set");
    let goal = store
        .get(&session_id)
        .unwrap()
        .expect("idle /goal writes the durable store");
    assert_eq!(goal.objective, "idle objective");
    assert_eq!(goal.status, octet_agent::GoalStatus::Active);
    assert!(goal_deadline.is_some(), "idle /goal arms the deadline");
    assert!(shell.debug_snapshot().contains("goal set"));

    apply_idle_goal_command(&app, &mut shell, GoalCommand::Status, &mut goal_deadline)
        .expect("idle /goal status");
    assert!(
        shell.has_overlay(),
        "idle /goal status opens the same report"
    );
    assert!(store.get(&session_id).unwrap().is_some());

    apply_idle_goal_command(&app, &mut shell, GoalCommand::Pause, &mut goal_deadline)
        .expect("idle /goal pause");
    assert_eq!(
        store.get(&session_id).unwrap().unwrap().status,
        octet_agent::GoalStatus::Paused
    );
    assert!(goal_deadline.is_none());

    apply_idle_goal_command(&app, &mut shell, GoalCommand::Resume, &mut goal_deadline)
        .expect("idle /goal resume");
    assert_eq!(
        store.get(&session_id).unwrap().unwrap().status,
        octet_agent::GoalStatus::Active
    );
    assert!(goal_deadline.is_some());

    apply_idle_goal_command(&app, &mut shell, GoalCommand::Clear, &mut goal_deadline)
        .expect("idle /goal clear");
    assert!(store.get(&session_id).unwrap().is_none());
    assert!(goal_deadline.is_none());
}
