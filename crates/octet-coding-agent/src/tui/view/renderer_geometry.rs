//! Bounded, revision-fenced geometry from a completed terminal frame.
//! Input navigation reads these rows rather than invoking transcript layout.
use super::{transcript_cache::SurfaceGeometry, ShellState};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct GeometryFence {
    transcript: u64,
    semantic: u64,
    theme: u64,
    input: u64,
    tool_input: u64,
    panel: u64,
    size: (u16, u16),
    verbose: bool,
    overlay: bool,
    scroll: usize,
    follow_tail: bool,
    anchor: Option<super::ViewportAnchor>,
}
impl GeometryFence {
    pub(super) fn capture(state: &ShellState) -> Self {
        Self {
            transcript: state.transcript_epoch,
            semantic: state.transcript_semantic_revision,
            theme: state.theme_epoch,
            input: state.editor.revision(),
            tool_input: state.tool_input_revision,
            panel: state.panel_epoch,
            size: state.size,
            verbose: state.verbose_tools,
            overlay: state.overlay.is_some(),
            scroll: state.scroll_from_bottom.get(),
            follow_tail: state.follow_tail,
            anchor: state.viewport_anchor.get(),
        }
    }
}

#[derive(Clone)]
pub(super) struct VisibleBlockGeometry {
    pub(super) index: usize,
    pub(super) id: u64,
    pub(super) start: usize,
    pub(super) rows: usize,
    pub(super) surface: SurfaceGeometry,
}

/// Panel geometry has its own ownership fence. Background transcript output
/// cannot invalidate a visible approval or force document layout on input.
#[derive(Clone)]
pub(super) struct PanelRenderReceipt {
    pub(super) panel_epoch: u64,
    pub(super) size: (u16, u16),
    pub(super) theme_epoch: u64,
    pub(super) confirmation: Option<super::panel_render::ConfirmationRenderMetadata>,
    pub(super) document_rows: usize,
    pub(super) document_body_rows: usize,
    pub(super) document_scroll: Option<usize>,
    pub(super) document_anchor: Option<usize>,
}
impl PanelRenderReceipt {
    pub(super) fn capture(state: &ShellState, rows: &[String]) -> Option<Self> {
        let panel = state.panel.as_ref()?;
        let (document_rows, document_scroll) = match panel {
            super::Panel::ReadOnlyDocument {
                text,
                styled,
                scroll_from_bottom,
                ..
            } => (
                super::panel_render::document_visual_row_count_styled(
                    text,
                    &state.theme,
                    state.size.0,
                    *styled,
                ),
                Some(*scroll_from_bottom),
            ),
            _ => (0, None),
        };
        Some(Self {
            panel_epoch: state.panel_epoch,
            size: state.size,
            theme_epoch: state.theme_epoch,
            confirmation: if matches!(
                panel,
                super::Panel::SelectList {
                    action: super::PanelAction::Confirmation,
                    ..
                }
            ) {
                super::panel_render::confirmation_metadata_for_rendered_panel(
                    state,
                    state.size.0,
                    rows,
                )
            } else {
                None
            },
            document_rows,
            document_body_rows: super::panel_render::document_body_rows(
                state,
                state.size.0,
                rows.len(),
            ),
            document_scroll,
            document_anchor: state.pending_panel_document_top,
        })
    }
    pub(super) fn is_current(&self, state: &ShellState) -> bool {
        self.panel_epoch == state.panel_epoch
            && self.size == state.size
            && self.theme_epoch == state.theme_epoch
            && state.panel.is_some()
    }
    pub(super) fn selection_is_current(&self, state: &ShellState) -> bool {
        let Some(metadata) = self.confirmation.as_ref() else {
            return true;
        };
        let Some(super::Panel::SelectList {
            action: super::PanelAction::Confirmation,
            items,
            selected,
            ..
        }) = state.panel.as_ref()
        else {
            return false;
        };
        items.get(*selected).is_some_and(|label| {
            super::panel_render::confirmation_enter_allowed(
                Some(metadata),
                *selected,
                label,
                state.theme.unicode(),
            )
        })
    }
}

