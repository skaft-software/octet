//! Presentation of derived views: `/answer` prompts, tool-confirmation notices, fork
//! rows, extension lifecycle forks, delegated session documents, and subagent rows.
//! Separate because every assertion here is about a rendered projection rather than
//! about run control.

use super::*;

use super::support::*;

#[test]
fn answer_now_prompt_preserves_optional_instruction_and_enforces_tool_free_synthesis() {
    let bare = answer_now_prompt(None);
    assert!(bare.contains("evidence already gathered"));
    assert!(bare.contains("Do not call tools"));

    let instructed = answer_now_prompt(Some("Be concise".into()));
    assert!(instructed.starts_with("Be concise\n\n"));
    assert!(instructed.contains("Do not call tools"));

    let input = answer_now_input(Some("Be concise".into()));
    assert!(input.answer_only);
    assert_eq!(input.display_text, "/answer Be concise");
    assert!(matches!(
        input.parts.as_slice(),
        [octet_agent::InputPart::Text(text)] if text.contains("Do not call tools")
    ));
}

#[test]
fn confirmation_notices_identify_core_tools_and_extensions() {
    assert_eq!(
        confirmation_notice(Some("write"), true),
        "write action approved"
    );
    assert_eq!(
        confirmation_notice(Some("bash"), false),
        "bash action denied"
    );
    assert_eq!(
        confirmation_notice(Some("custom_tool"), true),
        "extension action approved"
    );
    assert_eq!(confirmation_notice(None, false), "tool action denied");
}

#[test]
fn fork_message_projection_uses_the_active_branch_and_adds_a_head_row() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("source.jsonl");
    let mut session = Session::create(&path).unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("first prompt".into())],
            },
        )))
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text("answer".into())],
                model: octet_ai::ModelId("test".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("second prompt".into())],
            },
        )))
        .unwrap();

    let messages = active_fork_messages(&session);
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].text, "first prompt");
    assert_eq!(messages[1].text, "second prompt");
    assert!(messages[2].whole_conversation);
    assert_eq!(messages[2].entry_id, session.head().unwrap().0);
}

#[test]
fn extension_lifecycle_fork_copies_the_active_head_and_records_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let sessions =
        crate::session_store::SessionStore::new(&directory.path().join("sessions"), &workspace);
    let source_path = sessions.new_path("2026-03-16");
    let mut prepared = None;
    let mut source = open_launch_session(
        &mut prepared,
        SessionSelection::CreateNew(source_path.clone()),
    )
    .unwrap();
    let head = source
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("current head".into())],
            },
        )))
        .unwrap();
    drop(source);

    let destination = sessions.new_path("2026-03-17");
    let fork_path = fork_active_session(&sessions, &source_path, destination, Some(&head)).unwrap();
    let fork = Session::open(&fork_path).unwrap();
    let source_id = session_id_for_path(&source_path).unwrap();
    let fork_id = session_id_for_path(&fork_path).unwrap();

    assert_eq!(fork.head(), Some(head.clone()));
    assert!(fork.entry(&head).is_some());
    let metadata = sessions.load_metadata(&fork_id).unwrap();
    assert_eq!(
        metadata.forked_from_session_id.as_deref(),
        Some(source_id.as_str())
    );
    assert_eq!(
        metadata.forked_from_entry_id.as_deref(),
        Some(head.0.as_str())
    );
}

#[test]
fn extension_lifecycle_session_ids_fit_the_generated_result_bound() {
    assert_eq!(
        bounded_extension_session_id("x".repeat(MAX_JSON_RPC_ID_BYTES)).unwrap(),
        "x".repeat(MAX_JSON_RPC_ID_BYTES)
    );
    assert!(bounded_extension_session_id("x".repeat(MAX_JSON_RPC_ID_BYTES + 1)).is_err());
}

#[test]
fn web_search_menu_recommends_brave_and_keeps_searxng_and_disable() {
    let (items, descriptions) = web_search_menu_entries(true);
    assert_eq!(
        items,
        [
            "Brave Search (recommended)",
            "SearXNG",
            "Disable octet-web-search"
        ]
    );
    assert!(descriptions[0]
        .as_deref()
        .is_some_and(|description| description.contains("API key")));

    let (items, _) = web_search_menu_entries(false);
    assert_eq!(items, ["Brave Search (recommended)", "SearXNG"]);
}

