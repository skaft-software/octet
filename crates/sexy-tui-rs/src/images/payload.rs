//! The validated payload: bytes the rest of the subsystem is allowed to see.
//!
//! A [`TerminalImage`] is the only type in this crate that holds image bytes, and
//! it holds them privately. There is no path or URL constructor and no accessor
//! that returns a borrow without bounds, so "an image got into the renderer" and
//! "the renderer can only see bytes that were already validated and bounded" are
//! the same statement. The copy into a bounded allocation happens *before* the
//! caller's slice is adopted, and the capacity check rejects a source that would
//! have made the allocation larger than the limit allows.
//!
//! It is separate from [`super::format`] because a format is a claim about a
//! payload while a payload is a fact, and from [`super::inspect`] because the
//! constructor is where a fact is established.

use std::fmt;

use super::error::ImageError;
use super::format::{ImageDimensions, ImageFormat, ImageMetadata};
use super::inspect::inspect_image;
use super::limits::ImageLimits;

/// A validated, owned terminal image whose payload is intentionally private.
///
/// The type has no path or URL constructor. Validation occurs before any copy
/// from a borrowed slice, and its `Debug` representation includes only safe
/// format, dimensions, and byte-count summaries.
pub struct TerminalImage {
    pub(super) bytes: Vec<u8>,
    pub(super) format: ImageFormat,
    pub(super) dimensions: ImageDimensions,
    pub(super) metadata: ImageMetadata,
}

impl fmt::Debug for TerminalImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalImage")
            .field("format", &self.format)
            .field("dimensions", &self.dimensions)
            .field("byte_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl TerminalImage {
    /// Validate owned bytes with the default bounded limits.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, ImageError> {
        Self::from_bytes_with_metadata(bytes, ImageMetadata::default(), &ImageLimits::default())
    }

    /// Validate owned bytes and metadata under explicit limits.
    ///
    /// A source `Vec` with excess capacity is copied into one bounded allocation
    /// before retention, so caller-side spare capacity cannot bypass the payload
    /// allocation cap.
    pub fn from_bytes_with_metadata(
        bytes: Vec<u8>,
        metadata: ImageMetadata,
        limits: &ImageLimits,
    ) -> Result<Self, ImageError> {
        let (format, dimensions) = inspect_image(&bytes, &metadata, limits)?;
        let bytes = if bytes.capacity() > limits.max_payload_bytes {
            copy_image_bytes(&bytes, limits.max_payload_bytes)?
        } else {
            bytes
        };
        Ok(Self {
            bytes,
            format,
            dimensions,
            metadata,
        })
    }

    /// Validate a borrowed source before making one bounded owned copy.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, ImageError> {
        Self::from_slice_with_metadata(bytes, ImageMetadata::default(), &ImageLimits::default())
    }

    /// Validate a borrowed source and metadata before making one bounded copy.
    pub fn from_slice_with_metadata(
        bytes: &[u8],
        metadata: ImageMetadata,
        limits: &ImageLimits,
    ) -> Result<Self, ImageError> {
        let (format, dimensions) = inspect_image(bytes, &metadata, limits)?;
        Ok(Self {
            bytes: copy_image_bytes(bytes, limits.max_payload_bytes)?,
            format,
            dimensions,
            metadata,
        })
    }

    /// Validated source format.
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Validated source dimensions.
    pub const fn dimensions(&self) -> ImageDimensions {
        self.dimensions
    }

    /// Number of owned source bytes.
    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    /// Validated metadata. Raw payload bytes remain private.
    pub const fn metadata(&self) -> &ImageMetadata {
        &self.metadata
    }

    /// Stream the validated, bounded source to a native terminal transport.
    ///
    /// Like `ImageTerminalCommand::write_to`, this is an output-boundary API:
    /// never place the payload in semantic rows, diagnostic output or logs.
    /// No path is opened and no payload-sized copy is made by this method.
    pub fn write_payload_to(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&self.bytes)
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn copy_image_bytes(bytes: &[u8], max_capacity: usize) -> Result<Vec<u8>, ImageError> {
    let mut owned = Vec::new();
    owned
        .try_reserve_exact(bytes.len())
        .map_err(|_| ImageError::AllocationFailed)?;
    if owned.capacity() > max_capacity {
        return Err(ImageError::AllocationFailed);
    }
    owned.extend_from_slice(bytes);
    Ok(owned)
}
