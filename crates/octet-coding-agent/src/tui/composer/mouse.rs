//! Native-terminal pointer projection. The host owns terminal geometry and
//! selection; an extension adapter cannot position the host's hardware cursor.

use std::sync::Arc;

/// One painted text row, with source grapheme boundaries for each visible cell.
#[derive(Clone)]
pub(crate) struct ComposerMouseRow {
    pub(crate) row: usize,
    pub(crate) column: usize,
    pub(crate) cells: Vec<usize>,
    pub(crate) end: usize,
    pub(crate) click_end: usize,
}

/// Bounded to the visible composer, never to the transcript or entire draft.
#[derive(Clone, Default)]
pub(crate) struct ComposerMouseGeometry {
    pub(crate) top: usize,
    pub(crate) height: usize,
    pub(crate) rows: Vec<ComposerMouseRow>,
}

impl ComposerMouseGeometry {
    pub(crate) fn contains(&self, row: u16) -> bool {
        (self.top..self.top + self.height).contains(&usize::from(row))
    }

    pub(crate) fn offset(&self, row: u16, col: u16, click: bool) -> Option<usize> {
        let row = self.rows.iter().find(|line| line.row == usize::from(row))?;
        row.cells
            .get(usize::from(col).saturating_sub(row.column))
            .copied()
            .or(Some(if click { row.click_end } else { row.end }))
    }
}

/// A composer press participates in the screen's existing click/drag gesture.
/// Keep the pressed frame even if streaming paints new transcript rows.
pub(crate) struct ComposerPress {
    pub(crate) geometry: Arc<ComposerMouseGeometry>,
    pub(crate) anchor: Option<usize>,
}

/// Snap an intact attachment/paste placeholder to its nearest visible boundary.
/// Ordinary text and edited (no longer intact) chips remain grapheme-addressable.
pub(crate) fn atomic_cursor(text: &str, ledger: &super::AttachmentLedger, cursor: usize) -> usize {
    for entry in &ledger.entries {
        for (start, _) in text.match_indices(&entry.chip) {
            let end = start + entry.chip.len();
            if start < cursor && cursor < end {
                let left = sexy_tui_rs::visible_width(&text[start..cursor]);
                let right = sexy_tui_rs::visible_width(&text[cursor..end]);
                return if left < right { start } else { end };
            }
        }
    }
    cursor
}