#[test]
fn delegated_session_document_is_bounded_path_free_and_styled() {
    let directory = tempfile::tempdir().unwrap();
    let private_path = directory.path().join("private-child.jsonl");
    let mut session = Session::create(&private_path).unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text(
                    "Inspect the worker result.".into(),
                )],
            },
        )))
        .unwrap();
    let theme = test_theme();
    let text = delegated_session_text(&session, &theme, 80, false).unwrap();
    let overlay = delegated_session_overlay_text(&text, &theme);
    let plain = crate::tui::view::sanitize_for_terminal(&overlay);
    assert!(plain.contains("Delegated worker transcript"));
    assert!(plain.contains("read-only · mutation remains owner-bound"));
    assert!(plain.contains("Inspect the worker result."));
    assert!(
        overlay.contains('\x1b'),
        "the trusted theme styling was lost"
    );
    assert!(!overlay.contains(private_path.to_str().unwrap()));
    assert!(text.len() <= 128 * 1024);
}

#[test]
fn delegated_session_overlay_keeps_newest_output_within_the_exact_byte_cap() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
    for index in 0..20 {
        let marker = if index == 19 {
            "NEWEST-FINAL-WORKER-RESULT"
        } else {
            "older-worker-output"
        };
        session
            .append(EntryValue::Message(octet_ai::Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text(format!(
                        "block-{index:02}-{marker}-{}",
                        "x".repeat(16 * 1024)
                    ))],
                    model: octet_ai::ModelId("worker-test".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
    }

    let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
    assert!(text.contains("NEWEST-FINAL-WORKER-RESULT"), "{text}");
    assert!(!text.contains("block-00-older-worker-output"));
    assert!(text.contains("[older transcript entries omitted]"));
    assert!(text.len() <= 128 * 1024, "{}", text.len());
}

#[test]
fn delegated_session_renders_markdown_like_the_main_transcript() {
    // The worker transcript must flow through the exact same rich
    // markdown renderer as the main conversation: headings, bold, and
    // inline code keep their theme styling instead of being flattened to
    // raw text.
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
    let markdown = "# Heading One\n\nplain **bold** and `code` tail";
    session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::Text(markdown.into())],
                model: octet_ai::ModelId("worker-test".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
    let plain = crate::tui::view::sanitize_for_terminal(&text);

    assert!(text.contains('\x1b'), "styled block not found in {text}");
    assert!(plain.contains("Heading One"), "{plain}");
    assert!(plain.contains("plain bold and code tail"), "{plain}");
    assert!(!plain.contains("# Heading One"), "{plain}");
    assert!(!plain.contains("**bold**"), "{plain}");
    assert!(!plain.contains("`code`"), "{plain}");
}

#[test]
fn delegated_session_hydrates_main_transcript_tool_cards_and_results() {
    let directory = tempfile::tempdir().unwrap();
    let mut session = Session::create(directory.path().join("child.jsonl")).unwrap();
    let call_id = octet_ai::ToolCallId("worker-write".into());
    session
        .append(EntryValue::Message(octet_ai::Message::Assistant(
            octet_ai::AssistantMessage {
                content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                    async_execution: false,
                    id: call_id.clone(),
                    name: "write".into(),
                    arguments_json: serde_json::json!({
                        "path": "worker.rs",
                        "content": "pub fn worker() {}\n",
                    })
                    .to_string(),
                    argument_error: None,
                })],
                model: octet_ai::ModelId("worker-test".into()),
                protocol: octet_ai::Protocol::OpenAiChat,
            },
        )))
        .unwrap();
    session
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::ToolResult(octet_ai::ToolResult {
                    tool_call_id: call_id,
                    content: vec![octet_ai::ToolResultPart::Text(
                        "ok\nworker.rs  created\n--- /dev/null\n+++ b/worker.rs\n@@ -0,0 +1 @@\n+SUBAGENT-TOOL-RESULT"
                            .into(),
                    )],
                    is_error: false,
                    added_tool_names: None,
                })],
            },
        )))
        .unwrap();

    let text = delegated_session_text(&session, &test_theme(), 80, false).unwrap();
    let plain = crate::tui::view::sanitize_for_terminal(&text);
    assert!(plain.contains("Write"), "{plain}");
    assert!(plain.contains("SUBAGENT-TOOL-RESULT"), "{plain}");
}

