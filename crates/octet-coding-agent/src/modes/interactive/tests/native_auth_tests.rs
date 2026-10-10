//! Inert source/owner fixtures only. No OAuth, browser, provider, credential-store
//! reads/writes, or native terminal are invoked. These do not qualify live sign-in.

use super::*;
use crate::auth::codex::{login_progress_channel, DeviceLoginPhase, LoginFallback, LoginProgress};
use crossterm::event::KeyEvent;
use std::sync::atomic::{AtomicUsize, Ordering};

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

#[test]
fn synthetic_browser_device_fallback_and_progress_keep_instruction_source() {
    let browser = codex_login_document(&LoginProgress::Browser {
        url: "https://example.test/inert-sign-in?state=fixture".into(),
        callback_port: 1457,
    });
    assert!(browser.contains("https://example.test/inert-sign-in?state=fixture"));
    assert!(browser.contains("127.0.0.1:1457"));
    assert!(browser.contains("5 minutes"));
    for (phase, status) in [
        (DeviceLoginPhase::Waiting, "Waiting for authorization"),
        (DeviceLoginPhase::SlowDown, "slower polling"),
        (DeviceLoginPhase::Exchanging, "exchanging the device code"),
    ] {
        let document = codex_login_document(&LoginProgress::Device {
            user_code: "INERT-1234".into(),
            phase,
        });
        assert!(document.contains(crate::auth::codex::DEVICE_VERIFICATION_URI));
        assert!(document.contains("Code: INERT-1234"));
        assert!(document.contains(status));
    }
    for reason in [
        LoginFallback::BusyCallbackPorts,
        LoginFallback::NoBrowserOpener,
        LoginFallback::LimitedCredential,
    ] {
        assert!(codex_login_document(&LoginProgress::Fallback(reason)).contains("device code"));
    }
}

#[test]
fn synthetic_progress_is_single_slot_and_coalesces_obsolete_codes() {
    let (sender, receiver) = login_progress_channel();
    for index in 0..1000 {
        sender.send_replace(LoginProgress::Device {
            user_code: format!("INERT-{index}"),
            phase: DeviceLoginPhase::Waiting,
        });
    }
    let document = codex_login_document(&receiver.borrow());
    assert!(document.contains("Code: INERT-999"));
    assert!(!document.contains("Code: INERT-998"));
}

