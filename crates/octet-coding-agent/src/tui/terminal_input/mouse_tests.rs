//! Real PTY bytes through the single input decoder and native shell map.
use super::*;
use crate::tui::keymap::InputAction;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

pub(super) async fn real_pty_mouse_decoder_contract(
    master: &mut std::fs::File,
    input: &mut TerminalInput,
) {
    wheel_matrix(master, input).await;
    fragments_and_burst(master, input).await;
    drag_and_keyboard(master, input).await;
}

async fn wheel_matrix(master: &mut std::fs::File, input: &mut TerminalInput) {
    let mut shell = crate::tui::view::InteractiveShell::test_shell();
    for index in 0..200 {
        shell.notice(format!("decoder-view-row-{index:03}"));
    }
    shell.apply_edit(crate::tui::keymap::EditAction::Paste("draft-kept".into()));
    for modifier in 0..8 {
        let cb = ((modifier & 1) * 4) | ((modifier & 2) * 4) | ((modifier & 4) * 4);
        let mut modifiers = KeyModifiers::NONE;
        if cb & 4 != 0 {
            modifiers |= KeyModifiers::SHIFT;
        }
        if cb & 8 != 0 {
            modifiers |= KeyModifiers::ALT;
        }
        if cb & 16 != 0 {
            modifiers |= KeyModifiers::CONTROL;
        }
        for (button, kind) in [
            (64, MouseEventKind::ScrollUp),
            (65, MouseEventKind::ScrollDown),
        ] {
            // SGR 1006, legacy X10/1000, and decimal rxvt 1015 all decode to
            // the same zero-based cell event, including combined modifiers.
            for wire in [
                format!("\x1b[<{};31;11M", button | cb).into_bytes(),
                vec![27, b'[', b'M', (button | cb) + 32, 63, 43],
                format!("\x1b[{};31;11M", (button | cb) + 32).into_bytes(),
            ] {
                master.write_all(&wire).unwrap();
                let decoded = input.next().await.unwrap().unwrap();
                assert_eq!(
                    decoded,
                    Event::Mouse(MouseEvent {
                        kind,
                        column: 30,
                        row: 10,
                        modifiers
                    })
                );
                let step = if modifiers.contains(KeyModifiers::ALT) {
                    15
                } else {
                    3
                };
                let direction = if button == 64 { -step } else { step };
                assert_eq!(
                    shell.translate_input(Some(decoded), false),
                    InputAction::ScrollLines(direction)
                );
                shell.scroll_lines(direction);
                assert_eq!(shell.pending(), "draft-kept");
            }
        }
    }
}

async fn fragments_and_burst(master: &mut std::fs::File, input: &mut TerminalInput) {
    // Byte fragments stay in U6's sole decoder, not a parallel parser.
    master.write_all(b"\x1b[<").unwrap();
    assert!(spawn(input.next()).poll().is_pending());
    master.write_all(b"64;31;").unwrap();
    assert!(spawn(input.next()).poll().is_pending());
    master.write_all(b"11M").unwrap();
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 30,
            row: 10,
            modifiers: KeyModifiers::NONE,
        })
    );
    let burst = b"\x1b[<64;31;11M".repeat(192);
    // A PTY may hold less than the whole burst (notably on macOS). Feed it
    // concurrently so a blocking write cannot prevent the sole reader from
    // draining it. Keep the full burst and every event assertion.
    let mut writer = master.try_clone().unwrap();
    let feed = std::thread::spawn(move || writer.write_all(&burst));
    for _ in 0..192 {
        assert!(matches!(
            input.next().await.unwrap().unwrap(),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 30,
                row: 10,
                modifiers: KeyModifiers::NONE,
            })
        ));
    }
    feed.join().unwrap().unwrap();
}

async fn drag_and_keyboard(master: &mut std::fs::File, input: &mut TerminalInput) {
    for (wire, kind, column) in [
        (
            b"\x1b[<0;1;21M".as_slice(),
            MouseEventKind::Down(MouseButton::Left),
            0,
        ),
        (
            b"\x1b[<32;91;21M".as_slice(),
            MouseEventKind::Drag(MouseButton::Left),
            90,
        ),
        (
            b"\x1b[<0;91;21m".as_slice(),
            MouseEventKind::Up(MouseButton::Left),
            90,
        ),
        (
            b"\x1b[M\x20\x21\x35".as_slice(),
            MouseEventKind::Down(MouseButton::Left),
            0,
        ),
        (
            b"\x1b[M\x40\x7b\x35".as_slice(),
            MouseEventKind::Drag(MouseButton::Left),
            90,
        ),
        (
            b"\x1b[M\x23\x7b\x35".as_slice(),
            MouseEventKind::Up(MouseButton::Left),
            90,
        ),
    ] {
        master.write_all(wire).unwrap();
        assert_eq!(
            input.next().await.unwrap().unwrap(),
            Event::Mouse(MouseEvent {
                kind,
                column,
                row: 20,
                modifiers: KeyModifiers::NONE,
            })
        );
    }
    master
        .write_all(b"\x1b[D\x1b[200~paste-kept\x1b[201~")
        .unwrap();
    assert!(
        matches!(input.next().await.unwrap().unwrap(), Event::Key(key) if key.code == KeyCode::Left)
    );
    assert_eq!(
        input.next().await.unwrap().unwrap(),
        Event::Paste("paste-kept".into())
    );
}