#[test]
fn subagent_presentation_becomes_navigable_rows_with_opaque_session_references() {
    let mut snapshot: octet_agent::ExtensionPresentationSnapshot = serde_json::from_str(
        include_str!("../../../../fixtures/extension-presentation.json"),
    )
    .unwrap();
    let collection = snapshot.collection.as_mut().unwrap();
    collection.nodes[0].references = collection.detail.as_ref().unwrap().references.clone();
    let (title, entries) =
        subagent_view_entries_from_presentation(crate::extensions::ExtensionPresentationView {
            extension: "octet-subagents".into(),
            generation: 1,
            extension_instance_id: "instance".into(),
            resource_owner: Some("owner".into()),
            snapshot,
        })
        .unwrap();

    // The picker header is the collection's stable surface name only: the
    // per-worker states are on the rows and the key affordances are in the
    // panel action footer, so no counts or hints may be composed into it.
    assert_eq!(title, "Subagents");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].label, "test-review");
    assert!(entries[0].description.contains("running"));
    assert_eq!(
        entries[0].session_reference.as_deref(),
        Some("session-worker-1")
    );
    assert!(entries[0].fallback_detail.contains("bounded child session"));
    assert!(!entries[0].fallback_detail.contains(".jsonl"));
}

#[tokio::test]
async fn cancellable_wait_returns_none_on_ctrl_c() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        ))))
        .await
        .unwrap();
    drop(sender);
    let mut input = ReceiverStream::new(receiver);

    let mut shell = InteractiveShell::test_shell();
    let result = await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input).await;
    assert!(result.is_none());
    assert!(!shell.close_requested());
}

#[tokio::test]
async fn cancellable_wait_finishes_after_input_stream_closes() {
    let mut input = tokio_stream::empty::<std::io::Result<Event>>();
    let mut shell = InteractiveShell::test_shell();
    assert_eq!(
        await_with_ctrl_c(async { 42 }, &mut shell, &mut input).await,
        Some(42)
    );
}

#[tokio::test]
async fn cancellable_wait_propagates_ctrl_d_as_a_close_request() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (sender, receiver) = tokio::sync::mpsc::channel(1);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        ))))
        .await
        .unwrap();
    drop(sender);
    let mut input = ReceiverStream::new(receiver);
    let mut shell = InteractiveShell::test_shell();

    let result = await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input).await;

    assert!(result.is_none());
    assert!(shell.close_requested());
}

#[tokio::test]
async fn cancellable_wait_preserves_input_and_disclosure_before_escape() {
    use crossterm::event::KeyEvent;

    let mut input = tokio_stream::iter([
        Ok(Event::Resize(46, 8)),
        Ok(Event::Paste("draft during wait".into())),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
        Ok(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
    ]);
    let mut shell = InteractiveShell::test_shell();
    let was_verbose = shell.verbose_tools();
    // This budget bounds input handling, not the provider's timeout.
    let result = tokio::time::timeout(
        Duration::from_millis(250),
        await_with_ctrl_c(std::future::pending::<()>(), &mut shell, &mut input),
    )
    .await
    .expect("Escape must interrupt a held-open operation within 250 ms");
    assert!(result.is_none());
    assert_eq!(shell.pending(), "draft during wait");
    assert_ne!(shell.verbose_tools(), was_verbose);
    assert!(!shell.close_requested());
}

#[tokio::test]
async fn silent_startup_lifecycle_keeps_typed_input_and_names_the_phase_off_screen() {
    use crossterm::event::KeyEvent;

    // Typing before readiness is buffered into the draft, exactly as the
    // labeled wait does; nothing renders a phase label.
    let events = [
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))),
        Ok(Event::Paste(" draft during startup".into())),
    ];
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let mut done_tx = Some(done_tx);
    let source = tokio_stream::iter(events).chain(futures_util::stream::poll_fn(move |_| {
        if let Some(done_tx) = done_tx.take() {
            let _ = done_tx.send(());
        }
        std::task::Poll::Ready(None)
    }));
    let mut input = EventStream::from_stream(source);
    let mut shell = InteractiveShell::test_shell();
    let result =
        run_blocking_startup_lifecycle(&mut shell, &mut input, STARTUP_APP_OPERATION, move || {
            done_rx.recv()?;
            Ok(7)
        })
        .await
        .expect("the silent startup phase completes");
    assert_eq!(result, 7);
    assert_eq!(shell.pending(), "x draft during startup");

    // A failed silent phase stays attributable without rendering: the
    // operation name is carried by the diagnostic alone.
    let mut input =
        EventStream::from_stream(tokio_stream::iter(Vec::<std::io::Result<Event>>::new()));
    let error = run_blocking_startup_lifecycle(
        &mut shell,
        &mut input,
        STARTUP_MODELS_OPERATION,
        || -> anyhow::Result<()> { anyhow::bail!("catalog unavailable") },
    )
    .await
    .expect_err("the failed startup phase surfaces");
    assert_eq!(
        error.to_string(),
        "model discovery failed: catalog unavailable"
    );
}

