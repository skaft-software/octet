//! Bounded container-header inspection: the only place payload bytes are read.
//!
//! Accepting an image means proving four things about its header without
//! decompressing anything: that it is a container this subsystem claims to
//! understand, that its declared dimensions are nonzero and within the caller's
//! limits, that no metadata record exceeds its bound, and — for the animated
//! formats — that the payload is not a multi-frame stream. An animated PNG, GIF
//! or WebP is rejected outright, because a bounded source must not be able to
//! ask the terminal to decode unbounded frames.
//!
//! Every parser here is a bounded cursor walk: it counts what it reads against
//! [`super::limits`] and returns an error rather than continuing, so a truncated
//! or hostile header costs a fixed number of steps. They live together because
//! they share the container grammar and the item budget — separating PNG from
//! JPEG from GIF from WebP would put four copies of the same cursor discipline
//! into four files with no way to compare them.

use super::error::ImageError;
use super::format::{ImageDimensions, ImageFormat, ImageMetadata};
use super::helpers::{read_u16_be, read_u16_le, read_u32_be, read_u32_le, take_container_item};
use super::limits::ImageLimits;
use super::payload::TerminalImage;

pub(super) fn validate_existing_image(
    image: &TerminalImage,
    limits: &ImageLimits,
) -> Result<(), ImageError> {
    // A `TerminalImage` may have been accepted under different caller limits.
    // Re-inspection is bounded and ensures an encoder or planner cannot loosen
    // its own source, metadata, or container-record boundary by borrowing it.
    inspect_image(image.bytes(), image.metadata(), limits).map(|_| ())
}

pub(super) fn inspect_image(
    bytes: &[u8],
    metadata: &ImageMetadata,
    limits: &ImageLimits,
) -> Result<(ImageFormat, ImageDimensions), ImageError> {
    if bytes.is_empty() {
        return Err(ImageError::InvalidImage);
    }
    if bytes.len() > limits.max_payload_bytes {
        return Err(ImageError::PayloadTooLarge);
    }
    if let Some(filename) = metadata.filename() {
        if filename.as_str().len() > limits.max_filename_bytes {
            return Err(ImageError::UnsafeFilename);
        }
    }
    let format = ImageFormat::detect(bytes).ok_or(ImageError::UnsupportedFormat)?;
    let dimensions = match format {
        ImageFormat::Png => parse_png(bytes, limits)?,
        ImageFormat::Jpeg => parse_jpeg(bytes, limits)?,
        ImageFormat::Gif => parse_gif(bytes, limits)?,
        ImageFormat::Webp => parse_webp(bytes, limits)?,
    };
    validate_dimensions(dimensions, limits)?;
    if metadata
        .expected_dimensions()
        .is_some_and(|expected| expected != dimensions)
    {
        return Err(ImageError::MetadataDimensionMismatch);
    }
    Ok((format, dimensions))
}

pub(super) fn validate_dimensions(
    dimensions: ImageDimensions,
    limits: &ImageLimits,
) -> Result<(), ImageError> {
    if dimensions.width > limits.max_width || dimensions.height > limits.max_height {
        return Err(ImageError::DimensionsTooLarge);
    }
    let pixels = u64::from(dimensions.width)
        .checked_mul(u64::from(dimensions.height))
        .ok_or(ImageError::PixelCountTooLarge)?;
    if pixels > limits.max_pixels {
        return Err(ImageError::PixelCountTooLarge);
    }
    Ok(())
}

const fn crc32_table() -> [u32; 256] {
    let mut table = [0_u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 1 != 0 {
                0xedb8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }
        table[index] = value;
        index += 1;
    }
    table
}

const PNG_CRC_TABLE: [u32; 256] = crc32_table();

pub(super) fn png_crc32(parts: &[&[u8]]) -> u32 {
    let mut value = 0xffff_ffff_u32;
    for part in parts {
        for byte in *part {
            let index = usize::from(((value ^ u32::from(*byte)) & 0xff) as u8);
            value = PNG_CRC_TABLE[index] ^ (value >> 8);
        }
    }
    !value
}

