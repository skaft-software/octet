//! Generic clone-on-push undo stack.
//!
//! Ported from the upstream reference `packages/tui/src/undo-stack.ts`: pushing
//! a snapshot stores a deep clone, and a popped snapshot is returned directly
//! because it is already detached from the live state.

/// A stack of detached state snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoStack<S> {
    stack: Vec<S>,
}

impl<S: Clone> UndoStack<S> {
    /// An empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self { stack: Vec::new() }
    }

    /// Store a detached clone of `state`.
    pub fn push(&mut self, state: &S) {
        self.stack.push(state.clone());
    }

    /// Keep the newest snapshots that fit the editor's count and byte limits.
    pub(super) fn trim_to_budget(
        &mut self,
        max_count: usize,
        max_bytes: usize,
        size: impl Fn(&S) -> usize,
    ) {
        let mut bytes = 0usize;
        let keep = self
            .stack
            .iter()
            .rev()
            .take(max_count)
            .take_while(|state| {
                bytes = bytes.saturating_add(size(state));
                bytes <= max_bytes
            })
            .count();
        self.stack.drain(..self.stack.len() - keep);
    }

    /// Remove and return the newest snapshot.
    pub fn pop(&mut self) -> Option<S> {
        self.stack.pop()
    }

    /// Drop every snapshot.
    pub fn clear(&mut self) {
        self.stack.clear();
    }

    /// Number of retained snapshots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Whether the stack holds no snapshots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_snapshots_a_clone() {
        let mut stack = UndoStack::new();
        let mut state = vec!["first".to_owned()];
        stack.push(&state);
        state[0] = "mutated".to_owned();
        assert_eq!(stack.pop().unwrap(), vec!["first".to_owned()]);
    }

    #[test]
    fn pop_is_lifo_and_empties() {
        let mut stack = UndoStack::new();
        stack.push(&1u8);
        stack.push(&2u8);
        assert_eq!(stack.len(), 2);
        assert_eq!(stack.pop(), Some(2));
        assert_eq!(stack.pop(), Some(1));
        assert_eq!(stack.pop(), None);
        assert!(stack.is_empty());
    }

    #[test]
    fn clear_drops_every_snapshot() {
        let mut stack = UndoStack::new();
        stack.push(&());
        stack.clear();
        assert_eq!(stack.len(), 0);
    }
}
