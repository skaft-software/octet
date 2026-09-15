//! Hermetic acceptance fixtures for the bounded terminal-image foundation.

use sexy_tui_rs::{
    parse_terminal_image_reply, CellPixelSize, ColorDepth, ImageCapabilities, ImageCapabilityQuery,
    ImageError, ImageFallbackReason, ImageId, ImageLayout, ImageLimits, ImagePlanner,
    ImageProtocol, ImageProtocolEncoder, ImageRegistry, ImageViewport, TerminalCapabilities,
    TerminalImage, TerminalImageReply,
};

fn png(width: u32, height: u32, idat_len: usize) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);
    push_png_chunk(&mut output, b"IHDR", &header);
    let idat = (0..idat_len.max(1))
        .map(|index| (index as u8).wrapping_mul(31))
        .collect::<Vec<_>>();
    push_png_chunk(&mut output, b"IDAT", &idat);
    push_png_chunk(&mut output, b"IEND", &[]);
    output
}

fn push_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(u32::try_from(data.len()).unwrap()).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&png_crc32(&[kind, data]).to_be_bytes());
}

fn png_crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for part in parts {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
    }
    !crc
}

fn jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut output = vec![0xff, 0xd8];
    output.extend_from_slice(&[
        0xff,
        0xc0,
        0x00,
        0x11,
        8,
        (height >> 8) as u8,
        height as u8,
        (width >> 8) as u8,
        width as u8,
        3,
        1,
        0x11,
        0,
        2,
        0x11,
        0,
        3,
        0x11,
        0,
    ]);
    output.extend_from_slice(&[0xff, 0xda, 0x00, 0x08, 1, 1, 0, 0, 0x3f, 0, 0, 0xff, 0xd9]);
    output
}

fn gif(width: u16, height: u16) -> Vec<u8> {
    let mut output = b"GIF89a".to_vec();
    output.extend_from_slice(&width.to_le_bytes());
    output.extend_from_slice(&height.to_le_bytes());
    output.extend_from_slice(&[0x80, 0, 0]);
    output.extend_from_slice(&[0, 0, 0, 0xff, 0xff, 0xff]);
    output.push(0x2c);
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&width.to_le_bytes());
    output.extend_from_slice(&height.to_le_bytes());
    output.push(0);
    output.extend_from_slice(&[2, 2, 0x4c, 1, 0, 0x3b]);
    output
}

fn webp(width: u32, height: u32) -> Vec<u8> {
    let packed = (width - 1) | ((height - 1) << 14);
    let mut body = b"WEBPVP8L".to_vec();
    body.extend_from_slice(&6_u32.to_le_bytes());
    body.push(0x2f);
    body.extend_from_slice(&packed.to_le_bytes());
    body.push(0);
    let mut output = b"RIFF".to_vec();
    output.extend_from_slice(&(u32::try_from(body.len()).unwrap()).to_le_bytes());
    output.extend_from_slice(&body);
    output
}

fn image(bytes: Vec<u8>) -> TerminalImage {
    TerminalImage::from_bytes(bytes).unwrap()
}

fn image_id(value: u32) -> ImageId {
    ImageId::new(value).unwrap()
}

fn write_command(command: &sexy_tui_rs::ImageTerminalCommand<'_>) -> Vec<u8> {
    let mut output = Vec::new();
    command.write_to(&mut output).unwrap();
    output
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(TABLE[usize::from(a >> 2)] as char);
        output.push(TABLE[usize::from(((a & 0x03) << 4) | (b >> 4))] as char);
        output.push(if chunk.len() > 1 {
            TABLE[usize::from(((b & 0x0f) << 2) | (c >> 6))] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            TABLE[usize::from(c & 0x3f)] as char
        } else {
            '='
        });
    }
    output
}