pub(super) fn parse_png(bytes: &[u8], limits: &ImageLimits) -> Result<ImageDimensions, ImageError> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if !bytes.starts_with(SIGNATURE) || bytes.len() < SIGNATURE.len() + 25 {
        return Err(ImageError::InvalidImage);
    }
    let mut offset = SIGNATURE.len();
    let mut items = 0;
    let mut dimensions = None;
    let mut saw_idat = false;
    let mut idat_ended = false;

    loop {
        take_container_item(&mut items, limits)?;
        if bytes.len().saturating_sub(offset) < 12 {
            return Err(ImageError::InvalidImage);
        }
        let length = usize::try_from(read_u32_be(bytes, offset).ok_or(ImageError::InvalidImage)?)
            .map_err(|_| ImageError::InvalidImage)?;
        let data_start = offset.checked_add(8).ok_or(ImageError::InvalidImage)?;
        let crc_start = data_start
            .checked_add(length)
            .ok_or(ImageError::InvalidImage)?;
        let end = crc_start.checked_add(4).ok_or(ImageError::InvalidImage)?;
        if end > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
        let kind = &bytes[offset + 4..data_start];
        let data = &bytes[data_start..crc_start];
        let expected_crc = read_u32_be(bytes, crc_start).ok_or(ImageError::InvalidImage)?;
        if png_crc32(&[kind, data]) != expected_crc {
            return Err(ImageError::InvalidImage);
        }
        if kind == b"acTL" || kind == b"fcTL" || kind == b"fdAT" {
            return Err(ImageError::UnsupportedAnimation);
        }

        if items == 1 {
            if kind != b"IHDR" || length != 13 {
                return Err(ImageError::InvalidImage);
            }
            let parsed = ImageDimensions::new(
                read_u32_be(data, 0).ok_or(ImageError::InvalidImage)?,
                read_u32_be(data, 4).ok_or(ImageError::InvalidImage)?,
            )?;
            if !valid_png_header(data) {
                return Err(ImageError::InvalidImage);
            }
            dimensions = Some(parsed);
        } else if kind == b"IHDR" {
            return Err(ImageError::InvalidImage);
        }

        if kind == b"IDAT" {
            if dimensions.is_none() || idat_ended || length == 0 {
                return Err(ImageError::InvalidImage);
            }
            saw_idat = true;
        } else if saw_idat && kind != b"IEND" {
            // PNG requires all IDAT chunks to be consecutive.
            idat_ended = true;
        }
        if kind == b"IEND" {
            if length != 0 || !saw_idat || end != bytes.len() {
                return Err(ImageError::InvalidImage);
            }
            return dimensions.ok_or(ImageError::InvalidImage);
        }
        offset = end;
    }
}

pub(super) fn valid_png_header(data: &[u8]) -> bool {
    if data.len() != 13 || data[10] != 0 || data[11] != 0 || data[12] > 1 {
        return false;
    }
    matches!(
        (data[8], data[9]),
        (1 | 2 | 4 | 8 | 16, 0) | (8 | 16, 2 | 4 | 6) | (1 | 2 | 4 | 8, 3)
    )
}

pub(super) fn parse_jpeg(
    bytes: &[u8],
    limits: &ImageLimits,
) -> Result<ImageDimensions, ImageError> {
    if bytes.len() < 12 || !bytes.starts_with(&[0xff, 0xd8]) || !bytes.ends_with(&[0xff, 0xd9]) {
        return Err(ImageError::InvalidImage);
    }
    let mut offset = 2;
    let mut items = 0;
    let header_limit = bytes.len().min(limits.max_header_bytes);

    while offset < bytes.len().saturating_sub(2) {
        if offset >= header_limit || bytes.get(offset) != Some(&0xff) {
            return Err(ImageError::MetadataTooLarge);
        }
        while bytes.get(offset) == Some(&0xff) {
            offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
            if offset >= header_limit {
                return Err(ImageError::MetadataTooLarge);
            }
        }
        let marker = *bytes.get(offset).ok_or(ImageError::InvalidImage)?;
        offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
        if marker == 0 || marker == 0xff || marker == 0xd8 || marker == 0xd9 {
            return Err(ImageError::InvalidImage);
        }
        take_container_item(&mut items, limits)?;
        if matches!(marker, 0x01 | 0xd0..=0xd7) {
            continue;
        }
        let segment_length =
            usize::from(read_u16_be(bytes, offset).ok_or(ImageError::InvalidImage)?);
        if segment_length < 2 {
            return Err(ImageError::InvalidImage);
        }
        let data_start = offset.checked_add(2).ok_or(ImageError::InvalidImage)?;
        let end = offset
            .checked_add(segment_length)
            .ok_or(ImageError::InvalidImage)?;
        if end > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
        if end > header_limit {
            return Err(ImageError::MetadataTooLarge);
        }
        let data = &bytes[data_start..end];
        if is_jpeg_sof(marker) {
            if data.len() < 6 || data[0] == 0 {
                return Err(ImageError::InvalidImage);
            }
            let components = usize::from(data[5]);
            let minimum = 6usize
                .checked_add(components.checked_mul(3).ok_or(ImageError::InvalidImage)?)
                .ok_or(ImageError::InvalidImage)?;
            if components == 0 || data.len() < minimum {
                return Err(ImageError::InvalidImage);
            }
            return ImageDimensions::new(
                u32::from(read_u16_be(data, 3).ok_or(ImageError::InvalidImage)?),
                u32::from(read_u16_be(data, 1).ok_or(ImageError::InvalidImage)?),
            )
            .and_then(|dimensions| {
                // A start-of-frame alone is insufficient. JPEG must carry a
                // bounded start-of-scan marker before the exact final EOI.
                jpeg_has_bounded_sos(bytes, end, header_limit, limits, &mut items)
                    .map(|()| dimensions)
            });
        }
        offset = end;
    }
    Err(ImageError::InvalidImage)
}

