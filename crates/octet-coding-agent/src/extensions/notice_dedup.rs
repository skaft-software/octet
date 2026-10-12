//! Collapse an extension notice that keeps repeating.
//!
//! A misbehaving extension can raise the same notice on every lifecycle event;
//! showing each one buries the transcript. The first is shown; identical ones
//! within the window are dropped. Different notices are unaffected.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long an identical notice stays suppressed after it was last shown.
const WINDOW: Duration = Duration::from_secs(30);
/// Distinct notices remembered at once; the oldest is forgotten first.
const MAX_REMEMBERED: usize = 32;

#[derive(Default)]
pub(super) struct RecentNotices {
    shown: VecDeque<(String, Instant)>,
}

impl RecentNotices {
    /// Whether `message` should be shown now. A repeat inside the window returns
    /// `false` and does not extend the window, so a persistent problem
    /// resurfaces once per window rather than never.
    pub(super) fn admit(&mut self, message: &str, now: Instant) -> bool {
        self.shown
            .retain(|(_, at)| now.saturating_duration_since(*at) < WINDOW);
        if self.shown.iter().any(|(text, _)| text == message) {
            return false;
        }
        if self.shown.len() >= MAX_REMEMBERED {
            self.shown.pop_front();
        }
        self.shown.push_back((message.to_owned(), now));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identical_notice_is_shown_once_per_window() {
        let mut recent = RecentNotices::default();
        let start = Instant::now();
        assert!(recent.admit("[x Error] boom", start));
        for second in 1..30 {
            assert!(!recent.admit("[x Error] boom", start + Duration::from_secs(second)));
        }
        assert!(
            recent.admit("[x Error] boom", start + Duration::from_secs(31)),
            "a persistent problem resurfaces after the window"
        );
    }

    #[test]
    fn different_notices_are_all_shown_and_memory_is_bounded() {
        let mut recent = RecentNotices::default();
        let now = Instant::now();
        assert!(recent.admit("a", now));
        assert!(recent.admit("b", now));
        assert!(!recent.admit("a", now));
        for index in 0..100 {
            assert!(recent.admit(&format!("n{index}"), now));
        }
        assert!(recent.shown.len() <= MAX_REMEMBERED);
    }
}
