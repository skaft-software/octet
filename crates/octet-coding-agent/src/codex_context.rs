#![allow(missing_docs)]

//! Codex (Sign in with ChatGPT) context-window budgeting.
//!
//! This module owns the Codex context-window *policy* in one place:
//!
//! * the deliberate conservative working cap (`CODEX_CONTEXT_WINDOW_CAP`,
//!   272K, with the documented `gpt-5.6-luna` 372K exception), and
//! * the per-family advertised/entitled ceiling used to bound the explicit
//!   opt-in override.
//!
//! The 272K working cap is a deliberate product decision, not a defect: OpenAI
//! recommends a 272K Codex context limit, usage above 272K is double-priced
//! (guidance: roughly 2x input / 1.5x output for the whole request, not merely
//! the excess), and oversized long-running sessions were losing their Codex
//! websocket. octet therefore keeps the cap by default, reports the clamp
//! instead of hiding it, and only raises it through an explicit, acknowledged,
//! entitlement-checked opt-in.
//!
//! Stable entry points for the CLI and TUI frontends:
//!
//! * [`resolve_codex_context_window`] - pure resolution of the effective window
//!   from `(model_id, plan tier, discovered default, discovered max, user
//!   override)`.
//! * [`CodexContextOverride::parse`] - opt-in parsing, fail-closed.
//! * [`codex_context_session_note`] - the ONE user-facing note a session may
//!   print, for the effective session model only, and only when that model's
//!   window is reduced by the deliberate cap or above the 272K standard tier.
//! * [`CodexContextClamp::message`] - bounded user-visible clamp notice for a
//!   single deliberate reduction.
//! * [`CodexContextClampReporter::observe`] - once-per-transition notice
//!   de-duplication (no per-turn spam) for a frontend that tracks transitions.
//! * [`CODEX_ABOVE_STANDARD_TIER_OPERATION`] - the
//!   `Session::record_usage_uncertainty` operation identifier to use when
//!   [`CodexContextWindow::has_uncertain_usage`] is set.
//! * [`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`] - the exact wording that names
//!   the double-pricing cliff and the websocket risk; frontends must render it
//!   before accepting the acknowledgement flag.

use std::fmt;

/// The deliberate conservative working window octet budgets ordinary Codex
/// families against. Kept intentionally (see the module docs); do not raise it.
pub const CODEX_CONTEXT_WINDOW_CAP: u64 = 272_000;

/// Historical name for the conservative working window.
pub const CODEX_LEGACY_CONTEXT_WINDOW: u64 = 272_000;

/// Context window octet uses for the `gpt-5.6-*` family.
pub const CODEX_5_6_CONTEXT_WINDOW: u64 = 372_000;

/// Advertised/entitled ceiling for `gpt-6-astra`.
pub const CODEX_ASTRA_MAX_CONTEXT_WINDOW: u64 = 872_000;

/// Advertised/entitled ceiling for Pro/ProLite `gpt-5.4` and `codex-auto-review`.
pub const CODEX_PRO_CONTEXT_WINDOW: u64 = 1_000_000;

/// Default Codex output contract when the backend advertises nothing smaller.
pub const CODEX_MAX_OUTPUT_TOKENS: u64 = 128_000;

/// Model identifier with an input envelope distinct from its 128K output
/// contract.
pub const CODEX_ASTRA_MODEL_ID: &str = "gpt-6-astra";

/// The one family whose conservative working window is 372K rather than 272K.
pub const CODEX_LUNA_MODEL_ID: &str = "gpt-5.6-luna";

/// Smallest override octet accepts; below this a Codex session cannot work.
pub const CODEX_CONTEXT_OVERRIDE_MIN_TOKENS: u64 = 16_384;

/// Environment variable carrying the opt-in Codex context-window override.
pub const CODEX_CONTEXT_OVERRIDE_ENV: &str = "OCTET_CODEX_CONTEXT_WINDOW";

/// Environment variable carrying the explicit above-standard-tier
/// acknowledgement. Fail-closed: absent, empty, or unparseable means "not
/// acknowledged".
pub const CODEX_CONTEXT_ACKNOWLEDGE_ENV: &str =
    "OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF";

