//! The idle input owner: the native clipboard paste gesture, its cancel and settlement
//! fences, and the editor-ownership handoff.
//! Separate because clipboard arbitration is the one place where two input owners can
//! both claim the same keystroke.

use super::*;

use super::support::*;

/// The platform's native-paste gesture (`app.clipboard.pasteImage`):
/// `ctrl+v` on Unix, `alt+v` in the win32 keymap.
fn paste_gesture() -> Event {
    Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('v'),
        if cfg!(windows) {
            KeyModifiers::ALT
        } else {
            KeyModifiers::CONTROL
        },
    ))
}

#[tokio::test]
async fn idle_clipboard_gesture_inserts_native_text_without_submitting() {
    clipboard_read::set_test_text(Some("pasted from the clipboard".to_owned()));
    // Ctrl-D settles the idle wait without submitting or discarding the
    // draft the paste created.
    let (shell, idle) = idle_shell_after(vec![paste_gesture(), ctrl_key('d')]).await;
    clipboard_read::clear_test_text();

    assert!(matches!(idle, Idle::Quit));
    assert_eq!(shell.pending(), "pasted from the clipboard");
}

#[tokio::test]
async fn idle_clipboard_gesture_without_text_keeps_the_existing_fallback() {
    clipboard_read::set_test_text(None);
    let (shell, idle) = idle_shell_after(vec![
        paste_gesture(),
        // The terminal-originated bracketed paste remains the fallback when
        // no native transport produced text.
        Event::Paste("terminal bracketed paste".to_owned()),
        ctrl_key('d'),
    ])
    .await;
    clipboard_read::clear_test_text();

    assert!(matches!(idle, Idle::Quit));
    assert_eq!(shell.pending(), "terminal bracketed paste");
}

#[tokio::test]
async fn active_clipboard_completion_preserves_native_and_terminal_paste() {
    for native in [Some("native draft".to_owned()), None] {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        clipboard_read::set_test_text(native.clone());
        let gesture = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            if cfg!(windows) {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            },
        ));
        let mut events = vec![Ok(gesture)];
        if native.is_none() {
            events.push(Ok(Event::Paste("terminal draft".into())));
        }
        events.push(Ok(ctrl_key('d')));
        let mut input = tokio_stream::iter(events).chain(futures_util::stream::pending());
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(1),
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
        assert_eq!(ended, HostRunOutcome::Aborted);
        assert!(quit);
        assert_eq!(
            shell.pending(),
            native.as_deref().unwrap_or("terminal draft")
        );
        assert!(pending.is_empty());
    }
}

#[tokio::test]
async fn active_clipboard_slow_helper_cancels_on_ctrl_c_and_settlement() {
    for cancel in [true, false] {
        let (_server, _workspace, mut agent) = scripted_agent_with_delay(if cancel {
            Duration::from_secs(2)
        } else {
            Duration::from_millis(10)
        })
        .await;
        let mut shell = InteractiveShell::test_shell();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        // A deterministic wedged helper: input cannot send Ctrl-C until the
        // read is actually polled, and the helper cannot finish on its own.
        clipboard_read::set_test_helper(async move {
            let _guard = dropped_tx;
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Some("must never reach the draft".into())
        });
        let gesture = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            if cfg!(windows) {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            },
        ));
        let mut input = tokio_stream::iter([Ok(gesture)])
            .chain(
                futures_util::stream::once(async move {
                    started_rx.await.unwrap();
                    if cancel {
                        Ok(ctrl_key('c'))
                    } else {
                        futures_util::future::pending().await
                    }
                })
                .boxed(),
            )
            .chain(futures_util::stream::pending());
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(1),
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
        assert_eq!(
            ended,
            if cancel {
                HostRunOutcome::Aborted
            } else {
                HostRunOutcome::Completed
            }
        );
        assert!(shell.pending().is_empty());
        assert!(!quit);
        assert!(matches!(
            dropped_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        shell.extension_set_editor("replacement composer".into());
        assert!(
            release_tx.send(()).is_err(),
            "settled helper must be dropped"
        );
        tokio::task::yield_now().await;
        assert_eq!(shell.pending(), "replacement composer");
    }
}

