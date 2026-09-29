//! Durable head restoration, reconfiguration coalescing, command-queue parsing, the
//! startup update check, and the idle slash-Enter dispatch.
//! Separate because these are the small, purely synchronous seams between the idle
//! loop and durable state.

use super::*;

use super::support::*;

#[test]
fn a_failed_checkout_can_restore_the_previous_durable_head() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rollback.jsonl");
    let mut session = Session::create(&path).unwrap();
    let previous = session
        .append(EntryValue::Config {
            model: Some("model".to_string()),
            reasoning: Some("off".to_string()),
            reasoning_mode: None,
        })
        .unwrap();
    let target = session
        .append(EntryValue::Config {
            model: Some("missing-model".to_string()),
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    session.checkout(target).unwrap();
    drop(session);

    restore_session_head(&path, previous.clone()).unwrap();
    assert_eq!(Session::open(path).unwrap().head(), Some(previous));
}

#[test]
fn adjacent_reconfigurations_coalesce_but_boundaries_survive() {
    let mut queue = VecDeque::new();
    push_pending_action(
        &mut queue,
        PendingIdleAction::ChangeModel(ModelId("a".into())),
    );
    push_pending_action(
        &mut queue,
        PendingIdleAction::ChangeModel(ModelId("b".into())),
    );
    push_pending_action(&mut queue, PendingIdleAction::NewSession);
    push_pending_action(
        &mut queue,
        PendingIdleAction::ChangeModel(ModelId("c".into())),
    );
    assert_eq!(
        queue,
        VecDeque::from([
            PendingIdleAction::ChangeModel(ModelId("b".into())),
            PendingIdleAction::NewSession,
            PendingIdleAction::ChangeModel(ModelId("c".into())),
        ])
    );
}

#[test]
fn active_thinking_preference_writes_coalesce_without_crossing_barriers() {
    let mut queue = VecDeque::new();
    push_pending_action(
        &mut queue,
        PendingIdleAction::PersistThinkingPreference("low".into()),
    );
    push_pending_action(
        &mut queue,
        PendingIdleAction::PersistThinkingPreference("medium".into()),
    );
    push_pending_action(&mut queue, PendingIdleAction::NewSession);
    push_pending_action(
        &mut queue,
        PendingIdleAction::PersistThinkingPreference("high".into()),
    );

    assert_eq!(
        queue,
        VecDeque::from([
            PendingIdleAction::PersistThinkingPreference("medium".into()),
            PendingIdleAction::NewSession,
            PendingIdleAction::PersistThinkingPreference("high".into()),
        ])
    );
}

#[test]
fn command_queue_parses_reconfiguration_values() {
    let mut queue = VecDeque::new();
    queue_command(Command::Login(None), &mut queue).unwrap();
    queue_command(Command::Setup, &mut queue).unwrap();
    queue_command(Command::Thinking(Some("high".into())), &mut queue).unwrap();
    queue_command(Command::Resume(Some("id".into())), &mut queue).unwrap();
    assert_eq!(queue.pop_front(), Some(PendingIdleAction::Login(None)));
    assert_eq!(queue.pop_front(), Some(PendingIdleAction::Setup));
    assert!(matches!(
        queue.pop_front(),
        Some(PendingIdleAction::ChangeThinkingLevel(ThinkingLevel::High))
    ));
    assert_eq!(
        queue.pop_front(),
        Some(PendingIdleAction::ResumeSession(Some("id".into())))
    );
}

#[tokio::test]
async fn startup_update_is_skipped_offline_and_cancelled_with_its_owner() {
    let offline = startup_update_task(
        true,
        async { panic!("offline startup must not poll the release request") },
        |_| panic!("offline startup must not publish a notice"),
    );
    assert!(offline.is_empty());

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let check = startup_update_task(
        false,
        async move {
            let _ = started_tx.send(());
            let _ = held_rx.await;
            Some(semver::Version::new(9, 8, 7))
        },
        |_| panic!("exited startup must not publish a notice"),
    );
    started_rx.await.unwrap();
    assert_eq!(check.len(), 1);
    drop(check);
    let mut held_tx = held_tx;
    tokio::time::timeout(Duration::from_secs(1), held_tx.closed())
        .await
        .expect("owner exit must cancel the held check");
}

#[tokio::test]
async fn startup_update_publishes_once_and_failure_is_quiet() {
    for latest in [None, Some(semver::Version::new(9, 8, 7))] {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = calls.clone();
        let expected = usize::from(latest.is_some());
        let mut check = startup_update_task(false, async move { latest }, move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(check.len(), 1);
        check.join_next().await.unwrap().unwrap();
        assert!(check.is_empty());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), expected);
    }
}

#[tokio::test]
async fn idle_slash_enter_returns_the_highlighted_command_to_dispatch() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let mut shell = InteractiveShell::test_shell();
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    for event in [
        Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    ] {
        sender.send(Ok(event)).await.unwrap();
    }
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut scroll_tick = tokio::time::interval(Duration::from_millis(16));
    let mut extension_tick = tokio::time::interval(Duration::from_millis(50));
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let (reload_watcher, mut reload, mut reload_tick) = test_reload();

    let idle = wait_for_prompt(
        &mut shell,
        &mut input,
        &mut scroll_tick,
        &mut extension_tick,
        &mut extensions,
        None,
        &mut reload_tick,
        &reload_watcher,
        &mut reload,
    )
    .await
    .unwrap();
    let Idle::Command(command) = idle else {
        panic!("highlighted slash command was not handed to the idle dispatcher");
    };
    assert_eq!(command.trim(), "/resume");
    assert!(shell.pending_is_empty());
    assert!(!shell.slash_popup_open());
}
