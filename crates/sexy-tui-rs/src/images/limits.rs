//! Every bound the image subsystem enforces, and the policy object that carries
//! the adjustable ones.
//!
//! Two tiers of bound live here and the distinction is the point of the module.
//! The `HARD_MAX_*` constants are non-bypassable ceilings: no builder, no
//! override and no caller can raise them, so an image attachment can never grow
//! past what a terminal can be asked to absorb. The `DEFAULT_*` constants are
//! the starting values an [`ImageLimits`] takes, chosen to favour small
//! in-memory terminal attachments.
//!
//! They are together in one file because a bound that exists in only one of the
//! two tiers is a bug: a default above its hard ceiling, or a hard ceiling with
//! no default. Splitting them would make that mismatch invisible.

use std::time::Duration;

use super::error::ImageError;

/// Hard ceiling for an accepted encoded image payload.
pub const HARD_MAX_IMAGE_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;
/// Hard ceiling for all bytes emitted by one image protocol command.
pub const HARD_MAX_ENCODED_OUTPUT_BYTES: usize = 24 * 1024 * 1024;
/// Hard ceiling for one base64 protocol chunk.
pub const HARD_MAX_PROTOCOL_CHUNK_BYTES: usize = 4 * 1024;
/// Hard ceiling for chunks emitted by one Kitty transmission.
pub const HARD_MAX_PROTOCOL_CHUNKS: usize = 8_192;
/// Hard ceiling for a validated image width or height.
pub const HARD_MAX_IMAGE_DIMENSION: u32 = 16_384;
/// Hard ceiling for width times height before any decoder is involved.
///
/// At most eight decoded bytes per pixel are conservatively assumed for image
/// containers that can carry 16-bit RGBA samples, keeping even a hostile
/// terminal-side decompressor below a bounded working-set estimate.
pub const HARD_MAX_IMAGE_PIXELS: u64 = 16_000_000;
/// Hard ceiling for container records or sub-blocks inspected during validation.
pub const HARD_MAX_CONTAINER_ITEMS: usize = 8_192;
/// Hard ceiling for JPEG headers inspected before the scan payload.
pub const HARD_MAX_HEADER_BYTES: usize = 64 * 1024;
/// Hard ceiling for a metadata filename.
pub const HARD_MAX_FILENAME_BYTES: usize = 128;
/// Hard ceiling for one terminal capability reply.
pub const HARD_MAX_TERMINAL_REPLY_BYTES: usize = 1_024;
/// Hard ceiling a caller may use as a terminal-query deadline.
pub const HARD_MAX_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
/// Largest number of semantic rows an image reservation may create.
pub const MAX_RESERVED_IMAGE_ROWS: u16 = 512;
/// Largest number of cells requested in one image placement direction.
pub const MAX_IMAGE_CELL_COLUMNS: u16 = 512;
/// Hard ceiling for concurrently live targetable image IDs.
///
/// This bounds the registry's bookkeeping allocation independently of the
/// payload limit. Retiring an ID releases one live slot but never makes its ID
/// value reusable.
pub const HARD_MAX_LIVE_IMAGES: usize = 4_096;

const DEFAULT_MAX_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_ENCODED_OUTPUT_BYTES: usize = 6 * 1024 * 1024;
const DEFAULT_MAX_PROTOCOL_CHUNKS: usize = 2_048;
const DEFAULT_MAX_IMAGE_DIMENSION: u32 = 8_192;
const DEFAULT_MAX_IMAGE_PIXELS: u64 = 4_000_000;
const DEFAULT_MAX_CONTAINER_ITEMS: usize = 2_048;
const DEFAULT_MAX_HEADER_BYTES: usize = 32 * 1024;
const DEFAULT_MAX_FILENAME_BYTES: usize = 96;
const DEFAULT_MAX_TERMINAL_REPLY_BYTES: usize = 512;
pub(super) const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_millis(75);

pub(super) const KITTY_ST: &[u8] = b"\x1b\\";
pub(super) const ITERM_ST: &[u8] = b"\x1b\\";

/// All adjustable image bounds.
///
/// Every builder rejects values outside non-bypassable hard ceilings. Defaults
/// favor small in-memory terminal attachments; callers may lower limits for a
/// tighter boundary but cannot raise them past the documented hard caps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub(super) max_payload_bytes: usize,
    pub(super) max_encoded_output_bytes: usize,
    pub(super) max_protocol_chunk_bytes: usize,
    pub(super) max_protocol_chunks: usize,
    pub(super) max_width: u32,
    pub(super) max_height: u32,
    pub(super) max_pixels: u64,
    pub(super) max_container_items: usize,
    pub(super) max_header_bytes: usize,
    pub(super) max_filename_bytes: usize,
    pub(super) max_terminal_reply_bytes: usize,
    pub(super) query_timeout: Duration,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_payload_bytes: DEFAULT_MAX_PAYLOAD_BYTES,
            max_encoded_output_bytes: DEFAULT_MAX_ENCODED_OUTPUT_BYTES,
            max_protocol_chunk_bytes: HARD_MAX_PROTOCOL_CHUNK_BYTES,
            max_protocol_chunks: DEFAULT_MAX_PROTOCOL_CHUNKS,
            max_width: DEFAULT_MAX_IMAGE_DIMENSION,
            max_height: DEFAULT_MAX_IMAGE_DIMENSION,
            max_pixels: DEFAULT_MAX_IMAGE_PIXELS,
            max_container_items: DEFAULT_MAX_CONTAINER_ITEMS,
            max_header_bytes: DEFAULT_MAX_HEADER_BYTES,
            max_filename_bytes: DEFAULT_MAX_FILENAME_BYTES,
            max_terminal_reply_bytes: DEFAULT_MAX_TERMINAL_REPLY_BYTES,
            query_timeout: DEFAULT_QUERY_TIMEOUT,
        }
    }
}

