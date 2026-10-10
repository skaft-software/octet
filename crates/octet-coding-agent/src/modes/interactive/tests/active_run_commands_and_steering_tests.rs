//! Active-run command routing, changelog and skill prewarm, loopback steering
//! admission and recall, and abort restoration of undelivered steering.
//! Separate because the whole area is about what does and does not reach the
//! provider while a run is streaming.

use super::*;

use super::support::*;

#[tokio::test]
async fn changelog_startup_and_idle_skip_responses_context_prewarm() {
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // A real loopback WebSocket records response.create bodies, including
    // generate=false. No provider credentials or external endpoint exist.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let (requests, mut received) = tokio::sync::mpsc::unbounded_channel();
    let mut server = tokio::task::JoinSet::new();
    server.spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
        let (mut socket, _) = listener.accept().await.unwrap();
        let requests = requests.clone();
        peers.spawn(async move {
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(socket.read_u8().await.unwrap());
            assert!(head.len() < 16 * 1024);
        }
        let head = String::from_utf8(head).unwrap();
        assert!(head.starts_with("GET /v1/responses HTTP/1.1"));
        assert!(!head.to_ascii_lowercase().contains("authorization:"));
        let key = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("sec-websocket-key").then(|| value.trim())
        }).unwrap();
        let digest = ring::digest::digest(&ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes());
        let accept = base64::engine::general_purpose::STANDARD.encode(digest.as_ref());
        socket.write_all(format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").as_bytes()).await.unwrap();
        while let Ok(opcode) = socket.read_u8().await {
            if opcode == 0x88 { break; } // Client teardown may send Close.
            assert_eq!(opcode, 0x81, "fixture expects one complete JSON text frame");
            let flags = socket.read_u8().await.unwrap();
            assert_ne!(flags & 0x80, 0, "client frames must be masked");
            let length = match flags & 0x7f {
                126 => u64::from(socket.read_u16().await.unwrap()),
                127 => socket.read_u64().await.unwrap(),
                length => u64::from(length),
            };
            assert!(length <= 128 * 1024);
            let mut mask = [0; 4];
            socket.read_exact(&mut mask).await.unwrap();
            let mut body = vec![0; length as usize];
            socket.read_exact(&mut body).await.unwrap();
            for (index, byte) in body.iter_mut().enumerate() { *byte ^= mask[index % 4]; }
            let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(body["type"], "response.create");
            assert_eq!(body["generate"], false);
            let completed = br#"{"type":"response.completed","response":{"id":"fixture-prewarm"}}"#;
            socket.write_all(&[0x81, completed.len() as u8]).await.unwrap();
            socket.write_all(completed).await.unwrap();
            requests.send(body).unwrap();
        }
        });
        }
    });
    let mut model = scripted_model(&uri);
    Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
    Arc::make_mut(&mut model.endpoint).transport = octet_ai::EndpointTransport::WebSocketPreferred;
    let (workspace, agent) = scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
    let (_app_workspace, mut app) = crate::compaction::tests::app_for_estimate();
    app.agent = agent;
    app.model = model;
    let path = workspace.path().join("session.jsonl");
    let before = std::fs::read(&path).unwrap();
    let mut shell = InteractiveShell::test_shell();
    for prompt in ["/changelog", " /chang  "] {
        assert!(prepare_startup_input(&app, &mut shell, Some(prompt.into())).is_none());
        assert!(shell.has_overlay());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        received.try_recv().is_err(),
        "release-note startup sent context"
    );

    // An ordinary startup must still prewarm. This positive control also
    // proves the synthetic model can exercise the transport under test.
    assert!(prepare_startup_input(&app, &mut shell, None).is_none());
    tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    for command in ["/changelog", "/chang"] {
        schedule_idle_responses_prewarm(&app, &commands::parse(command));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        received.try_recv().is_err(),
        "idle release notes sent context"
    );
    // A live pooled connection intentionally does not receive another warm.
    schedule_idle_responses_prewarm(&app, &Command::Status);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        received.try_recv().is_err(),
        "live pool repeated optional setup"
    );
    // Positive controls need fresh clients/session affinities, not a timeout
    // waiting for a deliberately skipped frame on the already warm socket.
    let mut cold_workspaces = Vec::new();
    let (cold_workspace, cold_agent) =
        scripted_agent_for_route(app.model.clone(), octet_ai::AiClient::new());
    cold_workspaces.push(cold_workspace);
    app.agent = cold_agent;
    schedule_idle_responses_prewarm(&app, &Command::Status);
    tokio::time::timeout(Duration::from_secs(3), received.recv())
        .await
        .unwrap()
        .unwrap();
    // This is not a general startup slash-command dispatcher. Unknown
    // arguments, ordinary text, and explicit templates remain prompts.
    for (template, prompt) in [
        (None, "/changelog extra"),
        (None, "/status"),
        (None, "Explain the changelog"),
        (Some("fixture"), "/changelog"),
        (Some("fixture"), "Expanded template argument: /changelog"),
    ] {
        let (cold_workspace, cold_agent) =
            scripted_agent_for_route(app.model.clone(), octet_ai::AiClient::new());
        cold_workspaces.push(cold_workspace);
        app.agent = cold_agent;
        app.config.prompt_template = template.map(str::to_owned);
        let input = prepare_startup_input(&app, &mut shell, Some(prompt.into())).unwrap();
        assert_eq!(input.display_text, prompt);
        tokio::time::timeout(Duration::from_secs(3), received.recv())
            .await
            .unwrap()
            .unwrap();
    }
    assert_eq!(std::fs::read(&path).unwrap(), before);
    server.abort_all();
}

