//! Byte-level framing: what actually goes on the wire.
//!
//! This module turns a validated payload into the exact bytes of one protocol
//! command — a Kitty APC header plus bounded base64 PNG chunks, or an iTerm2
//! OSC 1337 inline file — and it is the only place base64 is produced. Every
//! length is computed with checked arithmetic and compared against the encoded
//! output cap before a single byte is written, so a payload that would produce
//! an oversized command fails without emitting a partial one.
//!
//! It is separate from [`super::plan`] and [`super::command`] because those
//! decide and describe; this one serialises. Keeping the encoder description
//! apart from the encoder is what makes it reviewable that no escape sequence is
//! assembled anywhere else.

use super::capabilities::ImageProtocol;
use super::command::ImageTransmission;
use super::error::ImageError;
use super::helpers::{base64_len, ceil_div_usize, checked_add, checked_mul};
use super::layout::ImageLayout;
use super::limits::{ImageLimits, HARD_MAX_PROTOCOL_CHUNK_BYTES};
use super::limits::{ITERM_ST, KITTY_ST};
use super::payload::TerminalImage;
use super::registry::ImageId;

pub(super) fn build_transmission<'a>(
    protocol: ImageProtocol,
    id: ImageId,
    image: &'a TerminalImage,
    layout: ImageLayout,
    limits: &ImageLimits,
) -> Result<(ImageTransmission<'a>, usize, usize), ImageError> {
    let source_len = image.bytes().len();
    let chunk_bytes = limits.max_protocol_chunk_bytes;
    let raw_chunk_bytes = chunk_bytes
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .filter(|value| *value > 0)
        .ok_or(ImageError::InvalidLimit)?;
    let chunks = ceil_div_usize(source_len, raw_chunk_bytes);
    if chunks == 0 || chunks > limits.max_protocol_chunks {
        return Err(ImageError::TooManyChunks);
    }
    let base64_len = base64_len(source_len)?;
    let (first_header, continuation_more, continuation_last, output_len) = match protocol {
        ImageProtocol::Kitty => {
            let first = kitty_transmit_header(id, layout, chunks > 1);
            let more = "\x1b_Gm=1;".to_owned();
            let last = "\x1b_Gm=0;".to_owned();
            let mut total = checked_add(first.len(), base64_len)?;
            total = checked_add(total, KITTY_ST.len())?;
            if chunks > 1 {
                let middle_count = chunks.saturating_sub(2);
                let middle = checked_mul(middle_count, checked_add(more.len(), KITTY_ST.len())?)?;
                total = checked_add(total, middle)?;
                total = checked_add(total, checked_add(last.len(), KITTY_ST.len())?)?;
            }
            (first, more, last, total)
        }
        ImageProtocol::Iterm2 => {
            let first = iterm_transmit_header(image, layout)?;
            let total = checked_add(checked_add(first.len(), base64_len)?, ITERM_ST.len())?;
            (first, String::new(), String::new(), total)
        }
    };
    if output_len > limits.max_encoded_output_bytes {
        return Err(ImageError::EncodedOutputTooLarge);
    }
    Ok((
        ImageTransmission {
            image,
            protocol,
            chunk_bytes,
            chunks,
            first_header,
            continuation_more,
            continuation_last,
        },
        output_len,
        chunks,
    ))
}

