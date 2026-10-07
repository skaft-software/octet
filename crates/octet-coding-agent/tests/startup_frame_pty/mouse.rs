//! Raw bytes -> octet's sole input decoder -> real shell -> emitted VT/OSC 52.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

fn history(mode: MouseMode, columns: u16, rows: u16) -> (PtyOctet, vt100::Parser, usize) {
    let mut octet = PtyOctet::spawn_at(
        Path::new(env!("CARGO_BIN_EXE_octet")),
        mode,
        None,
        true,
        (columns, rows),
        (2, false, false),
        StartupFixture::MouseHistory,
    );
    let mut parser = vt100::Parser::new(rows, columns, 1024);
    let mut consumed = 0;
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "MOUSE-END",
        STARTUP_TIMEOUT,
    );
    (octet, parser, consumed)
}

fn settle(octet: &mut PtyOctet, parser: &mut vt100::Parser, consumed: &mut usize) {
    octet.pty.drain_for(Duration::from_millis(150));
    parser.process(&octet.pty.output[*consumed..]);
    *consumed = octet.pty.output.len();
}

fn sgr(button: u8, column: u16, row: u16, release: bool) -> Vec<u8> {
    format!(
        "\x1b[<{button};{};{}{}",
        column + 1,
        row + 1,
        if release { 'm' } else { 'M' }
    )
    .into_bytes()
}

fn x10(button: u8, column: u8, row: u8) -> Vec<u8> {
    vec![27, b'[', b'M', button + 32, column + 33, row + 33]
}

fn row_text(parser: &vt100::Parser, row: u16, columns: u16) -> String {
    parser
        .screen()
        .contents_between(row, 0, row, columns)
        .trim_end()
        .to_owned()
}

fn clipboard(output: &[u8]) -> Option<String> {
    let prefix = b"\x1b]52;c;";
    let start = output
        .windows(prefix.len())
        .rposition(|bytes| bytes == prefix)?
        + prefix.len();
    let end = output[start..].iter().position(|byte| *byte == 7)? + start;
    Some(String::from_utf8(STANDARD.decode(&output[start..end]).unwrap()).unwrap())
}

#[test]
fn default_mouse_owner_decision_preserves_native_scrollback_and_selection() {
    let _lock = pty_test_lock().lock().unwrap();
    // Omit --mouse entirely: the real CLI/settings fallback must choose Auto.
    let (mut octet, mut parser, mut consumed) = history(MouseMode::Default, 100, 24);
    settle(&mut octet, &mut parser, &mut consumed);
    for mode in [1000, 1002, 1003, 1006, 1007, 1015, 1016] {
        assert!(
            !contains_bytes(&octet.pty.output, format!("\x1b[?{mode}h").as_bytes()),
            "default policy enabled mouse mode {mode}"
        );
    }
    assert_eq!(
        parser.screen().mouse_protocol_mode(),
        vt100::MouseProtocolMode::None
    );
    assert!(!uses_alternate_screen(&octet.pty.output));

    // Read/copy old text from the terminal's own saved rows, without sending
    // pointer input to octet. This models native scrollback/selection, not an
    // application-owned viewport or a physical client's clipboard permissions.
    // vt100 0.15 cannot read offsets taller than its viewport. Enlarge only a
    // disposable snapshot, leaving the live parser's cursor/wrap state intact.
    let mut native = vt100::Parser::new(24, 100, 1024);
    native.process(&octet.pty.output);
    native.set_size(1024, 100);
    native.set_scrollback(usize::MAX);
    let saved_rows = native.screen().scrollback();
    assert!(saved_rows > 350);
    let row = (0..saved_rows as u16)
        .find(|row| row_text(&native, *row, 100).contains("MOUSE-ROW-001"))
        .expect("complete resumed history remains in native scrollback");
    let selected = row_text(&native, row, 100);
    assert_eq!(
        selected.trim(),
        "1. MOUSE-ROW-001 clipboard line with unique words"
    );
    assert!(parser.screen().contents().contains("MOUSE-END"));
    octet.pty.write_input(b"native-draft");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "native-draft",
        STARTUP_TIMEOUT,
    );
    let capture = octet.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);
    for mode in [1000, 1002, 1003, 1006, 1007, 1015, 1016] {
        assert!(!contains_bytes(
            &capture.output,
            format!("\x1b[?{mode}h").as_bytes()
        ));
    }
    assert!(clipboard(&capture.output).is_none());
}

