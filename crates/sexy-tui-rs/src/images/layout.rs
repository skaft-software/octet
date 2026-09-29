//! Turning image dimensions into a bounded cell reservation.
//!
//! A terminal places an image by how many cells it occupies, so the only layout
//! question that matters is how a pixel box becomes a row-and-column
//! reservation. This module owns that arithmetic and nothing else: it takes
//! validated dimensions plus the viewport's cell size, and returns a
//! [`ImageLayout`] clamped to hard row and column ceilings, or a
//! [`ImageReservation`] that additionally reserves the semantic rows a renderer
//! must leave behind it.
//!
//! It is separate from [`super::format`] because a pixel measurement is not a
//! container fact, and from [`super::plan`] because a reservation is pure
//! geometry while a plan also has to decide whether the image is emitted at
//! all. The clamp lives here because an unbounded reservation is the one way
//! this subsystem could be talked into painting over a whole screen.

use super::capabilities::ImageCapabilities;
use super::error::ImageError;
use super::format::ImageDimensions;
use super::helpers::{ceil_div_u128, ceil_div_u64};
use super::limits::{MAX_IMAGE_CELL_COLUMNS, MAX_RESERVED_IMAGE_ROWS};
use super::payload::TerminalImage;
use crate::capabilities::CellPixelSize;

/// Terminal geometry used to derive a bounded image cell placement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageViewport {
    columns: u16,
    rows: u16,
    cell_pixel_size: Option<CellPixelSize>,
    estimated_cell_pixel_size: Option<CellPixelSize>,
}

impl ImageViewport {
    /// Build a viewport from terminal cell dimensions and an optional validated
    /// cell-pixel report.
    pub fn new(
        columns: u16,
        rows: u16,
        cell_pixel_size: Option<CellPixelSize>,
    ) -> Result<Self, ImageError> {
        if columns == 0 || rows == 0 {
            return Err(ImageError::InvalidLayout);
        }
        Ok(Self {
            columns,
            rows,
            cell_pixel_size,
            estimated_cell_pixel_size: None,
        })
    }

    /// Build a viewport while carrying the cell measurement from terminal
    /// image capabilities.
    pub fn with_capabilities(
        columns: u16,
        rows: u16,
        capabilities: ImageCapabilities,
    ) -> Result<Self, ImageError> {
        Self::new(columns, rows, capabilities.cell_pixel_size())
    }

    /// Use an approximate cell aspect only when no measured cell size exists.
    /// The reservation stays bounded in cells, but the displayed image's aspect
    /// may differ on terminals with unusual fonts. Callers needing exact
    /// geometry should leave this unset and use the one-cell fallback.
    pub fn with_estimated_cell_pixels(mut self, size: CellPixelSize) -> Self {
        self.estimated_cell_pixel_size = Some(size);
        self
    }

    /// Available character-cell columns.
    pub const fn columns(self) -> u16 {
        self.columns
    }

    /// Available character-cell rows.
    pub const fn rows(self) -> u16 {
        self.rows
    }

    /// Optional pixel size of one character cell.
    pub const fn cell_pixel_size(self) -> Option<CellPixelSize> {
        self.cell_pixel_size
    }
}

/// A bounded terminal image placement in character cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLayout {
    columns: u16,
    rows: u16,
}

impl ImageLayout {
    /// Validate an explicit placement. Explicit layouts remain bounded so a
    /// semantic reservation never allocates an unbounded number of rows.
    pub fn new(columns: u16, rows: u16) -> Result<Self, ImageError> {
        if columns == 0
            || rows == 0
            || columns > MAX_IMAGE_CELL_COLUMNS
            || rows > MAX_RESERVED_IMAGE_ROWS
        {
            return Err(ImageError::InvalidLayout);
        }
        Ok(Self { columns, rows })
    }

