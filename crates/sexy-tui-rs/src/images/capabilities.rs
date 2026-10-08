//! Deciding whether images work at all on this terminal, and parsing what the
//! terminal says back.
//!
//! This module holds the two halves of capability negotiation, which are the
//! same conversation seen from each end. [`ImageCapabilities::detect`] answers
//! "can this terminal take an image, and in which protocol" from the existing
//! conservative terminal profile, and [`ImageCapabilityQuery`] asks a terminal
//! that needs to be told — it can only confirm a protocol an interactive caller
//! already established, and it never enables images for a plain or
//! noninteractive profile. [`parse_terminal_image_reply`] reads the answer back.
//!
//! They are together because the accepted set is a decision, not a per-protocol
//! fact: Kitty takes only PNG and iTerm2 takes PNG/JPEG/GIF, and neither is
//! given a generic decode path for the formats it does not accept.
//!
//! The reply parsers are here rather than next to the encoder because they parse
//! *terminal output*, not a request. A reply is untrusted input on a channel the
//! application did not initiate a transaction on, and is bounded and discarded
//! exactly like one.

use std::fmt;
use std::io::{self, Write};
use std::time::Duration;

use crate::capabilities::CellPixelSize;
use crate::terminal::Terminal;
use crate::TerminalCapabilities;

use super::format::ImageFormat;
use super::limits::ImageLimits;
use super::registry::ImageId;

/// The directly encoded terminal image protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageProtocol {
    /// Kitty graphics APC commands with bounded direct PNG chunks.
    Kitty,
    /// iTerm2 OSC 1337 inline file commands.
    Iterm2,
}

impl ImageProtocol {
    /// Whether this protocol can accept the container bytes without conversion.
    pub const fn supports_format(self, format: ImageFormat) -> bool {
        match self {
            // Kitty `f=100` is PNG. It is deliberately not used as a generic
            // image decoder for JPEG, GIF, or WebP input.
            Self::Kitty => matches!(format, ImageFormat::Png),
            // iTerm2's documented inline-image compatibility set is kept to
            // PNG/JPEG/GIF. WebP behavior varies with host image frameworks.
            Self::Iterm2 => matches!(
                format,
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif
            ),
        }
    }

    /// Whether this protocol has a targetable delete operation.
    pub const fn supports_delete(self) -> bool {
        matches!(self, Self::Kitty)
    }
}

/// Explicit deterministic image capability overrides.
///
/// `force` is intended for caller-managed negotiation and test harnesses. It
/// can bypass environment heuristics, but never enables image output for a
/// plain or noninteractive [`TerminalCapabilities`] profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageCapabilityOverrides {
    /// Force this protocol after an interactive caller has established support.
    pub force: Option<ImageProtocol>,
    /// Disable image output even if terminal heuristics claim support.
    pub disable: bool,
    /// Override a caller-provided cell-pixel measurement for deterministic tests.
    pub cell_pixel_size: Option<CellPixelSize>,
}

/// Image-specific terminal capability state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCapabilities {
    protocol: Option<ImageProtocol>,
    cell_pixel_size: Option<CellPixelSize>,
    accepts_replies: bool,
    forced: bool,
}

impl ImageCapabilities {
    /// Detect image capability hints from the existing conservative terminal
    /// profile. This method sends no terminal query and performs no I/O.
    pub fn detect(terminal: &TerminalCapabilities, overrides: &ImageCapabilityOverrides) -> Self {
        let interactive = terminal.interactive && !terminal.plain;
        let forced = interactive && !overrides.disable && overrides.force.is_some();
        let protocol = if !interactive || overrides.disable {
            None
        } else if let Some(protocol) = overrides.force {
            Some(protocol)
        } else if terminal.kitty_graphics {
            // Keep a deterministic preference if a terminal reports both.
            Some(ImageProtocol::Kitty)
        } else if terminal.iterm2_images {
            Some(ImageProtocol::Iterm2)
        } else {
            None
        };
        Self {
            protocol,
            cell_pixel_size: if interactive {
                overrides.cell_pixel_size.or(terminal.cell_pixel_size)
            } else {
                None
            },
            accepts_replies: interactive && !overrides.disable,
            forced,
        }
    }