#[tokio::test]
async fn scripted_active_changelog_keeps_running_without_provider_input() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;
    let (_server, _workspace, mut agent) = scripted_agent().await;
    let mut shell = InteractiveShell::test_shell();
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    for character in "/changelog".chars() {
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
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut goal_deadline = None;
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
            &mut goal_deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    drop(run);
    assert_eq!(ended, HostRunOutcome::Completed);
    assert!(
        shell.has_overlay(),
        "the report stays open while the run settles"
    );
    assert!(pending.is_empty());
    assert!(!quit);
    assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/changelog"));
}

#[tokio::test]
async fn active_skill_invocations_queue_as_prompts_instead_of_unknown_commands() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (_server, _workspace, mut agent) = scripted_agent().await;
    let mut shell = InteractiveShell::test_shell();
    shell.set_skill_commands(Arc::from([("skill:review".into(), "Review".into())]));
    let (sender, receiver) = tokio::sync::mpsc::channel(64);
    for invocation in ["/skill:review inspect", "/skill:rev"] {
        for character in invocation.chars() {
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
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut goal_deadline = None;
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
            &mut goal_deadline,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    drop(run);
    assert_eq!(ended, HostRunOutcome::Completed);
    assert!(!quit);
    assert!(pending.is_empty());
    assert_eq!(shell.queued_follow_up_len(), 2);
    assert_eq!(
        shell.take_ready_follow_up().unwrap().transcript_text,
        "/skill:review inspect"
    );
    shell.settle_queued_follow_ups(true);
    assert_eq!(
        shell.take_ready_follow_up().unwrap().transcript_text,
        "/skill:review "
    );
    assert!(!format!("{:?}", agent.session().context().unwrap()).contains("/skill:"));
}

#[tokio::test]
async fn scripted_active_loop_enter_steers_and_never_forwards_active_model_command() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (_server, workspace, mut agent) = scripted_agent().await;
    let image = workspace.path().join("shot.png");
    std::fs::write(
        &image,
        include_bytes!("../../../../tests/fixtures/export_html/one-pixel.png"),
    )
    .unwrap();

    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
    for character in "steer first".chars() {
        shell.apply_edit(crate::tui::keymap::EditAction::Char(character));
    }
    shell.apply_edit(crate::tui::keymap::EditAction::Paste(
        image.display().to_string(),
    ));
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    for character in "/model gpt-4o-mini".chars() {
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
    // Keep the sender alive so the receiver remains pending rather than
    // signalling an input close that would abort the real run.
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
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

    assert_eq!(ended, HostRunOutcome::Completed);
    assert!(!quit);
    assert_eq!(
        pending.pop_front(),
        Some(PendingIdleAction::ChangeModel(ModelId(
            "gpt-4o-mini".into()
        )))
    );
    let context = agent.session().context().unwrap();
    let user_text = context
        .iter()
        .filter_map(|message| match message {
            octet_ai::Message::User(user) => user.content.iter().find_map(|part| match part {
                octet_ai::UserPart::Text(text) => Some(text.as_str()),
                _ => None,
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(user_text.contains(&"steer first"));
    assert!(!user_text.iter().any(|text| text.contains("/model")));
    assert!(context.iter().any(|message| matches!(
        message,
        octet_ai::Message::User(user)
            if user
                .content
                .iter()
                .any(|part| matches!(part, octet_ai::UserPart::Media(_)))
    )));
}

#[tokio::test]
async fn abort_restores_all_undelivered_steering_after_the_final_event() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (_server, _workspace, mut agent) = scripted_agent_with_delay(Duration::from_secs(2)).await;
    let mut shell = InteractiveShell::test_shell();
    for character in "steer first".chars() {
        shell.apply_edit(crate::tui::keymap::EditAction::Char(character));
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    for character in "steer second".chars() {
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
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    let _sender = sender;

    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
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
    assert_eq!(shell.pending(), "steer first\n\nsteer second");
    assert!(shell.debug_snapshot().contains("Interrupted"));
    let context = agent.session().context().unwrap();
    assert!(!context.iter().any(|message| matches!(
        message,
        octet_ai::Message::User(user)
            if user.content.iter().any(|part| matches!(
                part,
                octet_ai::UserPart::Text(text) if text.starts_with("steer ")
            ))
    )));
}

/// Drive one scripted run with a fixed event script and return its settled
/// outcome. The sender stays alive so the input stream remains pending
/// rather than signalling a close that would abort the run.
async fn drive_scripted_events(
    agent: &mut octet_agent::Agent,
    shell: &mut InteractiveShell,
    events: Vec<Event>,
) -> (HostRunOutcome, bool) {
    use tokio_stream::wrappers::ReceiverStream;
    let (sender, receiver) = tokio::sync::mpsc::channel(64);
    for event in events {
        sender.send(Ok(event)).await.unwrap();
    }
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let mut goal_deadline = None;
    let ended = tokio::time::timeout(
        Duration::from_secs(5),
        drive_active_run(
            &mut run,
            &control,
            shell,
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
        ),
    )
    .await
    .unwrap()
    .unwrap();
    drop(run);
    (ended, quit)
}

/// Every durable user-message text in the session, in order.
fn delivered_user_text(agent: &octet_agent::Agent) -> Vec<String> {
    agent
        .session()
        .context()
        .unwrap()
        .iter()
        .filter_map(|message| match message {
            octet_ai::Message::User(user) => Some(
                user.content
                    .iter()
                    .filter_map(|part| match part {
                        octet_ai::UserPart::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn full_steering_admission_restores_the_draft_without_a_shell_entry() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (_server, _workspace, mut agent) = scripted_agent_with_delay(Duration::from_secs(2)).await;
    let mut shell = InteractiveShell::test_shell();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
    let reservations = (0..64)
        .map(|_| control.prepare_steer("occupied").unwrap().0)
        .collect::<Vec<_>>();
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    sender
        .send(Ok(Event::Paste("refused steering".into())))
        .await
        .unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    sender
        .send(Ok(Event::Key(KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .await
        .unwrap();
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut extensions = crate::extensions::ExecutableExtensions::default();
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
        &mut extensions,
        &mut false,
        test_run_inspection(),
        &mut goal_deadline,
    )
    .await
    .unwrap();
    drop(reservations);
    drop(run);

    assert_eq!(ended, HostRunOutcome::Aborted);
    assert_eq!(shell.pending(), "refused steering");
    assert!(!shell.debug_snapshot().contains("Steering:"));
    assert!(!delivered_user_text(&agent)
        .iter()
        .any(|text| text.contains("refused steering")),);
}

#[tokio::test]
async fn recalled_live_steering_returns_to_the_editor_without_delivery() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_server, _workspace, mut agent) = scripted_agent_with_delay(Duration::from_secs(2)).await;
    let mut shell = InteractiveShell::test_shell();
    let (ended, quit) = drive_scripted_events(
        &mut agent,
        &mut shell,
        vec![
            Event::Paste("steer recalled".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            // Option/Alt+Up recalls the newest retractable live steering
            // before the provider boundary can claim it.
            dequeue_gesture(),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ],
    )
    .await;
    assert_eq!(ended, HostRunOutcome::Aborted);
    assert!(!quit);
    assert_eq!(shell.pending(), "steer recalled");
    assert!(!shell.debug_snapshot().contains("Steering:"));
    assert!(
        !delivered_user_text(&agent)
            .iter()
            .any(|text| text.contains("steer recalled")),
        "a recalled submission must not also be delivered"
    );
}

#[tokio::test]
async fn requeued_edited_live_steering_is_what_the_next_provider_request_carries() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_server, _workspace, mut agent) =
        scripted_agent_with_delay(Duration::from_millis(200)).await;
    let mut shell = InteractiveShell::test_shell();
    let (ended, quit) = drive_scripted_events(
        &mut agent,
        &mut shell,
        vec![
            Event::Paste("original".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            dequeue_gesture(),
            Event::Paste(" edited".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        ],
    )
    .await;
    assert_eq!(ended, HostRunOutcome::Completed);
    assert!(!quit);
    let delivered = delivered_user_text(&agent);
    assert!(
        delivered.iter().any(|text| text == "original edited"),
        "requeued edited steering is delivered: {delivered:?}"
    );
    assert!(
        !delivered.iter().any(|text| text == "original"),
        "the recalled draft is not delivered twice: {delivered:?}"
    );
    assert!(shell.pending().is_empty());
}

#[tokio::test]
async fn recalled_live_steering_is_not_double_counted_in_the_editor() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let (_server, _workspace, mut agent) = scripted_agent_with_delay(Duration::from_secs(2)).await;
    let mut shell = InteractiveShell::test_shell();
    let (ended, _quit) = drive_scripted_events(
        &mut agent,
        &mut shell,
        vec![
            Event::Paste("alpha".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Event::Paste("beta".into()),
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            dequeue_gesture(),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ],
    )
    .await;
    assert_eq!(ended, HostRunOutcome::Aborted);
    let pending = shell.pending();
    // The recalled entry is restored once and the still-queued entry is
    // restored once, with no duplicate append from either owner.
    assert_eq!(pending, "alpha\n\nbeta");
    assert_eq!(pending.matches("alpha").count(), 1);
    assert_eq!(pending.matches("beta").count(), 1);
    assert!(
        !delivered_user_text(&agent)
            .iter()
            .any(|text| text.contains("alpha") || text.contains("beta")),
        "recalled and aborted steering must not be delivered"
    );
}

#[test]
fn sticky_answer_steering_is_not_retractable_while_live_steering_is() {
    let mut shell = InteractiveShell::test_shell();
    let answer = answer_now_input(Some("keep tools off".into()));
    shell.queue_steering(&answer);
    let (live, receipt) = octet_agent::PreparedSteering::new("live steer");
    shell.queue_retractable_steering(
        receipt,
        "live steer".into(),
        "live steer".into(),
        Vec::new(),
    );
    shell.edit_queued_message();
    // Joint recall takes the newest retractable steering entry only.
    assert_eq!(shell.pending(), "live steer");
    assert!(
        shell
            .debug_snapshot()
            .contains("Steering: /answer keep tools off"),
        "sticky /answer stays queued: {}",
        shell.debug_snapshot()
    );
    // The recalled live submission is a no-op rather than a second append.
    drop(live);
}

#[tokio::test]
async fn active_run_subagents_without_extension_owner_stays_an_unknown_command() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use tokio_stream::wrappers::ReceiverStream;

    let (_server, _workspace, mut agent) = scripted_agent().await;
    let mut shell = InteractiveShell::test_shell();
    let (sender, receiver) = tokio::sync::mpsc::channel(32);
    for character in "/subagents".chars() {
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
    // Keep the sender alive so the receiver remains pending rather than
    // signalling an input close that would abort the real run.
    let _sender = sender;
    let mut input = ReceiverStream::new(receiver);
    let mut ticker = tokio::time::interval(Duration::from_millis(1));
    let mut pending = VecDeque::new();
    let mut quit = false;
    let mut executable_extensions = crate::extensions::ExecutableExtensions::default();
    let run_id = shell.begin_run("test");
    let mut run = agent.prompt("initial").await.unwrap();
    shell.set_awaiting_provider(run_id);
    let control = run.control();
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

    assert_eq!(ended, HostRunOutcome::Completed);
    assert_eq!(
        shell.debug_error().as_deref(),
        Some("unknown command: /subagents"),
        "without a octet-subagents owner the command must not open the live view"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_pi_command_is_published_and_executes_without_extension_menu() {
    let (directory, mut app, mut shell) = super::pi_contract_support::pi_ui_app(
        r#"
import { writeFileSync } from 'node:fs';
export default pi => pi.registerCommand('doom', {description: 'Fixture Doom', handler: args => {
  writeFileSync(TRACE, args);
}});
"#,
    );
    let mut input = EventStream::from_stream(futures_util::stream::pending());
    tokio::time::timeout(
        Duration::from_secs(8),
        resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input),
    )
    .await
    .expect("startup deadline")
    .unwrap();
    shell.finish_startup();
    request_extension_ui(&mut shell, &mut app);
    shell.apply_edit(crate::tui::keymap::EditAction::Paste("/doo".into()));
    shell.render();
    let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
    assert!(
        frame.contains("/doom"),
        "registered command not painted: {frame}"
    );
    shell.complete_slash_command();
    shell.apply_edit(crate::tui::keymap::EditAction::Paste(
        "fixture arguments".into(),
    ));
    let action = shell.translate_input(
        Some(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        ))),
        false,
    );
    let crate::tui::keymap::InputAction::Command(_) = action else {
        panic!("typed command not dispatched: {action:?}");
    };
    let command = commands::parse(&shell.drain_editor());
    let mut deadline = None;
    let mut reload =
        crate::reload::ReloadSupervisor::new(crate::reload::ReloadSettings::disabled());
    let mut input = EventStream::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(20),
        run_idle_command(
            app,
            &mut shell,
            &mut input,
            command,
            &mut deadline,
            None,
            &mut reload,
        ),
    )
    .await
    .expect("command deadline")
    .unwrap();
    let IdleCommandOutcome::Continue(mut app) = outcome else {
        panic!("extension command changed application");
    };
    let trace =
        std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap_or_else(|error| {
            panic!(
                "the extension command handler did not run: {error}; debug={:?}; {}",
                shell.debug_error(),
                app.executable_extensions.inspect_text()
            )
        });
    assert_eq!(trace, "fixture arguments");
    assert_eq!(shell.debug_error(), None);
    app.executable_extensions.shutdown().await;
}

/// A typed Pi command whose handler outlives one command-loop poll, so a
/// terminal-input failure is observed while the extension request is pending.
#[cfg(unix)]
const SLOW_DOOM_COMMAND_FIXTURE: &str = r#"
import { writeFileSync } from 'node:fs';
export default pi => pi.registerCommand('doom', {description: 'Fixture Doom', handler: async args => {
  await new Promise(resolve => setTimeout(resolve, 300));
  writeFileSync(TRACE, args);
}});
"#;

/// Deterministic terminal-input outcomes carrying exactly the production
/// `Result<TerminalEvent>` shapes: the source ends, or the read fails.
#[cfg(unix)]
#[derive(Clone, Copy)]
enum FailedTerminalInput {
    Ended,
    Error,
}

#[cfg(unix)]
impl futures_util::Stream for FailedTerminalInput {
    type Item = std::io::Result<Event>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::task::Poll::Ready(match *self.get_mut() {
            Self::Ended => None,
            Self::Error => Some(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "terminal input failed",
            ))),
        })
    }
}

/// A typed, published Pi command must still run when the terminal input source
/// ends or fails while the extension request is pending: the command's own
/// bounded request settles it, and only a real shutdown may cancel it.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn typed_pi_command_completes_when_terminal_input_ends_or_fails() {
    for (label, failure) in [
        ("ended", FailedTerminalInput::Ended),
        ("error", FailedTerminalInput::Error),
    ] {
        let (directory, mut app, mut shell) =
            super::pi_contract_support::pi_ui_app(SLOW_DOOM_COMMAND_FIXTURE);
        let mut startup_input = EventStream::from_stream(futures_util::stream::pending());
        tokio::time::timeout(
            Duration::from_secs(8),
            resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut startup_input),
        )
        .await
        .expect("startup deadline")
        .unwrap();
        shell.finish_startup();
        request_extension_ui(&mut shell, &mut app);
        shell.apply_edit(crate::tui::keymap::EditAction::Paste("/doo".into()));
        shell.render();
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        assert!(
            frame.contains("/doom"),
            "{label}: registered command not painted: {frame}"
        );
        let owner = app
            .executable_extensions
            .command_owner("doom")
            .expect("published command owner");
        let mut failure = failure;
        tokio::time::timeout(
            Duration::from_secs(20),
            run_typed_extension_command(
                &mut app,
                &mut shell,
                &mut failure,
                owner,
                "doom".into(),
                vec!["fixture arguments".into()],
            ),
        )
        .await
        .expect("command deadline")
        .unwrap();
        let trace =
            std::fs::read_to_string(directory.path().join("trace.jsonl")).unwrap_or_else(|error| {
                panic!(
                    "{label}: the extension command handler did not run: {error}; debug={:?}; {}",
                    shell.debug_error(),
                    app.executable_extensions.inspect_text()
                )
            });
        assert_eq!(trace, "fixture arguments", "{label}");
        assert_eq!(shell.debug_error(), None, "{label}");
        app.executable_extensions.shutdown().await;
    }
}

#[tokio::test]
async fn active_report_matrix_keeps_themed_native_chrome_and_draft() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    for command in [
        Command::Context,
        Command::Cache,
        Command::Status,
        Command::Extensions(commands::ExtensionsSubcommand::Status),
    ] {
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.begin_run("test");
        shell.apply_edit(crate::tui::keymap::EditAction::Paste("kept draft".into()));
        let (queue, quit) = run_active_command(&mut shell, command.clone(), &inspection).await;
        shell.render();
        let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
        assert!(
            frame.contains("\x1b[38;2;"),
            "theme lost for {command:?}: {frame}"
        );
        assert!(!frame.contains("^["), "ANSI escaped for {command:?}");
        assert!(shell.is_agent_run_active());
        assert_eq!(shell.pending(), "kept draft");
        assert!(queue.is_empty());
        assert!(!quit);
        assert_eq!(shell.debug_error(), None);
    }
}

#[test]
fn active_thinking_reserves_capacity_and_settles_once_without_claiming_delivery() {
    let directory = tempfile::tempdir().unwrap();
    let inspection = test_run_inspection_with_session(directory.path());
    let mut shell = InteractiveShell::test_shell();
    let high = ReasoningConfig::Effort(octet_ai::ReasoningEffort::High);
    let mut queue = VecDeque::from(vec![PendingIdleAction::Compact; MAX_PENDING_IDLE_ACTIONS]);
    assert!(!stage_active_thinking(&mut queue, &high));
    assert_eq!(queue.len(), MAX_PENDING_IDLE_ACTIONS);
    queue.pop_back();
    assert!(stage_active_thinking(&mut queue, &high));
    // The latest choice can replace the reserved slot even at capacity.
    assert!(stage_active_thinking(&mut queue, &ReasoningConfig::Off));
    assert!(stage_active_thinking(&mut queue, &high));
    settle_active_thinking(&mut shell, &inspection, &mut queue);
    assert_eq!(queue.len(), MAX_PENDING_IDLE_ACTIONS);
    assert_eq!(queue.back(), Some(&PendingIdleAction::ChangeThinking(high)));
    let before = shell.debug_snapshot();
    assert!(before.contains("active request unchanged"), "{before}");
    settle_active_thinking(&mut shell, &inspection, &mut queue);
    assert_eq!(
        shell.debug_snapshot(),
        before,
        "settlement must announce exactly once"
    );
}

#[tokio::test]
async fn active_management_roots_open_with_a_full_effect_queue_and_keep_the_draft() {
    for command in [commands::parse("/settings"), commands::parse("/extensions")] {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("unsent multiline\ndraft".into());
        let mut queue = VecDeque::from(vec![
            PendingIdleAction::NewSession;
            MAX_PENDING_IDLE_ACTIONS
        ]);
        let before = queue.clone();
        let mut menu = extension_menu::State::default();
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
        let mut deadline = None;
        let mut quit = false;
        handle_active_command(
            &mut shell,
            command,
            test_run_inspection(),
            &mut extensions,
            &octet_agent::ContextSnapshot::default(),
            &mut deadline,
            |_, _| Ok(None),
            &mut input,
            &mut queue,
            &mut quit,
            &mut menu,
        )
        .await
        .unwrap();
        assert!(shell.has_panel());
        assert!(
            !shell.has_overlay(),
            "management navigation is not diagnostics"
        );
        assert_eq!(queue, before, "opening a root reserves no effect capacity");
        assert_eq!(shell.pending(), "unsent multiline\ndraft");
        assert!(!quit);
        let (result, action) = shell
            .panel_input(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )))
            .unwrap();
        assert_eq!(result, PanelResult::Cancel);
        if menu.owns_action(&action) {
            menu.back(&mut shell);
        }
        assert!(!shell.has_panel());
        assert_eq!(queue, before);
        assert_eq!(shell.pending(), "unsent multiline\ndraft");
    }
}

#[tokio::test]
async fn held_provider_theme_matrix_applies_previews_and_keeps_native_chrome() {
    use crossterm::event::KeyEvent;
    for (command, tail, expected) in [
        ("/theme Cards", vec![], "Cards"),
        ("/settings theme Still", vec![], "Still"),
        ("/theme", vec![KeyCode::Up, KeyCode::Enter], "light"),
        ("/theme", vec![KeyCode::Up, KeyCode::Esc], "dark"),
        ("/settings images on", vec![], "dark"),
        (
            "/settings",
            vec![KeyCode::Down, KeyCode::Enter, KeyCode::Up, KeyCode::Enter],
            "dark",
        ),
    ] {
        let (server, started, release) = HeldApi::start(fast_response()).await;
        let mut model = scripted_model(&server.uri);
        Arc::make_mut(&mut model.spec).protocol = octet_ai::Protocol::OpenAiResponses;
        let (workspace, mut agent) =
            scripted_agent_for_route(model.clone(), octet_ai::AiClient::new());
        let mut inspection = test_run_inspection().clone();
        inspection.model = model;
        inspection.session_path = agent.session().path().to_owned();
        let mut config = terminal_theme_test_config(workspace.path().to_owned());
        config.color = crate::config::ColorMode::Always;
        config.theme = Some("dark".into());
        config.theme_explicit = true;
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.set_theme(crate::tui::theme::load_theme(&config));
        shell.set_runtime_config(config);
        let events = std::iter::once(Event::Paste(command.into()))
            .chain(std::iter::once(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))))
            .chain(
                tail.into_iter()
                    .map(|code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE))),
            )
            .chain(std::iter::once(Event::Paste("kept streaming draft".into())))
            .collect::<Vec<_>>();
        let (sender, receiver) = tokio::sync::mpsc::channel(16);
        let (handled_tx, handled) = tokio::sync::oneshot::channel();
        let mut input = ProbedInput {
            input: tokio_stream::wrappers::ReceiverStream::new(receiver),
            remaining: events.len(),
            handled: Some(handled_tx),
        };
        let mut pending = VecDeque::new();
        let mut ticker = tokio::time::interval(Duration::from_millis(1));
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let id = shell.begin_run("test");
        let mut run = agent.prompt("keep streaming").await.unwrap();
        let control = run.control();
        shell.set_awaiting_provider(id);
        let mut deadline = None;
        let mut made_tool_call = false;
        // Stop polling the driver only after its last input handler returned,
        // while the real HTTP response is still held. The Run remains owned.
        {
            let driver = drive_active_run(
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
                &mut deadline,
            );
            tokio::pin!(driver);
            let stimulus = async {
                started.await.unwrap();
                for event in events {
                    sender.send(Ok(event)).await.unwrap();
                }
                handled.await.unwrap();
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    _ = stimulus => {},
                    result = &mut driver => panic!("run settled before the provider gate: {result:?}"),
                }
            }).await.unwrap();
        }
        assert!(shell.is_agent_run_active(), "{command}");
        assert_eq!(server.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            shell.runtime_config().unwrap().theme.as_deref(),
            Some(expected)
        );
        assert_eq!(shell.pending(), "kept streaming draft");
        assert_eq!(
            pending.len(),
            usize::from(command != "/theme" || expected != "dark")
        );
        if command.contains("images") || command == "/settings" {
            assert!(shell.runtime_config().unwrap().show_images);
        }
        for report in [
            Command::Context,
            Command::Status,
            Command::Cache,
            Command::Extensions(commands::ExtensionsSubcommand::Status),
        ] {
            let expected_context_chrome = matches!(&report, Command::Context).then(|| {
                shell.theme().fg(
                    "muted",
                    "Review the estimated request context before the next turn",
                )
            });
            let (queue, quit) = run_active_command(&mut shell, report, &inspection).await;
            assert!(queue.is_empty());
            assert!(!quit);
            shell.render();
            let frame = shell.dump_rendered_frame().await.unwrap().join("\n");
            if let Some(expected) = expected_context_chrome {
                assert!(
                    frame.contains(&expected),
                    "native context chrome did not use the selected theme for {command}: {frame}"
                );
            }
            assert_eq!(
                shell.runtime_config().unwrap().theme.as_deref(),
                Some(expected),
                "native report changed the selected theme for {command}"
            );
            assert!(!frame.contains("^["));
            assert_eq!(shell.pending(), "kept streaming draft");
            shell.close_overlay();
        }
        release.send(true).unwrap();
        let outcome = tokio::time::timeout(
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
                &mut made_tool_call,
                &inspection,
                &mut deadline,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome, HostRunOutcome::Completed);
        drop(run);
        assert!(!delivered_user_text(&agent)
            .iter()
            .any(|text| text.contains("/theme") || text.contains("/settings")));
    }
}