#[tokio::test]
async fn synthetic_cancel_close_and_eof_drop_auth_before_any_commit() {
    let cases = [
        (Some(key(KeyCode::Esc, KeyModifiers::NONE)), false),
        (Some(key(KeyCode::Left, KeyModifiers::NONE)), false),
        (Some(key(KeyCode::Char('c'), KeyModifiers::CONTROL)), false),
        (Some(key(KeyCode::Char('d'), KeyModifiers::CONTROL)), true),
        (None, true),
    ];
    for (cancel, closing) in cases {
        let mut shell = InteractiveShell::test_shell();
        let pasted = "synthetic attachment prose\n".repeat(20);
        shell.prefill_editor("parent 🦀 draft ".into());
        shell.apply_edit(crate::tui::keymap::EditAction::Paste(pasted.clone()));
        shell.apply_edit(crate::tui::keymap::EditAction::Left);
        let original = shell.extension_editor_snapshot();
        assert!(original.text.contains("[Pasted text #"));
        let (sender, progress) = login_progress_channel();
        let (release, held) = tokio::sync::oneshot::channel::<()>();
        let commits = Arc::new(AtomicUsize::new(0));
        let commit_count = commits.clone();
        let auth = async move {
            let _sender = sender;
            let _ = held.await;
            commit_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let mut events = vec![
            Ok(Event::Paste("must not enter the draft".into())),
            Ok(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            Ok(key(KeyCode::Enter, KeyModifiers::NONE)),
            Ok(Event::Resize(42, 12)),
            Ok(Event::FocusGained),
        ];
        events.extend(cancel.map(Ok));
        let mut input = futures_util::stream::iter(events);
        assert!(await_codex_login(&mut shell, &mut input, progress, auth)
            .await
            .unwrap()
            .is_none());
        assert!(release.send(()).is_err(), "the auth future was dropped");
        assert_eq!(commits.load(Ordering::SeqCst), 0);
        assert_eq!(shell.extension_editor_snapshot(), original);
        assert!(!shell.has_panel());
        assert_eq!(shell.close_requested(), closing);
        assert!(
            shell.debug_snapshot().is_empty(),
            "instructions are transient"
        );
        // A normal submit after restoration must still expand the original chip.
        let InputAction::Submit(_) =
            shell.translate_input(Some(key(KeyCode::Enter, KeyModifiers::NONE)), false)
        else {
            panic!("restored composer owns submission")
        };
        let composed = shell.drain_composed();
        assert_eq!(composed.attachments.len(), 1);
        assert!(composed.transcript_text.contains(pasted.as_str()));
    }
}

#[tokio::test]
async fn synthetic_started_auth_is_cancelled_during_device_wait() {
    let mut shell = InteractiveShell::test_shell();
    shell.prefill_editor("retained draft".into());
    let original = shell.extension_editor_snapshot();
    let (sender, progress) = login_progress_channel();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release, held) = tokio::sync::oneshot::channel::<()>();
    let (input_tx, input_rx) = tokio::sync::mpsc::channel(4);
    let mut input = futures_util::stream::unfold(input_rx, |mut receiver| async move {
        receiver.recv().await.map(|event| (event, receiver))
    })
    .boxed();
    let auth = async move {
        sender.send_replace(LoginProgress::Device {
            user_code: "INERT-WAIT".into(),
            phase: DeviceLoginPhase::Waiting,
        });
        let _ = started_tx.send(());
        let _ = held.await;
        anyhow::bail!("synthetic credential commit must never be reached")
    };
    let cancel = async move {
        started_rx.await.unwrap();
        tokio::task::yield_now().await;
        input_tx.send(Ok(Event::Resize(60, 15))).await.unwrap();
        input_tx
            .send(Ok(key(KeyCode::Esc, KeyModifiers::NONE)))
            .await
            .unwrap();
    };
    let (outcome, ()) = tokio::join!(
        await_codex_login(&mut shell, &mut input, progress, auth),
        cancel
    );
    assert!(outcome.unwrap().is_none());
    assert!(release.send(()).is_err());
    assert_eq!(shell.extension_editor_snapshot(), original);
    assert!(shell.debug_snapshot().is_empty());
    assert!(!shell.has_panel());
}

#[tokio::test]
async fn synthetic_remapped_and_disabled_cancel_do_not_edit_parent_draft() {
    use crate::tui::keymap::keybindings::KeybindingsManager;
    use std::collections::BTreeMap;
    for bindings in [vec!["n".into()], Vec::new()] {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("keep me".into());
        shell.test_set_keybindings(KeybindingsManager::with_platform(
            "linux",
            false,
            BTreeMap::from([("tui.select.cancel".into(), bindings.clone())]),
        ));
        let original = shell.extension_editor_snapshot();
        let (_sender, progress) = login_progress_channel();
        let mut events = vec![Ok(key(KeyCode::Esc, KeyModifiers::NONE))];
        events.push(Ok(if bindings.is_empty() {
            key(KeyCode::Char('c'), KeyModifiers::CONTROL)
        } else {
            key(KeyCode::Char('n'), KeyModifiers::NONE)
        }));
        let mut input = futures_util::stream::iter(events);
        let outcome = await_codex_login(
            &mut shell,
            &mut input,
            progress,
            std::future::pending::<anyhow::Result<()>>(),
        )
        .await
        .unwrap();
        assert!(outcome.is_none());
        assert_eq!(shell.extension_editor_snapshot(), original);
        assert!(!shell.close_requested());
    }
}

#[tokio::test]
async fn synthetic_completion_failure_and_input_error_restore_the_composer() {
    for fail in [false, true] {
        let mut shell = InteractiveShell::test_shell();
        shell.prefill_editor("unchanged".into());
        let original = shell.extension_editor_snapshot();
        let (_sender, progress) = login_progress_channel();
        let mut input = futures_util::stream::pending();
        let result = await_codex_login(&mut shell, &mut input, progress, async {
            if fail {
                anyhow::bail!("inert auth failure")
            }
            Ok(())
        })
        .await;
        assert_eq!(result.is_err(), fail);
        assert!(!shell.has_panel());
        assert_eq!(shell.extension_editor_snapshot(), original);
        assert!(shell.debug_snapshot().is_empty());
    }
    let mut shell = InteractiveShell::test_shell();
    let (_sender, progress) = login_progress_channel();
    let mut input = futures_util::stream::iter([Err(std::io::Error::other("inert input failure"))]);
    assert!(await_codex_login(
        &mut shell,
        &mut input,
        progress,
        std::future::pending::<anyhow::Result<()>>()
    )
    .await
    .is_err());
    assert!(!shell.has_panel());
    assert!(shell.close_requested());
}

#[tokio::test]
async fn synthetic_late_cancel_after_published_commit_does_not_claim_rollback() {
    let mut shell = InteractiveShell::test_shell();
    let (sender, progress) = login_progress_channel();
    // This is a synthetic source fact, not a credential save.
    sender.send_replace(LoginProgress::SignedIn);
    let mut input = futures_util::stream::iter([Ok(key(KeyCode::Esc, KeyModifiers::NONE))]);
    let outcome = await_codex_login(
        &mut shell,
        &mut input,
        progress,
        std::future::pending::<anyhow::Result<()>>(),
    )
    .await
    .unwrap();
    assert_eq!(outcome, Some(()));
    assert!(!shell.has_panel());
}