pub(super) fn jpeg_has_bounded_sos(
    bytes: &[u8],
    mut offset: usize,
    header_limit: usize,
    limits: &ImageLimits,
    items: &mut usize,
) -> Result<(), ImageError> {
    while offset < bytes.len().saturating_sub(2) {
        if offset >= header_limit || bytes.get(offset) != Some(&0xff) {
            return Err(ImageError::MetadataTooLarge);
        }
        while bytes.get(offset) == Some(&0xff) {
            offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
            if offset >= header_limit {
                return Err(ImageError::MetadataTooLarge);
            }
        }
        let marker = *bytes.get(offset).ok_or(ImageError::InvalidImage)?;
        offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
        if marker == 0 || marker == 0xff || marker == 0xd8 || marker == 0xd9 {
            return Err(ImageError::InvalidImage);
        }
        take_container_item(items, limits)?;
        if matches!(marker, 0x01 | 0xd0..=0xd7) {
            continue;
        }
        let segment_length =
            usize::from(read_u16_be(bytes, offset).ok_or(ImageError::InvalidImage)?);
        if segment_length < 2 {
            return Err(ImageError::InvalidImage);
        }
        let data_start = offset.checked_add(2).ok_or(ImageError::InvalidImage)?;
        let end = offset
            .checked_add(segment_length)
            .ok_or(ImageError::InvalidImage)?;
        if end > bytes.len() || end > header_limit {
            return Err(ImageError::MetadataTooLarge);
        }
        if marker == 0xda {
            let data = &bytes[data_start..end];
            if data.is_empty() || end >= bytes.len().saturating_sub(2) {
                return Err(ImageError::InvalidImage);
            }
            let components = usize::from(data[0]);
            let minimum = 4usize
                .checked_add(components.checked_mul(2).ok_or(ImageError::InvalidImage)?)
                .ok_or(ImageError::InvalidImage)?;
            return (components > 0 && data.len() >= minimum)
                .then_some(())
                .ok_or(ImageError::InvalidImage);
        }
        offset = end;
    }
    Err(ImageError::InvalidImage)
}

pub(super) fn is_jpeg_sof(marker: u8) -> bool {
    matches!(
        marker,
        0xc0 | 0xc1 | 0xc2 | 0xc3 | 0xc5 | 0xc6 | 0xc7 | 0xc9 | 0xca | 0xcb | 0xcd | 0xce | 0xcf
    )
}