#[tokio::test]
async fn ctrl_s_follow_up_keeps_attachment_recall_and_full_queue_keeps_draft() {
    use crossterm::event::KeyEvent;
    let (_server, workspace, mut agent) = scripted_agent().await;
    let image = workspace.path().join("follow-up.png");
    std::fs::write(
        &image,
        include_bytes!("../../../../tests/fixtures/export_html/one-pixel.png"),
    )
    .unwrap();
    let mut shell = InteractiveShell::test_shell();
    shell.set_input_modalities(octet_ai::ModalitySet::none().with(octet_ai::Modality::Image));
    shell.apply_edit(crate::tui::keymap::EditAction::Paste(
        image.display().to_string(),
    ));
    let before = shell.pending();
    let (outcome, quit) = drive_scripted_events(
        &mut agent,
        &mut shell,
        vec![
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            dequeue_gesture(),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        ],
    )
    .await;
    assert_eq!(outcome, HostRunOutcome::Aborted);
    assert!(!quit);
    assert_eq!(shell.pending(), before);
    let recalled = shell.drain_composed();
    assert_eq!(recalled.attachments.len(), 1);
    shell.restore_unqueued_steering(recalled.display_text, recalled.attachments);
    assert_eq!(shell.queued_follow_up_len(), 0);
    assert_eq!(delivered_user_text(&agent), vec!["initial"]);

    let (_server, _workspace, mut agent) = scripted_agent().await;
    for _ in 0..MAX_PENDING_IDLE_ACTIONS {
        shell.queue_follow_up(ComposedInput::from_text("occupied".into()));
    }
    let (outcome, _) = drive_scripted_events(
        &mut agent,
        &mut shell,
        vec![
            Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        ],
    )
    .await;
    assert_eq!(outcome, HostRunOutcome::Aborted);
    assert_eq!(shell.queued_follow_up_len(), MAX_PENDING_IDLE_ACTIONS);
    assert!(shell
        .debug_error()
        .unwrap()
        .contains("follow-up queue is full"));
}

// -----------------------------------------------------------------------
// `/goal` applies immediately, mid-run or idle; it is never queued.
// -----------------------------------------------------------------------
