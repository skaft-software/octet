use std::sync::{Arc, Mutex};

use sexy_tui_rs::{ImageAnchor, ImageId, ImageLayout, ImageProtocol, Terminal};

use super::*;

fn terminal() -> OctetTerminal<Vec<u8>> {
    terminal_with_hyperlinks(false)
}

fn terminal_with_hyperlinks(supported: bool) -> OctetTerminal<Vec<u8>> {
    OctetTerminal {
        out: Vec::new(),
        size: Arc::new(Mutex::new((80, 24))),
        last_was_cr: false,
        hyperlink_filter: (!supported).then(Osc8Filter::default),
        pending: Vec::new(),
        pending_log: Vec::new(),
        image_store: TerminalImageStore::default(),
        write_log: None,
        in_synchronized_frame_depth: 0,
    }
}

#[test]
fn backend_without_hyperlink_support_keeps_labels_but_not_osc8_controls() {
    let mut backend = terminal();
    backend.write("\x1b]8;id=label;https://example.test/\x1b\\界é\x1b]8;;\x07");
    assert_eq!(backend.out, "界é".as_bytes());
}

#[test]
fn osc8_policy_preserves_other_controls_and_every_utf8_safe_chunk_boundary() {
    let body = concat!(
        "\x1b[32mgreen\x1b[0m\r\n",
        "\x1b]8;id=文;https://example.test/界\x1b\\界é\x1b]8;;\x07",
        "\x1b]2;title 8; unchanged\x07",
        "\x1b]52;c;aGVsbG8=\x1b\\",
        "\x1b]80;other OSC\x07",
        "\x1b]8;;https://example.test/\x07label\x1b]8;;\x1b\\",
        "\nlast\r\n"
    );
    let stripped = concat!(
        "\x1b[32mgreen\x1b[0m\r\n界é",
        "\x1b]2;title 8; unchanged\x07",
        "\x1b]52;c;aGVsbG8=\x1b\\",
        "\x1b]80;other OSC\x07label\nlast\r\n"
    );
    for supported in [false, true] {
        let text = if supported { body } else { stripped };
        let expected = format!(
            "{SYNC_OUTPUT_BEGIN}{}{SYNC_OUTPUT_END}",
            normalize_line_endings(text, &mut false)
        );
        for split in (0..=body.len()).filter(|index| body.is_char_boundary(*index)) {
            let mut backend = OctetTerminal {
                out: RecordingSink::default(),
                size: Arc::new(Mutex::new((80, 24))),
                last_was_cr: false,
                hyperlink_filter: (!supported).then(Osc8Filter::default),
                pending: Vec::new(),
                pending_log: Vec::new(),
                image_store: TerminalImageStore::default(),
                write_log: None,
                in_synchronized_frame_depth: 0,
            };
            backend.write(SYNC_OUTPUT_BEGIN);
            backend.write(&body[..split]);
            backend.write("");
            backend.write(&body[split..]);
            assert!(backend.out.frames.is_empty());
            backend.write(SYNC_OUTPUT_END);
            assert_eq!(
                backend.out.frames,
                [expected.as_bytes()],
                "{supported}, split {split}"
            );
        }
        // Many fragments, not only a single split. No UTF-8 grapheme is removed.
        let mut backend = terminal_with_hyperlinks(supported);
        backend.write(SYNC_OUTPUT_BEGIN);
        for ch in body.chars() {
            backend.write(ch.encode_utf8(&mut [0; 4]));
        }
        backend.write(SYNC_OUTPUT_END);
        assert_eq!(backend.out, expected.as_bytes());
    }
}

#[test]
fn unsupported_osc8_is_filtered_before_diagnostic_log_mirroring() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("hyperlink-write.log");
    let mut backend = terminal();
    backend.write_log = Some(File::create(&path).unwrap());
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write("\x1b]8;;https://example.test/\x07界é\x1b]8;;\x1b\\");
    backend.write(SYNC_OUTPUT_END);
    let expected = format!("{SYNC_OUTPUT_BEGIN}界é{SYNC_OUTPUT_END}");
    assert_eq!(backend.out, expected.as_bytes());
    assert_eq!(std::fs::read(path).unwrap(), expected.as_bytes());
}

