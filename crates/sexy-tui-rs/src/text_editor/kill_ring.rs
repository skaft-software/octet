//! Emacs-style kill ring for delete/yank/yank-pop.
//!
//! Ported from the upstream reference `packages/tui/src/kill-ring.ts`. A
//! consecutive run of kills accumulates into one entry: backward deletions
//! prepend and forward deletions append, so a `ctrl+k`-style run re-yanks in
//! source order.

/// Ring buffer of killed text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KillRing {
    ring: Vec<String>,
}

impl KillRing {
    /// An empty ring.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Push killed text.
    ///
    /// When `accumulate` is set and the ring already holds an entry, the text is
    /// merged into the newest entry instead of creating a new one. `prepend`
    /// selects the merge side: `true` for backward deletion, `false` for
    /// forward deletion. Empty text is ignored, exactly like upstream.
    pub fn push(&mut self, text: &str, prepend: bool, accumulate: bool) {
        if text.is_empty() {
            return;
        }
        if accumulate {
            if let Some(last) = self.ring.pop() {
                let merged = if prepend {
                    format!("{text}{last}")
                } else {
                    format!("{last}{text}")
                };
                self.ring.push(merged);
                return;
            }
        }
        self.ring.push(text.to_owned());
    }

    /// Newest entry without modifying the ring.
    #[must_use]
    pub fn peek(&self) -> Option<&str> {
        self.ring.last().map(String::as_str)
    }

    /// Move the newest entry to the front, for yank-pop cycling.
    ///
    /// The newest entry is still observable through [`Self::peek`] after a
    /// rotation only when the ring held exactly one entry; with two or more
    /// entries the previous newest becomes the oldest. A single-entry ring is
    /// unchanged.
    pub fn rotate(&mut self) {
        if self.ring.len() > 1 {
            if let Some(last) = self.ring.pop() {
                self.ring.insert(0, last);
            }
        }
    }

    /// Number of retained entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the ring holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }
}

#[cfg(test)]
mod tests;