/// `Session::record_usage_uncertainty` operation identifier for accepted Codex
/// attempts whose effective window is above the standard 272K tier, where the
/// whole request is double-priced and exact cost cannot be claimed.
pub const CODEX_ABOVE_STANDARD_TIER_OPERATION: &str = "codex-context-above-272k";

/// The wording a frontend must show before the operator sets the
/// acknowledgement flag. It names the double-pricing cliff and the long-session
/// websocket risk, which are the two reasons the cap exists.
pub const CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING: &str =
    "I understand that a Codex request above 272K tokens is double-priced (about 2x input and 1.5x output for the whole request, not only the excess) and that oversized long-running sessions are more likely to drop the Codex websocket.";

/// Which context-window tier the account's plan activates.
///
/// `Extended` mirrors `ChatGptPlan::uses_max_context_window` (consumer Pro and
/// ProLite); everything else, including an unauthenticated or unknown plan, is
/// `Default`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexContextTier {
    /// The backend's default window applies.
    Default,
    /// The backend's larger advertised window applies.
    Extended,
}

impl CodexContextTier {
    /// Derive the tier from the plan's `uses_max_context_window` signal.
    pub fn from_plan_entitlement(uses_max_context_window: bool) -> Self {
        if uses_max_context_window {
            Self::Extended
        } else {
            Self::Default
        }
    }

    fn selects_max_context_window(self) -> bool {
        matches!(self, Self::Extended)
    }
}

/// A user-requested Codex context window.
///
/// Opt-in only: [`Self::NONE`] (or [`Default`]) leaves the deliberate cap
/// untouched. Values at or below the deliberate cap narrow the window and need
/// no acknowledgement; values above it require the `Extended` tier and the
/// explicit acknowledgement wording.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CodexContextOverride {
    /// Requested effective window in tokens.
    pub requested_tokens: Option<u64>,
    /// Whether the operator accepted [`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`].
    pub acknowledge_cost_cliff: bool,
}

impl CodexContextOverride {
    /// No override: the deliberate cap applies unchanged.
    pub const NONE: Self = Self {
        requested_tokens: None,
        acknowledge_cost_cliff: false,
    };

    /// Build an override for an already-validated numeric request. This is the
    /// constructor a CLI/TUI frontend uses after showing
    /// [`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`].
    pub fn raising(requested_tokens: u64, acknowledge_cost_cliff: bool) -> Self {
        Self {
            requested_tokens: Some(requested_tokens),
            acknowledge_cost_cliff,
        }
    }

    /// The stable in-process entry point for a frontend that must publish a
    /// chosen override without editing `app::bootstrap`.
    ///
    /// Bootstrap resolves the override from the documented environment bridge
    /// (`OCTET_CODEX_CONTEXT_WINDOW` plus
    /// `OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF`), so the CLI flags and
    /// the TUI effort menu both publish through this one call. [`Self::NONE`]
    /// clears both variables, which restores the deliberate default without any
    /// persisted configuration change.
    ///
    /// A frontend must render [`CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING`] before it
    /// publishes an acknowledged value above the deliberate cap; resolution still
    /// fails closed if the published value is unacknowledged, above the model's
    /// entitlement, or on a non-entitled plan.
    pub fn publish(self) {
        match self.requested_tokens {
            Some(tokens) => {
                std::env::set_var(CODEX_CONTEXT_OVERRIDE_ENV, tokens.to_string());
                std::env::set_var(
                    CODEX_CONTEXT_ACKNOWLEDGE_ENV,
                    if self.acknowledge_cost_cliff { "1" } else { "0" },
                );
            }
            None => {
                std::env::remove_var(CODEX_CONTEXT_OVERRIDE_ENV);
                std::env::remove_var(CODEX_CONTEXT_ACKNOWLEDGE_ENV);
            }
        }
    }

