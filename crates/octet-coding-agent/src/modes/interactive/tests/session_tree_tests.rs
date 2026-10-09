//! Same-session navigation owns dialogs and provider cancellation only at idle.
use super::super::session_tree_menu::{apply_navigation, choose_navigation, TreeNavigationChoice};
use super::support::*;
use super::*;
use crossterm::event::KeyEvent;

fn key(code: KeyCode) -> std::io::Result<Event> {
    Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
}

fn user(text: &str) -> EntryValue {
    EntryValue::Message(octet_ai::Message::User(octet_ai::UserMessage {
        content: vec![octet_ai::UserPart::Text(text.into())],
    }))
}

fn assistant(text: &str) -> EntryValue {
    EntryValue::Message(octet_ai::Message::Assistant(octet_ai::AssistantMessage {
        content: vec![octet_ai::AssistantPart::Text(text.into())],
        model: ModelId("scripted".into()),
        protocol: octet_ai::Protocol::AnthropicMessages,
    }))
}

fn branch_fixture(session: &mut Session) -> (EntryId, EntryId, EntryId, EntryId) {
    let root = session.append(user("shared root")).unwrap();
    let abandoned = session.append(assistant("abandoned answer")).unwrap();
    session.checkout(root.clone()).unwrap();
    let target = session.append(user("revisit prompt")).unwrap();
    let head = session.append(assistant("current answer")).unwrap();
    (root, abandoned, target, head)
}

#[tokio::test]
async fn no_summary_workflow_searches_by_preview_without_changing_session_or_draft() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("tree.jsonl")).unwrap();
    let (_, _, target, head) = branch_fixture(&mut session);
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("retained draft".into());
    let before = shell.extension_editor_snapshot();
    let mut events = "revisit"
        .chars()
        .map(|c| key(KeyCode::Char(c)))
        .collect::<Vec<_>>();
    events.extend([key(KeyCode::Enter), key(KeyCode::Enter)]);
    let mut input = futures_util::stream::iter(events);
    let choice = choose_navigation(&session, &mut shell, &mut input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        choice,
        TreeNavigationChoice {
            target,
            summarize: false,
            custom_instructions: None
        }
    );
    assert_eq!(session.head(), Some(head));
    assert_eq!(shell.extension_editor_snapshot(), before);
}

#[tokio::test]
async fn custom_summary_cancel_returns_through_dialogs_without_prefilling_composer() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("tree.jsonl")).unwrap();
    let (_, _, _, head) = branch_fixture(&mut session);
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("draft survives every cancellation".into());
    let before = shell.extension_editor_snapshot();
    let mut events = "revisit"
        .chars()
        .map(|c| key(KeyCode::Char(c)))
        .collect::<Vec<_>>();
    events.extend([
        key(KeyCode::Enter),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
        Ok(Event::Paste("private summary focus".into())),
        key(KeyCode::Esc), // custom input -> summary choices
        key(KeyCode::Esc), // choices -> same tree selection
        key(KeyCode::Esc), // tree -> original draft
    ]);
    let mut input = futures_util::stream::iter(events);
    assert!(choose_navigation(&session, &mut shell, &mut input)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.head(), Some(head));
    assert_eq!(shell.extension_editor_snapshot(), before);
    assert!(!shell.debug_snapshot().contains("private summary focus"));
}

#[tokio::test]
async fn custom_summary_choice_keeps_instructions_separate_from_draft() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("tree.jsonl")).unwrap();
    let (_, _, target, _) = branch_fixture(&mut session);
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("draft".into());
    let mut events = "revisit"
        .chars()
        .map(|c| key(KeyCode::Char(c)))
        .collect::<Vec<_>>();
    events.extend([
        key(KeyCode::Enter),
        key(KeyCode::Down),
        key(KeyCode::Down),
        key(KeyCode::Enter),
        Ok(Event::Paste(
            "Focus on unresolved tests\nand the decision".into(),
        )),
        key(KeyCode::Enter),
    ]);
    let mut input = futures_util::stream::iter(events);
    let choice = choose_navigation(&session, &mut shell, &mut input)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(choice.target, target);
    assert!(choice.summarize);
    assert_eq!(
        choice.custom_instructions.as_deref(),
        Some("Focus on unresolved tests\nand the decision")
    );
    assert_eq!(shell.pending(), "draft");
}

#[tokio::test]
async fn current_head_selection_is_a_noop_without_summary_dialog() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("tree.jsonl")).unwrap();
    let (_, _, _, head) = branch_fixture(&mut session);
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("draft".into());
    let mut input = futures_util::stream::iter([key(KeyCode::Enter)]);
    assert!(choose_navigation(&session, &mut shell, &mut input)
        .await
        .unwrap()
        .is_none());
    assert_eq!(session.head(), Some(head));
    assert_eq!(shell.pending(), "draft");
    assert!(shell.debug_snapshot().contains("Already at this point"));
}

