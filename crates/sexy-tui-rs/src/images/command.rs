//! The opaque protocol command a renderer emits, and the encoder that builds
//! it.
//!
//! Image bytes must never reach a retained semantic frame: the frame is cloned,
//! compared, diffed and written into native scrollback, and a megabyte of
//! base64 in any of those is both a memory and a rendering problem. So the
//! command this module produces carries a *reservation* and a layout, never
//! payload, filename or source location, and it is only ever written through a
//! terminal sink — at the point a renderer decides to place it.
//!
//! It is separate from [`super::transmit`] because the encoder's job is to
//! decide *whether* an image can be sent under the current limits and to shape
//! the plan, while the transmit half owns the byte-level framing, chunking and
//! base64 that actually reaches the wire.

use std::fmt;
use std::io::{self, Write};

use crate::terminal::Terminal;

use super::capabilities::ImageProtocol;
use super::error::ImageError;
use super::helpers::checked_add;
use super::inspect::validate_existing_image;
use super::layout::ImageLayout;
use super::limits::ImageLimits;
use super::payload::TerminalImage;
use super::registry::{ImageAction, ImageId};
use super::transmit::{build_transmission, emit_transmission, kitty_delete_sequence};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommandKind {
    Place,
    Replace,
    Delete,
}

impl CommandKind {
    const fn action(self, id: ImageId) -> ImageAction {
        match self {
            Self::Place => ImageAction::Place(id),
            Self::Replace => ImageAction::Replace(id),
            Self::Delete => ImageAction::Delete(id),
        }
    }
}

pub(super) struct ImageTransmission<'a> {
    pub(super) image: &'a TerminalImage,
    pub(super) protocol: ImageProtocol,
    pub(super) chunk_bytes: usize,
    pub(super) chunks: usize,
    pub(super) first_header: String,
    pub(super) continuation_more: String,
    pub(super) continuation_last: String,
}

/// An opaque, bounded terminal image command.
///
/// Use [`Self::write_to`] or [`Self::emit_to_terminal`] to send it. The command
/// has no method returning raw protocol text, which keeps callers from
/// accidentally placing escape sequences in semantic rows or diagnostics.
pub struct ImageTerminalCommand<'a> {
    pub(super) protocol: ImageProtocol,
    kind: CommandKind,
    id: ImageId,
    transmission: Option<ImageTransmission<'a>>,
    delete_prefix: Option<String>,
    encoded_len: usize,
    payload_chunks: usize,
}

impl fmt::Debug for ImageTerminalCommand<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageTerminalCommand")
            .field("protocol", &self.protocol)
            .field("action", &self.kind.action(self.id))
            .field("encoded_len", &self.encoded_len)
            .field("payload_chunks", &self.payload_chunks)
            .finish()
    }
}

impl ImageTerminalCommand<'_> {
    /// Protocol selected for this command.
    pub const fn protocol(&self) -> ImageProtocol {
        self.protocol
    }

    /// Explicit lifecycle action represented by this command.
    pub const fn action(&self) -> ImageAction {
        self.kind.action(self.id)
    }

    /// Complete bounded output length, including headers and terminators.
    pub const fn encoded_len(&self) -> usize {
        self.encoded_len
    }

    /// Number of bounded base64 portions written. Kitty terminates each part;
    /// iTerm2 writes the same portions inside one OSC frame.
    pub const fn payload_chunks(&self) -> usize {
        self.payload_chunks
    }

    /// Stream the opaque command to any byte writer using fixed-size base64
    /// buffers. No output-sized allocation occurs here.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        self.visit_bytes(|bytes| writer.write_all(bytes))
    }

    /// Stream the opaque command through the terminal output channel. This is
    /// intentionally separate from [`super::plan::ImageRenderPlan::semantic_rows`].
    pub fn emit_to_terminal(&self, terminal: &mut dyn Terminal) {
        let _: Result<(), std::convert::Infallible> = self.visit_bytes(|bytes| {
            // Headers, base64 chunks, and terminators are generated ASCII. If
            // an internal invariant is ever broken, suppress bytes rather than
            // forwarding an unexpected terminal control sequence.
            if let Ok(text) = std::str::from_utf8(bytes) {
                terminal.write(text);
            }
            Ok(())
        });
    }

    fn visit_bytes<E, F>(&self, mut write: F) -> Result<(), E>
    where
        F: FnMut(&[u8]) -> Result<(), E>,
    {
        if let Some(delete) = &self.delete_prefix {
            write(delete.as_bytes())?;
        }
        if let Some(transmission) = &self.transmission {
            emit_transmission(transmission, &mut write)?;
        }
        Ok(())
    }
}

