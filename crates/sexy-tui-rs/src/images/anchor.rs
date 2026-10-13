//! The in-band anchor that lets a retained frame remember an image placement.
//!
//! A retained renderer diffs semantic rows, and a row that carries an image is
//! still a row. [`ImageAnchor`] is the escape a renderer embeds in that row to
//! name the placement — protocol, image ID and layout — and it is parsed back
//! out of the same string. It is deliberately not a terminal protocol command:
//! it travels inside a copyable, diffable, scrollback-bound semantic row, so it
//! has to be inert text and it has to survive being compared against a previous
//! frame byte for byte.
//!
//! It is its own module because it is the only image type with a *textual*
//! representation, and because the two consumers of that representation — a
//! renderer embedding it and the TUI's image-row detection reading it back —
//! must agree on the grammar. The escape itself is defined once, here.

use super::capabilities::ImageProtocol;
use super::layout::ImageLayout;
use super::limits::HARD_MAX_LIVE_IMAGES;
use super::registry::ImageId;

/// A private zero-width marker that connects a semantic reservation to an
/// opaque terminal-image command at the terminal-adapter boundary.
///
/// The marker is deliberately a DCS control string rather than a graphics
/// protocol command. Retained renderers may keep it alongside blank semantic
/// rows, while an adapter that owns the corresponding image bytes replaces it
/// with a bounded protocol command. It contains no image payload, filename, or
/// source location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageAnchor {
    protocol: ImageProtocol,
    id: ImageId,
    layout: ImageLayout,
}

const IMAGE_ANCHOR_PREFIX: &str = "\x1bP+octet-image;";
const IMAGE_ANCHOR_SUFFIX: &str = "\x1b\\";

impl ImageAnchor {
    /// Build an anchor for one already-planned placement.
    pub const fn new(protocol: ImageProtocol, id: ImageId, layout: ImageLayout) -> Self {
        Self {
            protocol,
            id,
            layout,
        }
    }

    /// Protocol selected for this placement.
    pub const fn protocol(self) -> ImageProtocol {
        self.protocol
    }

    /// Stable logical image ID.
    pub const fn id(self) -> ImageId {
        self.id
    }

    /// Bounded terminal-cell placement.
    pub const fn layout(self) -> ImageLayout {
        self.layout
    }

    /// Render the zero-width adapter marker. This is not a terminal graphics
    /// command and must not be emitted without an adapter that resolves it.
    pub fn marker(self) -> String {
        let protocol = match self.protocol {
            ImageProtocol::Kitty => "kitty",
            ImageProtocol::Iterm2 => "iterm2",
        };
        format!(
            "{IMAGE_ANCHOR_PREFIX}v=1,p={protocol},i={},c={},r={}{}",
            self.id.get(),
            self.layout.columns(),
            self.layout.rows(),
            IMAGE_ANCHOR_SUFFIX,
        )
    }

    /// Strictly parse one complete anchor marker. Unknown versions, fields,
    /// protocols, invalid IDs, and invalid layouts are rejected.
    pub fn parse(marker: &str) -> Option<Self> {
        let body = marker
            .strip_prefix(IMAGE_ANCHOR_PREFIX)?
            .strip_suffix(IMAGE_ANCHOR_SUFFIX)?;
        let mut fields = body.split(',');
        if fields.next()? != "v=1" {
            return None;
        }
        let protocol = match fields.next()?.strip_prefix("p=")? {
            "kitty" => ImageProtocol::Kitty,
            "iterm2" => ImageProtocol::Iterm2,
            _ => return None,
        };
        let id = ImageId::new(fields.next()?.strip_prefix("i=")?.parse().ok()?).ok()?;
        let columns = fields.next()?.strip_prefix("c=")?.parse().ok()?;
        let rows = fields.next()?.strip_prefix("r=")?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self::new(
            protocol,
            id,
            ImageLayout::new(columns, rows).ok()?,
        ))
    }

    /// Find every complete, bounded anchor in a rendered line. At most the
    /// global live-image limit is returned, so hostile text cannot create an
    /// unbounded parse result.
    pub fn parse_all(line: &str) -> Vec<Self> {
        let mut anchors = Vec::new();
        let mut search_from = 0;
        while anchors.len() < HARD_MAX_LIVE_IMAGES {
            let Some(relative_start) = line[search_from..].find(IMAGE_ANCHOR_PREFIX) else {
                break;
            };
            let start = search_from.saturating_add(relative_start);
            let body_start = start.saturating_add(IMAGE_ANCHOR_PREFIX.len());
            let Some(relative_end) = line[body_start..].find(IMAGE_ANCHOR_SUFFIX) else {
                break;
            };
            let end = body_start
                .saturating_add(relative_end)
                .saturating_add(IMAGE_ANCHOR_SUFFIX.len());
            if let Some(anchor) = Self::parse(&line[start..end]) {
                anchors.push(anchor);
            }
            search_from = end;
        }
        anchors
    }
}