#[test]
fn raw_mouse_touchpad_bursts_are_proportional_and_preserve_the_draft() {
    let _lock = pty_test_lock().lock().unwrap();
    let (mut octet, mut parser, mut consumed) = history(MouseMode::App, 100, 24);
    for mode in [1000, 1002, 1003, 1006] {
        assert!(contains_bytes(
            &octet.pty.output,
            format!("\x1b[?{mode}h").as_bytes()
        ));
    }
    // The host uses cells on the primary screen, not pixel coordinates or
    // alternate-scroll arrow emulation. 1016 is never requested.
    assert!(!uses_alternate_screen(&octet.pty.output));
    assert!(!contains_bytes(&octet.pty.output, b"\x1b[?1016h"));
    assert!(!contains_bytes(&octet.pty.output, b"\x1b[?1007h"));
    // Clear inherited pixel/alternate-scroll modes before requesting cells.
    assert!(contains_bytes(&octet.pty.output, b"\x1b[?1016l\x1b[?1007l"));
    octet.pty.write_input(b"draft-kept");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "draft-kept",
        STARTUP_TIMEOUT,
    );

    // Repeated identical events in one touchpad-sized burst, including
    // Shift/Ctrl modifiers; no per-frame event loss or composer history moves.
    let mut up = Vec::new();
    for index in 0..96 {
        up.extend(sgr(64 | [0, 4, 16][index % 3], 30, 10, false));
    }
    octet.pty.write_input(&up);
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "288 rows back",
        STARTUP_TIMEOUT,
    );
    assert!(parser.screen().contents().contains("draft-kept"));
    let top = row_text(&parser, 0, 100);
    octet.pty.write_input(&sgr(65, 30, 10, false).repeat(4));
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "276 rows back",
        STARTUP_TIMEOUT,
    );
    assert_ne!(row_text(&parser, 0, 100), top);
    octet.pty.write_input(&x10(65, 30, 10).repeat(92));
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "MOUSE-END",
        STARTUP_TIMEOUT,
    );
    settle(&mut octet, &mut parser, &mut consumed);
    assert!(!parser.screen().contents().contains("rows back"));
    assert!(
        parser.screen().contents().contains("draft-kept"),
        "{}",
        parser.screen().contents()
    );
    // Keyboard Up still belongs to the editor, not transcript wheel routing.
    octet.pty.write_input(b"\x1b[D!");
    await_screen(
        &mut octet,
        &mut parser,
        &mut consumed,
        "draft-kep!t",
        STARTUP_TIMEOUT,
    );
    let capture = octet.shutdown();
    assert!(capture.status.success());
    assert!(capture.termios_restored);
}

fn wrapped_multirow_copy(octet: &mut PtyOctet, parser: &mut vt100::Parser, consumed: &mut usize) {
    settle(octet, parser, consumed);
    let row = (0..16)
        .find(|row| row_text(parser, *row, 80).contains("MOUSE-WRAPPED"))
        .unwrap();
    let first = row_text(parser, row, 80);
    let second = row_text(parser, row + 1, 80);
    assert!(second.contains('界'));
    let expected = format!("{} {}", first.trim(), second.trim());
    let before = octet.pty.output.len();
    octet.pty.write_input(&sgr(0, 0, row, false));
    octet.pty.write_input(&sgr(32, 79, row + 1, false));
    settle(octet, parser, consumed);
    assert!((0..80).any(|col| parser.screen().cell(row + 1, col).unwrap().inverse()));
    octet.pty.write_input(&sgr(0, 79, row + 1, true));
    octet.wait_until(STARTUP_TIMEOUT, |bytes| {
        clipboard(&bytes[before..]).is_some()
    });
    assert_eq!(
        clipboard(&octet.pty.output[before..]).as_deref(),
        Some(expected.as_str())
    );
    settle(octet, parser, consumed);
}

