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

/// Records each sink call separately; a `Vec<u8>` would hide how many writes
/// one frame took.
#[derive(Default)]
struct RecordingSink {
    frames: Vec<Vec<u8>>,
}

impl std::io::Write for RecordingSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.frames.push(bytes.to_vec());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl FrameSink for RecordingSink {
    fn write_frame(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.frames.push(frame.to_vec());
        Ok(())
    }
}

#[test]
fn a_multi_row_frame_reaches_the_sink_in_one_write() {
    let mut backend = OctetTerminal {
        out: RecordingSink::default(),
        size: Arc::new(Mutex::new((80, 24))),
        last_was_cr: false,
        pending: Vec::new(),
        pending_log: Vec::new(),
        image_store: TerminalImageStore::default(),
        write_log: None,
        in_synchronized_frame_depth: 0,
    };
    // The shape of a differential repaint: cursor motion, per-row clears and
    // text, emitted through many backend calls inside one frame.
    backend.write(SYNC_OUTPUT_BEGIN);
    Terminal::move_by(&mut backend, -3);
    for row in 0..40 {
        Terminal::clear_line(&mut backend);
        backend.write(&format!("row {row} \u{2500}\u{1f600}\r\n"));
    }
    Terminal::hide_cursor(&mut backend);
    assert!(
        backend.out.frames.is_empty(),
        "nothing may reach the terminal mid-frame"
    );
    backend.write(SYNC_OUTPUT_END);
    assert_eq!(backend.out.frames.len(), 1, "one frame, one terminal write");
    let frame = String::from_utf8(backend.out.frames.remove(0)).unwrap();
    assert!(frame.starts_with(SYNC_OUTPUT_BEGIN));
    assert!(frame.ends_with(SYNC_OUTPUT_END));
    assert!(frame.contains("row 39"));
}

#[test]
fn console_frames_convert_to_utf16_losslessly_or_not_at_all() {
    let frame = "\x1b[2Kbox \u{2500} emoji \u{1f600}\r\n";
    let units = frame_utf16(frame.as_bytes()).expect("UTF-8 frame");
    assert_eq!(String::from_utf16(&units).unwrap(), frame);
    // A non-UTF-8 batch keeps the byte-oriented writer rather than being
    // lossily re-encoded.
    assert!(frame_utf16(b"\xff\xfe").is_none());
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

/// A valid, static 1x33 RGBA PNG generated from 33 filtered scanlines. The
/// encoder needs a real signature and inspectable dimensions before it will
/// hand anything to the protocol writer.
fn png_payload() -> Arc<TerminalImage> {
    Arc::new(
        TerminalImage::from_bytes(vec![
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 33,
            8, 6, 0, 0, 0, 24, 185, 193, 191, 0, 0, 0, 16, 73, 68, 65, 84, 120, 156, 99, 248, 207,
            192, 240, 159, 97, 144, 19, 0, 49, 155, 65, 191, 162, 42, 52, 239, 0, 0, 0, 0, 73, 69,
            78, 68, 174, 66, 96, 130,
        ])
        .expect("valid png"),
    )
}

#[test]
fn a_repeated_placement_reuses_the_encoded_bytes() {
    let id = ImageId::new(1).unwrap();
    let layout = ImageLayout::new(2, 1).unwrap();
    let marker = ImageAnchor::new(ImageProtocol::Kitty, id, layout).marker();
    let mut backend = terminal();
    backend.image_store.register(id, png_payload());

    backend.write(&marker);
    let first = backend.out.clone();
    assert!(!first.is_empty(), "first placement wrote nothing");

    // The same image on a later frame must not re-run base64 over the payload.
    backend.write(&marker);
    assert_eq!(backend.out, [first.clone(), first].concat());

    let cached = backend.image_store.encoded_len();
    assert_eq!(cached, 1, "one placement, one cached encoding");
}

#[test]
fn a_resized_placement_is_encoded_again_rather_than_reusing_the_old_layout() {
    let id = ImageId::new(1).unwrap();
    let mut backend = terminal();
    backend.image_store.register(id, png_payload());

    let narrow =
        ImageAnchor::new(ImageProtocol::Kitty, id, ImageLayout::new(2, 1).unwrap()).marker();
    let wide = ImageAnchor::new(ImageProtocol::Kitty, id, ImageLayout::new(8, 1).unwrap()).marker();
    backend.write(&narrow);
    let narrow_bytes = backend.out.clone();
    backend.write(&wide);
    let wide_bytes = backend.out[narrow_bytes.len()..].to_vec();

    // The header carries the placement rectangle, so a resized anchor must not
    // be served the previous size's bytes.
    assert_ne!(narrow_bytes, wide_bytes);
    assert!(String::from_utf8_lossy(&wide_bytes).contains("c=8,r=1"));
    assert_eq!(backend.image_store.encoded_len(), 2);
}

#[test]
fn re_registering_an_id_retires_its_cached_encoding() {
    let id = ImageId::new(1).unwrap();
    let marker =
        ImageAnchor::new(ImageProtocol::Kitty, id, ImageLayout::new(2, 1).unwrap()).marker();
    let mut backend = terminal();
    backend.image_store.register(id, png_payload());
    backend.write(&marker);
    assert_eq!(backend.image_store.encoded_len(), 1);

    // A new payload under the same id must never be served the old bytes.
    backend.image_store.register(id, png_payload());
    assert_eq!(backend.image_store.encoded_len(), 0);
}

#[test]
fn the_encoded_cache_stays_bounded_across_many_resizes() {
    let id = ImageId::new(1).unwrap();
    let mut backend = terminal();
    backend.image_store.register(id, png_payload());
    for columns in 1..=(MAX_ENCODED_IMAGES as u16 + 4) {
        let marker = ImageAnchor::new(
            ImageProtocol::Kitty,
            id,
            ImageLayout::new(columns, 1).unwrap(),
        )
        .marker();
        backend.write(&marker);
    }
    assert!(
        backend.image_store.encoded_len() <= MAX_ENCODED_IMAGES,
        "encoded cache grew to {}",
        backend.image_store.encoded_len()
    );
}
