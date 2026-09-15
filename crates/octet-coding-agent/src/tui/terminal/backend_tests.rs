use std::sync::{Arc, Mutex};

use sexy_tui_rs::{ImageAnchor, ImageId, ImageLayout, ImageProtocol, Terminal};

use super::*;

fn terminal() -> OctetTerminal<Vec<u8>> {
    OctetTerminal {
        out: Vec::new(),
        size: Arc::new(Mutex::new((80, 24))),
        last_was_cr: false,
        pending: Vec::new(),
        pending_log: Vec::new(),
        image_store: TerminalImageStore::default(),
        write_log: None,
        in_synchronized_frame_depth: 0,
    }
}

#[test]
fn backend_normalizes_bare_lf_without_changing_existing_crlf() {
    let mut backend = terminal();
    backend.write("a\nb\r\nc\r");
    backend.write("\nd");
    assert_eq!(backend.out, b"a\r\nb\r\nc\r\nd");
}

#[test]
fn synchronized_frame_is_one_atomic_backend_write_even_without_csi_2026_support() {
    let mut backend = terminal();
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write("frame");
    assert!(backend.out.is_empty());
    backend.write(SYNC_OUTPUT_END);
    assert_eq!(backend.out, b"\x1b[?2026hframe\x1b[?2026l");
}

#[test]
fn synchronized_nested_frame_markers_preserve_depth_semantics() {
    let mut backend = terminal();
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write("nested");
    backend.write(SYNC_OUTPUT_END);
    assert!(backend.out.is_empty());
    backend.write(SYNC_OUTPUT_END);
    assert_eq!(
        backend.out,
        b"\x1b[?2026h\x1b[?2026hnested\x1b[?2026l\x1b[?2026l"
    );
}

#[test]
fn clear_resets_rendition_before_erasing() {
    let mut backend = terminal();
    Terminal::clear_line(&mut backend);
    assert!(backend.out.starts_with(b"\x1b[0m"));
}

#[test]
fn unresolved_image_anchor_is_not_forwarded_as_terminal_control_data() {
    let id = ImageId::new(1).unwrap();
    let layout = ImageLayout::new(2, 1).unwrap();
    let marker = ImageAnchor::new(ImageProtocol::Kitty, id, layout).marker();
    let mut backend = terminal();
    backend.write(&format!("before{marker}after"));
    assert_eq!(backend.out, b"beforeafter");
    assert!(backend.pending_log.is_empty());
}

#[test]
fn malformed_dcs_is_suppressed_without_losing_prior_text() {
    let mut backend = terminal();
    backend.write("before\x1bP+octet-image;unterminated");
    assert_eq!(backend.out, b"before");
}
