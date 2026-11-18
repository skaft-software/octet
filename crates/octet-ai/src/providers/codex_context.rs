#![allow(missing_docs)]

//! Codex subscription context budgeting: provider default, provider maximum,
//! and an optional user-selected budget. Public API tariffs are not subscription
//! prices and do not impose a context-window cap or acknowledgement requirement.

use std::fmt;

pub const CODEX_LEGACY_CONTEXT_WINDOW: u64 = 272_000;
pub const CODEX_5_6_CONTEXT_WINDOW: u64 = 272_000;
pub const CODEX_ASTRA_MAX_CONTEXT_WINDOW: u64 = 872_000;
pub const CODEX_MAX_OUTPUT_TOKENS: u64 = 128_000;
pub const CODEX_ASTRA_MODEL_ID: &str = "gpt-6-astra";
pub const CODEX_LUNA_MODEL_ID: &str = "gpt-5.6-luna";
pub const CODEX_CONTEXT_OVERRIDE_MIN_TOKENS: u64 = 16_384;
pub const CODEX_CONTEXT_OVERRIDE_ENV: &str = "OCTET_CODEX_CONTEXT_WINDOW";

pub fn codex_gpt_6_model(model_id: &str) -> bool {
    matches!(
        model_id,
        "gpt-6-astra"
            | "gpt-6-sol"
            | "gpt-6.1-sol"
            | "gpt-6.1-terra"
            | "gpt-6-luna"
            | "gpt-6.2-sol"
            | "gpt-6.2-astra"
    )
}

/// An optional client-side budget. The provider still enforces its own limits.
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
        let Some(raw) = requested.map(str::trim).filter(|raw| !raw.is_empty()) else {
            return Ok(Self::NONE);
        };
        let tokens = raw.replace('_', "").parse::<u64>().map_err(|_| {
            format!("{CODEX_CONTEXT_OVERRIDE_ENV} must be an integer token count, got {raw:?}")
        })?;
        Ok(Self::new(tokens))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexContextWindowError {
    OverrideBelowMinimum {
        requested: u64,
        minimum: u64,
    },
    OverrideAboveMaximum {
        model_id: String,
        requested: u64,
        maximum: u64,
    },
}

impl fmt::Display for CodexContextWindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverrideBelowMinimum { requested, minimum } => write!(f,
                "Codex context-window override {requested} is below the {minimum}-token minimum"),
            Self::OverrideAboveMaximum { model_id, requested, maximum } => write!(f,
                "Codex context-window override {requested} exceeds the provider's {maximum}-token maximum for {model_id}"),
        }
    }
}
impl std::error::Error for CodexContextWindowError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexContextWindow {
    pub context_window: u64,
    pub default_context_window: u64,
    pub max_context_window: u64,
    pub max_output_tokens: u64,
    pub override_applied: bool,
}

pub fn context_window_label(tokens: u64) -> String {
    if tokens >= 1_000 && tokens.is_multiple_of(1_000) {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// Only an explicit change needs a session note; using a provider default does
/// not imply a clamp, a surcharge, transport instability, or missing token usage.
pub fn codex_context_session_note(model_id: &str, window: &CodexContextWindow) -> Option<String> {
    (window.override_applied && window.context_window != window.default_context_window).then(|| {
        format!(
            "note: Codex model {model_id} context window — provider default {}, maximum {}, effective {} (user override).",
            context_window_label(window.default_context_window),
            context_window_label(window.max_context_window),
            context_window_label(window.context_window),
        )
    })
}

/// Reviewed offline defaults, not account entitlements or public API limits.
/// Sol's 500K default follows https://developers.openai.com/codex/models;
/// remaining fallbacks follow the bundled catalog (see models/SOURCES.md).
/// Authenticated inventory, including smaller windows, overrides these values.
pub fn fallback_context_windows(model_id: &str) -> (u64, u64) {
    if matches!(model_id, "gpt-6.1-sol" | "gpt-6.1-terra" | "gpt-6.2-sol") {
        (500_000, CODEX_ASTRA_MAX_CONTEXT_WINDOW)
    } else if codex_gpt_6_model(model_id)
        || model_id.starts_with("gpt-5.6-")
        || model_id == "codex-auto-review"
    {
        (272_000, CODEX_ASTRA_MAX_CONTEXT_WINDOW)
    } else if model_id == "gpt-5.4" {
        (272_000, 1_000_000)
    } else {
        (CODEX_LEGACY_CONTEXT_WINDOW, CODEX_LEGACY_CONTEXT_WINDOW)
    }
}

/// Use the provider default unless the user selects a budget within the live
/// maximum. Neither subscription plan names nor API pricing select a budget.
/// Discovery normalizes positive limits before calling this pure resolver.
pub fn resolve_codex_context_window(
    model_id: &str,
    default_context_window: u64,
    max_context_window: u64,
    advertised_max_output_tokens: Option<u64>,
    user_override: CodexContextOverride,
) -> Result<CodexContextWindow, CodexContextWindowError> {
    let context_window = match user_override.requested_tokens {
        None => default_context_window,
        Some(requested) => {
            if requested < CODEX_CONTEXT_OVERRIDE_MIN_TOKENS {
                return Err(CodexContextWindowError::OverrideBelowMinimum {
                    requested,
                    minimum: CODEX_CONTEXT_OVERRIDE_MIN_TOKENS,
                });
            }
            if requested > max_context_window {
                return Err(CodexContextWindowError::OverrideAboveMaximum {
                    model_id: model_id.to_owned(),
                    requested,
                    maximum: max_context_window,
                });
            }
            requested
        }
    };
    let max_output_tokens = advertised_max_output_tokens
        .filter(|value| *value > 0)
        .unwrap_or(CODEX_MAX_OUTPUT_TOKENS)
        .min(if model_id == CODEX_ASTRA_MODEL_ID {
            CODEX_MAX_OUTPUT_TOKENS
        } else {
            u64::MAX
        })
        .min(context_window);
    Ok(CodexContextWindow {
        context_window,
        default_context_window,
        max_context_window,
        max_output_tokens,
        override_applied: user_override.requested_tokens.is_some(),
    })
}