pub(super) fn parse_gif(bytes: &[u8], limits: &ImageLimits) -> Result<ImageDimensions, ImageError> {
    if bytes.len() < 15 || !(bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Err(ImageError::InvalidImage);
    }
    let dimensions = ImageDimensions::new(
        u32::from(read_u16_le(bytes, 6).ok_or(ImageError::InvalidImage)?),
        u32::from(read_u16_le(bytes, 8).ok_or(ImageError::InvalidImage)?),
    )?;
    let mut offset = 13usize;
    let packed = bytes[10];
    if packed & 0x80 != 0 {
        let entries = 1usize << (usize::from(packed & 0x07) + 1);
        let table_bytes = entries.checked_mul(3).ok_or(ImageError::InvalidImage)?;
        offset = offset
            .checked_add(table_bytes)
            .ok_or(ImageError::InvalidImage)?;
        if offset > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
    }

    let mut items = 0;
    let mut saw_image = false;
    loop {
        take_container_item(&mut items, limits)?;
        let marker = *bytes.get(offset).ok_or(ImageError::InvalidImage)?;
        offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
        match marker {
            0x3b if saw_image && offset == bytes.len() => return Ok(dimensions),
            0x3b => return Err(ImageError::InvalidImage),
            0x21 => {
                let label = *bytes.get(offset).ok_or(ImageError::InvalidImage)?;
                offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
                // A plain-text extension is a GIF graphic-rendering block, so
                // allowing it alongside an image descriptor would admit a
                // multi-frame container without a second `0x2c` marker.
                if label == 0x01 {
                    return Err(ImageError::UnsupportedAnimation);
                }
                if label != 0xfe {
                    let fixed_len =
                        usize::from(*bytes.get(offset).ok_or(ImageError::InvalidImage)?);
                    offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
                    if (matches!(label, 0xf9) && fixed_len != 4)
                        || (matches!(label, 0xff) && fixed_len != 11)
                        || (matches!(label, 0x01) && fixed_len != 12)
                    {
                        return Err(ImageError::InvalidImage);
                    }
                    let fixed_start = offset;
                    offset = offset
                        .checked_add(fixed_len)
                        .ok_or(ImageError::InvalidImage)?;
                    if offset > bytes.len() {
                        return Err(ImageError::InvalidImage);
                    }
                    if label == 0xff {
                        let application = &bytes[fixed_start..offset];
                        // These registered application identifiers carry the
                        // Netscape/ANIMEXTS loop instructions. Reject them even
                        // when a malformed producer includes only one frame;
                        // accepting a loop marker would make the terminal
                        // decide whether to animate an otherwise bounded input.
                        if application.starts_with(b"NETSCAPE")
                            || application.starts_with(b"ANIMEXTS")
                        {
                            return Err(ImageError::UnsupportedAnimation);
                        }
                    }
                }
                skip_gif_subblocks(bytes, &mut offset, &mut items, limits)?;
            }
            0x2c => {
                if saw_image {
                    return Err(ImageError::UnsupportedAnimation);
                }
                let descriptor_start = offset;
                let descriptor_end = offset.checked_add(9).ok_or(ImageError::InvalidImage)?;
                if descriptor_end > bytes.len() {
                    return Err(ImageError::InvalidImage);
                }
                let left = u32::from(
                    read_u16_le(bytes, descriptor_start).ok_or(ImageError::InvalidImage)?,
                );
                let top = u32::from(
                    read_u16_le(bytes, descriptor_start + 2).ok_or(ImageError::InvalidImage)?,
                );
                let width = u32::from(
                    read_u16_le(bytes, descriptor_start + 4).ok_or(ImageError::InvalidImage)?,
                );
                let height = u32::from(
                    read_u16_le(bytes, descriptor_start + 6).ok_or(ImageError::InvalidImage)?,
                );
                if width == 0
                    || height == 0
                    || left
                        .checked_add(width)
                        .is_none_or(|right| right > dimensions.width)
                    || top
                        .checked_add(height)
                        .is_none_or(|bottom| bottom > dimensions.height)
                {
                    return Err(ImageError::InvalidImage);
                }
                let image_packed = bytes[descriptor_start + 8];
                offset = descriptor_end;
                if image_packed & 0x80 != 0 {
                    let entries = 1usize << (usize::from(image_packed & 0x07) + 1);
                    let table_bytes = entries.checked_mul(3).ok_or(ImageError::InvalidImage)?;
                    offset = offset
                        .checked_add(table_bytes)
                        .ok_or(ImageError::InvalidImage)?;
                    if offset > bytes.len() {
                        return Err(ImageError::InvalidImage);
                    }
                }
                let lzw_minimum = *bytes.get(offset).ok_or(ImageError::InvalidImage)?;
                if !(2..=8).contains(&lzw_minimum) {
                    return Err(ImageError::InvalidImage);
                }
                offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
                skip_gif_subblocks(bytes, &mut offset, &mut items, limits)?;
                saw_image = true;
            }
            _ => return Err(ImageError::InvalidImage),
        }
    }
}