#[test]
fn hermetic_fixtures_validate_and_protocol_matrix_is_conservative() {
    let fixtures = [
        (png(13, 7, 8), sexy_tui_rs::ImageFormat::Png, (13, 7)),
        (jpeg(13, 7), sexy_tui_rs::ImageFormat::Jpeg, (13, 7)),
        (gif(13, 7), sexy_tui_rs::ImageFormat::Gif, (13, 7)),
        (webp(13, 7), sexy_tui_rs::ImageFormat::Webp, (13, 7)),
    ];
    for (bytes, format, dimensions) in fixtures {
        let image = image(bytes);
        assert_eq!(image.format(), format);
        assert_eq!(
            (image.dimensions().width(), image.dimensions().height()),
            dimensions
        );
    }

    assert!(ImageProtocol::Kitty.supports_format(sexy_tui_rs::ImageFormat::Png));
    assert!(!ImageProtocol::Kitty.supports_format(sexy_tui_rs::ImageFormat::Jpeg));
    assert!(!ImageProtocol::Kitty.supports_format(sexy_tui_rs::ImageFormat::Gif));
    assert!(!ImageProtocol::Kitty.supports_format(sexy_tui_rs::ImageFormat::Webp));
    assert!(ImageProtocol::Iterm2.supports_format(sexy_tui_rs::ImageFormat::Png));
    assert!(ImageProtocol::Iterm2.supports_format(sexy_tui_rs::ImageFormat::Jpeg));
    assert!(ImageProtocol::Iterm2.supports_format(sexy_tui_rs::ImageFormat::Gif));
    assert!(!ImageProtocol::Iterm2.supports_format(sexy_tui_rs::ImageFormat::Webp));
}

