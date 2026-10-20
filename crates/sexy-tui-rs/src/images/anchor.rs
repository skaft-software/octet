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
use super::limits::{HARD_MAX_LIVE_IMAGES, MAX_RESERVED_IMAGE_ROWS};
use super::registry::ImageId;

/// The rows of a placement that are actually inside the visible window.
///
/// A retained viewport can cut an image: its anchor row may scroll above the
/// window, or its reserved rows may run past the window's last row. Kitty-
/// protocol terminals draw a placement over text at its full height, so an
/// uncropped placement would cover whatever follows the window (the composer,
/// the status bar). The crop names the visible slice of the placement's rows;
/// the encoder turns it into a source rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ImageCrop {
    skip_rows: u16,
    shown_rows: u16,
}

impl ImageCrop {
    /// Rows hidden above the window.
    pub const fn skip_rows(self) -> u16 {
        self.skip_rows
    }

    /// Rows drawn, starting at the anchor row.
    pub const fn shown_rows(self) -> u16 {
        self.shown_rows
    }
}

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
    crop: Option<ImageCrop>,
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
            crop: None,
        }
    }

    /// The same placement limited to `shown_rows` rows after skipping the first
    /// `skip_rows`. `None` when the window is empty or exceeds the layout; the
    /// full placement drops the crop.
    pub fn cropped(self, skip_rows: u16, shown_rows: u16) -> Option<Self> {
        let rows = self.layout.rows();
        if shown_rows == 0 || skip_rows.checked_add(shown_rows)? > rows {
            return None;
        }
        let crop = (skip_rows != 0 || shown_rows != rows).then_some(ImageCrop {
            skip_rows,
            shown_rows,
        });
        Some(Self { crop, ..self })
    }

    /// Visible slice of the placement's rows, when the window cut it.
    pub const fn crop(self) -> Option<ImageCrop> {
        self.crop
    }

    /// Rows the terminal draws for this anchor: the crop when present.
    pub const fn placed_rows(self) -> u16 {
        match self.crop {
            Some(crop) => crop.shown_rows,
            None => self.layout.rows(),
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
        let crop = self.crop.map_or_else(String::new, |crop| {
            format!(",t={},n={}", crop.skip_rows, crop.shown_rows)
        });
        format!(
            "{IMAGE_ANCHOR_PREFIX}v=1,p={protocol},i={},c={},r={}{crop}{}",
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
        let anchor = Self::new(protocol, id, ImageLayout::new(columns, rows).ok()?);
        let Some(skip) = fields.next() else {
            return Some(anchor);
        };
        let skip = skip.strip_prefix("t=")?.parse().ok()?;
        let shown = fields.next()?.strip_prefix("n=")?.parse().ok()?;
        if fields.next().is_some() {
            return None;
        }
        let cropped = anchor.cropped(skip, shown)?;
        // A crop that covers the whole placement is spelled without one.
        cropped.crop.is_some().then_some(cropped)
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

/// Limit every Kitty placement in `lines[start..end]` to that window.
///
/// Returns the window's rows. An anchor whose reserved rows run past `end` is
/// cropped to the rows that remain, and an image whose anchor row lies above
/// `start` but whose rows reach into the window is re-anchored on the first
/// window row with its hidden top skipped. Without this, a Kitty-protocol
/// terminal draws the whole placement over whatever follows the window.
pub fn crop_anchors_to_window(lines: &[String], start: usize, end: usize) -> Vec<String> {
    let end = end.min(lines.len());
    let start = start.min(end);
    let mut window = lines[start..end].to_vec();
    if window.is_empty() {
        return window;
    }
    for (offset, line) in window.iter_mut().enumerate() {
        if !line.contains(IMAGE_ANCHOR_PREFIX) {
            continue;
        }
        let remaining = end - start - offset;
        *line = rewrite_anchors(line, |anchor| {
            let rows = usize::from(anchor.layout().rows());
            if rows <= remaining {
                return Some(anchor);
            }
            // `remaining` < rows <= u16::MAX here.
            anchor.cropped(0, remaining as u16)
        });
    }
    let reach = usize::from(MAX_RESERVED_IMAGE_ROWS);
    let mut carried = String::new();
    let first = start.saturating_sub(reach);
    for (index, line) in lines[first..start].iter().enumerate() {
        let index = first + index;
        if !line.contains(IMAGE_ANCHOR_PREFIX) {
            continue;
        }
        for anchor in ImageAnchor::parse_all(line) {
            let rows = usize::from(anchor.layout().rows());
            let image_end = index + rows;
            if image_end <= start {
                continue;
            }
            let skip = start - index;
            let shown = image_end.min(end) - start;
            if let Some(cropped) = anchor.cropped(skip as u16, shown as u16) {
                carried.push_str(&cropped.marker());
            }
        }
    }
    if !carried.is_empty() {
        window[0].insert_str(0, &carried);
    }
    window
}

/// Replace each complete anchor in `line`; a `None` from `map` removes it.
fn rewrite_anchors(line: &str, mut map: impl FnMut(ImageAnchor) -> Option<ImageAnchor>) -> String {
    let mut out = String::with_capacity(line.len());
    let mut cursor = 0;
    while let Some(relative) = line[cursor..].find(IMAGE_ANCHOR_PREFIX) {
        let start = cursor + relative;
        let body = start + IMAGE_ANCHOR_PREFIX.len();
        let Some(relative_end) = line[body..].find(IMAGE_ANCHOR_SUFFIX) else {
            break;
        };
        let end = body + relative_end + IMAGE_ANCHOR_SUFFIX.len();
        out.push_str(&line[cursor..start]);
        match ImageAnchor::parse(&line[start..end]) {
            Some(anchor) => {
                if let Some(anchor) = map(anchor) {
                    out.push_str(&anchor.marker());
                }
            }
            None => out.push_str(&line[start..end]),
        }
        cursor = end;
    }
    out.push_str(&line[cursor..]);
    out
}