    /// Fit dimensions into a viewport without upscaling.
    ///
    /// With a measured cell size (or a caller's explicit approximate fallback),
    /// the calculation uses checked wide integer arithmetic and caps both axes
    /// to the viewport and semantic reservation limits. A measurement takes
    /// precedence over an estimate. With neither, reserve one cell by one cell.
    pub fn fit(dimensions: ImageDimensions, viewport: ImageViewport) -> Result<Self, ImageError> {
        let max_columns = viewport.columns.min(MAX_IMAGE_CELL_COLUMNS);
        let max_rows = viewport.rows.min(MAX_RESERVED_IMAGE_ROWS);
        if max_columns == 0 || max_rows == 0 {
            return Err(ImageError::InvalidLayout);
        }
        let Some(cell) = viewport
            .cell_pixel_size
            .or(viewport.estimated_cell_pixel_size)
        else {
            return Self::new(1, 1);
        };

        let image_width = u64::from(dimensions.width);
        let image_height = u64::from(dimensions.height);
        let cell_width = u64::from(cell.width());
        let cell_height = u64::from(cell.height());
        let max_width = u64::from(max_columns)
            .checked_mul(cell_width)
            .ok_or(ImageError::InvalidLayout)?;
        let max_height = u64::from(max_rows)
            .checked_mul(cell_height)
            .ok_or(ImageError::InvalidLayout)?;

        let (target_width, target_height) =
            if image_width <= max_width && image_height <= max_height {
                (image_width, image_height)
            } else if u128::from(max_width) * u128::from(image_height)
                <= u128::from(max_height) * u128::from(image_width)
            {
                let height = ceil_div_u128(
                    u128::from(image_height) * u128::from(max_width),
                    u128::from(image_width),
                );
                (
                    max_width,
                    u64::try_from(height).map_err(|_| ImageError::InvalidLayout)?,
                )
            } else {
                let width = ceil_div_u128(
                    u128::from(image_width) * u128::from(max_height),
                    u128::from(image_height),
                );
                (
                    u64::try_from(width).map_err(|_| ImageError::InvalidLayout)?,
                    max_height,
                )
            };

        let columns = ceil_div_u64(target_width, cell_width).min(u64::from(max_columns));
        let rows = ceil_div_u64(target_height, cell_height).min(u64::from(max_rows));
        Self::new(
            u16::try_from(columns).map_err(|_| ImageError::InvalidLayout)?,
            u16::try_from(rows).map_err(|_| ImageError::InvalidLayout)?,
        )
    }

    /// Placement width in cells.
    pub const fn columns(self) -> u16 {
        self.columns
    }

    /// Reserved semantic rows and placement height in cells.
    pub const fn rows(self) -> u16 {
        self.rows
    }
}

/// Compute a safe number of terminal rows for a pixel height.
///
/// If no cell-pixel report is available, this returns one rather than guessing
/// a terminal aspect ratio. A result too large for a terminal-cell reservation
/// is an error, never a wrapping arithmetic result.
pub fn cell_rows_for_pixels(
    image_height: u32,
    cell_pixel_size: Option<CellPixelSize>,
) -> Result<u16, ImageError> {
    if image_height == 0 {
        return Err(ImageError::InvalidDimensions);
    }
    let Some(cell) = cell_pixel_size else {
        return Ok(1);
    };
    let rows = ceil_div_u64(u64::from(image_height), u64::from(cell.height()));
    let rows = u16::try_from(rows).map_err(|_| ImageError::InvalidLayout)?;
    if rows == 0 || rows > MAX_RESERVED_IMAGE_ROWS {
        return Err(ImageError::InvalidLayout);
    }
    Ok(rows)
}

/// Why a plan contains semantic fallback text instead of protocol output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFallbackReason {
    /// No interactive terminal image protocol is available.
    UnsupportedTerminal,
    /// The selected terminal protocol does not accept this source format.
    UnsupportedFormat,
    /// The selected protocol cannot safely replace the target image.
    UnsupportedOperation,
}

/// A semantic-only image reservation.
///
/// The type stores only bounded blank rows or generated ASCII fallback text. It
/// never stores protocol sequences, base64, payload bytes, or caller filenames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageReservation {
    rows: u16,
    fallback: Option<String>,
}

impl ImageReservation {
    pub(super) fn blank(layout: ImageLayout) -> Self {
        Self {
            rows: layout.rows,
            fallback: None,
        }
    }

    pub(super) fn fallback(image: &TerminalImage, reason: ImageFallbackReason) -> Self {
        let reason = match reason {
            ImageFallbackReason::UnsupportedTerminal => "unsupported terminal",
            ImageFallbackReason::UnsupportedFormat => "unsupported format",
            ImageFallbackReason::UnsupportedOperation => "unsupported operation",
        };
        let dimensions = image.dimensions();
        Self {
            rows: 1,
            fallback: Some(format!(
                "[image: {} {}x{} ({reason})]",
                image.format().name(),
                dimensions.width(),
                dimensions.height(),
            )),
        }
    }

    /// Number of semantic rows reserved for this image or fallback.
    pub const fn rows(&self) -> u16 {
        self.rows
    }

    /// Generate semantic frame rows. These rows are safe to select, copy, log,
    /// and send through a text renderer; they cannot contain protocol bytes.
    pub fn semantic_rows(&self) -> Vec<String> {
        let mut rows = Vec::with_capacity(usize::from(self.rows));
        if let Some(fallback) = &self.fallback {
            rows.push(fallback.clone());
        } else {
            rows.resize(usize::from(self.rows), String::new());
        }
        rows
    }

    /// Generate the copy/log representation of the reservation.
    pub fn semantic_copy_text(&self) -> String {
        self.semantic_rows().join("\n")
    }

    /// Whether this reservation contains deterministic fallback text.
    pub const fn is_fallback(&self) -> bool {
        self.fallback.is_some()
    }
}
