//! Shared value fixtures for this crate's own unit tests.
//!
//! Two provider-independent shapes recur in this crate's fixtures, and in both
//! cases almost none of the fields is what a test is actually about.
//!
//! [`base_request`] names the default tail of the fifteen-field
//! [`Request`](crate::types::Request). Written out literally, all fifty-four
//! `Request` fixtures spelled out the same thirteen-field tail, and a change to
//! a field's default meant re-reading every one of them.
//!
//! [`reasoning_capability`] and [`token_budget_capability`] name the two shapes
//! a [`ReasoningCapability`](crate::types::ReasoningCapability) takes in a
//! fixture: an effort control over the full portable range, and a token budget
//! over a doubling ladder. A test states the two or three fields its assertion
//! is about and takes the rest from a fixture, so the interesting part of a
//! fixture is visible without scrolling past the scaffolding:
//!
//! ```ignore
//! let req = Request {
//!     messages: vec![user("hello")],
//!     ..base_request()
//! };
//! let capability = ReasoningCapability {
//!     preserves_state: false,
//!     ..reasoning_capability()
//! };
//! ```
//!
//! This lives in its own file because it is shared infrastructure for thirteen
//! unrelated test suites (`protocol::*`, `types` and `validate`) rather than an
//! assertion set of its own, and because the whole point of the seam is that
//! each *default* is stated in exactly one place. It is compiled only under
//! `cfg(test)`: no production code path may depend on a fixture.

use crate::types::{
    CacheRetention, CompatibilityMode, OpenAiChatReasoningMode, OutputFormat, OutputModalities,
    ReasoningCapability, ReasoningConfig, ReasoningControl, ReasoningEffort,
    ReasoningEffortBudgets, ReasoningMode, Request, ToolChoice,
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

/// A [`ReasoningCapability`] that selects reasoning by portable effort over the
/// full `Minimal..=High` range, exposes reasoning text, and preserves reasoning
/// state for continuation.
///
/// This is the shape every effort-selecting fixture in the crate wants unless
/// its assertion is about something else: no endpoint-specific selector list, no
/// token-budget map, and the standard OpenAI-compatible chat mode. The four
/// defaults below are the crate's own — `Minimal` is `default_min_effort`,
/// `High` is `default_max_effort`, and `Standard` is the `#[default]` chat mode
/// — so a test that overrides a field overrides a real default rather than an
/// arbitrary constant.
pub(crate) fn reasoning_capability() -> ReasoningCapability {
    ReasoningCapability {
        options: None,
        control: ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: true,
        effort_budgets: None,
        openai_chat_mode: OpenAiChatReasoningMode::Standard,
        min_effort: ReasoningEffort::Minimal,
        max_effort: ReasoningEffort::High,
    }
}

/// A [`ReasoningCapability`] that selects reasoning by token budget, paired
/// with a doubling budget ladder from 1 KiB at `Minimal` to 32 KiB at `Max`.
///
/// Separate from [`reasoning_capability`] because the two are different provider
/// contracts, not different spellings of one: a budget-control route must
/// supply an [`ReasoningEffortBudgets`] map (the struct documents it as required
/// exactly when `control` is `TokenBudget`), so the two fields always travel
/// together and a test that needs this control needs this ladder with it.
pub(crate) fn token_budget_capability() -> ReasoningCapability {
    ReasoningCapability {
        control: ReasoningControl::TokenBudget,
        effort_budgets: Some(ReasoningEffortBudgets {
            minimal: 1024,
            low: 2048,
            medium: 4096,
            high: 8192,
            xhigh: 16384,
            max: 32768,
        }),
        ..reasoning_capability()
    }
}