#[tokio::test]
async fn no_summary_navigates_same_file_and_revisits_branches_while_preserving_draft_and_cache_policy(
) {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let (root, abandoned, target, _) = branch_fixture(app.agent.session_mut());
    let path = app.agent.session().path().to_owned();
    let before = app.agent.session().entries().len();
    let mode = app.agent.cache_warming_mode();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    apply_navigation(
        &mut app,
        &mut shell,
        &mut input,
        TreeNavigationChoice {
            target,
            summarize: false,
            custom_instructions: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(app.agent.session().head(), Some(root));
    assert_eq!(shell.pending(), "revisit prompt");
    assert_eq!(app.agent.session().path(), path);
    assert_eq!(app.agent.session().entries().len(), before);
    assert_eq!(app.agent.cache_warming_mode(), mode);
    assert!(shell.current_run_id().is_none());
    apply_navigation(
        &mut app,
        &mut shell,
        &mut input,
        TreeNavigationChoice {
            target: abandoned.clone(),
            summarize: false,
            custom_instructions: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(app.agent.session().head(), Some(abandoned));
    assert_eq!(
        shell.pending(),
        "revisit prompt",
        "nonempty draft is never replaced by navigation"
    );
    assert!(shell.debug_snapshot().contains("abandoned answer"));
    assert!(!shell.debug_snapshot().contains("current answer"));
    assert_eq!(app.agent.session().path(), path);
    assert_eq!(app.agent.session().entries().len(), before);
}

#[tokio::test]
async fn real_default_and_custom_summaries_commit_once_in_the_same_session() {
    for instructions in [None, Some("Focus on outstanding test failures")] {
        // This fixture records request bodies only in its repeat-capable mode.
        let (server, _started, release) = HeldApi::start_with_repeat(text_turn(), true).await;
        let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
        app.agent
            .set_compaction_model(Some(scripted_model(&server.uri)));
        let (root, _, target, old_head) = branch_fixture(app.agent.session_mut());
        let path = app.agent.session().path().to_owned();
        let before = app.agent.session().entries().len();
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("do not overwrite this draft".into());
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        release.send(true).unwrap();
        tokio::time::timeout(
            Duration::from_secs(3),
            apply_navigation(
                &mut app,
                &mut shell,
                &mut input,
                TreeNavigationChoice {
                    target,
                    summarize: true,
                    custom_instructions: instructions.map(str::to_owned),
                },
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let summary_id = app.agent.session().head().unwrap();
        let summary = app.agent.session().entry(&summary_id).unwrap();
        assert_eq!(summary.parent, Some(root));
        assert!(
            matches!(&summary.value, EntryValue::BranchSummary { from_entry, summary, .. }
            if from_entry == &old_head && summary.contains("done"))
        );
        assert_eq!(app.agent.session().entries().len(), before + 1);
        assert_eq!(app.agent.session().path(), path);
        assert_eq!(shell.pending(), "do not overwrite this draft");
        assert!(
            shell.current_run_id().is_none(),
            "auxiliary summaries never create an assistant run"
        );
        let bodies = server.bodies.lock().unwrap();
        // Auxiliary summaries are real provider calls, not assistant runs.
        assert_eq!(bodies.len(), 1);
        assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(app.agent.session().usage_records().len(), 1);
        if let Some(instructions) = instructions {
            assert!(bodies
                .iter()
                .any(|body| body.to_string().contains(instructions)));
        }
    }
}

#[tokio::test]
async fn held_summary_pumps_resize_and_ctrl_c_without_clearing_draft_or_moving_head() {
    let (server, started, release) = HeldApi::start(text_turn()).await;
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.agent
        .set_compaction_model(Some(scripted_model(&server.uri)));
    let (_, _, target, old_head) = branch_fixture(app.agent.session_mut());
    let before = app.agent.session().entries().len();
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("retained cancellation draft".into());
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    let mut input = tokio_stream::wrappers::ReceiverStream::new(receiver);
    let stimulus = async move {
        started.await.unwrap();
        sender.send(Ok(Event::Resize(46, 12))).await.unwrap();
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))))
            .await
            .unwrap();
        let _held = (sender, release);
        std::future::pending::<()>().await;
    };
    tokio::pin!(stimulus);
    tokio::time::timeout(Duration::from_secs(3), async {
        tokio::select! {
            result = apply_navigation(&mut app, &mut shell, &mut input, TreeNavigationChoice {
                target, summarize: true, custom_instructions: None,
            }) => result.unwrap(),
            _ = &mut stimulus => unreachable!(),
        }
    })
    .await
    .unwrap();
    assert_eq!(app.agent.session().head(), Some(old_head));
    assert_eq!(app.agent.session().entries().len(), before);
    assert_eq!(shell.pending(), "retained cancellation draft");
    assert!(shell.debug_snapshot().contains("Tree navigation cancelled"));
    assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[tokio::test]
async fn busy_tree_is_only_queued_and_remains_an_ordering_barrier() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let head = inspection.read_only_session().unwrap().head();
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("busy draft".into());
    let (mut queue, quit) = run_active_command(&mut shell, Command::Tree, &inspection).await;
    assert!(!quit);
    assert_eq!(queue, VecDeque::from([PendingIdleAction::Tree]));
    assert_eq!(inspection.read_only_session().unwrap().head(), head);
    assert_eq!(shell.pending(), "busy draft");
    push_pending_action(
        &mut queue,
        PendingIdleAction::ChangeModel(ModelId("a".into())),
    );
    queue_command(Command::Tree, &mut queue).unwrap();
    push_pending_action(
        &mut queue,
        PendingIdleAction::ChangeModel(ModelId("b".into())),
    );
    assert_eq!(
        queue.len(),
        4,
        "a navigation boundary prevents model-control coalescing"
    );
    assert!(active_command_needs_idle_slot(&Command::Tree));
}

#[tokio::test]
async fn fork_before_a_user_and_at_head_create_distinct_files_without_moving_source() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let (root, abandoned, target, old_head) = branch_fixture(app.agent.session_mut());
    let source = app.agent.session().path().to_owned();
    let messages = active_fork_messages(app.agent.session());
    assert!(messages.iter().any(|message| message.entry_id == target.0));
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    for (entry, at, expected_head) in [
        (target, false, root),
        (old_head.clone(), true, old_head.clone()),
    ] {
        let id = fork_extension_session(&app, &mut shell, &mut input, Some(entry.0), at)
            .await
            .unwrap();
        let destination = app.sessions.path_by_id(&id).unwrap();
        assert_ne!(destination, source);
        let fork = Session::open_read_only(destination).unwrap();
        assert_eq!(fork.head(), Some(expected_head));
        assert!(
            fork.entry(&abandoned).is_none(),
            "the new file contains only the selected ancestry"
        );
        assert_eq!(app.agent.session().head(), Some(old_head.clone()));
    }
}

#[tokio::test]
async fn tree_search_and_page_selection_return_stable_original_entry_ids() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("tree.jsonl")).unwrap();
    let mut ids = Vec::new();
    for index in 0..40 {
        ids.push(
            session
                .append(assistant(&format!("answer-{index:02}")))
                .unwrap(),
        );
    }
    let mut shell = InteractiveShell::test_shell();
    let mut events = "answer-12"
        .chars()
        .map(|c| key(KeyCode::Char(c)))
        .collect::<Vec<_>>();
    events.push(key(KeyCode::Enter));
    let mut input = futures_util::stream::iter(events);
    assert_eq!(
        pickers::session_tree_picker(&mut shell, &mut input, &session, None)
            .await
            .unwrap(),
        Some(ids[12].clone())
    );
    let mut input = futures_util::stream::iter([
        key(KeyCode::Home),
        key(KeyCode::PageDown),
        key(KeyCode::Enter),
    ]);
    let id = pickers::session_tree_picker(&mut shell, &mut input, &session, None)
        .await
        .unwrap()
        .unwrap();
    assert!(ids.contains(&id));
    assert_ne!(id, ids[0]);
}

