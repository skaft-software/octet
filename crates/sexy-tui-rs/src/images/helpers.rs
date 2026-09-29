//! Checked arithmetic and bounded big-endian / little-endian readers.
//!
//! These are the primitives that make "bounded" mean something in the rest of
//! this module tree: every size computation that could overflow a `usize` on a
//! 32-bit target, and every header field read, goes through one of these. They
//! are shared rather than duplicated per parser so that a parser cannot
//! accidentally introduce a second, weaker definition of "does this fit".

use super::error::ImageError;
use super::limits::ImageLimits;

pub(super) fn take_container_item(
    items: &mut usize,
    limits: &ImageLimits,
) -> Result<(), ImageError> {
    *items = items.checked_add(1).ok_or(ImageError::MetadataTooLarge)?;
    (*items <= limits.max_container_items)
        .then_some(())
        .ok_or(ImageError::MetadataTooLarge)
}

pub(super) fn read_u16_be(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset.checked_add(1)?)?,
    ]))
}

pub(super) fn read_u16_le(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset.checked_add(1)?)?,
    ]))
}

pub(super) fn read_u32_be(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset.checked_add(1)?)?,
        *bytes.get(offset.checked_add(2)?)?,
        *bytes.get(offset.checked_add(3)?)?,
    ]))
}

pub(super) fn read_u32_le(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *bytes.get(offset)?,
        *bytes.get(offset.checked_add(1)?)?,
        *bytes.get(offset.checked_add(2)?)?,
        *bytes.get(offset.checked_add(3)?)?,
    ]))
}

pub(super) fn base64_len(length: usize) -> Result<usize, ImageError> {
    let full = length / 3;
    let remainder = length % 3;
    let base = full
        .checked_mul(4)
        .ok_or(ImageError::EncodedOutputTooLarge)?;
    let tail = usize::from(remainder != 0)
        .checked_mul(4)
        .ok_or(ImageError::EncodedOutputTooLarge)?;
    checked_add(base, tail)
}

pub(super) fn checked_add(left: usize, right: usize) -> Result<usize, ImageError> {
    left.checked_add(right)
        .ok_or(ImageError::EncodedOutputTooLarge)
}

pub(super) fn checked_mul(left: usize, right: usize) -> Result<usize, ImageError> {
    left.checked_mul(right)
        .ok_or(ImageError::EncodedOutputTooLarge)
}

pub(super) fn ceil_div_usize(value: usize, divisor: usize) -> usize {
    value / divisor + usize::from(value % divisor != 0)
}

pub(super) fn ceil_div_u64(value: u64, divisor: u64) -> u64 {
    value / divisor + u64::from(value % divisor != 0)
}

pub(super) fn ceil_div_u128(value: u128, divisor: u128) -> u128 {
    value / divisor + u128::from(value % divisor != 0)
}
