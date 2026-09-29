//! Unit tests for `crate::images`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `images.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::images`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::ColorDepth;

fn push_png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(u32::try_from(data.len()).unwrap()).to_be_bytes());
    output.extend_from_slice(kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&png_crc32(&[kind, data]).to_be_bytes());
}

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
    assert!((1..=16_384).contains(&width));
    assert!((1..=16_384).contains(&height));
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

fn image_id(value: u32) -> ImageId {
    ImageId::new(value).unwrap()
}

fn image(bytes: Vec<u8>) -> TerminalImage {
    TerminalImage::from_bytes(bytes).unwrap()
}

#[test]
fn validates_all_supported_container_headers_and_dimensions() {
    for (bytes, format, dimensions) in [
        (png(13, 7, 8), ImageFormat::Png, (13, 7)),
        (jpeg(13, 7), ImageFormat::Jpeg, (13, 7)),
        (gif(13, 7), ImageFormat::Gif, (13, 7)),
        (webp(13, 7), ImageFormat::Webp, (13, 7)),
    ] {
        let image = image(bytes);
        assert_eq!(image.format(), format);
        assert_eq!(image.dimensions().width(), dimensions.0);
        assert_eq!(image.dimensions().height(), dimensions.1);
    }
}

#[test]
fn format_matrix_is_explicit_and_conservative() {
    assert!(ImageProtocol::Kitty.supports_format(ImageFormat::Png));
    assert!(!ImageProtocol::Kitty.supports_format(ImageFormat::Jpeg));
    assert!(!ImageProtocol::Kitty.supports_format(ImageFormat::Gif));
    assert!(!ImageProtocol::Kitty.supports_format(ImageFormat::Webp));
    assert!(ImageProtocol::Iterm2.supports_format(ImageFormat::Png));
    assert!(ImageProtocol::Iterm2.supports_format(ImageFormat::Jpeg));
    assert!(ImageProtocol::Iterm2.supports_format(ImageFormat::Gif));
    assert!(!ImageProtocol::Iterm2.supports_format(ImageFormat::Webp));
}

#[test]
fn validates_metadata_dimensions_filenames_and_payload_bounds_before_copying() {
    let metadata = ImageMetadata::default()
        .with_expected_dimensions(ImageDimensions::new(2, 1).unwrap())
        .with_filename(ImageFilename::new("safe-image.png").unwrap());
    assert!(matches!(
        TerminalImage::from_slice_with_metadata(&png(1, 1, 4), metadata, &ImageLimits::default()),
        Err(ImageError::MetadataDimensionMismatch)
    ));
    assert_eq!(
        ImageFilename::new("../escape.png"),
        Err(ImageError::UnsafeFilename)
    );
    assert_eq!(ImageFilename::new("."), Err(ImageError::UnsafeFilename));
    assert_eq!(ImageFilename::new(".."), Err(ImageError::UnsafeFilename));
    assert_eq!(
        ImageFilename::new("bad\u{1b}name"),
        Err(ImageError::UnsafeFilename)
    );

    let tiny = ImageLimits::default().with_max_payload_bytes(50).unwrap();
    assert!(matches!(
        TerminalImage::from_slice_with_metadata(&png(1, 1, 64), ImageMetadata::default(), &tiny),
        Err(ImageError::PayloadTooLarge)
    ));

    let retained_limits = ImageLimits::default().with_max_payload_bytes(512).unwrap();
    let mut excess_capacity = Vec::with_capacity(1_024);
    excess_capacity.extend_from_slice(&png(1, 1, 4));
    let retained = TerminalImage::from_bytes_with_metadata(
        excess_capacity,
        ImageMetadata::default(),
        &retained_limits,
    )
    .unwrap();
    assert!(retained.bytes.capacity() <= retained_limits.max_payload_bytes());

    assert!(matches!(
        TerminalImage::from_bytes(png(9_000, 1, 1)),
        Err(ImageError::DimensionsTooLarge)
    ));
    assert!(matches!(
        TerminalImage::from_bytes(png(4_000, 2_000, 1)),
        Err(ImageError::PixelCountTooLarge)
    ));

    let already_validated = image(png(1, 1, 64));
    assert!(matches!(
        ImageProtocolEncoder::new(ImageProtocol::Kitty, tiny).encode_place(
            image_id(1),
            &already_validated,
            ImageLayout::new(1, 1).unwrap()
        ),
        Err(ImageError::PayloadTooLarge)
    ));
}