#[tokio::test]
async fn active_clipboard_editor_ownership_fences_text_and_fallback() {
    for text in [Some("stale native text".to_owned()), None] {
        for owner in ["extension", "search", "panel"] {
            let text = text.clone();
            let mut shell = InteractiveShell::test_shell();
            shell.begin_run("test");
            shell.extension_set_editor("original draft".into());
            let revision = shell.extension_editor_snapshot().revision;
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
            let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
            clipboard_read::set_test_helper(async move {
                let _guard = dropped_tx;
                let _ = started_tx.send(());
                let _ = release_rx.await;
                text
            });
            let mut read = Box::pin(clipboard_read::read_text());
            assert!(futures_util::poll!(&mut read).is_pending());
            started_rx.await.unwrap();
            // These changes bypass InputAction ownership-transfer checks.
            // Search and panels leave the normal editor revision unchanged.
            match owner {
                "extension" => {
                    shell.extension_set_editor("replacement composer".into());
                    assert_ne!(shell.extension_editor_snapshot().revision, revision);
                }
                "search" => {
                    assert!(shell.intercept_transcript_input(&transcript_search_open_key()));
                    assert!(shell.transcript_search_active());
                }
                "panel" => shell.open_panel(Panel::ReadOnlyDocument {
                    title: "Inspection".into(),
                    text: "Read-only document".into(),
                    styled: false,
                    scroll_from_bottom: 0,
                }),
                _ => unreachable!(),
            }
            if owner != "extension" {
                let editor = shell.extension_editor_snapshot();
                assert_eq!(editor.revision, revision);
                assert!(!editor.focused);
            }
            let before = shell.debug_snapshot();
            release_tx.send(()).unwrap();
            let gesture = Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char('v'),
                if cfg!(windows) {
                    KeyModifiers::ALT
                } else {
                    KeyModifiers::CONTROL
                },
            ));
            let fallback =
                settle_active_clipboard_read(&mut shell, revision, read.await, Some(gesture));
            assert!(
                fallback.is_none(),
                "stale gestures must not be replayed either"
            );
            assert_eq!(
                shell.pending(),
                if owner == "extension" {
                    "replacement composer"
                } else {
                    "original draft"
                }
            );
            assert_eq!(
                shell.debug_snapshot(),
                before,
                "{owner} must not receive stale paste"
            );
            assert!(matches!(
                dropped_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ));
        }
    }
}

#[tokio::test]
async fn active_clipboard_draft_handoff_drops_pending_read_before_replacement() {
    for boundary in ["queue", "steer", "recall", "command"] {
        let (_server, _workspace, mut agent) =
            scripted_agent_with_delay(Duration::from_secs(2)).await;
        let mut shell = InteractiveShell::test_shell();
        shell.extension_set_editor(if boundary == "command" {
            "/answer answer now".into()
        } else {
            "original draft".into()
        });
        let boundary_key = match boundary {
            "steer" => ctrl_key('s'),
            "recall" => {
                let queued = shell.drain_composed();
                shell.queue_follow_up(queued);
                dequeue_gesture()
            }
            _ => Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )),
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, mut dropped_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        clipboard_read::set_test_helper(async move {
            let _guard = dropped_tx;
            let _ = started_tx.send(());
            let _ = release_rx.await;
            Some("late clipboard payload".into())
        });
        let gesture = Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('v'),
            if cfg!(windows) {
                KeyModifiers::ALT
            } else {
                KeyModifiers::CONTROL
            },
        ));
        let mut input = tokio_stream::iter([Ok(gesture)])
            .chain(
                futures_util::stream::once(async move {
                    started_rx.await.unwrap();
                    Ok(boundary_key)
                })
                .boxed(),
            )
            .chain(
                futures_util::stream::once(async move {
                    // Checked before close or run settlement can clean up the
                    // helper: ownership transfer itself must cancel the read.
                    assert!(
                        matches!(
                            dropped_rx.try_recv(),
                            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
                        ),
                        "{boundary}"
                    );
                    assert!(release_tx.send(()).is_err(), "{boundary}");
                    Ok(Event::Paste(" replacement composer".into()))
                })
                .boxed(),
            )
            .chain(tokio_stream::iter([Ok(ctrl_key('d'))]))
            .chain(futures_util::stream::pending());
        let run_id = shell.begin_run("test");
        let mut run = agent.prompt("initial").await.unwrap();
        shell.set_awaiting_provider(run_id);
        let control = run.control();
        let mut ticker = tokio::time::interval(Duration::from_millis(16));
        let mut pending = VecDeque::new();
        let mut quit = false;
        let mut extensions = crate::extensions::ExecutableExtensions::default();
        let mut deadline = None;
        let ended = tokio::time::timeout(
            Duration::from_secs(1),
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
        assert_eq!(ended, HostRunOutcome::Aborted, "{boundary}");
        assert!(quit);
        assert!(
            shell.pending().contains("replacement composer"),
            "{boundary}"
        );
        assert!(
            !shell.pending().contains("late clipboard payload"),
            "{boundary}"
        );
    }
}

#[tokio::test]
async fn clipboard_gesture_is_consumed_on_the_active_run_path_too() {
    let mut shell = InteractiveShell::test_shell();
    shell.begin_run("test");
    let gesture = paste_gesture();

    clipboard_read::set_test_text(Some("steer text".to_owned()));
    assert!(paste_clipboard_text(&mut shell, &gesture).await);
    assert_eq!(shell.pending(), "steer text");

    // Only the declared gesture is consumed.
    let typed = Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    ));
    assert!(!paste_clipboard_text(&mut shell, &typed).await);

    // A failed read reports that nothing was pasted and leaves the draft.
    clipboard_read::set_test_text(None);
    assert!(!paste_clipboard_text(&mut shell, &gesture).await);
    assert_eq!(shell.pending(), "steer text");
    clipboard_read::clear_test_text();
}
