use super::*;
use crossterm::event::KeyEvent;

async fn frame(shell: &mut InteractiveShell) -> String {
    shell.render();
    sexy_tui_rs::strip_terminal_sequences(&shell.dump_rendered_frame().await.unwrap().join("\n"))
}

#[tokio::test]
async fn technical_details_do_not_approve_and_return_to_a_fresh_deny_default() {
    let (sink, mut progress) = ToolProgressSink::bounded_channel();
    let answer = tokio::spawn(async move {
        sink.confirmation(
            "Run this command?".into(),
            Some("Command: cargo test".into()),
            true,
            false,
        )
        .await
    });
    let ToolProgress::Confirmation(mut request) = progress.recv().await.unwrap() else {
        panic!("confirmation")
    };
    request.technical_detail =
        Some("effect: host_process\ncomplete intent sha256: fixture-digest".into());
    let expected_bytes = request.prompt.len()
        + request.detail.as_ref().unwrap().len()
        + request.technical_detail.as_ref().unwrap().len();
    let mut interaction = ActiveToolInteraction {
        id: ToolCallId("fixture".into()),
        tool: Some("bash".into()),
        request: ActiveToolRequest::Confirmation(request),
    };
    assert_eq!(interaction.request_bytes(), expected_bytes);
    let mut shell = InteractiveShell::test_shell();
    shell.set_size(80, 24);
    shell.prefill_editor("unsent 雪 draft".into());
    interaction.open(&mut shell);
    let before = frame(&mut shell).await;
    assert!(before.contains("Run this command?"));
    assert!(before.contains("Command: cargo test"));
    assert!(!before.contains("fixture-digest") && !before.contains("host_process"));
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        assert!(!interaction.input(&mut shell, &key(code)));
    }
    assert!(!answer.is_finished());
    assert!(frame(&mut shell).await.contains("fixture-digest"));
    assert!(
        !interaction.input(&mut shell, &key(KeyCode::Enter)),
        "document Enter has no approval authority"
    );
    assert!(
        !interaction.input(&mut shell, &key(KeyCode::Esc)),
        "Esc returns from details, not from consent"
    );
    assert!(!frame(&mut shell).await.contains("fixture-digest"));
    assert_eq!(shell.highlighted_panel_index(), Some(0));
    assert_eq!(shell.pending(), "unsent 雪 draft");
    assert!(interaction.input(&mut shell, &key(KeyCode::Enter)));
    assert!(
        !answer.await.unwrap(),
        "returning from details selects Deny"
    );
}

#[tokio::test]
async fn dropping_or_interrupting_a_technical_details_view_denies() {
    for interrupt in [false, true] {
        let (sink, mut progress) = ToolProgressSink::bounded_channel();
        let answer = tokio::spawn(async move {
            sink.confirmation(
                "Write this file?".into(),
                Some("File: notes.txt".into()),
                true,
                false,
            )
            .await
        });
        let ToolProgress::Confirmation(mut request) = progress.recv().await.unwrap() else {
            panic!("confirmation")
        };
        request.technical_detail = Some("fixture diagnostics".into());
        let mut interaction = ActiveToolInteraction {
            id: ToolCallId("fixture".into()),
            tool: None,
            request: ActiveToolRequest::Confirmation(request),
        };
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        interaction.open(&mut shell);
        for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
            interaction.input(
                &mut shell,
                &Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            );
        }
        if interrupt {
            assert!(interaction.input(
                &mut shell,
                &Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            ));
        }
        drop(interaction);
        assert!(!answer.await.unwrap());
    }
}

#[tokio::test]
async fn foreground_effect_picker_details_require_a_fresh_decision_and_cancel_closed() {
    for decision in [None, Some(false), Some(true)] {
        let (sink, mut progress) = ToolProgressSink::bounded_channel();
        let answer = tokio::spawn(async move {
            sink.confirmation(
                "Run this command?".into(),
                Some("Command: cargo test".into()),
                true,
                false,
            )
            .await
        });
        let ToolProgress::Confirmation(mut request) = progress.recv().await.unwrap() else {
            panic!("confirmation")
        };
        request.technical_detail = Some("fixture diagnostics".into());
        let mut shell = InteractiveShell::test_shell();
        shell.set_size(80, 24);
        shell.prefill_editor("unsent 雪 draft".into());
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut events = vec![
            key(KeyCode::Down),
            key(KeyCode::Down),
            key(KeyCode::Enter),
            key(KeyCode::Enter),
        ];
        if let Some(approved) = decision {
            events.push(key(KeyCode::Esc));
            if approved {
                events.push(key(KeyCode::Down));
            }
            events.push(key(KeyCode::Enter));
        } else {
            events.push(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )));
        }
        let mut input = futures_util::stream::iter(events.into_iter().map(Ok));
        let approved = confirmation_picker(&mut shell, &mut input, &request)
            .await
            .unwrap();
        assert_eq!(approved, decision.unwrap_or(false));
        assert_eq!(shell.pending(), "unsent 雪 draft");
        request.respond(approved);
        assert_eq!(answer.await.unwrap(), approved);
    }
}