#[test]
fn rejects_corrupt_truncated_and_polyglot_containers() {
    let fixtures = [png(2, 2, 4), jpeg(2, 2), gif(2, 2), webp(2, 2)];
    for fixture in fixtures {
        let mut truncated = fixture.clone();
        truncated.pop();
        assert!(TerminalImage::from_bytes(truncated).is_err());
        let mut polyglot = fixture;
        polyglot.extend_from_slice(b"not-an-image-tail");
        assert!(TerminalImage::from_bytes(polyglot).is_err());
    }
    let mut corrupt = png(2, 2, 4);
    corrupt[20] ^= 0x80;
    assert!(matches!(
        TerminalImage::from_bytes(corrupt),
        Err(ImageError::InvalidImage)
    ));
    assert!(matches!(
        TerminalImage::from_bytes(b"\x89PNG\r\n\x1a\n".to_vec()),
        Err(ImageError::InvalidImage)
    ));
    assert!(matches!(
        TerminalImage::from_bytes(Vec::new()),
        Err(ImageError::InvalidImage)
    ));

    let mut separated_idat = png(2, 2, 4);
    let mut split_chunks = Vec::new();
    push_png_chunk(&mut split_chunks, b"tEXt", b"safe");
    push_png_chunk(&mut split_chunks, b"IDAT", &[1]);
    let iend = separated_idat.len() - 12;
    separated_idat.splice(iend..iend, split_chunks);
    assert!(matches!(
        TerminalImage::from_bytes(separated_idat),
        Err(ImageError::InvalidImage)
    ));

    let mut duplicate_webp_payload = webp(2, 2);
    let second_payload = duplicate_webp_payload[12..].to_vec();
    duplicate_webp_payload.extend_from_slice(&second_payload);
    let declared = u32::try_from(duplicate_webp_payload.len() - 8).unwrap();
    duplicate_webp_payload[4..8].copy_from_slice(&declared.to_le_bytes());
    assert!(matches!(
        TerminalImage::from_bytes(duplicate_webp_payload),
        Err(ImageError::InvalidImage)
    ));
}