#[test]
fn malformed_polyglot_and_animated_fixtures_are_rejected() {
    for fixture in [png(2, 2, 4), jpeg(2, 2), gif(2, 2), webp(2, 2)] {
        let mut truncated = fixture.clone();
        truncated.pop();
        assert!(TerminalImage::from_bytes(truncated).is_err());
        let mut polyglot = fixture;
        polyglot.extend_from_slice(b"not-an-image-tail");
        assert!(TerminalImage::from_bytes(polyglot).is_err());
    }

    let mut apng = png(2, 2, 4);
    let mut animation_control = Vec::new();
    push_png_chunk(&mut animation_control, b"acTL", &[0; 8]);
    let iend = apng.len() - 12;
    apng.splice(iend..iend, animation_control);
    assert!(matches!(
        TerminalImage::from_bytes(apng),
        Err(ImageError::UnsupportedAnimation)
    ));

    let mut looping_gif = gif(2, 2);
    assert_eq!(looping_gif.pop(), Some(0x3b));
    looping_gif.extend_from_slice(&[
        0x21, 0xff, 11, b'N', b'E', b'T', b'S', b'C', b'A', b'P', b'E', b'2', b'.', b'0', 3, 1, 0,
        0, 0, 0x3b,
    ]);
    assert!(matches!(
        TerminalImage::from_bytes(looping_gif),
        Err(ImageError::UnsupportedAnimation)
    ));

    let mut webp_body = b"WEBPVP8X".to_vec();
    webp_body.extend_from_slice(&10_u32.to_le_bytes());
    webp_body.extend_from_slice(&[0x02, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    webp_body.extend_from_slice(b"ANMF");
    webp_body.extend_from_slice(&0_u32.to_le_bytes());
    let mut animated_webp = b"RIFF".to_vec();
    animated_webp.extend_from_slice(&(u32::try_from(webp_body.len()).unwrap()).to_le_bytes());
    animated_webp.extend_from_slice(&webp_body);
    assert!(matches!(
        TerminalImage::from_bytes(animated_webp),
        Err(ImageError::UnsupportedAnimation)
    ));
}

#[test]
fn planner_revalidates_images_and_keeps_fallback_semantic() {
    assert_eq!(
        ImageLimits::default().with_max_filename_bytes(0),
        Err(ImageError::InvalidLimit)
    );

    let source = image(png(1, 1, 4));
    let smaller = ImageLimits::default()
        .with_max_payload_bytes(source.byte_len() - 1)
        .unwrap();
    let rejected = ImagePlanner::new(ImageCapabilities::forced(None, None), smaller).plan_place(
        image_id(1),
        &source,
        ImageViewport::new(40, 20, None).unwrap(),
    );
    assert!(matches!(rejected, Err(ImageError::PayloadTooLarge)));

    let cell = CellPixelSize::new(8, 16).unwrap();
    let planner = ImagePlanner::new(
        ImageCapabilities::forced(Some(ImageProtocol::Kitty), Some(cell)),
        ImageLimits::default(),
    );
    let second_image = image(png(16, 33, 16));
    let plan = planner
        .plan_place(
            image_id(2),
            &second_image,
            ImageViewport::new(40, 20, Some(cell)).unwrap(),
        )
        .unwrap();
    assert_eq!(plan.layout().unwrap().rows(), 3);
    assert_eq!(plan.semantic_rows(), vec![String::new(); 3]);
    assert!(plan.terminal_command().is_some());
    assert!(!plan.semantic_copy_text().contains('\u{1b}'));

    let plain = ImagePlanner::new(
        ImageCapabilities::forced(None, None),
        ImageLimits::default(),
    )
    .plan_place(
        image_id(3),
        &source,
        ImageViewport::new(40, 20, None).unwrap(),
    )
    .unwrap();
    assert_eq!(
        plain.fallback_reason(),
        Some(ImageFallbackReason::UnsupportedTerminal)
    );
    assert!(plain.terminal_command().is_none());
    assert!(plain.semantic_copy_text().starts_with("[image: PNG"));
    assert!(!plain.semantic_copy_text().contains('\u{1b}'));
}

#[test]
fn capability_queries_are_bounded_correlated_and_exact() {
    let limits = ImageLimits::default();
    let id = image_id(77);

    let kitty = ImageCapabilityQuery::kitty_graphics(id, &limits);
    let mut query_bytes = Vec::new();
    kitty.write_to(&mut query_bytes).unwrap();
    assert_eq!(query_bytes, b"\x1b_Ga=q,i=77,s=1,v=1,f=24;\x1b\\");
    assert_eq!(kitty.timeout(), limits.query_timeout());
    assert!(kitty.parse_reply("\x1b_Gi=77;OK\x1b\\", &limits).is_some());
    assert!(kitty.parse_reply("\x1b_Gi=78;OK\x1b\\", &limits).is_none());
    assert!(kitty
        .parse_reply("\x1b_Gi=77;OK\x1b\\\x1b[6;16;8t", &limits)
        .is_none());

    let cells = ImageCapabilityQuery::cell_pixels(&limits);
    let mut cell_bytes = Vec::new();
    cells.write_to(&mut cell_bytes).unwrap();
    assert_eq!(cell_bytes, b"\x1b[16t");
    assert!(matches!(
        cells.parse_reply("\x1b[6;16;8t", &limits),
        Some(TerminalImageReply::CellPixels(size)) if size == CellPixelSize::new(8, 16).unwrap()
    ));
    assert!(cells
        .parse_reply("\x1b]1337;ReportCellSize=16;8\x07", &limits)
        .is_none());

    let iterm = ImageCapabilityQuery::iterm2_cell_pixels(&limits);
    let mut iterm_bytes = Vec::new();
    iterm.write_to(&mut iterm_bytes).unwrap();
    assert_eq!(iterm_bytes, b"\x1b]1337;ReportCellSize\x1b\\");
    assert!(matches!(
        iterm.parse_reply("\x1b]1337;ReportCellSize=16;8\x07", &limits),
        Some(TerminalImageReply::Iterm2CellPixels(size)) if size == CellPixelSize::new(8, 16).unwrap()
    ));
    assert!(iterm.parse_reply("\x1b[6;16;8t", &limits).is_none());
    assert!(parse_terminal_image_reply(
        &"x".repeat(limits.max_terminal_reply_bytes() + 1),
        None,
        &limits
    )
    .is_none());

    let mut terminal = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    terminal.kitty_graphics = true;
    terminal.cell_pixel_size = Some(CellPixelSize::new(9, 18).unwrap());
    let capabilities = ImageCapabilities::detect(&terminal, &Default::default());
    assert_eq!(capabilities.protocol(), Some(ImageProtocol::Kitty));
    assert_eq!(capabilities.cell_pixel_size(), terminal.cell_pixel_size);
    assert_eq!(
        ImageCapabilities::detect(
            &TerminalCapabilities::plain(),
            &sexy_tui_rs::ImageCapabilityOverrides {
                force: Some(ImageProtocol::Kitty),
                ..Default::default()
            },
        )
        .protocol(),
        None
    );
}

#[test]
fn protocol_commands_are_bounded_and_semantically_separate() {
    let source_bytes = png(3, 2, 97);
    let source = image(source_bytes.clone());
    let limits = ImageLimits::default()
        .with_max_protocol_chunk_bytes(16)
        .unwrap()
        .with_max_protocol_chunks(256)
        .unwrap();
    let command = ImageProtocolEncoder::new(ImageProtocol::Kitty, limits.clone())
        .encode_place(image_id(9), &source, ImageLayout::new(2, 3).unwrap())
        .unwrap();
    let wire = write_command(&command);
    assert_eq!(command.encoded_len(), wire.len());
    assert!(wire.len() <= limits.max_encoded_output_bytes());
    let wire = String::from_utf8(wire).unwrap();
    let sequences = wire
        .split("\x1b\\")
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(sequences.len(), command.payload_chunks());
    let joined = sequences
        .iter()
        .map(|sequence| sequence.split_once(';').unwrap().1)
        .collect::<String>();
    assert_eq!(joined, base64(&source_bytes));
    assert!(sequences.iter().all(|sequence| {
        sequence
            .split_once(';')
            .is_some_and(|(_, payload)| payload.len() <= 16)
    }));

    let iterm_image = image(jpeg(2, 1));
    let iterm = ImageProtocolEncoder::new(ImageProtocol::Iterm2, limits.clone())
        .encode_place(image_id(10), &iterm_image, ImageLayout::new(2, 1).unwrap())
        .unwrap();
    let iterm_wire = write_command(&iterm);
    assert!(iterm_wire.starts_with(b"\x1b]1337;File=size="));
    assert!(iterm_wire
        .windows(b";inline=1;doNotMoveCursor=1;width=2;height=1;preserveAspectRatio=1:".len())
        .any(|window| window
            == b";inline=1;doNotMoveCursor=1;width=2;height=1;preserveAspectRatio=1:"));
    // One OSC frame has two ESC bytes: its introducer and the ST terminator.
    // Check the complete wire contract, including multi-chunk base64 payload,
    // rather than mistaking ST for a second image frame.
    let jpeg_bytes = jpeg(2, 1);
    let expected = format!(
        "\x1b]1337;File=size={};inline=1;doNotMoveCursor=1;width=2;height=1;preserveAspectRatio=1:{}\x1b\\",
        jpeg_bytes.len(), base64(&jpeg_bytes),
    );
    assert!(iterm.payload_chunks() > 1);
    assert_eq!(iterm_wire, expected.as_bytes());
    assert_eq!(iterm.encoded_len(), iterm_wire.len());
    assert!(iterm_wire.len() <= limits.max_encoded_output_bytes());

    let webp_image = image(webp(2, 1));
    assert_eq!(
        ImageProtocolEncoder::new(ImageProtocol::Iterm2, ImageLimits::default())
            .encode_place(image_id(11), &webp_image, ImageLayout::new(2, 1).unwrap())
            .unwrap_err(),
        ImageError::UnsupportedFormatForProtocol
    );
}

#[test]
fn registry_anchors_and_kitty_lifecycle_are_stale_delete_resistant() {
    let mut registry = ImageRegistry::new();
    let placed = registry.place().unwrap();
    let id = placed.id();
    assert!(registry.is_live(id));

    let image = image(png(2, 2, 8));
    let layout = ImageLayout::new(2, 2).unwrap();
    let encoder = ImageProtocolEncoder::new(ImageProtocol::Kitty, ImageLimits::default());
    let replacement = registry.replace(id).unwrap();
    let replace_wire = write_command(
        &encoder
            .encode_replace(replacement.id(), &image, layout)
            .unwrap(),
    );
    assert!(replace_wire.starts_with(b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=T,"));

    let anchor = sexy_tui_rs::ImageAnchor::new(ImageProtocol::Kitty, id, layout);
    let marker = anchor.marker();
    assert_eq!(
        sexy_tui_rs::ImageAnchor::parse_all(&format!("before{marker}after")),
        vec![anchor]
    );
    assert!(!marker.contains("_G"));
    assert!(!marker.contains("IDAT"));

    let deleted = registry.delete(id).unwrap();
    let delete_wire = write_command(&encoder.encode_delete(deleted.id()).unwrap());
    assert_eq!(delete_wire, b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    assert_eq!(registry.delete(id), Err(ImageError::StaleImageId));
    assert_eq!(registry.replace(id), Err(ImageError::StaleImageId));
    assert_eq!(registry.place().unwrap().id().get(), 2);

    let iterm = ImageProtocolEncoder::new(ImageProtocol::Iterm2, ImageLimits::default());
    assert_eq!(
        iterm.encode_replace(id, &image, layout).unwrap_err(),
        ImageError::UnsupportedOperation
    );
    assert_eq!(
        iterm.encode_delete(id).unwrap_err(),
        ImageError::UnsupportedOperation
    );
}
