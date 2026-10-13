//! Kitty graphics placement bookkeeping for the retained-frame renderer.
//!
//! Everything in this module answers one question: *which images does the
//! terminal currently hold, and which of them must be deleted before these
//! rows are repainted?* It is separate from the renderers themselves because
//! the answer is a property of the Kitty wire protocol, not of any one
//! renderer: the normative Pi differential path, the inline-scrollback
//! compatibility path and the destructive replay fallback all consult the same
//! predicates and the same deletion escapes, and a divergence between them
//! would leak orphaned placements rather than produce a wrong frame.
//!
//! Two shapes of row can carry a placement: a raw `\x1b_G` graphics escape
//! (which this module parses) and an [`ImageAnchor`] emitted by the image
//! subsystem. Both are folded into one id set so the two encodings can never
//! disagree about what is on screen.

use std::collections::BTreeSet;

use crate::images::{ImageAnchor, ImageProtocol};
use crate::utils::visible_width;

use super::TUI;

/// Whether a rendered row carries a Kitty graphics placement.
pub(crate) fn is_image_line(line: &str) -> bool {
    line.contains("\x1b_G")
        || ImageAnchor::parse_all(line)
            .iter()
            .any(|anchor| anchor.protocol() == ImageProtocol::Kitty)
}

/// Kitty graphics protocol escape that deletes every placed image. Destructive
/// inline replays must emit it before rebuilding rows they may have erased.
pub(crate) fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

const KITTY_SEQUENCE_PREFIX: &str = "\x1b_G";
#[derive(Clone, Debug)]
struct KittyImageHeader {
    ids: Vec<u32>,
    rows: usize,
}

fn parse_kitty_image_headers(line: &str) -> Vec<KittyImageHeader> {
    let mut headers = Vec::new();
    let mut search_from = 0;
    while let Some(relative_start) = line[search_from..].find(KITTY_SEQUENCE_PREFIX) {
        let sequence_start = search_from.saturating_add(relative_start);
        let params_start = sequence_start.saturating_add(KITTY_SEQUENCE_PREFIX.len());
        let Some(relative_end) = line[params_start..].find(';') else {
            break;
        };
        let params_end = params_start.saturating_add(relative_end);
        let mut header = KittyImageHeader {
            ids: Vec::new(),
            rows: 1,
        };
        for parameter in line[params_start..params_end].split(',') {
            let Some((key, value)) = parameter.split_once('=') else {
                continue;
            };
            let Ok(value) = value.parse::<u32>() else {
                continue;
            };
            if value == 0 {
                continue;
            }
            match key {
                "i" => header.ids.push(value),
                "r" => header.rows = value as usize,
                _ => {}
            }
        }
        headers.push(header);
        search_from = params_end.saturating_add(1);
    }
    headers
}

fn extract_kitty_image_ids(line: &str) -> Vec<u32> {
    let mut ids = parse_kitty_image_headers(line)
        .into_iter()
        .flat_map(|header| header.ids)
        .collect::<Vec<_>>();
    ids.extend(
        ImageAnchor::parse_all(line)
            .into_iter()
            .filter(|anchor| anchor.protocol() == ImageProtocol::Kitty)
            .map(|anchor| anchor.id().get()),
    );
    ids
}

fn extract_kitty_image_rows(line: &str) -> usize {
    parse_kitty_image_headers(line)
        .into_iter()
        .map(|header| header.rows)
        .chain(
            ImageAnchor::parse_all(line)
                .into_iter()
                .filter(|anchor| anchor.protocol() == ImageProtocol::Kitty)
                .map(|anchor| usize::from(anchor.layout().rows())),
        )
        .max()
        .unwrap_or(1)
}

pub(super) fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

impl<'a> TUI<'a> {
    pub(super) fn pi_collect_kitty_image_ids(lines: &[String]) -> BTreeSet<u32> {
        lines
            .iter()
            .flat_map(|line| extract_kitty_image_ids(line))
            .collect()
    }

