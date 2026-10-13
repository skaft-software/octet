//! The single error type for every image boundary.
//!
//! Bounded image validation, layout derivation, capability negotiation and
//! protocol planning all fail through this one type, and its variants
//! deliberately carry no payload: no hostile bytes, no filenames, no raw
//! protocol replies. That is a security property, not a brevity one — an image
//! filename and a base64 chunk are both attacker-controlled, so a variant that
//! embedded either would turn `Display` into an escape-injection path the
//! moment a caller logged the error.
//!
//! It is its own module so that the "no payload" rule has exactly one place to
//! be broken, and so a future variant has to be written next to that warning.

use std::fmt;

/// Errors from bounded image validation, layout, or protocol planning.
///
/// Variants deliberately omit hostile payloads, filenames, replies, and raw
/// protocol bytes so logging an error cannot become an escape-injection path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// A caller attempted to configure a value outside a fixed hard bound.
    InvalidLimit,
    /// Input bytes exceeded the configured payload cap.
    PayloadTooLarge,
    /// A protocol command would exceed the configured output cap.
    EncodedOutputTooLarge,
    /// A protocol transmission would require too many chunks.
    TooManyChunks,
    /// The input did not form a minimally valid, bounded image container.
    InvalidImage,
    /// The input did not begin with PNG, JPEG, GIF, or WebP container bytes.
    UnsupportedFormat,
    /// The container declares more than one animation frame.
    UnsupportedAnimation,
    /// A dimension was zero or cannot be represented by the requested layout.
    InvalidDimensions,
    /// A dimension exceeded the configured width or height cap.
    DimensionsTooLarge,
    /// Width times height exceeded the configured pixel cap.
    PixelCountTooLarge,
    /// Container records or header bytes exceeded a configured parsing cap.
    MetadataTooLarge,
    /// A filename was empty, too long, path-like, or contained unsafe bytes.
    UnsafeFilename,
    /// Caller-provided dimensions did not match the validated container header.
    MetadataDimensionMismatch,
    /// A requested cell layout was zero or exceeded the semantic-row cap.
    InvalidLayout,
    /// A bounded copy could not reserve its destination allocation.
    AllocationFailed,
    /// Image ID zero is not valid for a targetable Kitty operation.
    InvalidImageId,
    /// The monotonic ID allocator has no non-reused values left.
    ImageIdExhausted,
    /// Registry bookkeeping reached its hard concurrent-live-ID cap.
    TooManyLiveImages,
    /// Replace or delete referenced an ID that is not currently live.
    StaleImageId,
    /// The selected protocol does not accept the source image format directly.
    UnsupportedFormatForProtocol,
    /// The selected protocol has no safe implementation for this operation.
    UnsupportedOperation,
    /// An operation was supplied to an incompatible planner or encoder method.
    InvalidAction,
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidLimit => "invalid terminal image limit",
            Self::PayloadTooLarge => "terminal image payload exceeds its bound",
            Self::EncodedOutputTooLarge => "terminal image protocol output exceeds its bound",
            Self::TooManyChunks => "terminal image transmission has too many chunks",
            Self::InvalidImage => "invalid or truncated terminal image container",
            Self::UnsupportedFormat => "unsupported terminal image format",
            Self::UnsupportedAnimation => "animated terminal images are unsupported",
            Self::InvalidDimensions => "invalid terminal image dimensions",
            Self::DimensionsTooLarge => "terminal image dimensions exceed their bound",
            Self::PixelCountTooLarge => "terminal image pixel count exceeds its bound",
            Self::MetadataTooLarge => "terminal image metadata exceeds its bound",
            Self::UnsafeFilename => "unsafe terminal image filename",
            Self::MetadataDimensionMismatch => "terminal image metadata dimensions do not match",
            Self::InvalidLayout => "invalid terminal image cell layout",
            Self::AllocationFailed => "bounded terminal image allocation failed",
            Self::InvalidImageId => "invalid terminal image ID",
            Self::ImageIdExhausted => "terminal image IDs are exhausted",
            Self::TooManyLiveImages => "too many live terminal images",
            Self::StaleImageId => "stale terminal image ID",
            Self::UnsupportedFormatForProtocol => {
                "terminal image format is unsupported by the selected protocol"
            }
            Self::UnsupportedOperation => {
                "terminal image operation is unsupported by the selected protocol"
            }
            Self::InvalidAction => "invalid terminal image action",
        };
        f.write_str(message)
    }
}

impl std::error::Error for ImageError {}
