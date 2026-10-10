//! Deciding what a renderer should do with an image, and saying so in a form a
//! retained frame can hold.
//!
//! [`ImagePlanner`] is the decision point, and [`ImageRenderPlan`] is its answer:
//! either a reservation plus an opaque command, a reservation with ASCII
//! fallback text, or a reservation with a reason. The plan is the intended
//! handoff to a renderer — take `semantic_rows` for the retained frame, then
//! write the command at the placement point.
//!
//! It is separate from [`super::command`] because planning is a policy decision
//! that is allowed to say no (unsupported format, missing capability, a payload
//! that no longer fits its own limits) and encoding is not. Collapsing them
//! would make "we chose fallback" and "we could not encode it" the same return
//! value, and a renderer could not tell a deliberate ASCII downgrade from a bug.

use std::fmt;
use std::io::{self, Write};

use crate::terminal::Terminal;

use super::capabilities::ImageCapabilities;
use super::command::{CommandKind, ImageProtocolEncoder, ImageTerminalCommand};
use super::error::ImageError;
use super::inspect::validate_existing_image;
use super::layout::{ImageFallbackReason, ImageLayout, ImageReservation, ImageViewport};
use super::limits::ImageLimits;
use super::payload::TerminalImage;
use super::registry::{ImageAction, ImageId};

/// A semantic reservation plus optional, out-of-band protocol command.
///
/// This is the intended handoff to a future renderer integration: use semantic
/// rows for its retained frame, then write the command at the placement point.
/// Serialize that emission with normal terminal writes. The foundation does not
/// alter TUI lifecycle or scrollback itself.
pub struct ImageRenderPlan<'a> {
    reservation: ImageReservation,
    command: Option<ImageTerminalCommand<'a>>,
    layout: Option<ImageLayout>,
    fallback_reason: Option<ImageFallbackReason>,
}

impl fmt::Debug for ImageRenderPlan<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageRenderPlan")
            .field("reservation", &self.reservation)
            .field("has_terminal_command", &self.command.is_some())
            .field("layout", &self.layout)
            .field("fallback_reason", &self.fallback_reason)
            .finish()
    }
}

impl<'a> ImageRenderPlan<'a> {
    /// Semantic-only reservation for frame, selection, copy, or logs.
    pub const fn reservation(&self) -> &ImageReservation {
        &self.reservation
    }

    /// Generate semantic rows without protocol or payload bytes.
    pub fn semantic_rows(&self) -> Vec<String> {
        self.reservation.semantic_rows()
    }

    /// Generate the semantic copy/log representation without protocol bytes.
    pub fn semantic_copy_text(&self) -> String {
        self.reservation.semantic_copy_text()
    }

    /// Optional opaque protocol output to write separately from semantic text.
    pub const fn terminal_command(&self) -> Option<&ImageTerminalCommand<'a>> {
        self.command.as_ref()
    }

    /// Placement geometry when a terminal command is available. This lets a
    /// retained renderer create a zero-width [`super::anchor::ImageAnchor`] without exposing
    /// protocol bytes in its semantic frame.
    pub const fn layout(&self) -> Option<ImageLayout> {
        self.layout
    }

    /// Why this plan has no terminal command.
    pub const fn fallback_reason(&self) -> Option<ImageFallbackReason> {
        self.fallback_reason
    }

    /// Write the optional protocol command to a generic byte writer.
    pub fn write_protocol_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        match &self.command {
            Some(command) => command.write_to(writer),
            None => Ok(()),
        }
    }

    /// Emit the optional protocol command through the terminal output channel.
    pub fn emit_protocol_to_terminal(&self, terminal: &mut dyn Terminal) {
        if let Some(command) = &self.command {
            command.emit_to_terminal(terminal);
        }
    }
}

/// Capability-aware protocol and semantic reservation planner.
#[derive(Clone, Debug)]
pub struct ImagePlanner {
    capabilities: ImageCapabilities,
    limits: ImageLimits,
}

impl ImagePlanner {
    /// Construct a planner from already detected or forced capabilities.
    pub fn new(capabilities: ImageCapabilities, limits: ImageLimits) -> Self {
        Self {
            capabilities,
            limits,
        }
    }

    /// Current image capability state.
    pub const fn capabilities(&self) -> ImageCapabilities {
        self.capabilities
    }

    /// Plan a new placement and its semantic reservation.
    pub fn plan_place<'a>(
        &self,
        id: ImageId,
        image: &'a TerminalImage,
        viewport: ImageViewport,
    ) -> Result<ImageRenderPlan<'a>, ImageError> {
        self.plan(CommandKind::Place, id, image, viewport)
    }

    /// Plan a replacement. Kitty yields delete-then-transmit; iTerm2 yields a
    /// semantic fallback because it cannot target an existing inline image.
    pub fn plan_replace<'a>(
        &self,
        id: ImageId,
        image: &'a TerminalImage,
        viewport: ImageViewport,
    ) -> Result<ImageRenderPlan<'a>, ImageError> {
        self.plan(CommandKind::Replace, id, image, viewport)
    }

    /// Plan a targetable delete. `Ok(None)` means no image terminal is active;
    /// iTerm2 returns an explicit unsupported-operation error rather than a
    /// false success.
    pub fn plan_delete(
        &self,
        action: ImageAction,
    ) -> Result<Option<ImageTerminalCommand<'static>>, ImageError> {
        let ImageAction::Delete(id) = action else {
            return Err(ImageError::InvalidAction);
        };
        let Some(protocol) = self.capabilities.protocol() else {
            return Ok(None);
        };
        ImageProtocolEncoder::new(protocol, self.limits.clone())
            .encode_delete(id)
            .map(Some)
    }

    fn plan<'a>(
        &self,
        kind: CommandKind,
        id: ImageId,
        image: &'a TerminalImage,
        viewport: ImageViewport,
    ) -> Result<ImageRenderPlan<'a>, ImageError> {
        // Validate even fallback-only plans against this planner's limits. A
        // TerminalImage can be handed across components that chose different
        // bounds, and unsupported terminals must not become a validation
        // bypass merely because they emit text instead of protocol bytes.
        validate_existing_image(image, &self.limits)?;
        let Some(protocol) = self.capabilities.protocol() else {
            return Ok(fallback_plan(
                image,
                ImageFallbackReason::UnsupportedTerminal,
            ));
        };
        if !protocol.supports_format(image.format()) {
            return Ok(fallback_plan(image, ImageFallbackReason::UnsupportedFormat));
        }
        let layout = ImageLayout::fit(image.dimensions(), viewport)?;
        let encoder = ImageProtocolEncoder::new(protocol, self.limits.clone());
        let command = match kind {
            CommandKind::Place => encoder.encode_place(id, image, layout),
            CommandKind::Replace => encoder.encode_replace(id, image, layout),
            CommandKind::Delete => return Err(ImageError::InvalidAction),
        };
        match command {
            Ok(command) => Ok(ImageRenderPlan {
                reservation: ImageReservation::blank(layout),
                command: Some(command),
                layout: Some(layout),
                fallback_reason: None,
            }),
            Err(ImageError::UnsupportedOperation) => Ok(fallback_plan(
                image,
                ImageFallbackReason::UnsupportedOperation,
            )),
            Err(error) => Err(error),
        }
    }
}

fn fallback_plan(image: &TerminalImage, reason: ImageFallbackReason) -> ImageRenderPlan<'_> {
    ImageRenderPlan {
        reservation: ImageReservation::fallback(image, reason),
        command: None,
        layout: None,
        fallback_reason: Some(reason),
    }
}
