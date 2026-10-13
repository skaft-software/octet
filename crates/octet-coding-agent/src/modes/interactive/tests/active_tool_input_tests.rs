//! Active tool requests share the temporary ordinary editor without touching the draft.

use super::*;
use crossterm::event::KeyEvent;
use octet_agent::tool::ToolInputResponse;
use sexy_tui_rs::TextEditAction;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn repeat(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new_with_kind(
        code,
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    ))
}

async fn input_request(
    secret: bool,
) -> (
    ActiveToolInteraction,
    tokio::task::JoinHandle<Option<ToolInputResponse>>,
) {
    let (sink, mut progress) = ToolProgressSink::bounded_channel();
    let answer = tokio::spawn(async move { sink.input("Fixture input?".into(), secret).await });
    let ToolProgress::Input(request) = progress.recv().await.unwrap() else {
        panic!("input request")
    };
    (
        ActiveToolInteraction {
            id: ToolCallId("fixture-input".into()),
            tool: Some("bash".into()),
            request: ActiveToolRequest::Input(request, Default::default()),
        },
        answer,
    )
}

async fn response(
    answer: tokio::task::JoinHandle<Option<ToolInputResponse>>,
) -> Option<ToolInputResponse> {
    tokio::time::timeout(Duration::from_secs(1), answer)
        .await
        .expect("request settled")
        .unwrap()
}

fn assert_private_buffer_empty(interaction: &ActiveToolInteraction) {
    let ActiveToolRequest::Input(_, secret) = &interaction.request else {
        panic!("input request")
    };
    assert_eq!(secret.byte_len(), 0);
}

#[tokio::test]
async fn secret_host_keys_are_not_remapped_picker_actions_and_enter_can_clear_overflow() {
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::collections::BTreeMap;
    for confirm in [vec!["y".into()], Vec::new()] {
        let (mut interaction, answer) = input_request(true).await;
        let mut shell = InteractiveShell::test_shell();
        shell.test_set_keybindings(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([
                ("tui.select.confirm".into(), confirm),
                ("tui.select.cancel".into(), vec!["n".into()]),
                ("tui.select.down".into(), vec!["j".into()]),
            ]),
        ));
        interaction.open(&mut shell);
        assert!(!interaction.input(&mut shell, &Event::Paste("x".repeat(4097))));
        assert!(!interaction.input(&mut shell, &key(KeyCode::Enter)));
        assert!(!answer.is_finished());
        for character in ['y', 'n', 'j'] {
            assert!(!interaction.input(&mut shell, &key(KeyCode::Char(character))));
            assert!(!answer.is_finished());
        }
        assert!(interaction.input(&mut shell, &key(KeyCode::Enter)));
        assert_eq!(response(answer).await.unwrap().as_bytes(), b"ynj");
    }
}

#[tokio::test]
async fn ordinary_input_edits_shared_temporary_editor_and_preserves_draft() {
    let (mut interaction, answer) = input_request(false).await;
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("parent draft".into());
    let draft = shell.extension_editor_snapshot();
    interaction.open(&mut shell);
    for event in [
        Event::Paste("abc".into()),
        key(KeyCode::Home),
        key(KeyCode::Right),
        key(KeyCode::Delete),
        key(KeyCode::Char('x')),
        key(KeyCode::End),
        key(KeyCode::Backspace),
        key(KeyCode::Home),
        key(KeyCode::Char('B')),
        key(KeyCode::Right),
        key(KeyCode::Backspace),
        key(KeyCode::End),
        Event::Paste("C!".into()),
        key(KeyCode::Left),
        key(KeyCode::Right),
    ] {
        assert!(!interaction.input(&mut shell, &event));
    }
    // Native input and terminal input must edit the same temporary buffer.
    shell.edit_tool_input(TextEditAction::Char('?'));
    assert_private_buffer_empty(&interaction);
    assert_eq!(shell.pending(), draft.text);
    assert_eq!(shell.extension_editor_snapshot().cursor, draft.cursor);
    assert_eq!(shell.extension_editor_snapshot().revision, draft.revision);
    assert!(!interaction.input(&mut shell, &repeat(KeyCode::Enter)));
    assert!(!answer.is_finished(), "repeat cannot submit");
    assert!(interaction.input(&mut shell, &key(KeyCode::Enter)));
    assert_eq!(response(answer).await.unwrap().as_bytes(), b"BxC!?");
    assert_eq!(shell.pending(), "parent draft");
    assert!(
        shell.end_tool_input().is_none(),
        "temporary editor was removed"
    );
}

