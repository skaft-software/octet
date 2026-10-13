//! The container vocabulary: what a payload is, how big it is, and what the
//! caller says about it.
//!
//! These four types are the vocabulary every other image module speaks. They are
//! grouped because none of them can be understood alone: a [`ImageFormat`] says
//! nothing without [`ImageDimensions`], a [`ImageFilename`] is only meaningful
//! next to the limits that bound it, and [`ImageMetadata`] is the optional half
//! of a payload description. Splitting them would give every consumer four
//! single-purpose files to import.
//!
//! [`ImageFormat::detect`] recognises a leading container marker but does *not*
//! accept the container; acceptance is [`super::inspect`]'s job, under
//! [`super::limits::ImageLimits`]. Keeping recognition separate from validation is what lets a
//! caller cheaply route a payload to the fallback path without paying for a
//! bounded parse.

use super::error::ImageError;
use super::limits::HARD_MAX_FILENAME_BYTES;

/// A direct image container format accepted by the validator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// Portable Network Graphics.
    Png,
    /// Joint Photographic Experts Group image data.
    Jpeg,
    /// Graphics Interchange Format image data.
    Gif,
    /// RIFF/WebP image data.
    Webp,
}

impl ImageFormat {
    /// Detect a leading container marker without accepting the container as
    /// valid. Use [`super::payload::TerminalImage::from_bytes`] for full bounded validation.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(&[0xff, 0xd8]) {
            Some(Self::Jpeg)
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            Some(Self::Gif)
        } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
            Some(Self::Webp)
        } else {
            None
        }
    }

    /// Stable ASCII name suitable for a fallback row.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Webp => "WebP",
        }
    }
}

/// Validated image dimensions in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageDimensions {
    pub(super) width: u32,
    pub(super) height: u32,
}

impl ImageDimensions {
    /// Construct nonzero dimensions. Image payload validation additionally
    /// applies the configured width, height, and pixel-count limits.
    pub const fn new(width: u32, height: u32) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            Err(ImageError::InvalidDimensions)
        } else {
            Ok(Self { width, height })
        }
    }

    /// Width in pixels.
    pub const fn width(self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// A deliberately narrow, display-safe image filename.
///
/// Filenames are metadata only; this type never reads a path. It permits ASCII
/// letters, digits, `.`, `_`, and `-`, which can be base64 encoded safely for
/// iTerm2 without preserving attacker-controlled control characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ImageFilename(String);

impl ImageFilename {
    /// Validate a metadata filename against the fixed hard bound.
    pub fn new(value: &str) -> Result<Self, ImageError> {
        if value.is_empty()
            || matches!(value, "." | "..")
            || value.len() > HARD_MAX_FILENAME_BYTES
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(ImageError::UnsafeFilename);
        }
        Ok(Self(value.to_owned()))
    }

    /// Return the already validated filename.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Optional, validated metadata supplied with a byte payload.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageMetadata {
    filename: Option<ImageFilename>,
    expected_dimensions: Option<ImageDimensions>,
}

impl ImageMetadata {
    /// Attach a validated filename. The selected [`super::limits::ImageLimits`] applies a
    /// second, potentially smaller, bound when bytes are accepted.
    pub fn with_filename(mut self, filename: ImageFilename) -> Self {
        self.filename = Some(filename);
        self
    }

    /// Require the source header to match a caller-known dimension pair.
    pub fn with_expected_dimensions(mut self, dimensions: ImageDimensions) -> Self {
        self.expected_dimensions = Some(dimensions);
        self
    }

    /// The optional validated filename.
    pub fn filename(&self) -> Option<&ImageFilename> {
        self.filename.as_ref()
    }

    /// The optional caller-provided dimensions.
    pub const fn expected_dimensions(&self) -> Option<ImageDimensions> {
        self.expected_dimensions
    }
}
