//! Turning the child tree into a frame, and describing what changed.
//!
//! This module owns the two ends of a retained frame. It *composes* a frame by
//! asking the active children to render at a width, normalizes each produced
//! row to that width, and reduces the result to the lazy-update handshake the
//! renderers consume. It also owns [`FrameChangeHints`], the row-level facts
//! that must be captured while the previous frame is still alive: a lazy
//! replacement moves that frame into the next one, so a renderer cannot
// rediscover "which rows changed, and did any of them hold an image" after the
//! fact without walking a frame that is already gone.
//!
//! It is separate from the renderers because none of it writes to the
//! terminal. A renderer decides *which* rows to touch; this module decides what
//! the rows are and what the difference between two of them means, so the three
//! renderers cannot disagree about either.

use std::cmp::Ordering;

use crate::utils::visible_width;

use super::kitty::is_image_line;
use super::CURSOR_MARKER;
use super::{CommitCursor, FrameUpdate, TUI};

/// Exact row-level facts captured while the old retained frame is still
/// available. A lazy update moves that frame into the next frame, so terminal
/// writing must not try to rediscover these facts afterward.
#[derive(Debug)]
pub(super) struct FrameChangeHints {
    pub(super) first_changed: usize,
    pub(super) fixed_height: Option<FixedHeightChangeHints>,
    pub(super) affected_tail_has_image: bool,
}

#[derive(Debug)]
pub(super) struct FixedHeightChangeHints {
    pub(super) last_changed: Option<usize>,
    pub(super) changed_rows: Vec<usize>,
    pub(super) image_rows: Vec<usize>,
}

pub(super) fn frame_change_hints(
    previous: &[String],
    stable_prefix: usize,
    replacement: &[String],
) -> FrameChangeHints {
    let previous_tail = &previous[stable_prefix..];
    let mut changed_rows = Vec::new();
    let mut image_rows = Vec::new();
    for (offset, (old, new)) in previous_tail.iter().zip(replacement).enumerate() {
        if old == new {
            continue;
        }
        let row = stable_prefix.saturating_add(offset);
        changed_rows.push(row);
        if is_image_line(old) || is_image_line(new) {
            image_rows.push(row);
        }
    }
    let shared_len = previous_tail.len().min(replacement.len());
    let first_changed = changed_rows
        .first()
        .copied()
        .unwrap_or_else(|| stable_prefix.saturating_add(shared_len));
    let changed_offset = first_changed.saturating_sub(stable_prefix);
    let affected_tail_has_image = previous_tail[changed_offset.min(previous_tail.len())..]
        .iter()
        .chain(replacement[changed_offset.min(replacement.len())..].iter())
        .any(|line| is_image_line(line));
    let fixed_height =
        (stable_prefix.saturating_add(replacement.len()) == previous.len()).then(|| {
            FixedHeightChangeHints {
                last_changed: changed_rows.last().copied(),
                changed_rows,
                image_rows,
            }
        });
    FrameChangeHints {
        first_changed,
        fixed_height,
        affected_tail_has_image,
    }
}

impl<'a> TUI<'a> {
    pub(super) fn root_render_update_without_cursor(&self, width: u16) -> Option<FrameUpdate> {
        // Pi does not consume a semantic commit handshake. None passed to the
        // cursor-aware API means bootstrap, not opt-out of that metadata work.
        (self.children.len() == 1)
            .then(|| self.children[0].render_update(width))
            .flatten()
    }

    pub(super) fn root_render_update(
        &self,
        width: u16,
        cursor: Option<CommitCursor>,
    ) -> Option<FrameUpdate> {
        // Lazy updates require exactly one child: a multi-child frame has no
        // single stable prefix to reuse.
        (self.children.len() == 1)
            .then(|| self.children[0].render_update_with_cursor(width, cursor))
            .flatten()
    }

    pub(super) fn root_render(&self, width: u16) -> Vec<String> {
        let mut lines = Vec::new();
        for child in &self.children {
            lines.extend(child.render(width));
        }
        lines
    }

    pub(super) fn prepare_line(&self, line: String, width: u16) -> String {
        if self.capabilities.plain {
            ensure_plain_line(&line, width)
        } else if self.inline_scrollback {
            clip_line_width(&line, width)
        } else {
            ensure_line_width(&line, width)
        }
    }
}

/// Ensure a line is exactly `width` columns wide without splitting a grapheme
/// or an ANSI sequence.
fn ensure_line_width(line: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    let visible = visible_width(line);
    match visible.cmp(&width) {
        Ordering::Less => format!("{}{}", line, " ".repeat(width - visible)),
        Ordering::Greater => crate::utils::truncate_to_width(line, width, Some("")),
        Ordering::Equal => line.to_owned(),
    }
}

/// Clip a line to `width` columns without right-padding it. Used by inline
/// scrollback mode, where trailing pad spaces would pollute native selection.
fn clip_line_width(line: &str, width: u16) -> String {
    let width = usize::from(width);
    if width == 0 {
        return String::new();
    }
    if visible_width(line) > width {
        crate::utils::truncate_to_width(line, width, Some(""))
    } else {
        line.to_owned()
    }
}

fn ensure_plain_line(line: &str, width: u16) -> String {
    let mut safe = String::new();
    for (index, part) in line.split(CURSOR_MARKER).enumerate() {
        if index > 0 {
            safe.push_str(CURSOR_MARKER);
        }
        safe.push_str(&crate::sanitize::sanitize_line(part, true));
    }
    crate::utils::truncate_to_width(
        &safe,
        usize::from(width),
        Some(crate::GlyphSet::ASCII.ellipsis),
    )
}