#[test]
fn rejects_animated_containers_before_terminal_side_decoding() {
    let mut animated_png = png(2, 2, 4);
    let mut control = Vec::new();
    push_png_chunk(&mut control, b"acTL", &[0; 8]);
    let iend = animated_png.len() - 12;
    animated_png.splice(iend..iend, control);
    assert!(matches!(
        TerminalImage::from_bytes(animated_png),
        Err(ImageError::UnsupportedAnimation)
    ));

    let mut animated_gif = gif(2, 2);
    assert_eq!(animated_gif.pop(), Some(0x3b));
    animated_gif.extend_from_slice(&[
        0x2c, 0, 0, 0, 0, 2, 0, 2, 0, 0, // image descriptor
        2, 2, 0x4c, 1, 0, // LZW stream
        0x3b,
    ]);
    assert!(matches!(
        TerminalImage::from_bytes(animated_gif),
        Err(ImageError::UnsupportedAnimation)
    ));

    let mut plain_text_gif = gif(2, 2);
    let mut plain_text_extension = vec![0x21, 0x01, 12];
    plain_text_extension.extend_from_slice(&[0; 12]);
    plain_text_extension.push(0);
    // Header (6) + logical screen descriptor (7) + two-color table (6).
    plain_text_gif.splice(19..19, plain_text_extension);
    assert!(matches!(
        TerminalImage::from_bytes(plain_text_gif),
        Err(ImageError::UnsupportedAnimation)
    ));

    let mut body = b"WEBPVP8X".to_vec();
    body.extend_from_slice(&10_u32.to_le_bytes());
    body.extend_from_slice(&[0x02, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    body.extend_from_slice(b"ANMF");
    body.extend_from_slice(&0_u32.to_le_bytes());
    let mut animated_webp = b"RIFF".to_vec();
    animated_webp.extend_from_slice(&(u32::try_from(body.len()).unwrap()).to_le_bytes());
    animated_webp.extend_from_slice(&body);
    assert!(matches!(
        TerminalImage::from_bytes(animated_webp),
        Err(ImageError::UnsupportedAnimation)
    ));
}

#[test]
fn layout_uses_cell_pixels_resizes_and_never_wraps() {
    let cell = CellPixelSize::new(8, 16).unwrap();
    assert_eq!(cell_rows_for_pixels(33, Some(cell)), Ok(3));
    assert_eq!(cell_rows_for_pixels(33, None), Ok(1));
    assert!(cell_rows_for_pixels(u32::MAX, Some(CellPixelSize::new(1, 1).unwrap())).is_err());

    let dimensions = ImageDimensions::new(1_600, 800).unwrap();
    let wide =
        ImageLayout::fit(dimensions, ImageViewport::new(20, 10, Some(cell)).unwrap()).unwrap();
    assert_eq!((wide.columns(), wide.rows()), (20, 5));
    let narrow =
        ImageLayout::fit(dimensions, ImageViewport::new(10, 10, Some(cell)).unwrap()).unwrap();
    assert_eq!((narrow.columns(), narrow.rows()), (10, 3));
    let unknown = ImageLayout::fit(dimensions, ImageViewport::new(10, 10, None).unwrap()).unwrap();
    assert_eq!((unknown.columns(), unknown.rows()), (1, 1));
    let estimated = ImageLayout::fit(
        dimensions,
        ImageViewport::new(20, 10, None)
            .unwrap()
            .with_estimated_cell_pixels(cell),
    )
    .unwrap();
    assert_eq!((estimated.columns(), estimated.rows()), (20, 5));
    let measured = ImageLayout::fit(
        dimensions,
        ImageViewport::new(20, 10, Some(CellPixelSize::new(16, 16).unwrap()))
            .unwrap()
            .with_estimated_cell_pixels(cell),
    )
    .unwrap();
    assert_eq!((measured.columns(), measured.rows()), (20, 10));
    assert!(ImageLayout::new(1, MAX_RESERVED_IMAGE_ROWS + 1).is_err());
}

#[test]
fn capability_forcing_and_reply_parsing_are_deterministic_and_bounded() {
    let mut terminal = TerminalCapabilities::interactive(ColorDepth::TrueColor, true);
    terminal.kitty_graphics = false;
    terminal.iterm2_images = false;
    let mut detected = ImageCapabilities::detect(&terminal, &ImageCapabilityOverrides::default());
    assert_eq!(detected.protocol(), None);

    let forced = ImageCapabilities::detect(
        &terminal,
        &ImageCapabilityOverrides {
            force: Some(ImageProtocol::Iterm2),
            cell_pixel_size: Some(CellPixelSize::new(9, 18).unwrap()),
            ..ImageCapabilityOverrides::default()
        },
    );
    assert_eq!(forced.protocol(), Some(ImageProtocol::Iterm2));
    assert_eq!(forced.cell_pixel_size().unwrap().width(), 9);
    let plain_forced = ImageCapabilities::detect(
        &TerminalCapabilities::plain(),
        &ImageCapabilityOverrides {
            force: Some(ImageProtocol::Kitty),
            ..ImageCapabilityOverrides::default()
        },
    );
    assert_eq!(plain_forced.protocol(), None);

    let id = image_id(77);
    let limits = ImageLimits::default();
    let reply = "\x1b_Gi=77;OK\x1b\\";
    assert_eq!(
        parse_terminal_image_reply(reply, Some(id), &limits),
        Some(TerminalImageReply::KittyGraphicsSupported { query_id: id })
    );
    assert_eq!(
        parse_terminal_image_reply(reply, Some(image_id(78)), &limits),
        None
    );
    assert_eq!(
        parse_terminal_image_reply("\x1b_Gi=77;OK\x1b\\\x1b[6;16;8t", Some(id), &limits),
        None
    );
    assert_eq!(
        parse_terminal_image_reply("\x1b[6;16;8t", None, &limits),
        Some(TerminalImageReply::CellPixels(
            CellPixelSize::new(8, 16).unwrap()
        ))
    );
    assert_eq!(
        parse_terminal_image_reply("\x1b]1337;ReportCellSize=16;8\u{7}", None, &limits),
        Some(TerminalImageReply::Iterm2CellPixels(
            CellPixelSize::new(8, 16).unwrap()
        ))
    );
    assert!(parse_terminal_image_reply(&"x".repeat(513), None, &limits).is_none());

    detected.apply_reply(TerminalImageReply::KittyGraphicsSupported { query_id: id });
    assert_eq!(detected.protocol(), Some(ImageProtocol::Kitty));
    detected.apply_reply(TerminalImageReply::CellPixels(
        CellPixelSize::new(8, 16).unwrap(),
    ));
    assert_eq!(detected.cell_pixel_size().unwrap().height(), 16);

    let query = ImageCapabilityQuery::kitty_graphics(id, &limits);
    let mut wire = Vec::new();
    query.write_to(&mut wire).unwrap();
    assert_eq!(wire, b"\x1b_Ga=q,i=77,s=1,v=1,f=24;\x1b\\");
    assert_eq!(query.timeout(), DEFAULT_QUERY_TIMEOUT);
    assert!(matches!(
        query.parse_reply(reply, &limits),
        Some(TerminalImageReply::KittyGraphicsSupported { query_id }) if query_id == id
    ));
    assert!(query.parse_reply("\x1b[6;16;8t", &limits).is_none());
    let cell_query = ImageCapabilityQuery::cell_pixels(&limits);
    assert!(matches!(
        cell_query.parse_reply("\x1b[6;16;8t", &limits),
        Some(TerminalImageReply::CellPixels(_))
    ));
    assert!(cell_query
        .parse_reply("\x1b]1337;ReportCellSize=16;8\u{7}", &limits)
        .is_none());
}

#[test]
fn kitty_chunks_are_bounded_and_reassemble_to_one_base64_stream() {
    let image = image(png(3, 2, 97));
    let limits = ImageLimits::default()
        .with_max_protocol_chunk_bytes(16)
        .unwrap()
        .with_max_protocol_chunks(256)
        .unwrap();
    let command = ImageProtocolEncoder::new(ImageProtocol::Kitty, limits)
        .encode_place(image_id(9), &image, ImageLayout::new(2, 3).unwrap())
        .unwrap();
    let mut wire = Vec::new();
    command.write_to(&mut wire).unwrap();
    assert_eq!(command.encoded_len(), wire.len());
    assert!(command.payload_chunks() > 1);
    let wire = String::from_utf8(wire).unwrap();
    assert!(wire.starts_with("\x1b_Ga=T,t=d,f=100,i=9,q=2,c=2,r=3,C=1,m=1;"));
    assert!(wire.ends_with("\x1b\\"));
    let sequences = wire
        .split("\x1b\\")
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(sequences.len(), command.payload_chunks());
    let joined = sequences
        .iter()
        .map(|sequence| sequence.split_once(';').unwrap().1)
        .collect::<String>();
    assert_eq!(joined, base64_string(image.bytes()).unwrap());
    assert!(sequences.iter().all(|sequence| {
        sequence.split_once(';').is_some_and(|(_, body)| {
            body.len() <= 16
                && body.bytes().all(|byte| {
                    byte.is_ascii_alphabetic()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'+' | b'/' | b'=')
                })
        })
    }));
}

#[test]
fn iterm2_encodes_supported_formats_as_one_bounded_osc_frame() {
    let limits = ImageLimits::default();
    for source in [png(2, 1, 5), jpeg(2, 1), gif(2, 1)] {
        let image = image(source);
        let command = ImageProtocolEncoder::new(ImageProtocol::Iterm2, limits.clone())
            .encode_place(image_id(11), &image, ImageLayout::new(2, 1).unwrap())
            .unwrap();
        let mut wire = Vec::new();
        command.write_to(&mut wire).unwrap();
        let wire = String::from_utf8(wire).unwrap();
        assert!(wire.starts_with("\x1b]1337;File=size="), "{wire:?}");
        assert!(
            wire.contains(";inline=1;doNotMoveCursor=1;width=2;height=1;preserveAspectRatio=1:")
        );
        assert!(wire.ends_with("\x1b\\"));
        assert_eq!(wire.matches("\x1b]1337;File=").count(), 1);
        assert_eq!(wire.matches("\x1b\\").count(), 1);
    }
    let webp = image(webp(2, 1));
    assert!(matches!(
        ImageProtocolEncoder::new(ImageProtocol::Iterm2, limits).encode_place(
            image_id(11),
            &webp,
            ImageLayout::new(2, 1).unwrap()
        ),
        Err(ImageError::UnsupportedFormatForProtocol)
    ));
}

#[test]
fn registry_replace_delete_and_iterm2_limits_are_explicit() {
    let image = image(png(2, 2, 8));
    let layout = ImageLayout::new(2, 2).unwrap();
    let mut registry = ImageRegistry::default();
    let placed = registry.place().unwrap();
    let id = placed.id();
    assert_eq!(id.get(), 1);
    assert!(registry.is_live(id));
    let replace = registry.replace(id).unwrap();
    let kitty = ImageProtocolEncoder::new(ImageProtocol::Kitty, ImageLimits::default());
    let command = kitty.encode_replace(replace.id(), &image, layout).unwrap();
    let mut wire = Vec::new();
    command.write_to(&mut wire).unwrap();
    let wire = String::from_utf8(wire).unwrap();
    assert!(wire.starts_with("\x1b_Ga=d,d=I,i=1,q=2\x1b\\\x1b_Ga=T,"));

    let deleted = registry.delete(id).unwrap();
    let delete = kitty.encode_delete(deleted.id()).unwrap();
    let mut delete_wire = Vec::new();
    delete.write_to(&mut delete_wire).unwrap();
    assert_eq!(delete_wire, b"\x1b_Ga=d,d=I,i=1,q=2\x1b\\");
    let tiny_output = ImageProtocolEncoder::new(
        ImageProtocol::Kitty,
        ImageLimits::default()
            .with_max_encoded_output_bytes(1)
            .unwrap(),
    );
    assert!(matches!(
        tiny_output.encode_delete(id),
        Err(ImageError::EncodedOutputTooLarge)
    ));
    assert_eq!(registry.delete(id), Err(ImageError::StaleImageId));
    assert_eq!(registry.replace(id), Err(ImageError::StaleImageId));
    assert_eq!(registry.place().unwrap().id().get(), 2);

    let iterm = ImageProtocolEncoder::new(ImageProtocol::Iterm2, ImageLimits::default());
    assert!(matches!(
        iterm.encode_replace(id, &image, layout),
        Err(ImageError::UnsupportedOperation)
    ));
    assert!(matches!(
        iterm.encode_delete(id),
        Err(ImageError::UnsupportedOperation)
    ));

    let mut final_id = ImageRegistry {
        next: u32::MAX,
        live: BTreeSet::new(),
    };
    assert_eq!(final_id.place().unwrap().id().get(), u32::MAX);
    assert_eq!(final_id.place(), Err(ImageError::ImageIdExhausted));
}

#[test]
fn registry_bounds_concurrent_live_bookkeeping() {
    let mut registry = ImageRegistry::new();
    for _ in 0..HARD_MAX_LIVE_IMAGES {
        registry.place().unwrap();
    }
    assert_eq!(registry.place(), Err(ImageError::TooManyLiveImages));
    registry.delete(image_id(1)).unwrap();
    assert_eq!(registry.place().unwrap().id().get(), 4_097);
}

#[test]
fn image_anchor_round_trips_without_graphics_payload() {
    let anchor = ImageAnchor::new(
        ImageProtocol::Kitty,
        image_id(41),
        ImageLayout::new(7, 3).unwrap(),
    );
    let marker = anchor.marker();
    assert_eq!(ImageAnchor::parse(&marker), Some(anchor));
    assert_eq!(
        ImageAnchor::parse_all(&format!("before{marker}after")),
        vec![anchor]
    );
    assert!(!marker.contains("IDAT"));
    assert!(!marker.contains("_G"));
    assert!(ImageAnchor::parse("\x1bP+octet-image;v=2,p=kitty,i=41,c=7,r=3\x1b\\").is_none());
}

#[test]
fn planner_reserves_rows_without_protocol_or_payload_in_semantic_text() {
    let png_image = image(png(16, 33, 16));
    let planner = ImagePlanner::new(
        ImageCapabilities::forced(
            Some(ImageProtocol::Kitty),
            Some(CellPixelSize::new(8, 16).unwrap()),
        ),
        ImageLimits::default(),
    );
    let plan = planner
        .plan_place(
            image_id(41),
            &png_image,
            ImageViewport::new(40, 20, Some(CellPixelSize::new(8, 16).unwrap())).unwrap(),
        )
        .unwrap();
    assert_eq!(plan.reservation().rows(), 3);
    let copy = plan.semantic_copy_text();
    assert!(!copy.contains('\u{1b}'));
    assert!(!copy.contains("_G"));
    assert!(!copy.contains("1337"));
    assert!(!copy.contains("IDAT"));
    assert!(plan.terminal_command().is_some());
    let mut protocol = Vec::new();
    plan.write_protocol_to(&mut protocol).unwrap();
    assert!(protocol.starts_with(b"\x1b_G"));

    let plain = ImagePlanner::new(
        ImageCapabilities::forced(None, None),
        ImageLimits::default(),
    )
    .plan_place(
        image_id(42),
        &png_image,
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

    let jpeg = image(jpeg(2, 1));
    let kitty_fallback = planner
        .plan_place(
            image_id(43),
            &jpeg,
            ImageViewport::new(40, 20, None).unwrap(),
        )
        .unwrap();
    assert_eq!(
        kitty_fallback.fallback_reason(),
        Some(ImageFallbackReason::UnsupportedFormat)
    );
    let webp = image(webp(2, 1));
    let iterm_fallback = ImagePlanner::new(
        ImageCapabilities::forced(Some(ImageProtocol::Iterm2), None),
        ImageLimits::default(),
    )
    .plan_place(
        image_id(44),
        &webp,
        ImageViewport::new(40, 20, None).unwrap(),
    )
    .unwrap();
    assert_eq!(
        iterm_fallback.fallback_reason(),
        Some(ImageFallbackReason::UnsupportedFormat)
    );
}

#[test]
fn deterministic_property_style_headers_remain_bounded() {
    let cell = CellPixelSize::new(7, 13).unwrap();
    let mut seed = 0x5eed_f00d_u32;
    for _ in 0..128 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let width = 1 + seed % 400;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let height = 1 + seed % 400;
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let payload = usize::try_from(1 + seed % 64).unwrap();
        let image = image(png(width, height, payload));
        let viewport = ImageViewport::new(80, 30, Some(cell)).unwrap();
        let layout = ImageLayout::fit(image.dimensions(), viewport).unwrap();
        assert!((1..=MAX_IMAGE_CELL_COLUMNS).contains(&layout.columns()));
        assert!((1..=MAX_RESERVED_IMAGE_ROWS).contains(&layout.rows()));
        let command = ImageProtocolEncoder::new(ImageProtocol::Kitty, ImageLimits::default())
            .encode_place(image_id(100), &image, layout)
            .unwrap();
        let mut wire = Vec::new();
        command.write_to(&mut wire).unwrap();
        assert_eq!(wire.len(), command.encoded_len());
        assert!(wire.len() <= ImageLimits::default().max_encoded_output_bytes());
    }
}

#[test]
fn debug_and_errors_never_echo_raw_payload_or_hostile_metadata() {
    let image = image(png(2, 2, 32));
    let debug = format!("{image:?}");
    assert!(!debug.contains("IDAT"));
    assert!(!debug.contains("\u{1b}"));
    let error = ImageFilename::new("bad\u{1b}title").unwrap_err();
    assert!(!error.to_string().contains("title"));
    let command = ImageProtocolEncoder::new(ImageProtocol::Kitty, ImageLimits::default())
        .encode_place(image_id(5), &image, ImageLayout::new(1, 1).unwrap())
        .unwrap();
    assert!(!format!("{command:?}").contains("IDAT"));
}
