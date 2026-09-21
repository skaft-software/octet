//! Detached snapshots with explicit count and caller-supplied byte budgets.

use std::collections::VecDeque;

/// A bounded stack of detached state snapshots.
///
/// Generic states have no knowable heap size. Callers that retain heap data use
/// `with_limits` and `push_owned_with_size` to supply its retained byte cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UndoStack<S> {
    stack: VecDeque<(S, usize)>,
    bytes: usize,
    max_count: usize,
    max_bytes: usize,
}

impl<S> Default for UndoStack<S> {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> UndoStack<S> {
    /// An empty, count-bounded stack. Heap-owning callers should use explicit
    /// byte costs rather than attempting to infer deep size from `size_of`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(128, usize::MAX)
    }

    /// An empty stack with explicit snapshot count and retained-byte limits.
    #[must_use]
    pub fn with_limits(max_count: usize, max_bytes: usize) -> Self {
        Self {
            stack: VecDeque::new(),
            bytes: 0,
            max_count,
            max_bytes,
        }
    }

    /// Store an already detached snapshot without cloning it again.
    pub fn push_owned(&mut self, state: S) {
        self.push_owned_with_size(state, 0);
    }

    /// Store a snapshot charged at `bytes`, evicting oldest entries first.
    /// An oversized edit clears history: skipping that boundary would make a
    /// later undo jump across an edit for which no snapshot was retained.
    pub fn push_owned_with_size(&mut self, state: S, bytes: usize) {
        if self.max_count == 0 || bytes > self.max_bytes {
            self.clear();
            return;
        }
        while self.stack.len() >= self.max_count || self.bytes > self.max_bytes - bytes {
            let (_, cost) = self
                .stack
                .pop_front()
                .expect("nonempty over-budget history");
            self.bytes -= cost;
        }
        self.bytes += bytes;
        self.stack.push_back((state, bytes));
    }

    /// Remove and return the newest snapshot.
    pub fn pop(&mut self) -> Option<S> {
        let (state, bytes) = self.stack.pop_back()?;
        self.bytes -= bytes;
        Some(state)
    }

    /// Drop every snapshot.
    pub fn clear(&mut self) {
        self.stack.clear();
        self.bytes = 0;
    }

    /// Number of retained snapshots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.stack.len()
    }

    /// Caller-accounted bytes retained by snapshots.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    /// Whether the stack holds no snapshots.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
}

impl<S: Clone> UndoStack<S> {
    /// Store a detached clone of `state` (count budget only).
    pub fn push(&mut self, state: &S) {
        self.push_owned(state.clone());
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
    fn owned_snapshot_keeps_its_allocation() {
        let mut stack = UndoStack::new();
        let state = String::from("detached");
        let allocation = state.as_ptr();
        stack.push_owned(state);
        let restored = stack.pop().unwrap();
        assert_eq!(restored.as_ptr(), allocation);
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

    #[test]
    fn oldest_snapshots_expire_under_both_budgets() {
        let mut stack = UndoStack::with_limits(2, 5);
        stack.push_owned_with_size("a", 2);
        stack.push_owned_with_size("b", 2);
        stack.push_owned_with_size("c", 3);
        assert_eq!(stack.retained_bytes(), 5);
        assert_eq!(stack.pop(), Some("c"));
        assert_eq!(stack.pop(), Some("b"));
        assert_eq!(stack.retained_bytes(), 0);
        stack.push_owned_with_size("a", 0);
        stack.push_owned_with_size("b", 0);
        stack.push_owned_with_size("c", 0);
        assert_eq!(stack.len(), 2);
        stack.push_owned_with_size("too big", 6);
        assert!(stack.is_empty());
        stack.push_owned_with_size("new", 1);
        stack.clear();
        assert_eq!(stack.retained_bytes(), 0);
    }
}