    /// Build deterministic capability state for a test harness or a caller
    /// that has already performed its own bounded negotiation.
    pub const fn forced(
        protocol: Option<ImageProtocol>,
        cell_pixel_size: Option<CellPixelSize>,
    ) -> Self {
        Self {
            protocol,
            cell_pixel_size,
            accepts_replies: protocol.is_some(),
            forced: protocol.is_some(),
        }
    }

    /// Selected protocol, if terminal images are usable.
    pub const fn protocol(self) -> Option<ImageProtocol> {
        self.protocol
    }

    /// Validated cell-pixel measurement, if a caller supplied or parsed one.
    pub const fn cell_pixel_size(self) -> Option<CellPixelSize> {
        self.cell_pixel_size
    }

    /// Apply one already-correlated, bounded terminal reply.
    ///
    /// A forced selection is never changed by a late reply. This method does
    /// not accept raw terminal text; use [`parse_terminal_image_reply`] first.
    pub fn apply_reply(&mut self, reply: TerminalImageReply) {
        match reply {
            TerminalImageReply::KittyGraphicsSupported { .. }
                if self.accepts_replies && !self.forced =>
            {
                self.protocol = Some(ImageProtocol::Kitty);
            }
            TerminalImageReply::CellPixels(size) | TerminalImageReply::Iterm2CellPixels(size)
                if self.accepts_replies =>
            {
                self.cell_pixel_size = Some(size);
            }
            _ => {}
        }
    }
}

/// A bounded, caller-owned terminal image capability query.
///
/// Query construction and reply parsing are deliberately separate from the
/// terminal lifecycle. This type never reads from a terminal or sleeps: callers
/// must perform at most one bounded poll using [`Self::timeout`] and then feed
/// exactly one reply to [`Self::parse_reply`].
#[derive(Clone)]
pub struct ImageCapabilityQuery {
    bytes: String,
    kind: ImageQueryKind,
    expected_kitty_query: Option<ImageId>,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ImageQueryKind {
    KittyGraphics,
    CellPixels,
    Iterm2CellPixels,
}

impl fmt::Debug for ImageCapabilityQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageCapabilityQuery")
            .field("expected_kitty_query", &self.expected_kitty_query)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl ImageCapabilityQuery {
    /// Build a Kitty graphics support query correlated to a nonzero image ID.
    pub fn kitty_graphics(id: ImageId, limits: &ImageLimits) -> Self {
        Self {
            // Do not set Kitty's quiet mode here: this is the one command for
            // which the caller needs the correlated success reply.
            bytes: format!("\x1b_Ga=q,i={},s=1,v=1,f=24;\x1b\\", id.get()),
            kind: ImageQueryKind::KittyGraphics,
            expected_kitty_query: Some(id),
            timeout: limits.query_timeout,
        }
    }

    /// Build the standard xterm cell-pixel query (`CSI 16 t`).
    pub fn cell_pixels(limits: &ImageLimits) -> Self {
        Self {
            bytes: "\x1b[16t".to_owned(),
            kind: ImageQueryKind::CellPixels,
            expected_kitty_query: None,
            timeout: limits.query_timeout,
        }
    }

    /// Build iTerm2's cell-size query. The corresponding parser accepts both
    /// BEL and ST terminated replies, but no trailing data.
    pub fn iterm2_cell_pixels(limits: &ImageLimits) -> Self {
        Self {
            bytes: "\x1b]1337;ReportCellSize\x1b\\".to_owned(),
            kind: ImageQueryKind::Iterm2CellPixels,
            expected_kitty_query: None,
            timeout: limits.query_timeout,
        }
    }

    /// Maximum caller-owned wait for this one query attempt.
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Expected Kitty query ID, used to reject confused replies.
    pub const fn expected_kitty_query(&self) -> Option<ImageId> {
        self.expected_kitty_query
    }

    /// Parse exactly one reply for this specific query kind.
    ///
    /// Unlike [`parse_terminal_image_reply`], this method rejects a syntactically
    /// valid reply for another query type. It is the preferred correlation
    /// boundary after the caller-owned one-shot poll.
    pub fn parse_reply(&self, reply: &str, limits: &ImageLimits) -> Option<TerminalImageReply> {
        if reply.is_empty() || reply.len() > limits.max_terminal_reply_bytes {
            return None;
        }
        match self.kind {
            ImageQueryKind::KittyGraphics => {
                let expected = self.expected_kitty_query?;
                let id = parse_kitty_reply(reply)?;
                (id == expected)
                    .then_some(TerminalImageReply::KittyGraphicsSupported { query_id: id })
            }
            ImageQueryKind::CellPixels => {
                parse_standard_cell_reply(reply).map(TerminalImageReply::CellPixels)
            }
            ImageQueryKind::Iterm2CellPixels => {
                parse_iterm2_cell_reply(reply).map(TerminalImageReply::Iterm2CellPixels)
            }
        }
    }

    /// Write query bytes to a generic output without exposing them as semantic
    /// text. The query is fixed-size and contains no payload data.
    pub fn write_to<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        writer.write_all(self.bytes.as_bytes())
    }