#[tokio::test]
async fn secret_input_stays_private_and_repeat_edits_do_not_submit_or_cancel() {
    let (mut interaction, answer) = input_request(true).await;
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("parent draft".into());
    interaction.open(&mut shell);
    assert!(!interaction.input(&mut shell, &Event::Paste("private-token\r\n".into())));
    assert!(!interaction.input(&mut shell, &repeat(KeyCode::Char('é'))));
    assert!(!interaction.input(&mut shell, &repeat(KeyCode::Backspace)));
    assert!(!interaction.input(&mut shell, &repeat(KeyCode::Enter)));
    assert!(!interaction.input(&mut shell, &repeat(KeyCode::Esc)));
    assert!(!interaction.input(
        &mut shell,
        &Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
    ));
    // Ordinary shared edits have no secret editor to write into.
    shell.edit_tool_input(TextEditAction::Paste("must-not-enter-secret".into()));
    assert_eq!(shell.pending(), "parent draft");
    assert_eq!(shell.extension_editor_snapshot().text, "parent draft");
    assert!(!shell.debug_snapshot().contains("private-token"));
    let ActiveToolRequest::Input(request, secret) = &interaction.request else {
        panic!("input request")
    };
    assert_eq!(secret.byte_len(), "private-token".len());
    assert!(!format!("{request:?}").contains("private-token"));
    assert!(!answer.is_finished());
    assert!(interaction.input(&mut shell, &key(KeyCode::Enter)));
    assert_eq!(response(answer).await.unwrap().as_bytes(), b"private-token");
    assert_private_buffer_empty(&interaction);
    assert!(shell.end_tool_input().is_none());
    assert_eq!(shell.pending(), "parent draft");
}

#[tokio::test]
async fn overflow_enter_clears_without_submitting_then_allows_fresh_input() {
    for secret in [false, true] {
        let (mut interaction, answer) = input_request(secret).await;
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("parent draft".into());
        interaction.open(&mut shell);
        assert!(!interaction.input(&mut shell, &Event::Paste("a".repeat(4095))));
        assert!(!interaction.input(&mut shell, &key(KeyCode::Char('é'))));
        assert!(shell.tool_input_overflowed(), "limit counts UTF-8 bytes");
        assert!(!interaction.input(&mut shell, &key(KeyCode::Backspace)));
        assert!(
            shell.tool_input_overflowed(),
            "editing does not clear rejection"
        );
        assert!(!interaction.input(&mut shell, &repeat(KeyCode::Enter)));
        assert!(
            shell.tool_input_overflowed(),
            "repeat cannot clear rejection"
        );
        assert!(!interaction.input(&mut shell, &key(KeyCode::Enter)));
        assert!(!shell.tool_input_overflowed());
        assert_private_buffer_empty(&interaction);
        assert!(
            !answer.is_finished(),
            "truncated input must not be submitted"
        );
        assert_eq!(shell.pending(), "parent draft");
        assert!(shell.remote_ui_input_blocked(), "request remains active");

        assert!(!interaction.input(&mut shell, &Event::Paste("b".repeat(4097))));
        assert!(shell.tool_input_overflowed(), "oversized paste is rejected");
        assert!(!interaction.input(&mut shell, &key(KeyCode::Enter)));
        assert!(!shell.tool_input_overflowed());
        assert!(!answer.is_finished());
        assert!(!interaction.input(&mut shell, &Event::Paste("fresh".into())));
        assert!(interaction.input(&mut shell, &key(KeyCode::Enter)));
        assert_eq!(response(answer).await.unwrap().as_bytes(), b"fresh");
        assert_eq!(shell.pending(), "parent draft");
        assert!(shell.end_tool_input().is_none());
    }
}

#[tokio::test]
async fn escape_and_control_c_cancel_clear_buffers_and_preserve_draft() {
    for secret in [false, true] {
        for cancel in [
            key(KeyCode::Esc),
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        ] {
            let (mut interaction, answer) = input_request(secret).await;
            let mut shell = InteractiveShell::test_shell();
            shell.prefill_editor("parent draft".into());
            interaction.open(&mut shell);
            assert!(!interaction.input(&mut shell, &Event::Paste("discard this".into())));
            shell.mark_tool_input_overflow();
            assert!(interaction.input(&mut shell, &cancel));
            assert!(response(answer).await.is_none());
            assert_private_buffer_empty(&interaction);
            assert!(!shell.tool_input_overflowed());
            assert!(shell.end_tool_input().is_none());
            assert_eq!(shell.pending(), "parent draft");
            assert!(!shell.debug_snapshot().contains("discard this"));
        }
    }
}

#[tokio::test]
async fn dropping_active_input_still_cancels_unanswered_request() {
    for secret in [false, true] {
        let (mut interaction, answer) = input_request(secret).await;
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("parent draft".into());
        interaction.open(&mut shell);
        assert!(!interaction.input(&mut shell, &Event::Paste("unanswered".into())));
        drop(interaction);
        assert!(response(answer).await.is_none());
        // Existing queue/settlement callers own this lifecycle cleanup.
        shell.set_tool_input_prompt(None);
        assert!(shell.end_tool_input().is_none());
        assert_eq!(shell.pending(), "parent draft");
    }
}
