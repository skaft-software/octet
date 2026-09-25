//! Bounded, deterministic preparation of inline user images before session append.
//!
//! Call this once on the canonical user input before it is persisted. Never resize
//! only a provider request assembled from history: the next turn would replay the
//! original bytes, invalidating prompt-cache prefixes and repeating the work.

use std::io::Cursor;

use bytes::Bytes;
use image::{GenericImageView as _, ImageFormat, ImageReader};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::types::{ImageMedia, ImageSource};

/// Host-wide upper bound on the encoded image accepted by this preparation step.
/// The TUI, host, and read tool currently enforce the same five-MiB admission cap.
pub const MAX_USER_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Maximum pixel count we will decode, regardless of compressed input size.
pub const MAX_USER_IMAGE_PIXELS: u64 = 16_000_000;
const MAX_DECODE_ALLOC_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RESIZE_ATTEMPTS: usize = 12;

/// Selected model's per-image input constraints. The host's five-MiB file cap
/// remains independent of these model limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageInputLimits {
    /// Largest accepted width in pixels.
    pub max_width: u32,
    /// Largest accepted height in pixels.
    pub max_height: u32,
    /// Largest accepted encoded image in bytes.
    pub max_bytes: usize,
}

impl ImageInputLimits {
    /// Reject zero or excessive declared dimensions; encoded output is always
    /// capped independently by the local five-MiB admission limit.
    pub fn validate(self) -> Result<(), ImageInputError> {
        if self.max_width == 0
            || self.max_height == 0
            || self.max_bytes == 0
            || self.max_width as u64 > MAX_USER_IMAGE_PIXELS
            || self.max_height as u64 > MAX_USER_IMAGE_PIXELS
        {
            Err(ImageInputError::InvalidLimits)
        } else {
            Ok(())
        }
    }
}

/// Preparation failure. Neither source bytes nor file paths are included in errors.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ImageInputError {
    /// The caller supplied an invalid or unbounded model policy.
    #[error("invalid model image input limits")]
    InvalidLimits,
    /// The source exceeds the local encoded-byte admission limit.
    #[error("image exceeds the 5 MiB input limit")]
    InputTooLarge,
    /// The image header advertises an excessive decoded size.
    #[error("image exceeds the 16-million-pixel decode limit")]
    PixelLimit,
    /// Invalid, truncated, mismatched or unsupported image data.
    #[error("image is invalid or does not match its PNG/JPEG/GIF/WebP media type")]
    InvalidImage,
    /// No bounded resize produced an image within the selected model's limit.
    #[error("image cannot fit the selected model's image input limit")]
    ModelLimit,
}

/// Prepare an inline user image for the selected model. An unchanged image
/// retains its exact bytes, MIME and quality hint. A resized image is encoded
/// as PNG (preserving transparency) and has a matching MIME; the source is
/// replaced in the returned canonical value, not the caller's persisted value.
/// URL/provider references have no local bytes to resize and are left alone.
///
/// The caller must apply this to the complete user input *before* appending
/// history, and must check all images before committing any of that input.
/// Unknown model limits should be represented by a conservative host policy,
/// not inferred from a provider or image file name.
pub fn prepare_user_image(
    image: &ImageMedia,
    limits: ImageInputLimits,
) -> Result<ImageMedia, ImageInputError> {
    limits.validate()?;
    let ImageSource::Inline(bytes) = &image.source else {
        return Ok(image.clone());
    };
    if bytes.len() > MAX_USER_IMAGE_BYTES {
        return Err(ImageInputError::InputTooLarge);
    }
    let format = match image.media_type.as_ref().map(mime::Mime::essence_str) {
        Some("image/png") => ImageFormat::Png,
        Some("image/jpeg") => ImageFormat::Jpeg,
        Some("image/gif") => ImageFormat::Gif,
        Some("image/webp") => ImageFormat::WebP,
        _ => return Err(ImageInputError::InvalidImage),
    };
    if image::guess_format(bytes).ok() != Some(format) {
        return Err(ImageInputError::InvalidImage);
    }
    let (width, height) = reader(bytes, format)
        .into_dimensions()
        .map_err(|_| ImageInputError::InvalidImage)?;
    if width == 0 || height == 0 || width as u64 * height as u64 > MAX_USER_IMAGE_PIXELS {
        return Err(ImageInputError::PixelLimit);
    }
    // Validate the pixels even on the no-resize path. A valid image signature
    // and dimensions alone cannot establish that a truncated payload is safe.
    let decoded = reader(bytes, format)
        .decode()
        .map_err(|_| ImageInputError::InvalidImage)?;
    if width <= limits.max_width && height <= limits.max_height && bytes.len() <= limits.max_bytes {
        return Ok(image.clone());
    }
    let (width, height) = decoded.dimensions();
    let scale = (limits.max_width as f64 / width as f64)
        .min(limits.max_height as f64 / height as f64)
        .min(1.0);
    let mut width = (width as f64 * scale).floor().max(1.0) as u32;
    let mut height = (height as f64 * scale).floor().max(1.0) as u32;
    let output_limit = limits.max_bytes.min(MAX_USER_IMAGE_BYTES);
    for _ in 0..MAX_RESIZE_ATTEMPTS {
        let resized = decoded.resize_exact(width, height, image::imageops::FilterType::Triangle);
        let mut output = Cursor::new(Vec::new());
        resized
            .write_to(&mut output, ImageFormat::Png)
            .map_err(|_| ImageInputError::InvalidImage)?;
        if output.get_ref().len() <= output_limit {
            return Ok(ImageMedia {
                source: ImageSource::Inline(Bytes::from(output.into_inner())),
                media_type: Some(mime::IMAGE_PNG),
                detail: image.detail,
            });
        }
        if width == 1 && height == 1 {
            break;
        }
        // A predictable bounded retry, even when source compression was unusually good.
        if width <= 4 && height <= 4 {
            width = 1;
            height = 1;
        } else {
            width = (width * 3 / 4).max(1);
            height = (height * 3 / 4).max(1);
        }
    }
    Err(ImageInputError::ModelLimit)
}