#[derive(Clone)]
pub(super) struct ReportRenderReceipt {
    body: super::ReportBody,
    size: (u16, u16),
    theme_epoch: u64,
    pub(super) maximum: usize,
    pub(super) page_rows: usize,
}
impl ReportRenderReceipt {
    fn capture(state: &ShellState) -> Option<Self> {
        let super::ShellOverlay::Report(report) = state.overlay.as_ref()? else {
            return None;
        };
        let (maximum, page_rows) = super::viewport::report_scroll_metrics_for_state(state)?;
        Some(Self {
            body: report.body.clone(),
            size: state.size,
            theme_epoch: state.theme_epoch,
            maximum,
            page_rows,
        })
    }
    pub(super) fn is_current(&self, state: &ShellState) -> bool {
        let Some(super::ShellOverlay::Report(report)) = state.overlay.as_ref() else {
            return false;
        };
        if self.size != state.size || self.theme_epoch != state.theme_epoch {
            return false;
        }
        match (&self.body, &report.body) {
            (
                super::ReportBody::Text {
                    text: left,
                    styled: a,
                },
                super::ReportBody::Text {
                    text: right,
                    styled: b,
                },
            ) => a == b && Arc::ptr_eq(left, right),
            (super::ReportBody::Markdown(left), super::ReportBody::Markdown(right)) => {
                Arc::ptr_eq(left, right)
            }
            (super::ReportBody::Context(left), super::ReportBody::Context(right)) => {
                Arc::ptr_eq(left, right)
            }
            _ => false,
        }
    }
}

pub(super) struct RenderedGeometry {
    fence: GeometryFence,
    pub(super) panel: Option<PanelRenderReceipt>,
    pub(super) report: Option<ReportRenderReceipt>,
    pub(super) generation: u64,
    pub(super) total_rows: usize,
    pub(super) visible_start: usize,
    pub(super) visible_lines: Vec<String>,
    pub(super) viewport_rows: usize,
    pub(super) blocks: Vec<VisibleBlockGeometry>,
}
impl RenderedGeometry {
    pub(super) fn capture(state: &ShellState, mut fence: GeometryFence) -> Arc<Self> {
        let chrome =
            super::shell_chrome::shell_chrome(state, state.size.0, std::time::Instant::now());
        let total_rows = state.transcript_cache.borrow().lines.len();
        let scroll =
            super::viewport::resolved_scroll_from_bottom(state, total_rows, chrome.transcript_rows);
        let viewport_rows =
            super::viewport::transcript_viewport_capacity(chrome.transcript_rows, scroll > 0);
        let end = total_rows.saturating_sub(scroll);
        let start = end.saturating_sub(viewport_rows);
        fence.scroll = state.scroll_from_bottom.get();
        fence.anchor = state.viewport_anchor.get();
        let cache = state.transcript_cache.borrow();
        let first_block = cache
            .block_starts
            .partition_point(|row| *row <= start)
            .saturating_sub(1);
        let blocks = (first_block..cache.block_starts.len())
            .take_while(|index| cache.block_starts[*index] < end)
            .filter_map(|index| {
                Some(VisibleBlockGeometry {
                    index,
                    id: *state.transcript_commit_ids.get(index)?,
                    start: cache.block_starts[index],
                    rows: cache.block_lengths[index],
                    surface: cache.block_geometries[index],
                })
            })
            .collect();
        Arc::new(Self {
            fence,
            panel: PanelRenderReceipt::capture(state, &chrome.panel),
            report: ReportRenderReceipt::capture(state),
            generation: cache.generation,
            total_rows,
            visible_start: start,
            visible_lines: cache.lines[start..end].to_vec(),
            viewport_rows,
            blocks,
        })
    }
    pub(super) fn is_current(&self, state: &ShellState) -> bool {
        self.fence == GeometryFence::capture(state)
    }
}
impl ShellState {
    /// `None` means no matching emitted frame; ignore pointer input and request
    /// a new paint, rather than guessing coordinates or laying out on input.
    pub(super) fn retained_render_geometry(&self) -> Option<&RenderedGeometry> {
        self.render_geometry
            .as_deref()
            .filter(|geometry| geometry.is_current(self))
    }
}