impl ImageLimits {
    /// Maximum accepted source bytes.
    pub const fn max_payload_bytes(&self) -> usize {
        self.max_payload_bytes
    }

    /// Maximum complete encoded protocol output bytes.
    pub const fn max_encoded_output_bytes(&self) -> usize {
        self.max_encoded_output_bytes
    }

    /// Maximum base64 bytes in each bounded emission buffer.
    pub const fn max_protocol_chunk_bytes(&self) -> usize {
        self.max_protocol_chunk_bytes
    }

    /// Maximum Kitty chunks in one transmission.
    pub const fn max_protocol_chunks(&self) -> usize {
        self.max_protocol_chunks
    }

    /// Maximum accepted source width.
    pub const fn max_width(&self) -> u32 {
        self.max_width
    }

    /// Maximum accepted source height.
    pub const fn max_height(&self) -> u32 {
        self.max_height
    }

    /// Maximum accepted source pixel count.
    pub const fn max_pixels(&self) -> u64 {
        self.max_pixels
    }

    /// Maximum container records or sub-blocks examined by the validators.
    pub const fn max_container_items(&self) -> usize {
        self.max_container_items
    }

    /// Maximum JPEG header bytes examined before scan data.
    pub const fn max_header_bytes(&self) -> usize {
        self.max_header_bytes
    }

    /// Maximum filename bytes accepted after filename validation.
    pub const fn max_filename_bytes(&self) -> usize {
        self.max_filename_bytes
    }

    /// Maximum terminal reply bytes accepted by the strict reply parser.
    pub const fn max_terminal_reply_bytes(&self) -> usize {
        self.max_terminal_reply_bytes
    }

    /// Caller-owned deadline recommended for one nonblocking query attempt.
    pub const fn query_timeout(&self) -> Duration {
        self.query_timeout
    }

    /// Set a source payload bound.
    pub fn with_max_payload_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_IMAGE_PAYLOAD_BYTES {
            return Err(ImageError::InvalidLimit);
        }
        self.max_payload_bytes = value;
        Ok(self)
    }

    /// Set a complete protocol output bound.
    pub fn with_max_encoded_output_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_ENCODED_OUTPUT_BYTES {
            return Err(ImageError::InvalidLimit);
        }
        self.max_encoded_output_bytes = value;
        Ok(self)
    }

    /// Set an emission chunk bound. It must be a nonzero base64 quartet count.
    pub fn with_max_protocol_chunk_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if !(4..=HARD_MAX_PROTOCOL_CHUNK_BYTES).contains(&value) || value % 4 != 0 {
            return Err(ImageError::InvalidLimit);
        }
        self.max_protocol_chunk_bytes = value;
        Ok(self)
    }

    /// Set a Kitty chunk-count bound.
    pub fn with_max_protocol_chunks(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_PROTOCOL_CHUNKS {
            return Err(ImageError::InvalidLimit);
        }
        self.max_protocol_chunks = value;
        Ok(self)
    }

    /// Set width and height bounds together.
    pub fn with_max_dimensions(mut self, width: u32, height: u32) -> Result<Self, ImageError> {
        if width == 0
            || height == 0
            || width > HARD_MAX_IMAGE_DIMENSION
            || height > HARD_MAX_IMAGE_DIMENSION
        {
            return Err(ImageError::InvalidLimit);
        }
        self.max_width = width;
        self.max_height = height;
        Ok(self)
    }

    /// Set a pixel-count bound.
    pub fn with_max_pixels(mut self, value: u64) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_IMAGE_PIXELS {
            return Err(ImageError::InvalidLimit);
        }
        self.max_pixels = value;
        Ok(self)
    }

    /// Set a container item bound.
    pub fn with_max_container_items(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_CONTAINER_ITEMS {
            return Err(ImageError::InvalidLimit);
        }
        self.max_container_items = value;
        Ok(self)
    }

    /// Set the bounded JPEG header scan length.
    pub fn with_max_header_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_HEADER_BYTES {
            return Err(ImageError::InvalidLimit);
        }
        self.max_header_bytes = value;
        Ok(self)
    }

    /// Set a smaller accepted filename length.
    pub fn with_max_filename_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_FILENAME_BYTES {
            return Err(ImageError::InvalidLimit);
        }
        self.max_filename_bytes = value;
        Ok(self)
    }

    /// Set the terminal reply parser bound.
    pub fn with_max_terminal_reply_bytes(mut self, value: usize) -> Result<Self, ImageError> {
        if value == 0 || value > HARD_MAX_TERMINAL_REPLY_BYTES {
            return Err(ImageError::InvalidLimit);
        }
        self.max_terminal_reply_bytes = value;
        Ok(self)
    }

    /// Set the caller-owned terminal query deadline.
    pub fn with_query_timeout(mut self, value: Duration) -> Result<Self, ImageError> {
        if value.is_zero() || value > HARD_MAX_QUERY_TIMEOUT {
            return Err(ImageError::InvalidLimit);
        }
        self.query_timeout = value;
        Ok(self)
    }
}
