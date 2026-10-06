//! Unit tests for `crate::media`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::media`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

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
