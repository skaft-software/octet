//! Reuse of per-message request work across the requests of one conversation.
//!
//! A caller that sends the same growing history turn after turn would
//! otherwise pay for the whole history on every request: a deep copy to build
//! the [`Request`](crate::Request), a compatibility scan, a validation walk and
//! a full wire serialization. [`RequestHistory`] lets that caller keep the
//! results for the part of the history that did not change.
//!
//! The caller owns the handle and identifies each message with a *stamp*: a
//! process-unique number from [`RequestHistory::fresh_stamp`] that it replaces
//! whenever it changes that message. Equal stamps therefore mean identical
//! content, and nothing here compares addresses or guesses which request a
//! history belongs to: the handle travels with the request in
//! [`HostRequestOptions::history`](crate::HostRequestOptions::history), and
//! every cached result is checked against the stamps of the request in hand.
//!
//! The handle also hands the caller's message vector back once the request is
//! encoded, so the caller can lend its own history instead of copying it. The
//! vector that comes back is always the one that was lent, unmodified: a
//! history that needs compatibility rewriting is returned before the rewrite
//! and the request continues on a copy.
//!
//! All cached state lives in the handle and is released with it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::types::Message;

static NEXT_STAMP: AtomicU64 = AtomicU64::new(1);

/// Caller-owned reuse state for one conversation's requests.
///
/// Cloning yields another handle to the same state.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct RequestHistory {
    state: Arc<Mutex<HistoryState>>,
}

impl std::fmt::Debug for RequestHistory {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RequestHistory")
            .finish_non_exhaustive()
    }
}

/// A lent history that came back from the client.
#[doc(hidden)]
#[derive(Debug)]
pub struct ReturnedHistory {
    /// The messages that were lent, unmodified.
    pub messages: Vec<Message>,
    /// The stamps that were lent with them, one per message.
    pub stamps: Vec<u64>,
}

#[derive(Default)]
pub(crate) struct HistoryState {
    /// Stamps of the history lent to the request now being prepared.
    lent: Option<Vec<u64>>,
    returned: Option<ReturnedHistory>,
    caches: PrefixCaches,
}

/// Results kept for the unchanged leading messages of a history.
#[derive(Default)]
pub(crate) struct PrefixCaches {
    pub(crate) clean: crate::transform::CleanPrefix,
    pub(crate) validated: crate::validate::ValidatedPrefix,
    pub(crate) chat: crate::protocol::openai_chat::ChatPrefixCache,
}

/// The lent history of the request in hand: one stamp per message, and the
/// results kept for it.
pub(crate) struct LentHistory<'a> {
    pub(crate) stamps: &'a [u64],
    pub(crate) caches: &'a mut PrefixCaches,
}

impl RequestHistory {
    /// Creates an empty handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a stamp no other message in this process carries.
    pub fn fresh_stamp() -> u64 {
        NEXT_STAMP.fetch_add(1, Ordering::Relaxed)
    }

    /// Declares the history of the next request sent with this handle.
    ///
    /// `stamps` holds one stamp per message of that request, in order. A
    /// history lent earlier and not yet reclaimed is forgotten.
    pub fn lend(&self, stamps: Vec<u64>) {
        let mut state = self.lock();
        state.lent = Some(stamps);
        state.returned = None;
    }

    /// Takes back the lent history, if the client returned it.
    ///
    /// `None` means the request never reached the point where the history is
    /// handed back, or no history was lent. Either way nothing stays lent.
    pub fn reclaim(&self) -> Option<ReturnedHistory> {
        let mut state = self.lock();
        state.lent = None;
        state.returned.take()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, HistoryState> {
        match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                // A panic while a cache was being extended may have left it
                // half-written. Dropping everything is always correct.
                let mut guard = poisoned.into_inner();
                *guard = HistoryState::default();
                self.state.clear_poison();
                guard
            }
        }
    }

    /// Hands `messages` back to the caller if they are still the lent history.
    pub(crate) fn give_back(&self, messages: Vec<Message>) {
        self.lock().give_back(messages);
    }
}

impl HistoryState {
    /// The stamps and kept results for `messages`, when they are the history
    /// lent for this request.
    pub(crate) fn lent(&mut self, messages: &[Message]) -> Option<LentHistory<'_>> {
        let stamps = self.lent.as_deref()?;
        (stamps.len() == messages.len()).then_some(LentHistory {
            stamps,
            caches: &mut self.caches,
        })
    }

    /// Hands `messages` back to the caller if they are still the lent history.
    /// After this the request no longer counts as carrying it.
    pub(crate) fn give_back(&mut self, messages: Vec<Message>) {
        if let Some(stamps) = self.lent.take() {
            if stamps.len() == messages.len() {
                self.returned = Some(ReturnedHistory { messages, stamps });
            }
        }
    }
}

/// The number of leading entries two stamp lists share.
pub(crate) fn common_stamp_prefix(kept: &[u64], current: &[u64]) -> usize {
    kept.iter()
        .zip(current)
        .take_while(|(kept, current)| kept == current)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UserMessage;

    fn user(text: &str) -> Message {
        Message::User(UserMessage {
            content: vec![crate::types::UserPart::Text(text.to_owned())],
        })
    }

    #[test]
    fn a_lent_history_comes_back_once_and_only_while_lent() {
        let history = RequestHistory::new();
        assert!(history.reclaim().is_none());

        let stamps = vec![RequestHistory::fresh_stamp(), RequestHistory::fresh_stamp()];
        history.lend(stamps.clone());
        history.give_back(vec![user("a"), user("b")]);
        let returned = history.reclaim().expect("the lent history is returned");
        assert_eq!(returned.stamps, stamps);
        assert_eq!(returned.messages.len(), 2);
        assert!(history.reclaim().is_none());

        // Nothing is lent now, so a later hand-back is not the caller's history.
        history.give_back(vec![user("c")]);
        assert!(history.reclaim().is_none());
    }

    #[test]
    fn a_history_of_another_length_is_not_the_lent_one() {
        let history = RequestHistory::new();
        history.lend(vec![RequestHistory::fresh_stamp()]);
        assert!(history.lock().lent(&[user("a"), user("b")]).is_none());
        assert!(history.lock().lent(&[user("a")]).is_some());
        history.give_back(vec![user("a"), user("b")]);
        assert!(history.reclaim().is_none());
    }

    #[test]
    fn stamps_are_unique_and_prefixes_compare_by_position() {
        let first = RequestHistory::fresh_stamp();
        let second = RequestHistory::fresh_stamp();
        assert_ne!(first, second);
        assert_eq!(common_stamp_prefix(&[1, 2, 3], &[1, 2, 4, 5]), 2);
        assert_eq!(common_stamp_prefix(&[], &[1]), 0);
        assert_eq!(common_stamp_prefix(&[1, 2], &[1, 2]), 2);
    }
}
