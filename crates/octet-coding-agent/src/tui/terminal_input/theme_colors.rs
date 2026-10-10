//! Queried Pi system-theme colors, under the ordinary frontend input owner.
use super::*;

/// A complete terminal palette, never a mixture of partial OSC 4 replies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TerminalThemeColors {
    pub(crate) foreground: Option<(u8, u8, u8)>,
    pub(crate) background: Option<(u8, u8, u8)>,
    pub(crate) palette: Option<[(u8, u8, u8); 16]>,
}

/// Returns whether the active selector is Pi, also used to refresh on focus.
pub(crate) type ThemeColorHandler = Arc<dyn Fn(TerminalThemeColors) -> bool + Send + Sync>;

#[derive(Clone, Copy)]
pub(super) enum ColorTarget {
    Foreground,
    Palette(usize),
}

impl BackgroundReplies {
    pub(super) fn begin_theme_query(&mut self, now: Instant) -> Vec<u8> {
        self.theme_queried = true;
        let mut query = Vec::new();
        if self.begin_query(now) {
            query.extend_from_slice(b"\x1b]11;?\x1b\\");
        }
        // No timeout can safely turn an outstanding reply into editor input.
        if self.pending_slots & (1 << 16) == 0 {
            self.pending_slots |= 1 << 16;
            query.extend_from_slice(b"\x1b]10;?\x1b\\");
        }
        if self.pending_slots & 0xffff == 0 {
            self.pending_slots |= 0xffff;
            self.received_palette = [None; 16];
            for slot in 0..16 {
                query.extend_from_slice(format!("\x1b]4;{slot};?\x1b\\").as_bytes());
            }
        }
        query
    }

    pub(super) fn publish_theme_colors(&self) {
        if let Some(handler) = &self.theme_handler {
            let _ = handler(self.theme_colors);
        }
    }

    pub(super) fn accept_theme_color(&mut self, target: ColorTarget, color: RgbColor) {
        let rgb = (color.r, color.g, color.b);
        match target {
            ColorTarget::Foreground => {
                self.pending_slots &= !(1 << 16);
                self.theme_colors.foreground = Some(rgb);
            }
            ColorTarget::Palette(slot) => {
                self.pending_slots &= !(1 << slot);
                self.received_palette[slot] = Some(rgb);
                if self.pending_slots & 0xffff == 0 {
                    self.theme_colors.palette = Some(self.received_palette.map(Option::unwrap));
                }
            }
        }
        self.publish_theme_colors();
    }

    pub(super) fn identified_color_reply(&self, text: &str) -> bool {
        text.starts_with(OSC11_PREFIX)
            || self.pending_slots & (1 << 16) != 0 && text.starts_with("\x1b]10;")
            || (0..16).any(|slot| {
                self.pending_slots & (1 << slot) != 0
                    && text.starts_with(&format!("\x1b]4;{slot};"))
            })
    }

    pub(super) fn candidate_status(&self, text: &str) -> Candidate {
        let background = candidate_status(text);
        if !matches!(background, Candidate::NotReply) && self.pending {
            return background;
        }
        if text.len() > MAX_REPLY_BYTES {
            return Candidate::NotReply;
        }
        let mut headers = Vec::new();
        if self.pending_slots & (1 << 16) != 0 {
            headers.push(("\x1b]10;".to_owned(), ColorTarget::Foreground));
        }
        for slot in 0..16 {
            if self.pending_slots & (1 << slot) != 0 {
                headers.push((format!("\x1b]4;{slot};"), ColorTarget::Palette(slot)));
            }
        }
        for (header, target) in headers {
            if header.starts_with(text) {
                return Candidate::Prefix;
            }
            let Some(body) = text.strip_prefix(&header) else {
                continue;
            };
            // Use the same bounded RGB grammar as OSC 11. Only a reply to an
            // actually outstanding target is consumed; unrelated keys survive.
            return match candidate_status(&format!("{OSC11_PREFIX}{body}")) {
                Candidate::Color(color) => Candidate::ThemeColor(target, color),
                other => other,
            };
        }
        Candidate::NotReply
    }
}