    /// Parse the environment-variable form of the override.
    ///
    /// Fail-closed: an unparseable token count or an unrecognised
    /// acknowledgement value is an error, and callers must fall back to
    /// [`Self::NONE`] rather than guessing.
    pub fn parse(requested: Option<&str>, acknowledged: Option<&str>) -> Result<Self, String> {
        let requested_tokens = match normalize(requested) {
            None => None,
            Some(raw) => {
                let digits = raw.replace('_', "");
                Some(digits.parse::<u64>().map_err(|_| {
                    format!(
                        "{CODEX_CONTEXT_OVERRIDE_ENV} must be an integer token count, got {raw:?}"
                    )
                })?)
            }
        };
        let acknowledge_cost_cliff = match normalize(acknowledged) {
            None => false,
            Some(raw) => match raw.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => {
                    return Err(format!(
                        "{CODEX_CONTEXT_ACKNOWLEDGE_ENV} must be one of 1/true/yes/on or 0/false/no/off, got {raw:?}"
                    ));
                }
            },
        };
        Ok(Self {
            requested_tokens,
            acknowledge_cost_cliff,
        })
    }

    fn is_requested(self) -> bool {
        self.requested_tokens.is_some()
    }
}

fn normalize(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// Fail-closed reasons the opt-in override was refused.
///
/// Every variant means the deliberate cap stays in force; none of them silently
/// clamps the requested value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodexContextWindowError {
    /// The requested window is too small for a Codex session.
    OverrideBelowMinimum {
        requested: u64,
        minimum: u64,
    },
    /// The requested window is above what the model is entitled to.
    OverrideAboveEntitlement {
        model_id: String,
        requested: u64,
        entitled_max_context_window: u64,
    },
    /// A window above the deliberate cap needs the Pro/ProLite entitlement.
    OverrideRequiresEntitlement {
        model_id: String,
        requested: u64,
        working_context_window: u64,
    },
    /// A window above the deliberate cap needs the explicit acknowledgement.
    OverrideRequiresAcknowledgement {
        model_id: String,
        requested: u64,
        working_context_window: u64,
    },
}

impl fmt::Display for CodexContextWindowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OverrideBelowMinimum { requested, minimum } => write!(
                formatter,
                "Codex context-window override {requested} is below the {minimum}-token minimum"
            ),
            Self::OverrideAboveEntitlement {
                model_id,
                requested,
                entitled_max_context_window,
            } => write!(
                formatter,
                "Codex context-window override {requested} is above the {entitled_max_context_window}-token entitlement for {model_id}; octet will not request a window the model is not entitled to"
            ),
            Self::OverrideRequiresEntitlement {
                model_id,
                requested,
                working_context_window,
            } => write!(
                formatter,
                "Codex context-window override {requested} is above the deliberate {working_context_window}-token cap for {model_id} and requires a Codex Pro or ProLite plan"
            ),
            Self::OverrideRequiresAcknowledgement {
                model_id,
                requested,
                working_context_window,
            } => write!(
                formatter,
                "Codex context-window override {requested} is above the deliberate {working_context_window}-token cap for {model_id}: {CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING}"
            ),
        }
    }
}

impl std::error::Error for CodexContextWindowError {}

/// A bounded, user-visible explanation of a deliberate Codex context clamp.
///
/// Constructed by [`resolve_codex_context_window`] when the effective window is
/// below the window the model advertises or the plan entitles. Frontends render
/// [`Self::message`]; the notice is a typed value rather than an ad-hoc print.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexContextClamp {
    /// Model the clamp applies to.
    pub model_id: String,
    /// Window the model advertises, or the plan entitles, before the cap.
    pub advertised_context_window: u64,
    /// Window octet actually budgets against.
    pub effective_context_window: u64,
}

impl CodexContextClamp {
    /// The canonical, fixed-template notice text for one deliberate reduction.
    ///
    /// Plain user language only: no internal API or operation identifiers, and
    /// the windows are labelled so the reader never has to guess which number is
    /// which. The effective window is the same one
    /// [`codex_context_session_note`] reports.
    pub fn message(&self) -> String {
        let effective = self.effective_context_window;
        let advertised = self.advertised_context_window.max(effective);
        format!(
            "note: Codex model {model} context window — advertised {advertised}, effective {effective}. octet's deliberate {cap} Codex cap reduces the advertised window by {reduction} because OpenAI recommends a 272K Codex context limit, usage above 272K is double-priced (about 2x input and 1.5x output for the whole request, not just the excess), and oversized long-running sessions can drop the Codex websocket. {remedy}",
            model = self.model_id,
            advertised = context_window_label(advertised),
            effective = context_window_label(effective),
            cap = context_window_label(CODEX_CONTEXT_WINDOW_CAP),
            reduction = context_window_label(advertised - effective),
            remedy = codex_context_remedy(),
        )
    }
}

