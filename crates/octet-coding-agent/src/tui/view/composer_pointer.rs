//! Composer participation in the screen-level press/drag/release path.

use super::tern_completion::{Completion, Source};
use super::*;
use crate::tui::composer::mouse::{atomic_cursor, ComposerPress};

#[derive(Clone)]
pub(super) struct CompletionHit {
    row: usize,
    source: Source,
    index: usize,
    id: String,
    entry: (String, String),
}

pub(super) fn completion_hits(
    state: &ShellState,
    top: usize,
    height: usize,
    revision: u64,
) -> Vec<CompletionHit> {
    if height < 2 {
        return Vec::new();
    }
    let Some(menu) = Completion::capture_revision(state, revision) else {
        return Vec::new();
    };
    let count = height - 1;
    let start = match menu.source {
        Source::Extension => 0,
        Source::Path => menu.selected.saturating_sub(count - 1),
        Source::Slash => {
            let maximum = menu.entries.len().saturating_sub(count);
            let mut start = state.slash_scroll.min(maximum);
            if menu.selected < start {
                start = menu.selected;
            } else if menu.selected >= start + count {
                start = menu.selected + 1 - count;
            }
            start.min(maximum)
        }
    };
    menu.entries
        .iter()
        .enumerate()
        .skip(start)
        .take(count)
        .enumerate()
        .map(|(row, (index, entry))| CompletionHit {
            row: top + row,
            source: menu.source,
            index,
            id: menu.id.clone(),
            entry: entry.clone(),
        })
        .collect()
}

impl InteractiveShell {
    pub(super) fn begin_composer_pointer(&mut self, row: u16, col: u16, extend: bool) -> bool {
        let mut state = self.state.borrow_mut();
        if normal_editor_focused(&state) {
            let hit = state
                .composer_pointer_geometry()
                .and_then(|frame| {
                    frame
                        .completions
                        .iter()
                        .find(|hit| hit.row == usize::from(row))
                })
                .cloned();
            if let Some(hit) = hit {
                state.pending_screen_press = Some(ScreenPress {
                    cell: (row, col),
                    moved: extend,
                    fence: renderer_geometry::GeometryFence::capture(&state),
                    target: ScreenPressTarget::Completion(hit),
                });
                return true;
            }
        }
        // Pi custom editors own their cursor. Never move the native mirror.
        if !tern::editor_focused(&state) {
            return false;
        }
        let geometry = if state.render_threaded {
            state
                .composer_pointer_geometry()
                .map(|frame| frame.composer.clone())
        } else {
            let chrome = shell_chrome(&state, state.size.0, Instant::now());
            Some(Arc::new(
                crate::tui::composer_surface::composer_mouse_geometry(
                    &state,
                    chrome.transcript_rows
                        + chrome.header.len()
                        + chrome.error.len()
                        + chrome.pending.len()
                        + chrome.panel.len()
                        + chrome.extension_above.len()
                        + chrome.subagents.len()
                        + chrome.working.len(),
                    chrome.composer.len(),
                ),
            ))
        };
        let Some(geometry) = geometry.filter(|geometry| geometry.contains(row)) else {
            return false;
        };
        let anchor = geometry.offset(row, col, false);
        state.pending_screen_press = Some(ScreenPress {
            cell: (row, col),
            moved: extend,
            fence: renderer_geometry::GeometryFence::capture(&state),
            target: ScreenPressTarget::Composer(ComposerPress { geometry, anchor }),
        });
        state.selection_pointer_cell = Some((row, col));
        state.pending_selection_anchor = None;
        state.selection_dragging = false;
        state.transcript_selection = None;
        state.composer_selection = None;
        true
    }

    pub(super) fn extend_composer_pointer(state: &mut ShellState, row: u16, col: u16) -> bool {
        let Some(press) = state.pending_screen_press.as_ref() else {
            return false;
        };
        if matches!(press.target, ScreenPressTarget::Link(_)) {
            return false;
        }
        let focused = if matches!(press.target, ScreenPressTarget::Completion(_)) {
            normal_editor_focused(state)
        } else {
            tern::editor_focused(state)
        };
        if !press.fence.composer_current(state) || !focused {
            state.pending_screen_press = None;
            state.composer_selection = None;
            return true;
        }
        let press = state.pending_screen_press.as_mut().unwrap();
        press.moved |= press.cell != (row, col);
        let ScreenPressTarget::Composer(composer) = &press.target else {
            return true;
        };
        if press.moved {
            if let (Some(anchor), Some(focus)) =
                (composer.anchor, composer.geometry.offset(row, col, false))
            {
                state.composer_selection = Some((
                    state.editor.text_revision(),
                    anchor.min(focus)..anchor.max(focus),
                ));
            }
        }
        true
    }

    pub(super) fn end_composer_pointer(&mut self, row: u16, col: u16) {
        let mut state = self.state.borrow_mut();
        let Some(press) = state.pending_screen_press.take() else {
            return;
        };
        state.selection_pointer_cell = None;
        let composer = match press.target {
            ScreenPressTarget::Composer(composer) => composer,
            ScreenPressTarget::Completion(hit) => {
                // Pi compatibility: packages/tui/src/components/editor.ts:639-652
                // and components/select-list.ts:110-145 route popup clicks to
                // the list, not to the text cursor.
                if !press.moved
                    && press.cell == (row, col)
                    && Completion::capture(&state).is_some_and(|menu| {
                        menu.id == hit.id
                            && menu.source == hit.source
                            && menu.entries.get(hit.index) == Some(&hit.entry)
                    })
                {
                    match hit.source {
                        Source::Slash => state.slash_selection = hit.index,
                        Source::Path => state.path_selection = hit.index,
                        Source::Extension => {
                            state.extension_autocomplete_selection =
                                Some((state.editor.revision(), hit.index))
                        }
                    }
                    drop(state);
                    match hit.source {
                        Source::Slash => self.complete_slash_command(),
                        Source::Path => self.complete_path(),
                        Source::Extension => {
                            self.accept_extension_autocomplete();
                        }
                    }
                }
                return;
            }
            ScreenPressTarget::Link(_) => unreachable!(),
        };
        // Pi compatibility: packages/tui/src/components/editor.ts:655-700.
        // Only the screen's synthesized left click places a cursor. Presses,
        // movement (even away and back), and release remain selection-owned.
        if !press.moved && press.cell == (row, col) {
            if let Some(offset) = composer.geometry.offset(row, col, true) {
                let offset = atomic_cursor(state.editor.text(), &state.ledger, offset);
                let revision = state.editor.revision();
                state.editor.set_cursor(offset); // Ends undo coalescing, even at the same offset.
                if state.editor.revision() != revision {
                    invalidate_editor_autocomplete(&mut state);
                }
                state.prompt_history_navigation = None;
                state.composer_preferred_column = None;
            }
        } else if let Some((revision, range)) = &state.composer_selection {
            if *revision == state.editor.text_revision() && !range.is_empty() {
                let text = sanitize_for_terminal(&state.editor.text()[range.clone()]);
                state.copy_buffer = Some(text.clone());
                drop(state);
                Self::set_clipboard(&text);
            }
        }
    }
}