impl<S> TerminalInput<S> {
    pub(crate) fn with_theme_color_handler(mut self, handler: ThemeColorHandler) -> Self {
        self.replies.theme_handler = Some(handler);
        self
    }

    pub(in crate::tui) fn query_theme_colors(&mut self, refresh: bool) -> io::Result<()> {
        use std::io::Write;
        if self.replies.theme_queried && !refresh {
            return Ok(());
        }
        let query = self.replies.begin_theme_query(Instant::now());
        if !query.is_empty() {
            let mut out = std::io::stdout();
            out.write_all(&query)?;
            out.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::decoded;
    use super::*;

    #[test]
    fn pi_colors_use_only_complete_palette_and_keep_genuine_fragmented_input() {
        for split in [false, true] {
            let mut replies = BackgroundReplies::default();
            assert!(!replies.begin_theme_query(Instant::now()).is_empty());
            assert!(replies.begin_theme_query(Instant::now()).is_empty());
            let mut now = Instant::now();
            for slot in (0..16).rev() {
                for event in decoded(&format!("\x1b]4;{slot};rgb:12/34/56\x1b\\"), split) {
                    now += Duration::from_millis(100);
                    replies.push_filtered(event, now);
                }
                assert!(replies.ready.is_empty());
                assert_eq!(replies.theme_colors.palette.is_some(), slot == 0);
            }
            assert_eq!(replies.theme_colors.palette, Some([(0x12, 0x34, 0x56); 16]));
            for event in decoded("\x1b]10;rgb:ff/ee/dd\x07", split) {
                replies.push_filtered(event, now);
            }
            assert_eq!(replies.theme_colors.foreground, Some((255, 238, 221)));
            let typing = decoded("draft", split);
            for event in typing.clone() {
                replies.push_filtered(event, now);
            }
            assert_eq!(
                replies
                    .ready
                    .drain(..)
                    .map(|packet| packet.event)
                    .collect::<Vec<_>>(),
                typing
            );
        }
    }

    #[test]
    fn focus_refresh_does_not_duplicate_outstanding_queries_or_block_other_targets() {
        let mut replies = BackgroundReplies::default();
        replies.begin_theme_query(Instant::now());
        // A terminal that never answers OSC 10 must still refresh its complete
        // OSC 4 palette. Outstanding targets are never queried twice.
        for slot in 0..16 {
            for event in decoded(&format!("\x1b]4;{slot};rgb:12/34/56\x07"), false) {
                replies.push_filtered(event, Instant::now());
            }
        }
        let old = replies.theme_colors.palette;
        let query = replies.begin_theme_query(Instant::now());
        assert!(!query.windows(4).any(|bytes| bytes == b"]10;"));
        assert!(!query.windows(4).any(|bytes| bytes == b"]11;"));
        assert_eq!(
            query.windows(3).filter(|bytes| *bytes == b"]4;").count(),
            16
        );
        assert!(replies.begin_theme_query(Instant::now()).is_empty());
        for slot in 0..16 {
            for event in decoded(&format!("\x1b]4;{slot};rgb:ab/cd/ef\x07"), false) {
                replies.push_filtered(event, Instant::now());
            }
            if slot != 15 {
                assert_eq!(replies.theme_colors.palette, old);
            }
        }
        assert_eq!(replies.theme_colors.palette, Some([(0xab, 0xcd, 0xef); 16]));
        assert!(replies.ready.is_empty());
    }

    #[test]
    fn pi_colors_unrequested_targets_and_paste_are_not_protocol() {
        let mut replies = BackgroundReplies::default();
        replies.begin_theme_query(Instant::now());
        for wire in [
            "\x1b]4;16;rgb:ff/ff/ff\x07",
            "\x1b[200~\x1b]10;rgb:ff/ff/ff\x07\x1b[201~",
        ] {
            let input = decoded(wire, false);
            for event in input.clone() {
                replies.push_filtered(event, Instant::now());
            }
            assert_eq!(
                replies
                    .ready
                    .drain(..)
                    .map(|packet| packet.event)
                    .collect::<Vec<_>>(),
                input
            );
        }
        assert_eq!(replies.theme_colors, TerminalThemeColors::default());
    }
}