#[tokio::test]
async fn osc11_startup_lifecycle_keeps_handed_off_typing_paste_and_shortcuts() {
    use crossterm::event::KeyEvent;

    let events = [
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        ))),
        Ok(Event::Paste(" draft during startup".into())),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        ))),
        Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))),
    ];
    let (finished, settled) = tokio::sync::oneshot::channel();
    let mut finished = Some(finished);
    // Settle only after the input owner has processed every queued event.
    let source = tokio_stream::iter(events).chain(futures_util::stream::poll_fn(move |_| {
        if let Some(finished) = finished.take() {
            let _ = finished.send(());
        }
        std::task::Poll::Ready(None)
    }));
    let mut input = EventStream::from_stream(source);
    let mut shell = InteractiveShell::test_shell();
    let verbose = shell.verbose_tools();
    let result = await_lifecycle(&mut shell, &mut input, "starting…", async move {
        settled.await?;
        Ok(42)
    })
    .await
    .unwrap();
    assert_eq!(result, 42);
    assert_eq!(shell.pending(), "x draft during startup");
    assert_ne!(shell.verbose_tools(), verbose);
    assert!(!shell.close_requested());
}

#[test]
fn startup_picker_close_is_a_graceful_exit_but_other_errors_survive() {
    let mut shell = InteractiveShell::test_shell();
    shell.request_close();
    assert_eq!(
        startup_launch_outcome::<u8>(&shell, Err(anyhow::anyhow!("selection cancelled"))).unwrap(),
        None
    );

    let shell = InteractiveShell::test_shell();
    let error = startup_launch_outcome::<u8>(&shell, Err(anyhow::anyhow!("selection cancelled")))
        .unwrap_err();
    assert_eq!(error.to_string(), "selection cancelled");
}

#[test]
fn bounded_shell_output_keeps_head_and_tail_within_budget() {
    let mut output = BoundedShellOutput::new(10);
    output.push(b"0123");
    output.push(b"456789");
    output.push(b"abcdef");

    assert_eq!(output.head, b"01234");
    assert_eq!(output.tail, b"bcdef");
    assert_eq!(output.total_bytes, 16);
    let rendered = output.render("stdout");
    assert!(rendered.starts_with("01234\n"), "{rendered:?}");
    assert!(rendered.contains("stdout truncated; 6 bytes omitted"));
    assert!(rendered.ends_with("\nbcdef"), "{rendered:?}");
}

#[test]
fn bounded_shell_output_does_not_claim_untruncated_tail_was_omitted() {
    let mut output = BoundedShellOutput::new(10);
    output.push("012345é".as_bytes());

    assert_eq!(output.total_bytes, 8);
    assert_eq!(output.render("stdout"), "012345é");
}

// Drives `sh` with `yes`/`head` to fill both pipes: Unix-only.
#[cfg(unix)]
#[tokio::test]
async fn shell_pipes_are_drained_concurrently_with_process_exit() {
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg("yes o | head -c 1048576 & yes e | head -c 1048576 >&2 & wait")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout = std::sync::Arc::new(std::sync::Mutex::new(BoundedShellOutput::new(1024)));
    let stderr = std::sync::Arc::new(std::sync::Mutex::new(BoundedShellOutput::new(1024)));
    let (updates, mut update_rx) = tokio::sync::mpsc::unbounded_channel();

    let status = tokio::time::timeout(Duration::from_secs(5), async {
        let (_, _, status) = tokio::join!(
            drain_shell_pipe(&mut stdout_pipe, &stdout, &updates),
            drain_shell_pipe(&mut stderr_pipe, &stderr, &updates),
            child.wait(),
        );
        status
    })
    .await
    .expect("full stdout and stderr pipes must not deadlock")
    .unwrap();

    assert!(status.success());
    let stdout = stdout.lock().unwrap();
    let stderr = stderr.lock().unwrap();
    assert_eq!(stdout.total_bytes, 1_048_576);
    assert_eq!(stderr.total_bytes, 1_048_576);
    assert_eq!(stdout.head.len() + stdout.tail.len(), 1024);
    assert_eq!(stderr.head.len() + stderr.tail.len(), 1024);
    assert!(
        update_rx.try_recv().is_ok(),
        "pipe reads must wake live rendering"
    );
}