#[tokio::test]
async fn busy_import_and_share_queue_only_intents_without_reading_a_file_or_confirming() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let source = directory.path().join("does not exist.jsonl");
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("busy portability draft".into());
    for (command, action) in [
        (
            Command::Import(source.clone()),
            PendingIdleAction::Import(source),
        ),
        (Command::Share, PendingIdleAction::Share),
    ] {
        assert!(active_command_needs_idle_slot(&command));
        let (queue, quit) = run_active_command(&mut shell, command, &inspection).await;
        assert!(!quit);
        assert_eq!(queue, VecDeque::from([action]));
        assert_eq!(shell.pending(), "busy portability draft");
        assert!(!shell.has_overlay());
        assert!(shell.current_run_id().is_none());
    }
}

#[tokio::test]
async fn import_default_deny_and_escape_do_not_read_or_create_a_session_or_clear_draft() {
    use super::super::session_tree_menu;
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let before = std::fs::read(app.agent.session().path()).unwrap();
    let head = app.agent.session().head();
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("import cancellation draft".into());
    let editor = shell.extension_editor_snapshot();
    for event in [key(KeyCode::Enter), key(KeyCode::Esc)] {
        let mut input = futures_util::stream::iter([event]);
        let result = session_tree_menu::import(
            &mut app,
            &mut shell,
            &mut input,
            "missing source.jsonl".into(),
        )
        .await
        .unwrap();
        assert!(result.is_none());
        assert_eq!(shell.extension_editor_snapshot(), editor);
        assert_eq!(app.agent.session().head(), head);
        assert_eq!(std::fs::read(app.agent.session().path()).unwrap(), before);
        assert!(
            !shell.debug_snapshot().contains("import: "),
            "denial must not try to open the source"
        );
    }
}

