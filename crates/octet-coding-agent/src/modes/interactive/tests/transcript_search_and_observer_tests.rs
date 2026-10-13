//! Transcript search ownership across idle and active runs, the configuration commit
//! observer, scoped model cycling, and custom compaction instructions.
//! Separate because each of these is an admission-control question rather than a
//! rendering question.

use super::*;

use super::support::*;

#[tokio::test]
async fn transcript_search_idle_owner_intercepts_paste_before_composer_admission() {
    clipboard_read::set_test_text(Some("native clipboard must stay out of the draft".into()));
    let (mut shell, idle) = idle_shell_after(vec![
        Event::Paste("preserved draft".into()),
        transcript_search_open_key(),
        Event::Paste("search query /tmp/image.png".into()),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            if cfg!(windows) {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            },
        )),
        ctrl_key('d'),
    ])
    .await;
    clipboard_read::clear_test_text();
    assert!(matches!(idle, Idle::Quit));
    assert!(shell.transcript_search_active());
    assert_eq!(shell.pending(), "preserved draft");
    assert!(shell.drain_composed().attachments.is_empty());
}

#[tokio::test]
async fn transcript_search_active_owner_consumes_query_and_escape_without_interrupting_run() {
    let (_server, _workspace, mut agent) =
        scripted_agent_with_delay(Duration::from_millis(100)).await;
    let mut shell = InteractiveShell::test_shell();
    shell.extension_set_editor("preserved active draft".into());
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    for event in [
        transcript_search_open_key(),
        Event::Paste("search query /tmp/image.png".into()),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            if cfg!(windows) {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            },
        )),
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        )),
    ] {
        sender.send(Ok(event)).await.unwrap();
    }
    let _sender = sender;
    let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut deadline = None;
    clipboard_read::set_test_text(Some("native clipboard must stay out of the draft".into()));
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        drive_active_run(
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
            &mut false,
            test_run_inspection(),
            &mut deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    clipboard_read::clear_test_text();
    drop(run);
    assert_eq!(ended, HostRunOutcome::Completed);
    assert!(!quit);
    assert!(pending.is_empty());
    assert!(!shell.transcript_search_active());
    assert_eq!(shell.pending(), "preserved active draft");
    assert!(shell.drain_composed().attachments.is_empty());
    assert!(!format!("{:?}", agent.session().context().unwrap()).contains("search query"));
}

#[tokio::test]
async fn configuration_commit_observer_skips_noops_and_unknown_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("config.toml");
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let missing = configuration_snapshot(&path);
    assert_eq!(missing, Some(None));
    assert!(!observe_configuration_commit(&mut extensions, missing.clone(), Some(&path)).await);
    std::fs::write(&path, b"theme = \"dark\"\n").unwrap();
    assert!(observe_configuration_commit(&mut extensions, missing, Some(&path)).await);
    let unchanged = configuration_snapshot(&path);
    assert!(!observe_configuration_commit(&mut extensions, unchanged, Some(&path)).await);
    assert!(!observe_configuration_commit(&mut extensions, None, Some(&path)).await);
    assert!(!observe_configuration_commit(&mut extensions, Some(None), None).await);
    std::fs::write(&path, vec![b'x'; 1024 * 1024 + 1]).unwrap();
    assert!(configuration_snapshot(&path).is_none());
    #[cfg(unix)]
    {
        let link = path.with_file_name("link.toml");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(configuration_snapshot(&link).is_none());
    }
    let failed: anyhow::Result<()> = persist_configuration(Some(&mut extensions), || {
        anyhow::bail!("failed before commit")
    })
    .await;
    assert!(failed.is_err());
}

#[tokio::test]
async fn scoped_model_cycle_reaches_idle_owner_without_draining_draft() {
    for draft in ["unfinished prompt", "/model second"] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("test", "first", "off");
        shell.set_model_cycle(vec!["first".into(), "second".into()]);
        shell.extension_set_editor(draft.into());
        let mut input = tokio_stream::iter([Ok(ctrl_key('p'))]);
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
            None,
        )
        .await
        .unwrap();
        assert!(matches!(idle, Idle::Command(text) if text == "/model second"));
        assert_eq!(shell.pending(), draft);
    }
}

#[tokio::test]
async fn scoped_model_cycle_queues_on_active_owner_without_draining_draft() {
    for draft in ["unfinished active prompt", "/model second"] {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_millis(100)).await;
        let mut shell = InteractiveShell::test_shell();
        shell.set_identity("test", "scripted", "off");
        shell.set_model_cycle(vec!["scripted".into(), "second".into(), "third".into()]);
        shell.extension_set_editor(draft.into());
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        sender.send(Ok(ctrl_key('p'))).await.unwrap();
        sender.send(Ok(ctrl_key('p'))).await.unwrap();
        let _sender = sender;
        let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(5),
            drive_active_run(
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
                &mut false,
                test_run_inspection(),
                &mut deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        drop(run);
        assert_eq!(ended, HostRunOutcome::Completed);
        assert!(!quit);
        assert_eq!(shell.pending(), draft);
        assert_eq!(
            pending,
            VecDeque::from([PendingIdleAction::ChangeModel(ModelId("third".into()))])
        );
        assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/model"));
    }
}

#[tokio::test]
async fn compact_custom_instructions_queue_and_reach_only_the_summary_wire() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    for queued in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(text_turn()),
            )
            .mount(&server)
            .await;
        let (_directory, mut app) = fast_test_app(scripted_model(&server.uri()));
        seed_compaction_session(&mut app.agent);
        app.config.compaction.keep_recent_tokens = 1;
        let mut shell = InteractiveShell::test_shell();
        let requested = commands::parse("/compact preserve the API contract");
        let instructions = if queued {
            let (mut queue, quit) =
                run_active_command(&mut shell, requested, test_run_inspection()).await;
            assert!(!quit);
            assert!(server.received_requests().await.unwrap().is_empty());
            let Some(PendingIdleAction::CompactWithInstructions(value)) = queue.pop_front() else {
                panic!("missing queued instructions")
            };
            assert!(queue.is_empty());
            value
        } else {
            let Command::CompactWithInstructions(value) = requested else {
                panic!("missing instructions")
            };
            value
        };
        let mut input = futures_util::stream::pending();
        compact_interactively(
            &mut app,
            &mut shell,
            &mut input,
            !queued,
            Some(&instructions),
        )
        .await;
        assert_eq!(shell.debug_error(), None);
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        assert!(body["system"].to_string().contains(&instructions));
        assert!(!body["messages"].to_string().contains(&instructions));
        assert_eq!(
            app.agent.session().usage_records().len(),
            1,
            "summary accounted exactly once"
        );
        assert!(!format!("{:?}", app.agent.session().context().unwrap()).contains(&instructions));
    }
}

#[tokio::test]
async fn compact_custom_instructions_fail_closed_for_native_and_oversize() {
    let server = wiremock::MockServer::start().await;
    let (_directory, mut app) = fast_test_app(scripted_codex_model(&server.uri()));
    app.config.compaction.mode = CompactionMode::NativeResponses;
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending();
    compact_interactively(
        &mut app,
        &mut shell,
        &mut input,
        true,
        Some("preserve evidence"),
    )
    .await;
    assert!(shell
        .debug_snapshot()
        .contains("custom instructions require local compaction mode"));
    let oversized = "x".repeat(16 * 1024 + 1);
    compact_interactively(&mut app, &mut shell, &mut input, true, Some(&oversized)).await;
    assert!(shell.debug_error().unwrap().contains("at most 16 KiB"));
    assert!(server.received_requests().await.unwrap().is_empty());
}
