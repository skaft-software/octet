//! Shared value fixtures for this crate's own unit tests.
//!
//! A provider-independent [`Request`](crate::types::Request) has fifteen
//! fields, and almost none of them is what a codec test is actually about.
//! Written out literally, all fifty-four `Request` fixtures in the crate
//! spelled out the same thirteen-field default tail, and a change to a
//! field's default meant re-reading every one of them.
//!
//! [`base_request`] names that tail once. Tests state the two or three
//! fields their assertion is about and take the rest from it, so the
//! interesting part of a fixture is visible without scrolling past the
//! scaffolding:
//!
//! ```ignore
//! let req = Request {
//!     messages: vec![user("hello")],
//!     ..base_request()
//! };
//! ```
//!
//! This lives in its own file because it is shared infrastructure for twelve
//! unrelated test suites (`protocol::*`, `types`, `validate`) rather than an
//! assertion set of its own, and because the whole point of the seam is that
//! the *default* is stated in exactly one place. It is compiled only under
//! `cfg(test)`: no production code path may depend on a fixture.

use crate::types::{
    CacheRetention, CompatibilityMode, OutputFormat, OutputModalities, ReasoningConfig,
    ReasoningMode, Request, ToolChoice,
};

/// A [`Request`] with every field at the crate's default value.
///
/// Each value below is the `#[default]` variant of its own type, so this is
/// exactly `Request::default()` for every field that has one — with the
/// exception of `system`, `messages`, `tools` and `session_id`, which have no
/// meaningful default beyond "unset". A test that wants a different value
/// names it explicitly before the `..base_request()` tail; a test that does
/// not care gets the default for free and stays readable.
pub(crate) fn base_request() -> Request {
    Request {
        system: None,
        messages: Vec::new(),
        tools: Vec::new(),
        tool_choice: ToolChoice::Auto,
        max_output_tokens: None,
        temperature: None,
        stop: Vec::new(),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: ReasoningMode::Standard,
        responses: None,
        output_format: OutputFormat::Text,
        output_modalities: OutputModalities::Text,
        compatibility: CompatibilityMode::Strict,
        cache_retention: CacheRetention::Short,
        session_id: None,
    }
}