    pub(super) fn pi_record_kitty_state(&mut self, lines: &[String], known_image_free: bool) {
        if known_image_free {
            self.previous_frame_has_kitty = false;
            self.previous_kitty_image_ids.clear();
            return;
        }
        self.previous_frame_has_kitty = lines.iter().any(|line| is_image_line(line));
        self.previous_kitty_image_ids = Self::pi_collect_kitty_image_ids(lines);
    }

    pub(super) fn pi_delete_kitty_images(&self, ids: &BTreeSet<u32>) -> String {
        ids.iter()
            .map(|image_id| delete_kitty_image(*image_id))
            .collect()
    }

    pub(super) fn pi_kitty_image_reserved_rows(
        &self,
        lines: &[String],
        index: usize,
        max_index: usize,
    ) -> usize {
        let rows = lines
            .get(index)
            .map_or(1, |line| extract_kitty_image_rows(line));
        if rows <= 1 {
            return 1;
        }
        let max_rows = rows
            .min(max_index.saturating_sub(index).saturating_add(1))
            .min(lines.len().saturating_sub(index));
        let mut reserved_rows = 1;
        while reserved_rows < max_rows {
            let line = lines
                .get(index.saturating_add(reserved_rows))
                .map_or("", String::as_str);
            if is_image_line(line) || visible_width(line) > 0 {
                break;
            }
            reserved_rows = reserved_rows.saturating_add(1);
        }
        reserved_rows
    }

    pub(super) fn pi_expand_changed_range_for_kitty_images(
        &self,
        first_changed: usize,
        last_changed: usize,
        new_lines: &[String],
        previous_lines: &[String],
        previous_offset: usize,
    ) -> (usize, usize) {
        let mut expanded_first = first_changed;
        let mut expanded_last = last_changed;
        for (index, line) in previous_lines.iter().enumerate() {
            if extract_kitty_image_ids(line).is_empty() {
                continue;
            }
            let block_end = previous_offset
                .saturating_add(index)
                .saturating_add(self.pi_kitty_image_reserved_rows(
                    previous_lines,
                    index,
                    previous_lines.len().saturating_sub(1),
                ))
                .saturating_sub(1);
            let absolute_index = previous_offset.saturating_add(index);
            if absolute_index >= first_changed
                || (absolute_index <= last_changed && block_end >= first_changed)
            {
                expanded_first = expanded_first.min(absolute_index);
                expanded_last = expanded_last.max(block_end);
            }
        }
        for index in 0..new_lines.len() {
            if extract_kitty_image_ids(&new_lines[index]).is_empty() {
                continue;
            }
            let block_end = index
                .saturating_add(self.pi_kitty_image_reserved_rows(
                    new_lines,
                    index,
                    new_lines.len().saturating_sub(1),
                ))
                .saturating_sub(1);
            if index >= first_changed || (index <= last_changed && block_end >= first_changed) {
                expanded_first = expanded_first.min(index);
                expanded_last = expanded_last.max(block_end);
            }
        }
        (expanded_first, expanded_last)
    }

    pub(super) fn pi_delete_changed_kitty_images(
        &self,
        first_changed: usize,
        last_changed: usize,
        previous_lines: &[String],
        previous_offset: usize,
    ) -> String {
        if last_changed < first_changed || previous_lines.is_empty() {
            return String::new();
        }
        let first = first_changed
            .max(previous_offset)
            .saturating_sub(previous_offset);
        let last = last_changed
            .saturating_sub(previous_offset)
            .min(previous_lines.len().saturating_sub(1));
        if first > last {
            return String::new();
        }
        let mut ids = BTreeSet::new();
        for line in &previous_lines[first..=last] {
            ids.extend(extract_kitty_image_ids(line));
        }
        self.pi_delete_kitty_images(&ids)
    }
}