pub(super) fn skip_gif_subblocks(
    bytes: &[u8],
    offset: &mut usize,
    items: &mut usize,
    limits: &ImageLimits,
) -> Result<(), ImageError> {
    loop {
        let length = usize::from(*bytes.get(*offset).ok_or(ImageError::InvalidImage)?);
        *offset = offset.checked_add(1).ok_or(ImageError::InvalidImage)?;
        if length == 0 {
            return Ok(());
        }
        take_container_item(items, limits)?;
        *offset = offset.checked_add(length).ok_or(ImageError::InvalidImage)?;
        if *offset > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
    }
}

pub(super) fn parse_webp(
    bytes: &[u8],
    limits: &ImageLimits,
) -> Result<ImageDimensions, ImageError> {
    if bytes.len() < 20 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WEBP" {
        return Err(ImageError::InvalidImage);
    }
    let declared = usize::try_from(read_u32_le(bytes, 4).ok_or(ImageError::InvalidImage)?)
        .map_err(|_| ImageError::InvalidImage)?;
    if declared
        .checked_add(8)
        .is_none_or(|expected| expected != bytes.len())
    {
        return Err(ImageError::InvalidImage);
    }

    let mut offset = 12usize;
    let mut items = 0;
    let mut dimensions = None;
    let mut saw_extended_header = false;
    let mut saw_payload = false;
    while offset < bytes.len() {
        take_container_item(&mut items, limits)?;
        let header_end = offset.checked_add(8).ok_or(ImageError::InvalidImage)?;
        if header_end > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
        let kind = &bytes[offset..offset + 4];
        let length =
            usize::try_from(read_u32_le(bytes, offset + 4).ok_or(ImageError::InvalidImage)?)
                .map_err(|_| ImageError::InvalidImage)?;
        let data_start = header_end;
        let data_end = data_start
            .checked_add(length)
            .ok_or(ImageError::InvalidImage)?;
        if data_end > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
        let data = &bytes[data_start..data_end];
        if kind == b"VP8X" {
            if saw_extended_header || saw_payload || data.len() != 10 || data[1..4] != [0, 0, 0] {
                return Err(ImageError::InvalidImage);
            }
            saw_extended_header = true;
            if data[0] & 0x02 != 0 {
                return Err(ImageError::UnsupportedAnimation);
            }
            let width = u32::from(data[4]) | (u32::from(data[5]) << 8) | (u32::from(data[6]) << 16);
            let height =
                u32::from(data[7]) | (u32::from(data[8]) << 8) | (u32::from(data[9]) << 16);
            set_webp_dimensions(
                &mut dimensions,
                ImageDimensions::new(width.saturating_add(1), height.saturating_add(1))?,
            )?;
        } else if kind == b"VP8 " {
            if saw_payload || data.len() < 11 || data[3..6] != [0x9d, 0x01, 0x2a] {
                return Err(ImageError::InvalidImage);
            }
            let width = u32::from(read_u16_le(data, 6).ok_or(ImageError::InvalidImage)? & 0x3fff);
            let height = u32::from(read_u16_le(data, 8).ok_or(ImageError::InvalidImage)? & 0x3fff);
            set_webp_dimensions(&mut dimensions, ImageDimensions::new(width, height)?)?;
            saw_payload = true;
        } else if kind == b"VP8L" {
            if saw_payload || data.len() < 6 || data[0] != 0x2f {
                return Err(ImageError::InvalidImage);
            }
            let packed = read_u32_le(data, 1).ok_or(ImageError::InvalidImage)?;
            let width = 1 + (packed & 0x3fff);
            let height = 1 + ((packed >> 14) & 0x3fff);
            set_webp_dimensions(&mut dimensions, ImageDimensions::new(width, height)?)?;
            saw_payload = true;
        } else if kind == b"ANIM" || kind == b"ANMF" {
            return Err(ImageError::UnsupportedAnimation);
        }
        offset = data_end
            .checked_add(length & 1)
            .ok_or(ImageError::InvalidImage)?;
        if offset > bytes.len() {
            return Err(ImageError::InvalidImage);
        }
    }
    if offset != bytes.len() || !saw_payload {
        return Err(ImageError::InvalidImage);
    }
    dimensions.ok_or(ImageError::InvalidImage)
}

pub(super) fn set_webp_dimensions(
    target: &mut Option<ImageDimensions>,
    dimensions: ImageDimensions,
) -> Result<(), ImageError> {
    match target {
        Some(current) if *current != dimensions => Err(ImageError::InvalidImage),
        Some(_) => Ok(()),
        None => {
            *target = Some(dimensions);
            Ok(())
        }
    }
}
