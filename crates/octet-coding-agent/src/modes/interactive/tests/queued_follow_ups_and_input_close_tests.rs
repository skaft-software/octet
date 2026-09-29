//! Queued follow-up dispatch and the closed-input path taken while an aborted run
//! settles.
//! Separate because a closed stream is a distinct terminal state from a pending one
//! and must not be conflated with it.

use super::*;

use super::support::*;

#[tokio::test]
async fn queued_follow_ups_dispatch_only_after_completion_or_escape_settlement() {
    use crossterm::event::KeyEvent;
    for (cancel, dispatch, quit_expected, escape_first) in [
        (
            Some(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            true,
            false,
            false,
        ),
        (
            Some(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            false,
            false,
            false,
        ),
        (
            Some(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            false,
            true,
            false,
        ),
        (
            Some(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            false,
            false,
            true,
        ),
        (None, true, false, false),
    ] {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(10)).await;
        let mut shell = InteractiveShell::test_shell();
        let mut events = vec![
            Event::Paste("queued first".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Event::Paste("queued second".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            dequeue_gesture(),
            Event::Paste(" edited".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ];
        if escape_first {
            events.push(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        }
        if let Some(cancel) = cancel {
            events.push(Event::Key(cancel));
            // Neither repeated Escape nor a second fresh Escape can arm a
            // Ctrl+C cancellation after settlement has already started.
            events.push(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Esc,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )));
            events.push(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)));
        }
        let mut input =
            tokio_stream::iter(events.into_iter().map(Ok)).chain(futures_util::stream::pending());
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut quit = false;
        let mut goal_deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(2),
            drive_active_run(
                &mut run,
                &control,
                &mut shell,
                &mut input,
                &mut ticker,
                &mut VecDeque::new(),
                &mut quit,
                None,
                None,
                &mut crate::extensions::ExecutableExtensions::default(),
                &mut false,
                test_run_inspection(),
                &mut goal_deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(run.next().await.is_none(), "one terminal event");
        drop(run);
        assert_eq!(quit, quit_expected);
        assert_eq!(
            ended,
            if cancel.is_some() {
                HostRunOutcome::Aborted
            } else {
                HostRunOutcome::Completed
            }
        );
        assert_eq!(agent.session().checkpoints().len(), 1);
        assert_eq!(
            shell
                .take_ready_follow_up()
                .map(|input| input.transcript_text),
            dispatch.then(|| "queued first".to_owned())
        );
        assert!(shell.take_ready_follow_up().is_none());
        shell.edit_queued_message();
        assert_eq!(shell.pending(), "queued second edited");
    }
}

struct EndsThenPanics(bool);

impl Stream for EndsThenPanics {
    type Item = std::io::Result<Event>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        assert!(!self.0, "a closed input stream was polled more than once");
        self.0 = true;
        std::task::Poll::Ready(None)
    }
}

#[tokio::test]
async fn closed_input_is_disabled_while_the_aborted_run_settles() {
    let (_server, _workspace, mut agent) = scripted_agent().await;
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut input = EndsThenPanics(false);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut executable_extensions = crate::extensions::ExecutableExtensions::default();

    let mut goal_deadline = None;
    let ended = drive_active_run(
        &mut run,
        &control,
        &mut shell,
        &mut input,
        &mut ticker,
        &mut pending,
        &mut quit,
        None,
        None,
        &mut executable_extensions,
        &mut false,
        test_run_inspection(),
        &mut goal_deadline,
    )
    .await
    .unwrap();
    drop(run);

    assert_eq!(ended, HostRunOutcome::Aborted);
    assert!(quit);
    assert!(shell.debug_snapshot().contains("Interrupted"));
}
