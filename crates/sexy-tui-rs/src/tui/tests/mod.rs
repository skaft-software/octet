//! Shared fixtures for the `crate::tui` test modules.
//!
//! `RecordingTerminal`, the one-line/mutable/lazy `Component`
//! stand-ins, and the pinned-frame builders live here because
//! every area below asserts against the same write tape. Each
//! area module reaches them through `use super::*`, which is also
//! why `crate::tui` is imported once, here.
//!
//! These assertions were extracted from `tui.rs`; each module is a
//! child of `crate::tui::tests`, so `use super::*` reaches exactly
//! the private items it reached while the tests were inline.

use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct RecordingTerminal {
    size: Rc<Cell<(u16, u16)>>,
    clears: Rc<Cell<usize>>,
    tail_clears: Rc<Cell<usize>>,
    stops: Rc<Cell<usize>>,
    shows: Rc<Cell<usize>>,
    writes: Rc<RefCell<Vec<String>>>,
    capabilities: crate::capabilities::TerminalCapabilities,
}

impl Terminal for RecordingTerminal {
    fn start_events(
        &mut self,
        _on_input: Box<dyn FnMut(TerminalInput)>,
        _on_resize: Box<dyn FnMut()>,
    ) {
    }
    fn stop(&mut self) {
        self.stops.set(self.stops.get() + 1);
    }
    fn write(&mut self, data: &str) {
        self.writes.borrow_mut().push(data.to_owned());
    }
    fn columns(&self) -> u16 {
        self.size.get().0
    }
    fn rows(&self) -> u16 {
        self.size.get().1
    }
    fn move_by(&mut self, _lines: i16) {}
    fn hide_cursor(&mut self) {}
    fn show_cursor(&mut self) {
        self.shows.set(self.shows.get() + 1);
    }
    fn clear_line(&mut self) {}
    fn clear_from_cursor(&mut self) {
        self.tail_clears.set(self.tail_clears.get() + 1);
    }
    fn clear_screen(&mut self) {
        self.clears.set(self.clears.get() + 1);
    }
    fn capabilities(&self) -> crate::capabilities::TerminalCapabilities {
        self.capabilities
    }
}

type RecordingParts = (
    RecordingTerminal,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
    Rc<Cell<usize>>,
    Rc<RefCell<Vec<String>>>,
);

fn recording_terminal(
    size: Rc<Cell<(u16, u16)>>,
    capabilities: crate::capabilities::TerminalCapabilities,
) -> RecordingParts {
    let clears = Rc::new(Cell::new(0));
    let tail_clears = Rc::new(Cell::new(0));
    let stops = Rc::new(Cell::new(0));
    let shows = Rc::new(Cell::new(0));
    let writes = Rc::new(RefCell::new(Vec::new()));
    (
        RecordingTerminal {
            size,
            clears: clears.clone(),
            tail_clears: tail_clears.clone(),
            stops: stops.clone(),
            shows: shows.clone(),
            writes: writes.clone(),
            capabilities,
        },
        clears,
        tail_clears,
        stops,
        shows,
        writes,
    )
}

fn test_commit_position(row: usize) -> CommitPosition {
    CommitPosition {
        cursor: CommitCursor {
            generation: 0,
            block: row as u64,
            segment: 0,
        },
        row,
    }
}

fn test_pinned_frame_with_stable(
    acknowledged: Option<CommitCursor>,
    target: Option<usize>,
    stable_rows: usize,
) -> PinnedFrame {
    PinnedFrame {
        generation: 0,
        acknowledged: acknowledged.map(|cursor| CommitPosition {
            row: cursor.block as usize,
            cursor,
        }),
        target: target.map(test_commit_position),
        stable_rows,
        viewport_surface: false,
    }
}

fn test_pinned_frame(acknowledged: Option<CommitCursor>, target: Option<usize>) -> PinnedFrame {
    test_pinned_frame_with_stable(acknowledged, target, target.unwrap_or(0))
}

struct OneLine;

impl Component for OneLine {
    fn render(&self, _width: u16) -> Vec<String> {
        vec!["line".to_owned()]
    }

    fn invalidate(&mut self) {}
}

struct MutableLines(Rc<RefCell<Vec<String>>>);

impl Component for MutableLines {
    fn render(&self, _width: u16) -> Vec<String> {
        self.0.borrow().clone()
    }

    fn invalidate(&mut self) {}
}

struct LazyTail {
    stable_prefix: usize,
    tail: Rc<RefCell<String>>,
    full_renders: Rc<Cell<usize>>,
    replacement_rows: Rc<Cell<usize>>,
}