#[tokio::test]
async fn confirmed_import_is_an_input_pumped_new_copy_and_does_not_submit_or_replace_the_owner() {
    use super::super::session_tree_menu;
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    branch_fixture(app.agent.session_mut());
    let original = app.agent.session().path().to_owned();
    let source = directory.path().join("source with spaces.jsonl");
    let before = std::fs::read(&original).unwrap();
    std::fs::write(&source, &before).unwrap();
    let head = app.agent.session().head();
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("retained import draft".into());
    let editor = shell.extension_editor_snapshot();
    let mut input = futures_util::stream::iter([
        key(KeyCode::Down),
        key(KeyCode::Enter),
        Ok(Event::Resize(54, 16)),
        key(KeyCode::Enter),
    ])
    .chain(futures_util::stream::pending());
    let destination = tokio::time::timeout(
        Duration::from_secs(3),
        session_tree_menu::import(&mut app, &mut shell, &mut input, source.clone()),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_ne!(destination, source);
    assert_ne!(destination, original);
    assert!(destination.starts_with(app.sessions.dir()));
    let imported = Session::open_read_only(&destination).unwrap();
    assert_eq!(imported.head(), head);
    assert_eq!(
        imported.entries().len(),
        app.agent.session().entries().len()
    );
    assert_eq!(app.agent.session().path(), original);
    assert_eq!(std::fs::read(original).unwrap(), before);
    assert_eq!(std::fs::read(source).unwrap(), before);
    assert_eq!(shell.extension_editor_snapshot(), editor);
    assert!(shell.current_run_id().is_none());
}

#[tokio::test]
async fn share_confirmation_is_default_cancel_preserves_draft_and_snapshot_cleanup_is_owned() {
    use super::super::session_tree_menu;
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    app.agent
        .session_mut()
        .append(user("synthetic share preview"))
        .unwrap();
    let store =
        crate::session_store::SessionStore::for_directory(directory.path(), directory.path());
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("share cancellation draft".into());
    let editor = shell.extension_editor_snapshot();
    for event in [key(KeyCode::Enter), key(KeyCode::Esc)] {
        let prepared = crate::session_commands::prepare_share(&store, "session").unwrap();
        let snapshot = prepared.package_path().to_owned();
        assert!(prepared.warning().contains("UNLISTED"));
        assert!(prepared.warning().contains("Anyone with the link"));
        assert_eq!(prepared.sha256().len(), 64);
        let mut input = futures_util::stream::iter([event]);
        assert!(
            !session_tree_menu::confirm_share(&app, &mut shell, &mut input, &prepared)
                .await
                .unwrap()
        );
        assert_eq!(shell.extension_editor_snapshot(), editor);
        assert!(snapshot.exists());
        drop(prepared);
        assert!(!snapshot.exists());
        assert!(shell.current_run_id().is_none());
    }
}

#[tokio::test]
async fn portability_ctrl_c_pumps_input_until_owned_work_settles_without_clearing_or_sending_draft()
{
    use super::super::session_tree_menu;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("cooperative cancellation draft".into());
    let editor = shell.extension_editor_snapshot();
    let cancelled = AtomicBool::new(false);
    let settled = AtomicBool::new(false);
    let work = async {
        while !cancelled.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        settled.store(true, Ordering::Release);
        Ok::<_, anyhow::Error>(())
    };
    let mut input = futures_util::stream::iter([
        Ok(Event::Resize(50, 15)),
        key(KeyCode::Enter),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))),
    ])
    .chain(futures_util::stream::pending());
    tokio::time::timeout(
        Duration::from_secs(3),
        session_tree_menu::drive_portability(
            work,
            &cancelled,
            &mut app.executable_extensions,
            &mut shell,
            &mut input,
            "test portability work",
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(settled.load(Ordering::Acquire));
    assert_eq!(shell.extension_editor_snapshot(), editor);
    assert!(shell.current_run_id().is_none());
}

#[tokio::test]
async fn navigation_preserves_even_a_whitespace_only_user_draft() {
    let (_directory, mut app) = crate::compaction::tests::app_for_estimate();
    let (_, _, target, _) = branch_fixture(app.agent.session_mut());
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("  \n ".into());
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    apply_navigation(
        &mut app,
        &mut shell,
        &mut input,
        TreeNavigationChoice {
            target,
            summarize: false,
            custom_instructions: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(shell.pending(), "  \n ");
    assert!(shell.current_run_id().is_none());
}