#[test]
fn raw_mouse_drag_copies_painted_markdown_wrapped_rows_and_excludes_chrome() {
    let _lock = pty_test_lock().lock().unwrap();
    for (columns, rows) in [(137, 48), (80, 24), (46, 12)] {
        let (mut octet, mut parser, mut consumed) = history(MouseMode::App, columns, rows);
        octet.pty.write_input(b"draft-private");
        await_screen(
            &mut octet,
            &mut parser,
            &mut consumed,
            "draft-private",
            STARTUP_TIMEOUT,
        );
        if columns == 80 {
            wrapped_multirow_copy(&mut octet, &mut parser, &mut consumed);
        }
        // Resume, PageUp and PageDown must leave pointer coordinates aligned.
        octet.pty.write_input(b"\x1b[5~\x1b[6~");
        octet.pty.write_input(&sgr(64, 20, 3, false).repeat(4));
        settle(&mut octet, &mut parser, &mut consumed);
        for legacy in [false, true] {
            let row = (2..rows - 7)
                .find(|row| row_text(&parser, *row, columns).contains("MOUSE-ROW-"))
                .expect("visible numbered row");
            let painted = row_text(&parser, row, columns);
            let expected = painted.trim_matches([' ', '│']).trim().to_owned();
            let before = octet.pty.output.len();
            let event = |button, column, release| {
                if legacy {
                    x10(if release { 3 } else { button }, column as u8, row as u8)
                } else {
                    sgr(button, column, row, release)
                }
            };
            octet.pty.write_input(&event(0, 0, false));
            octet
                .pty
                .write_input(&event(32, columns.min(220) - 2, false));
            settle(&mut octet, &mut parser, &mut consumed);
            let highlighted =
                (0..columns).any(|col| parser.screen().cell(row, col).unwrap().inverse());
            octet.pty.write_input(&event(0, columns.min(220) - 2, true));
            octet.wait_until(STARTUP_TIMEOUT, |bytes| {
                clipboard(&bytes[before..]).is_some()
            });
            assert_eq!(
                clipboard(&octet.pty.output[before..]).as_deref(),
                Some(expected.as_str()),
                "{columns}x{rows}, legacy={legacy}, painted={painted:?}"
            );
            assert!(contains_bytes(
                &octet.pty.output[before..],
                format!("\x1b]52;c;{}\x07", STANDARD.encode(&expected)).as_bytes()
            ));
            assert!(highlighted, "drag lacks highlight");
            // The explicit selection-copy key uses the same remote transport.
            let before = octet.pty.output.len();
            octet.pty.write_input(b"\x1b[99;6u");
            octet.wait_until(STARTUP_TIMEOUT, |bytes| {
                clipboard(&bytes[before..]).is_some()
            });
            assert_eq!(
                clipboard(&octet.pty.output[before..]).as_deref(),
                Some(expected.as_str())
            );
            settle(&mut octet, &mut parser, &mut consumed);
        }
        // A press originating in composer/footer must never acquire transcript.
        let before = octet.pty.output.len();
        let mut chrome = sgr(0, 1, rows - 1, false);
        chrome.extend(sgr(32, 20, rows - 1, false));
        chrome.extend(sgr(0, 20, rows - 1, true));
        octet.pty.write_input(&chrome);
        settle(&mut octet, &mut parser, &mut consumed);
        assert!(
            clipboard(&octet.pty.output[before..]).is_none(),
            "chrome recopied stale selection"
        );
        assert!(parser.screen().contents().contains("draft-private"));
        let capture = octet.shutdown();
        assert!(capture.status.success());
        assert!(capture.termios_restored);
    }
}