impl Component for LazyTail {
    fn render(&self, _width: u16) -> Vec<String> {
        self.full_renders.set(self.full_renders.get() + 1);
        Vec::new()
    }

    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        self.replacement_rows.set(1);
        Some(FrameUpdate {
            stable_prefix: self.stable_prefix,
            replacement: vec![self.tail.borrow().clone()],
            pinned: None,
            resize_replay: None,
            reanchor_viewport: false,
            rebuild_scrollback: false,
        })
    }

    fn invalidate(&mut self) {}
}

struct LazyFallbackLines {
    lines: Rc<RefCell<Vec<String>>>,
    full_renders: Rc<Cell<usize>>,
}

impl Component for LazyFallbackLines {
    fn render(&self, _width: u16) -> Vec<String> {
        self.full_renders.set(self.full_renders.get() + 1);
        self.lines.borrow().clone()
    }

    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        Some(FrameUpdate {
            stable_prefix: 0,
            replacement: self.lines.borrow().clone(),
            pinned: None,
            resize_replay: None,
            reanchor_viewport: false,
            rebuild_scrollback: false,
        })
    }

    fn invalidate(&mut self) {}
}

struct LazyFixedLines {
    lines: Rc<RefCell<Vec<String>>>,
}

impl Component for LazyFixedLines {
    fn render(&self, _width: u16) -> Vec<String> {
        panic!("lazy fixed-height updates must not invoke the full renderer")
    }

    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        Some(FrameUpdate {
            stable_prefix: 0,
            replacement: self.lines.borrow().clone(),
            pinned: None,
            resize_replay: None,
            reanchor_viewport: false,
            rebuild_scrollback: false,
        })
    }

    fn invalidate(&mut self) {}
}

struct LazyObscuredLines {
    displayed: Rc<RefCell<Vec<String>>>,
    replay: Rc<RefCell<Vec<String>>>,
}

impl Component for LazyObscuredLines {
    fn render(&self, _width: u16) -> Vec<String> {
        panic!("lazy obscured updates must not invoke the full renderer")
    }

    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        Some(FrameUpdate {
            stable_prefix: 0,
            replacement: self.displayed.borrow().clone(),
            pinned: None,
            resize_replay: Some(self.replay.borrow().clone()),
            reanchor_viewport: false,
            rebuild_scrollback: false,
        })
    }

    fn invalidate(&mut self) {}
}

struct LazyPinnedLines {
    lines: Rc<RefCell<Vec<String>>>,
    commit_boundary: Rc<Cell<usize>>,
    rebuild_scrollback: Rc<Cell<bool>>,
    generation: Rc<Cell<u64>>,
}

impl Component for LazyPinnedLines {
    fn render(&self, _width: u16) -> Vec<String> {
        self.lines.borrow().clone()
    }

    fn render_update(&self, width: u16) -> Option<FrameUpdate> {
        self.render_update_with_cursor(width, None)
    }

    fn render_update_with_cursor(
        &self,
        _width: u16,
        cursor: Option<CommitCursor>,
    ) -> Option<FrameUpdate> {
        let boundary = self.commit_boundary.get();
        let generation = self.generation.get();
        let acknowledged = cursor
            .filter(|cursor| cursor.generation == generation)
            .map(|cursor| CommitPosition {
                row: cursor.block as usize,
                cursor,
            });
        let target = (boundary > 0).then_some(CommitPosition {
            cursor: CommitCursor {
                generation,
                block: boundary as u64,
                segment: 0,
            },
            row: boundary,
        });
        Some(FrameUpdate {
            stable_prefix: 0,
            replacement: self.lines.borrow().clone(),
            pinned: Some(PinnedFrame {
                generation,
                acknowledged,
                target,
                stable_rows: boundary,
                viewport_surface: false,
            }),
            resize_replay: None,
            reanchor_viewport: false,
            rebuild_scrollback: self.rebuild_scrollback.replace(false),
        })
    }

    fn invalidate(&mut self) {}
}

struct LazyReanchoredLines {
    lines: Rc<RefCell<Vec<String>>>,
    reanchor: Rc<Cell<bool>>,
    rebuild_scrollback: Rc<Cell<bool>>,
}

impl Component for LazyReanchoredLines {
    fn render(&self, _width: u16) -> Vec<String> {
        panic!("lazy viewport updates must not invoke the full renderer")
    }

    fn render_update(&self, _width: u16) -> Option<FrameUpdate> {
        Some(FrameUpdate {
            stable_prefix: 0,
            replacement: self.lines.borrow().clone(),
            pinned: None,
            resize_replay: None,
            reanchor_viewport: self.reanchor.replace(false),
            rebuild_scrollback: self.rebuild_scrollback.replace(false),
        })
    }

    fn invalidate(&mut self) {}
}

// --- Cohesive areas under test ---
mod lazy_updates;
mod native_scrollback;
mod pinned_frames;
mod presentation_lifecycle;
mod screen_output;