/// A protocol encoder that keeps payload output opaque and bounded.
#[derive(Clone, Debug)]
pub struct ImageProtocolEncoder {
    pub(super) protocol: ImageProtocol,
    limits: ImageLimits,
}

impl ImageProtocolEncoder {
    /// Construct an encoder for a directly supported protocol.
    pub fn new(protocol: ImageProtocol, limits: ImageLimits) -> Self {
        Self { protocol, limits }
    }

    /// Selected protocol.
    pub const fn protocol(&self) -> ImageProtocol {
        self.protocol
    }

    /// Encode a new image placement.
    pub fn encode_place<'a>(
        &self,
        id: ImageId,
        image: &'a TerminalImage,
        layout: ImageLayout,
    ) -> Result<ImageTerminalCommand<'a>, ImageError> {
        self.encode_transmission(CommandKind::Place, id, image, layout)
    }

    /// Encode a replacement. Kitty emits a targeted delete immediately before
    /// transmission; iTerm2 returns [`ImageError::UnsupportedOperation`] rather
    /// than pretending that an unaddressable OSC image was replaced.
    pub fn encode_replace<'a>(
        &self,
        id: ImageId,
        image: &'a TerminalImage,
        layout: ImageLayout,
    ) -> Result<ImageTerminalCommand<'a>, ImageError> {
        if !self.protocol.supports_delete() {
            return Err(ImageError::UnsupportedOperation);
        }
        self.encode_transmission(CommandKind::Replace, id, image, layout)
    }

    /// Encode a targeted delete. iTerm2 has no equivalent targetable command
    /// and therefore returns [`ImageError::UnsupportedOperation`].
    pub fn encode_delete(&self, id: ImageId) -> Result<ImageTerminalCommand<'static>, ImageError> {
        if !self.protocol.supports_delete() {
            return Err(ImageError::UnsupportedOperation);
        }
        let delete = kitty_delete_sequence(id);
        if delete.len() > self.limits.max_encoded_output_bytes {
            return Err(ImageError::EncodedOutputTooLarge);
        }
        Ok(ImageTerminalCommand {
            protocol: self.protocol,
            kind: CommandKind::Delete,
            id,
            transmission: None,
            encoded_len: delete.len(),
            delete_prefix: Some(delete),
            payload_chunks: 0,
        })
    }

    fn encode_transmission<'a>(
        &self,
        kind: CommandKind,
        id: ImageId,
        image: &'a TerminalImage,
        layout: ImageLayout,
    ) -> Result<ImageTerminalCommand<'a>, ImageError> {
        validate_existing_image(image, &self.limits)?;
        if !self.protocol.supports_format(image.format()) {
            return Err(ImageError::UnsupportedFormatForProtocol);
        }
        let (transmission, output_len, payload_chunks) =
            build_transmission(self.protocol, id, image, layout, &self.limits)?;
        let delete_prefix = (kind == CommandKind::Replace).then(|| kitty_delete_sequence(id));
        let encoded_len = delete_prefix.as_ref().map_or(Ok(output_len), |delete| {
            checked_add(delete.len(), output_len)
        })?;
        if encoded_len > self.limits.max_encoded_output_bytes {
            return Err(ImageError::EncodedOutputTooLarge);
        }
        Ok(ImageTerminalCommand {
            protocol: self.protocol,
            kind,
            id,
            transmission: Some(transmission),
            delete_prefix,
            encoded_len,
            payload_chunks,
        })
    }
}
