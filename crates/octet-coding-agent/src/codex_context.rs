//! Frontend Codex context configuration and caller-owned environment publication.
#![allow(missing_docs)]

use octet_ai::providers::codex_context as shared;
pub use shared::{
    codex_context_session_note, codex_gpt_6_model, context_window_label, fallback_context_windows,
    CodexContextWindow, CodexContextWindowError, CODEX_5_6_CONTEXT_WINDOW,
    CODEX_ASTRA_MAX_CONTEXT_WINDOW, CODEX_ASTRA_MODEL_ID, CODEX_CONTEXT_OVERRIDE_ENV,
    CODEX_CONTEXT_OVERRIDE_MIN_TOKENS, CODEX_LEGACY_CONTEXT_WINDOW, CODEX_LUNA_MODEL_ID,
    CODEX_MAX_OUTPUT_TOKENS,
};

/// Frontend launch configuration; publication is intentionally outside the AI crate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodexContextOverride {
    pub requested_tokens: Option<u64>,
}
impl CodexContextOverride {
    pub const NONE: Self = Self {
        requested_tokens: None,
    };
    pub fn new(tokens: u64) -> Self {
        Self {
            requested_tokens: Some(tokens),
        }
    }
    pub fn parse(requested: Option<&str>) -> Result<Self, String> {
        shared::CodexContextOverride::parse(requested).map(|value| Self {
            requested_tokens: value.requested_tokens,
        })
    }
    /// Preserve the frontend's public launch-only override entry point.
    pub fn publish(self) {
        match self.requested_tokens {
            Some(tokens) => std::env::set_var(CODEX_CONTEXT_OVERRIDE_ENV, tokens.to_string()),
            None => std::env::remove_var(CODEX_CONTEXT_OVERRIDE_ENV),
        }
    }
}

pub fn resolve_codex_context_window(
    model_id: &str,
    default_context_window: u64,
    max_context_window: u64,
    advertised_max_output_tokens: Option<u64>,
    user_override: CodexContextOverride,
) -> Result<CodexContextWindow, CodexContextWindowError> {
    shared::resolve_codex_context_window(
        model_id,
        default_context_window,
        max_context_window,
        advertised_max_output_tokens,
        shared::CodexContextOverride {
            requested_tokens: user_override.requested_tokens,
        },
    )
}