    /// Emit the bounded query through a terminal output sink.
    pub fn emit_to_terminal(&self, terminal: &mut dyn Terminal) {
        terminal.write(&self.bytes);
    }
}

/// A strictly parsed terminal image capability reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalImageReply {
    /// A Kitty graphics query completed successfully for this exact ID.
    KittyGraphicsSupported {
        /// The query ID supplied by the caller-owned query.
        query_id: ImageId,
    },
    /// A standard `CSI 6 ; height ; width t` cell-pixel report.
    CellPixels(CellPixelSize),
    /// An iTerm2 `ReportCellSize=height;width` cell-pixel report.
    Iterm2CellPixels(CellPixelSize),
}

/// Parse exactly one bounded terminal reply.
///
/// The parser accepts no prefixes, suffixes, concatenated frames, unknown
/// fields, or error text. Kitty success is accepted only when `expected_kitty`
/// matches the reply ID; this is the correlation boundary that prevents an old
/// or unrelated reply from enabling graphics support. Prefer
/// [`ImageCapabilityQuery::parse_reply`] when the originating query is known,
/// because it additionally rejects valid replies for another query type. This
/// function performs no I/O and never returns hostile text for logging.
pub fn parse_terminal_image_reply(
    reply: &str,
    expected_kitty: Option<ImageId>,
    limits: &ImageLimits,
) -> Option<TerminalImageReply> {
    if reply.is_empty() || reply.len() > limits.max_terminal_reply_bytes {
        return None;
    }

    if let Some(expected) = expected_kitty {
        if let Some(id) = parse_kitty_reply(reply) {
            return (id == expected)
                .then_some(TerminalImageReply::KittyGraphicsSupported { query_id: id });
        }
    }

    parse_standard_cell_reply(reply)
        .map(TerminalImageReply::CellPixels)
        .or_else(|| parse_iterm2_cell_reply(reply).map(TerminalImageReply::Iterm2CellPixels))
}

fn parse_kitty_reply(reply: &str) -> Option<ImageId> {
    let value = reply.strip_prefix("\x1b_Gi=")?.strip_suffix(";OK\x1b\\")?;
    ImageId::new(parse_bounded_decimal(value, 10)?).ok()
}

fn parse_standard_cell_reply(reply: &str) -> Option<CellPixelSize> {
    let value = reply.strip_prefix("\x1b[6;")?.strip_suffix('t')?;
    let (height, width) = parse_cell_pair(value)?;
    CellPixelSize::new(width, height)
}

fn parse_iterm2_cell_reply(reply: &str) -> Option<CellPixelSize> {
    let value = reply.strip_prefix("\x1b]1337;ReportCellSize=")?;
    let value = value
        .strip_suffix('\u{7}')
        .or_else(|| value.strip_suffix("\x1b\\"))?;
    let (height, width) = parse_cell_pair(value)?;
    CellPixelSize::new(width, height)
}

fn parse_cell_pair(value: &str) -> Option<(u16, u16)> {
    let (height, width) = value.split_once(';')?;
    if width.contains(';') {
        return None;
    }
    let height = u16::try_from(parse_bounded_decimal(height, 5)?).ok()?;
    let width = u16::try_from(parse_bounded_decimal(width, 5)?).ok()?;
    Some((height, width))
}

fn parse_bounded_decimal(value: &str, max_digits: usize) -> Option<u32> {
    if value.is_empty()
        || value.len() > max_digits
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    value.bytes().try_fold(0_u32, |number, byte| {
        number.checked_mul(10)?.checked_add(u32::from(byte - b'0'))
    })
}