pub(super) fn emit_transmission<E, F>(
    transmission: &ImageTransmission<'_>,
    write: &mut F,
) -> Result<(), E>
where
    F: FnMut(&[u8]) -> Result<(), E>,
{
    let raw_chunk_bytes = transmission.chunk_bytes / 4 * 3;
    let mut buffer = [0_u8; HARD_MAX_PROTOCOL_CHUNK_BYTES];
    match transmission.protocol {
        ImageProtocol::Kitty => {
            for chunk in 0..transmission.chunks {
                if chunk == 0 {
                    write(transmission.first_header.as_bytes())?;
                } else if chunk + 1 == transmission.chunks {
                    write(transmission.continuation_last.as_bytes())?;
                } else {
                    write(transmission.continuation_more.as_bytes())?;
                }
                let start = chunk.saturating_mul(raw_chunk_bytes);
                let end = start
                    .checked_add(raw_chunk_bytes)
                    .unwrap_or(transmission.image.bytes().len())
                    .min(transmission.image.bytes().len());
                let encoded =
                    encode_base64_into(&transmission.image.bytes()[start..end], &mut buffer);
                write(&buffer[..encoded])?;
                write(KITTY_ST)?;
            }
        }
        ImageProtocol::Iterm2 => {
            write(transmission.first_header.as_bytes())?;
            for chunk in 0..transmission.chunks {
                let start = chunk.saturating_mul(raw_chunk_bytes);
                let end = start
                    .checked_add(raw_chunk_bytes)
                    .unwrap_or(transmission.image.bytes().len())
                    .min(transmission.image.bytes().len());
                let encoded =
                    encode_base64_into(&transmission.image.bytes()[start..end], &mut buffer);
                write(&buffer[..encoded])?;
            }
            write(ITERM_ST)?;
        }
    }
    Ok(())
}

pub(super) fn kitty_transmit_header(id: ImageId, layout: ImageLayout, more: bool) -> String {
    // The TUI reserves image rows itself; Kitty otherwise advances the cursor by
    // the placement rectangle after displaying the image.
    format!(
        "\x1b_Ga=T,t=d,f=100,i={},q=2,c={},r={},C=1,m={};",
        id.get(),
        layout.columns(),
        layout.rows(),
        u8::from(more),
    )
}

pub(super) fn kitty_delete_sequence(id: ImageId) -> String {
    format!("\x1b_Ga=d,d=I,i={},q=2\x1b\\", id.get())
}

pub(super) fn iterm_transmit_header(
    image: &TerminalImage,
    layout: ImageLayout,
) -> Result<String, ImageError> {
    let mut header = String::from("\x1b]1337;File=");
    if let Some(filename) = image.metadata().filename() {
        header.push_str("name=");
        header.push_str(&base64_string(filename.as_str().as_bytes())?);
        header.push(';');
    }
    // The retained semantic reservation owns cursor advancement and scrollback;
    // keep this protocol side effect from moving the cursor independently.
    header.push_str(&format!(
        "size={};inline=1;doNotMoveCursor=1;width={};height={};preserveAspectRatio=1:",
        image.byte_len(),
        layout.columns(),
        layout.rows(),
    ));
    Ok(header)
}

pub(super) fn base64_string(bytes: &[u8]) -> Result<String, ImageError> {
    let length = base64_len(bytes.len())?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(length)
        .map_err(|_| ImageError::AllocationFailed)?;
    encoded.resize(length, 0);
    let written = encode_base64_into(bytes, &mut encoded);
    debug_assert_eq!(written, length);
    String::from_utf8(encoded).map_err(|_| ImageError::InvalidImage)
}

pub(super) fn encode_base64_into(input: &[u8], output: &mut [u8]) -> usize {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut source = 0usize;
    let mut target = 0usize;
    while source + 3 <= input.len() {
        let a = input[source];
        let b = input[source + 1];
        let c = input[source + 2];
        output[target] = TABLE[usize::from(a >> 2)];
        output[target + 1] = TABLE[usize::from(((a & 0x03) << 4) | (b >> 4))];
        output[target + 2] = TABLE[usize::from(((b & 0x0f) << 2) | (c >> 6))];
        output[target + 3] = TABLE[usize::from(c & 0x3f)];
        source += 3;
        target += 4;
    }
    let remainder = input.len().saturating_sub(source);
    if remainder == 1 {
        let a = input[source];
        output[target] = TABLE[usize::from(a >> 2)];
        output[target + 1] = TABLE[usize::from((a & 0x03) << 4)];
        output[target + 2] = b'=';
        output[target + 3] = b'=';
        target += 4;
    } else if remainder == 2 {
        let a = input[source];
        let b = input[source + 1];
        output[target] = TABLE[usize::from(a >> 2)];
        output[target + 1] = TABLE[usize::from(((a & 0x03) << 4) | (b >> 4))];
        output[target + 2] = TABLE[usize::from((b & 0x0f) << 2)];
        output[target + 3] = b'=';
        target += 4;
    }
    target
}