impl fmt::Display for CodexContextClamp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

/// Reports a clamp notice at most once per transition.
///
/// Hold one reporter per session: repeating the same clamp reports nothing, a
/// session without a clamp reports nothing, and the reporter re-arms so a later
/// clamp (a model switch, a plan change, or removing an override) is reported
/// again.
#[derive(Clone, Debug, Default)]
pub struct CodexContextClampReporter {
    last: Option<CodexContextClamp>,
}

impl CodexContextClampReporter {
    /// Observe the session's current clamp state and return a notice only when
    /// the clamped state changed.
    pub fn observe(&mut self, clamp: Option<CodexContextClamp>) -> Option<CodexContextClamp> {
        match (self.last.take(), clamp) {
            (_, None) => None,
            (Some(previous), Some(current)) if previous == current => {
                self.last = Some(current);
                None
            }
            (_, Some(current)) => {
                self.last = Some(current.clone());
                Some(current)
            }
        }
    }
}

/// The resolved Codex context envelope for one model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexContextWindow {
    /// Effective context window octet budgets against.
    pub context_window: u64,
    /// Window the model advertises, or the plan entitles, before the cap. Equal
    /// to `context_window` when nothing was clamped.
    pub advertised_context_window: u64,
    /// The model's entitlement ceiling; the maximum an override may request.
    pub entitled_max_context_window: u64,
    /// Bounded output contract (never above `context_window`).
    pub max_output_tokens: u64,
    /// The tier the account's plan activated.
    pub tier: CodexContextTier,
    /// Whether the explicit opt-in override produced `context_window`.
    pub override_applied: bool,
    /// Present when the deliberate cap reduced the window.
    pub clamp: Option<CodexContextClamp>,
    /// `true` when `context_window` is above the standard 272K tier, where the
    /// whole request is priced differently and exact cost/usage must not be
    /// claimed.
    pub has_uncertain_usage: bool,
}

impl CodexContextWindow {
    /// The `Session::record_usage_uncertainty` operation identifier to record
    /// when [`Self::has_uncertain_usage`] is set.
    pub fn uncertain_usage_operation(&self) -> Option<&'static str> {
        self.has_uncertain_usage
            .then_some(CODEX_ABOVE_STANDARD_TIER_OPERATION)
    }
}

