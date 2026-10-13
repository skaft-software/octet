//! Bounded, out-of-band terminal image primitives.
//!
//! This module accepts only owned image bytes supplied by the caller; it never
//! reads paths, URLs, environment values, or network resources. It validates a
//! small set of container headers without decompression, derives a bounded cell
//! reservation, and keeps protocol output opaque and separate from semantic
//! text. A renderer can put [`ImageRenderPlan::semantic_rows`] into its
//! copyable frame and emit [`ImageTerminalCommand`] only through a terminal
//! output sink.
//!
//! The protocol matrix is intentionally conservative: Kitty receives only PNG
//! (`f=100`); iTerm2 receives PNG, JPEG, and GIF through OSC 1337. WebP and all
//! other combinations receive deterministic ASCII fallback text rather than a
//! guessed conversion. Animated PNG, GIF, and WebP containers are rejected so a
//! bounded source cannot request unbounded terminal-side frame decoding. iTerm2
//! has no target-ID delete operation, so replace and delete are explicitly
//! unavailable there.
//!
//! This file is the module seam. It owns the public surface and nothing else.
//! The work is split along the path one image takes, from a byte payload to an
//! opaque terminal command, so that each step has exactly one place where its
//! rule lives:
//!
//! * `error` — the single error type. Its variants carry no payload, because a
//!   filename and a base64 chunk are both attacker-controlled and an error that
//!   embedded either would make `Display` an escape-injection path.
//! * `limits` — every `HARD_MAX_*` ceiling and every default, together, so a
//!   default above its ceiling is impossible to write.
//! * `format` — the container vocabulary: format, dimensions, validated
//!   filename, and the caller's optional metadata.
//! * `inspect` — the bounded header walk, and the only place payload bytes are
//!   read. Animated containers are rejected here.
//! * `payload` — [`TerminalImage`], the only type that holds bytes, holding them
//!   privately so "validated" and "reachable" are the same statement.
//! * `registry` — monotonic image IDs. Never reusing one is what makes a delayed
//!   terminal delete harmless.
//! * `capabilities` — whether this terminal takes images at all, in which
//!   protocol, and how a terminal reply is parsed back.
//! * `layout` — pixel box to bounded cell reservation, and the ASCII fallback
//!   reason.
//! * `transmit` — the byte-level framing and the only base64 in the crate.
//! * `command` — the opaque, payload-free command a renderer emits, and the
//!   encoder that decides whether an image may be sent.
//! * `anchor` — the one image type with a textual form, so a retained frame can
//!   remember a placement in a diffable row.
//! * `plan` — the decision, and the object a renderer actually consumes.
//! * `helpers` — checked arithmetic and bounded integer readers, so "bounded"
//!   has one definition rather than one per parser.

mod anchor;
mod capabilities;
mod command;
mod error;
mod format;
mod helpers;
mod inspect;
mod layout;
mod limits;
mod payload;
mod plan;
mod registry;
mod transmit;

pub use self::anchor::ImageAnchor;
pub use self::capabilities::{
    parse_terminal_image_reply, ImageCapabilities, ImageCapabilityOverrides, ImageCapabilityQuery,
    ImageProtocol, TerminalImageReply,
};
pub use self::command::{ImageProtocolEncoder, ImageTerminalCommand};
pub use self::error::ImageError;
pub use self::format::{ImageDimensions, ImageFilename, ImageFormat, ImageMetadata};
pub use self::layout::{
    cell_rows_for_pixels, ImageFallbackReason, ImageLayout, ImageReservation, ImageViewport,
};
pub use self::limits::{
    ImageLimits, HARD_MAX_CONTAINER_ITEMS, HARD_MAX_ENCODED_OUTPUT_BYTES, HARD_MAX_FILENAME_BYTES,
    HARD_MAX_HEADER_BYTES, HARD_MAX_IMAGE_DIMENSION, HARD_MAX_IMAGE_PAYLOAD_BYTES,
    HARD_MAX_IMAGE_PIXELS, HARD_MAX_LIVE_IMAGES, HARD_MAX_PROTOCOL_CHUNKS,
    HARD_MAX_PROTOCOL_CHUNK_BYTES, HARD_MAX_QUERY_TIMEOUT, HARD_MAX_TERMINAL_REPLY_BYTES,
    MAX_IMAGE_CELL_COLUMNS, MAX_RESERVED_IMAGE_ROWS,
};
pub use self::payload::TerminalImage;
pub use self::plan::{ImagePlanner, ImageRenderPlan};
pub use self::registry::{ImageAction, ImageId, ImageRegistry};

/// Container header inspection, the accepted format set, the hard ceilings, the
/// layout arithmetic, and the ASCII fallback marker.
///
/// The assertions live in `tests/`, one file per cohesive area, so these source
/// files read as pure implementation.
#[cfg(test)]
mod tests;
