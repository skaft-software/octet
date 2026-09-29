//! OpenAI Responses private wire protocol codec.
//!
//! One body tree, one always-on stream, and an opaque replay format the codec
//! must round-trip byte-for-byte. Those four concerns fail in four different
//! ways, so each lives in its own sibling module and this file is only the seam
//! that names them:
//!
//! - `wire` — the private `Serialize` tree for everything we can send, the two
//!   gates that decide which optional fields a route may carry, and the
//!   documented computer-action vocabulary. Nothing here reads a response.
//! - `input` — the ordered walk from canonical history to `input` items, plus
//!   the async-marker validation that gates them. Replay order is a property of
//!   the conversation, not of the body, so it is testable without a transport.
//! - `request` — `build_request`, the private compact variant, raw-compact
//!   control validation, session-affinity headers, and the tool/reasoning/text
//!   mapping they share. Fails before a byte leaves the process.
//! - `stream` — the SSE frame tree, terminal reconciliation, the computer-action
//!   validator and `decode_stream_event`. Owns every piece of canonical state
//!   built incrementally.
//!
//! The three entry points other modules call (`build_request`,
//! `build_compact_request`, `decode_stream_event`) and the two replay encoders
//! are re-exported here, so the paths callers use are unchanged by the split.
//! `COMPUTER_TOOL_NAME` stays put in `wire`, where the wire vocabulary it names
//! is defined.

mod input;
mod request;
mod stream;
mod wire;

pub(crate) use input::{encode_canonical_input, encode_replay_input};
pub(crate) use request::{
    build_compact_request, build_request, responses_affinity_headers, validate_compact_reasoning,
};
pub(crate) use stream::decode_stream_event;

#[cfg(test)]
mod tests;

/// Offline fixture matrix for the OpenAI Responses stream decoder
/// (design §19; plan Task 11.2).
#[cfg(test)]
mod fixture_tests;

#[cfg(test)]
#[path = "openai_responses_gpt6_tests.rs"]
mod gpt6_tests;