/// `272K`, `372K`, ... for whole thousands, otherwise the exact token count.
///
/// Every window octet prints goes through this label so a user never has to
/// guess which bare number means what.
pub fn context_window_label(tokens: u64) -> String {
    if tokens >= 1_000 && tokens % 1_000 == 0 {
        format!("{}K", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

/// The in-app remedies first, with the environment variables as the scriptable
/// alternative. Shared by every Codex context note.
fn codex_context_remedy() -> String {
    format!(
        "Choose the window in-app with --codex-context-window <TOKENS> (the model effort menu offers the same choice), or script it with {CODEX_CONTEXT_OVERRIDE_ENV} plus {CODEX_CONTEXT_ACKNOWLEDGE_ENV}=1 above 272K; see docs/codex-context.md."
    )
}

/// The ONE user-facing note an effective Codex session model needs, if any.
///
/// Call this once per session, for the model the session actually runs. It is
/// deliberately not a catalog-time notice: enumerating Codex models must never
/// print anything, so a session whose effective model is not a Codex route
/// prints no note at all.
///
/// `None` when the effective window is neither reduced by the deliberate cap nor
/// above the 272K standard tier. The text is plain user language: it names no
/// internal API or operation identifier, labels every window it quotes, and
/// states the same effective window in every variant (the 372K documented
/// `gpt-5.6-luna` window therefore never reads as a clamp to 272K).
pub fn codex_context_session_note(model_id: &str, window: &CodexContextWindow) -> Option<String> {
    let effective = window.context_window;
    let advertised = window.advertised_context_window.max(effective);
    let entitled = window.entitled_max_context_window.max(effective);
    let reduction = advertised - effective;
    if reduction == 0 && !window.has_uncertain_usage {
        return None;
    }
    let mut note = format!(
        "note: Codex model {model_id} context window — advertised {advertised}, entitled {entitled}, effective {effective}.",
        advertised = context_window_label(advertised),
        entitled = context_window_label(entitled),
        effective = context_window_label(effective),
    );
    if reduction > 0 {
        note.push_str(&format!(
            " octet's deliberate {cap} Codex cap reduces the advertised window by {reduction} because OpenAI recommends a 272K Codex context limit, usage above 272K is double-priced (about 2x input and 1.5x output for the whole request, not just the excess), and oversized long-running sessions can drop the Codex websocket.",
            cap = context_window_label(CODEX_CONTEXT_WINDOW_CAP),
            reduction = context_window_label(reduction),
        ));
    }
    if window.has_uncertain_usage {
        note.push_str(&format!(
            " At {effective} — above the {cap} standard tier — every request is double-priced (about 2x input and 1.5x output for the whole request, not just the excess) and long-running sessions are likelier to drop the Codex websocket, so this session's usage is recorded as uncertain instead of an exact cost.",
            effective = context_window_label(effective),
            cap = context_window_label(CODEX_CONTEXT_WINDOW_CAP),
        ));
    }
    note.push(' ');
    note.push_str(&codex_context_remedy());
    Some(note)
}

/// The deliberate conservative working window for a Codex model.
///
/// This is octet's budgeting cap, not the provider's advertised maximum. It is
/// 372K for `gpt-5.6-luna` and 272K for every other family.
pub fn working_context_window(model_id: &str) -> u64 {
    if model_id == CODEX_LUNA_MODEL_ID {
        CODEX_5_6_CONTEXT_WINDOW
    } else {
        CODEX_CONTEXT_WINDOW_CAP
    }
}

/// The `(default, advertised maximum)` context windows for a known Codex family.
///
/// These are checked-in discovery fallbacks; the authenticated `/models`
/// response remains authoritative for plan-specific advertised limits, and the
/// advertised maximum is what bounds the explicit override.
pub fn entitled_context_windows(model_id: &str) -> (u64, u64) {
    if model_id == CODEX_ASTRA_MODEL_ID {
        (CODEX_LEGACY_CONTEXT_WINDOW, CODEX_ASTRA_MAX_CONTEXT_WINDOW)
    } else if model_id == "gpt-5.4" || model_id == "codex-auto-review" {
        (CODEX_LEGACY_CONTEXT_WINDOW, CODEX_PRO_CONTEXT_WINDOW)
    } else if model_id.starts_with("gpt-5.6-") {
        (CODEX_5_6_CONTEXT_WINDOW, CODEX_5_6_CONTEXT_WINDOW)
    } else {
        (CODEX_LEGACY_CONTEXT_WINDOW, CODEX_LEGACY_CONTEXT_WINDOW)
    }
}

/// The model's entitlement ceiling: the largest window an override may request.
pub fn entitled_max_context_window(model_id: &str) -> u64 {
    entitled_context_windows(model_id).1
}

/// Resolve the effective Codex context window for one model.
///
/// Pure: the result depends only on the arguments. Without an override the
/// deliberate cap applies exactly as before, and any deliberate reduction is
/// reported through [`CodexContextWindow::clamp`]. With an override every gate
/// must pass or the call fails closed; the deliberate cap is never silently
/// replaced by a refused value.
///
/// `discovered_default_context_window` and `discovered_max_context_window` are
/// the backend values before octet's cap; the plan tier selects which one
/// applies.
pub fn resolve_codex_context_window(
    model_id: &str,
    tier: CodexContextTier,
    discovered_default_context_window: u64,
    discovered_max_context_window: u64,
    advertised_max_output_tokens: Option<u64>,
    user_override: CodexContextOverride,
) -> Result<CodexContextWindow, CodexContextWindowError> {
    let working = working_context_window(model_id);
    // The checked-in family ceiling is only a fallback, never authority to
    // exceed the authenticated backend's advertised entitlement.
    let entitled_max = entitled_max_context_window(model_id).min(discovered_max_context_window);
    let requested = if tier.selects_max_context_window() {
        discovered_max_context_window
    } else {
        discovered_default_context_window
    };
    let capped = requested.min(working).max(1);
    let context_window = match user_override.requested_tokens {
        None => capped,
        Some(requested_tokens) => apply_override(
            model_id,
            tier,
            working,
            entitled_max,
            requested_tokens,
            user_override.acknowledge_cost_cliff,
        )?,
    };
    let max_output_tokens = advertised_max_output_tokens
        .filter(|value| *value > 0)
        .unwrap_or(CODEX_MAX_OUTPUT_TOKENS)
        .min(if model_id == CODEX_ASTRA_MODEL_ID {
            CODEX_MAX_OUTPUT_TOKENS
        } else {
            u64::MAX
        })
        .min(context_window)
        .max(1);
    // Only a deliberate cap is a clamp. A user-chosen lower window is not
    // reported as one, and a value the override produced is already explicit.
    let clamp = (!user_override.is_requested() && capped < requested).then(|| CodexContextClamp {
        model_id: model_id.to_owned(),
        advertised_context_window: requested,
        effective_context_window: capped,
    });
    Ok(CodexContextWindow {
        context_window,
        advertised_context_window: requested,
        entitled_max_context_window: entitled_max,
        max_output_tokens,
        tier,
        override_applied: user_override.is_requested(),
        clamp,
        has_uncertain_usage: context_window > CODEX_CONTEXT_WINDOW_CAP,
    })
}

fn apply_override(
    model_id: &str,
    tier: CodexContextTier,
    working_context_window: u64,
    entitled_max_context_window: u64,
    requested: u64,
    acknowledge_cost_cliff: bool,
) -> Result<u64, CodexContextWindowError> {
    if requested < CODEX_CONTEXT_OVERRIDE_MIN_TOKENS {
        return Err(CodexContextWindowError::OverrideBelowMinimum {
            requested,
            minimum: CODEX_CONTEXT_OVERRIDE_MIN_TOKENS,
        });
    }
    if requested > entitled_max_context_window {
        return Err(CodexContextWindowError::OverrideAboveEntitlement {
            model_id: model_id.to_owned(),
            requested,
            entitled_max_context_window,
        });
    }
    if requested > working_context_window {
        if !tier.selects_max_context_window() {
            return Err(CodexContextWindowError::OverrideRequiresEntitlement {
                model_id: model_id.to_owned(),
                requested,
                working_context_window,
            });
        }
        if !acknowledge_cost_cliff {
            return Err(CodexContextWindowError::OverrideRequiresAcknowledgement {
                model_id: model_id.to_owned(),
                requested,
                working_context_window,
            });
        }
    }
    Ok(requested)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> CodexContextWindow {
        resolve_codex_context_window(
            CODEX_ASTRA_MODEL_ID,
            CodexContextTier::Extended,
            CODEX_CONTEXT_WINDOW_CAP,
            CODEX_ASTRA_MAX_CONTEXT_WINDOW,
            Some(256_000),
            CodexContextOverride::NONE,
        )
        .unwrap()
    }

    #[test]
    fn parse_is_fail_closed() {
        assert_eq!(
            CodexContextOverride::parse(Some(" 500_000 "), Some("TRUE")).unwrap(),
            CodexContextOverride::raising(500_000, true)
        );
        assert_eq!(
            CodexContextOverride::parse(None, Some("")).unwrap(),
            CodexContextOverride::NONE
        );
        assert!(CodexContextOverride::parse(Some("many"), None).is_err());
        assert!(CodexContextOverride::parse(Some("500000"), Some("maybe")).is_err());
    }

    #[test]
    fn deliberate_cap_is_kept_and_reported_as_a_clamp() {
        let resolved = ctx();
        assert_eq!(resolved.context_window, CODEX_CONTEXT_WINDOW_CAP);
        assert_eq!(resolved.advertised_context_window, CODEX_ASTRA_MAX_CONTEXT_WINDOW);
        assert_eq!(resolved.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
        assert!(
            !resolved.has_uncertain_usage,
            "the deliberate cap keeps accounting on the standard published tier"
        );
        let clamp = resolved.clamp.expect("the deliberate cap must be visible");
        assert_eq!(clamp.model_id, CODEX_ASTRA_MODEL_ID);
        assert_eq!(clamp.effective_context_window, CODEX_CONTEXT_WINDOW_CAP);
        let message = clamp.message();
        assert!(message.contains(CODEX_ASTRA_MODEL_ID), "{message}");
        assert!(message.contains("double-priced"), "{message}");
        assert!(message.contains("websocket"), "{message}");
    }

    #[test]
    fn reporter_fires_once_per_transition() {
        let clamp = ctx().clamp.unwrap();
        let mut reporter = CodexContextClampReporter::default();
        assert_eq!(reporter.observe(Some(clamp.clone())), Some(clamp.clone()));
        assert_eq!(reporter.observe(Some(clamp.clone())), None);
        assert_eq!(reporter.observe(None), None);
        assert_eq!(reporter.observe(Some(clamp.clone())), Some(clamp.clone()));
        assert_eq!(reporter.observe(Some(clamp)), None);
    }

    #[test]
    fn live_discovery_bounds_acknowledged_overrides_below_the_family_table() {
        let resolve = |override_| {
            resolve_codex_context_window(
                CODEX_ASTRA_MODEL_ID,
                CodexContextTier::Extended,
                CODEX_CONTEXT_WINDOW_CAP,
                400_000,
                None,
                override_,
            )
        };
        assert!(matches!(
            resolve(CodexContextOverride::raising(500_000, true)),
            Err(CodexContextWindowError::OverrideAboveEntitlement {
                requested: 500_000,
                entitled_max_context_window: 400_000,
                ..
            })
        ));
        let explicit = resolve(CodexContextOverride::raising(400_000, true)).unwrap();
        assert_eq!(explicit.context_window, 400_000);
        assert_eq!(explicit.entitled_max_context_window, 400_000);
        assert!(explicit.has_uncertain_usage);
        let default = resolve(CodexContextOverride::NONE).unwrap();
        assert_eq!(default.context_window, CODEX_CONTEXT_WINDOW_CAP);
        assert!(!default.has_uncertain_usage);
        assert!(default.clamp.is_some());
    }

    #[test]
    fn override_gates_fail_closed() {
        let plain = CodexContextOverride::raising(500_000, false);
        assert!(matches!(
            resolve_codex_context_window(
                CODEX_ASTRA_MODEL_ID,
                CodexContextTier::Default,
                CODEX_CONTEXT_WINDOW_CAP,
                CODEX_ASTRA_MAX_CONTEXT_WINDOW,
                None,
                plain,
            ),
            Err(CodexContextWindowError::OverrideRequiresEntitlement { .. })
        ));
        assert!(matches!(
            resolve_codex_context_window(
                CODEX_ASTRA_MODEL_ID,
                CodexContextTier::Extended,
                CODEX_CONTEXT_WINDOW_CAP,
                CODEX_ASTRA_MAX_CONTEXT_WINDOW,
                None,
                plain,
            ),
            Err(CodexContextWindowError::OverrideRequiresAcknowledgement { .. })
        ));
        assert!(matches!(
            resolve_codex_context_window(
                CODEX_ASTRA_MODEL_ID,
                CodexContextTier::Extended,
                CODEX_CONTEXT_WINDOW_CAP,
                CODEX_ASTRA_MAX_CONTEXT_WINDOW,
                None,
                CodexContextOverride::raising(4_000_000, true),
            ),
            Err(CodexContextWindowError::OverrideAboveEntitlement { .. })
        ));
        assert!(matches!(
            resolve_codex_context_window(
                CODEX_ASTRA_MODEL_ID,
                CodexContextTier::Extended,
                CODEX_CONTEXT_WINDOW_CAP,
                CODEX_ASTRA_MAX_CONTEXT_WINDOW,
                None,
                CodexContextOverride::raising(1, true),
            ),
            Err(CodexContextWindowError::OverrideBelowMinimum { .. })
        ));
    }
}