fn reader(bytes: &Bytes, format: ImageFormat) -> ImageReader<Cursor<&[u8]>> {
    let mut reader = ImageReader::with_format(Cursor::new(bytes.as_ref()), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_USER_IMAGE_PIXELS as u32);
    limits.max_image_height = Some(MAX_USER_IMAGE_PIXELS as u32);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    reader
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ImageDetail, Media};
    use image::DynamicImage;

    fn png(width: u32, height: u32) -> ImageMedia {
        let raster = DynamicImage::new_rgba8(width, height);
        let mut buffer = Cursor::new(Vec::new());
        raster.write_to(&mut buffer, ImageFormat::Png).unwrap();
        match Media::image_bytes(Bytes::from(buffer.into_inner()), mime::IMAGE_PNG) {
            Media::Image(image) => image,
            _ => unreachable!(),
        }
    }

    #[test]
    fn unchanged_preserves_exact_bytes_and_metadata() {
        let mut original = png(4, 3);
        original.detail = Some(ImageDetail::High);
        let prepared = prepare_user_image(
            &original,
            ImageInputLimits {
                max_width: 4,
                max_height: 3,
                max_bytes: 10_000,
            },
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&original).unwrap(),
            serde_json::to_value(&prepared).unwrap()
        );
    }

    #[test]
    fn resize_before_history_is_stable_and_preserves_aspect() {
        let original = png(80, 40);
        let limits = ImageInputLimits {
            max_width: 20,
            max_height: 20,
            max_bytes: 5_000,
        };
        let prepared = prepare_user_image(&original, limits).unwrap();
        assert_eq!(prepared.media_type, Some(mime::IMAGE_PNG));
        let ImageSource::Inline(ref encoded) = prepared.source else {
            panic!("inline image expected")
        };
        assert_eq!(
            image::load_from_memory(encoded).unwrap().dimensions(),
            (20, 10)
        );
        assert_eq!(
            serde_json::to_value(&prepared).unwrap(),
            serde_json::to_value(prepare_user_image(&original, limits).unwrap()).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&prepared).unwrap(),
            serde_json::to_value(prepare_user_image(&prepared, limits).unwrap()).unwrap()
        );
    }

    #[test]
    fn bounded_rejection_of_bad_header_and_oversized_input() {
        let mut bad = png(2, 2);
        bad.source = ImageSource::Inline(Bytes::from_static(b"not an image"));
        assert_eq!(
            prepare_user_image(
                &bad,
                ImageInputLimits {
                    max_width: 2,
                    max_height: 2,
                    max_bytes: 1000
                }
            )
            .unwrap_err(),
            ImageInputError::InvalidImage
        );
        bad = png(2, 2);
        if let ImageSource::Inline(ref mut data) = bad.source {
            *data = data.slice(..33);
        }
        assert_eq!(
            prepare_user_image(
                &bad,
                ImageInputLimits {
                    max_width: 2,
                    max_height: 2,
                    max_bytes: 1000
                }
            )
            .unwrap_err(),
            ImageInputError::InvalidImage
        );
        bad = png(2, 2);
        bad.media_type = Some(mime::IMAGE_JPEG);
        assert_eq!(
            prepare_user_image(
                &bad,
                ImageInputLimits {
                    max_width: 2,
                    max_height: 2,
                    max_bytes: 1000
                }
            )
            .unwrap_err(),
            ImageInputError::InvalidImage
        );
        bad.source = ImageSource::Inline(Bytes::from(vec![0; MAX_USER_IMAGE_BYTES + 1]));
        assert_eq!(
            prepare_user_image(
                &bad,
                ImageInputLimits {
                    max_width: 2,
                    max_height: 2,
                    max_bytes: 1000
                }
            )
            .unwrap_err(),
            ImageInputError::InputTooLarge
        );
        let mut buffer = Cursor::new(Vec::new());
        DynamicImage::new_rgba8(2, 2)
            .write_to(&mut buffer, ImageFormat::Gif)
            .unwrap();
        let mut gif = buffer.into_inner();
        // GIF stores unsigned 16-bit dimensions without an image-header CRC.
        gif[6..10].copy_from_slice(&[0xff, 0xff, 0xff, 0xff]);
        let bomb = match Media::image_bytes(Bytes::from(gif), mime::IMAGE_GIF) {
            Media::Image(image) => image,
            _ => unreachable!(),
        };
        assert_eq!(
            prepare_user_image(
                &bomb,
                ImageInputLimits {
                    max_width: 2,
                    max_height: 2,
                    max_bytes: 1000
                }
            )
            .unwrap_err(),
            ImageInputError::PixelLimit
        );
    }

    #[test]
    fn impossible_byte_limit_rejects_without_modifying_source() {
        let original = png(4, 3);
        let old = serde_json::to_value(&original).unwrap();
        assert_eq!(
            prepare_user_image(
                &original,
                ImageInputLimits {
                    max_width: 4,
                    max_height: 3,
                    max_bytes: 1
                }
            )
            .unwrap_err(),
            ImageInputError::ModelLimit
        );
        assert_eq!(serde_json::to_value(original).unwrap(), old);
    }
}