#[derive(Default)]
struct ShortWritingSink {
    bytes: Vec<u8>,
    writes: usize,
    flushes: usize,
}

impl std::io::Write for ShortWritingSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = bytes.len().min(3);
        self.bytes.extend_from_slice(&bytes[..written]);
        self.writes += 1;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.flushes += 1;
        Ok(())
    }
}

impl FrameSink for ShortWritingSink {}

#[test]
fn filtered_hyperlink_frame_retains_write_all_and_single_flush_on_short_sink_writes() {
    let mut backend = OctetTerminal {
        out: ShortWritingSink::default(),
        size: Arc::new(Mutex::new((80, 24))),
        last_was_cr: false,
        hyperlink_filter: Some(Osc8Filter::default()),
        pending: Vec::new(),
        pending_log: Vec::new(),
        image_store: TerminalImageStore::default(),
        write_log: None,
        in_synchronized_frame_depth: 0,
    };
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write("\x1b]8;;https://example.test/\x07界é\x1b]8;;\x07");
    assert_eq!(backend.out.writes, 0);
    assert_eq!(backend.out.flushes, 0);
    backend.write(SYNC_OUTPUT_END);
    assert_eq!(
        backend.out.bytes,
        format!("{SYNC_OUTPUT_BEGIN}界é{SYNC_OUTPUT_END}").as_bytes()
    );
    assert!(backend.out.writes > 1);
    assert_eq!(backend.out.flushes, 1);
}

#[test]
fn backend_normalizes_bare_lf_without_changing_existing_crlf() {
    let mut backend = terminal();
    backend.write("a\nb\r\nc\r");
    backend.write("\nd");
    assert_eq!(backend.out, b"a\r\nb\r\nc\r\nd");
}

#[test]
fn normalized_frames_borrow_their_source_and_preserve_chunk_boundaries() {
    let source = "界 👩‍💻\r\nnext\r\n";
    let mut preceding_cr = false;
    assert!(matches!(
        normalize_line_endings(source, &mut preceding_cr),
        Cow::Borrowed(_)
    ));
    for source in ["", source, "\n界\r\n\n👩‍💻\r\n", "a\r\nb\nc\r\n"] {
        let mut previous = false;
        let expected = normalize_line_endings(source, &mut previous).into_owned();
        for split in (0..=source.len()).filter(|index| source.is_char_boundary(*index)) {
            let mut previous = false;
            let mut actual = normalize_line_endings(&source[..split], &mut previous).into_owned();
            // An empty write must not lose a trailing CR from the prior chunk.
            assert!(normalize_line_endings("", &mut previous).is_empty());
            actual.push_str(&normalize_line_endings(&source[split..], &mut previous));
            assert_eq!(actual, expected, "split at {split}");
        }
    }
}

#[test]
fn diagnostic_payload_is_retained_only_when_logging_is_enabled() {
    let payload = "synthetic native history\r\n".repeat(40_000);
    let mut backend = terminal();
    backend.write(SYNC_OUTPUT_BEGIN);
    backend.write(&payload);
    assert!(backend.pending_log.is_empty());
    assert_eq!(backend.pending_log.capacity(), 0);
    backend.write(SYNC_OUTPUT_END);
    let expected = backend.out.clone();

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic-write.log");
    let mut logged = terminal();
    logged.write_log = Some(File::create(&path).unwrap());
    logged.write(SYNC_OUTPUT_BEGIN);
    logged.write(&payload);
    assert!(!logged.pending_log.is_empty());
    logged.write(SYNC_OUTPUT_END);
    assert_eq!(logged.out, expected);
    assert_eq!(std::fs::read(path).unwrap(), expected);
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
        hyperlink_filter: Some(Osc8Filter::default()),
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
