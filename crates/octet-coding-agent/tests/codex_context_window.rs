#![allow(missing_docs)]

//! Behavioural coverage for the deliberate Codex context-window cap, the
//! explicit opt-in override, and the typed clamp notice.
//!
//! `CodexContextTier::Extended` mirrors `ChatGptPlan::uses_max_context_window`
//! (consumer Pro and ProLite); `Default` covers Plus, free, unknown, and
//! unauthenticated accounts. The plan-to-tier mapping itself is asserted by the
//! in-crate tests, because `ChatGptPlan` is crate-private.

use octet_sdk::codex_context::{
    codex_context_session_note, entitled_context_windows, resolve_codex_context_window,
    working_context_window, CodexContextClamp, CodexContextClampReporter, CodexContextOverride,
    CodexContextTier, CodexContextWindow, CodexContextWindowError, CODEX_5_6_CONTEXT_WINDOW,
    CODEX_ABOVE_STANDARD_TIER_OPERATION, CODEX_ASTRA_MAX_CONTEXT_WINDOW,
    CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING, CODEX_CONTEXT_WINDOW_CAP, CODEX_MAX_OUTPUT_TOKENS,
    CODEX_PRO_CONTEXT_WINDOW,
};

fn resolve(
    model_id: &str,
    tier: CodexContextTier,
    user_override: CodexContextOverride,
) -> Result<CodexContextWindow, CodexContextWindowError> {
    let (default_context_window, max_context_window) = entitled_context_windows(model_id);
    resolve_codex_context_window(
        model_id,
        tier,
        default_context_window,
        max_context_window,
        Some(CODEX_MAX_OUTPUT_TOKENS),
        user_override,
    )
}

fn default_tier(model_id: &str) -> CodexContextWindow {
    resolve(
        model_id,
        CodexContextTier::Default,
        CodexContextOverride::NONE,
    )
    .unwrap()
}

fn extended_tier(model_id: &str) -> CodexContextWindow {
    resolve(
        model_id,
        CodexContextTier::Extended,
        CodexContextOverride::NONE,
    )
    .unwrap()
}

// Model-window projection of openai/codex 126a0ee92ed2c5e73cd8a8f7e37563e8527552d7,
// codex-rs/models-manager/models.json. Not account entitlement or pricing evidence.
#[test]
fn current_codex_catalog_windows_keep_standard_tier_defaults() {
    for (id, default, maximum) in [
        ("gpt-6-astra", 272_000, 872_000),
        ("gpt-6.1-sol", 272_000, 872_000),
        ("gpt-6.1-terra", 500_000, 872_000),
        ("gpt-6-sol", 272_000, 872_000),
        ("gpt-6-luna", 272_000, 872_000),
        ("gpt-5.6-sol", 272_000, 872_000),
        ("gpt-5.6-terra", 272_000, 872_000),
        ("gpt-5.6-luna", 272_000, 872_000),
        ("gpt-5.5", 272_000, 272_000),
        ("codex-auto-review", 272_000, 872_000),
    ] {
        assert_eq!(entitled_context_windows(id), (default, maximum), "{id}");
        if maximum > CODEX_CONTEXT_WINDOW_CAP {
            let raised = resolve(
                id,
                CodexContextTier::Extended,
                CodexContextOverride::raising(maximum, true),
            )
            .unwrap();
            assert_eq!(raised.context_window, maximum, "{id}");
            assert!(raised.has_uncertain_usage, "{id}");
            assert!(
                matches!(
                    resolve(
                        id,
                        CodexContextTier::Extended,
                        CodexContextOverride::raising(maximum + 1, true)
                    ),
                    Err(CodexContextWindowError::OverrideAboveEntitlement { .. })
                ),
                "{id}"
            );
        }
        for tier in [CodexContextTier::Default, CodexContextTier::Extended] {
            let resolved = resolve_codex_context_window(
                id,
                tier,
                default,
                maximum,
                None,
                CodexContextOverride::NONE,
            )
            .unwrap();
            assert_eq!(resolved.context_window, 272_000, "{id}/{tier:?}");
            assert!(!resolved.has_uncertain_usage, "{id}/{tier:?}");
        }
    }
}

#[test]
fn luna_requires_consent_immediately_above_the_standard_tier() {
    for requested in [272_001, 300_000, 372_000] {
        assert!(matches!(
            resolve(
                "gpt-5.6-luna",
                CodexContextTier::Default,
                CodexContextOverride::raising(requested, true)
            ),
            Err(CodexContextWindowError::OverrideRequiresEntitlement { .. })
        ));
        assert!(matches!(
            resolve(
                "gpt-5.6-luna",
                CodexContextTier::Extended,
                CodexContextOverride::raising(requested, false)
            ),
            Err(CodexContextWindowError::OverrideRequiresAcknowledgement { .. })
        ));
    }
}

#[test]
fn gpt_6_1_sol_has_the_same_deliberate_working_cap_and_entitlement() {
    assert_eq!(working_context_window("gpt-6.1-sol"), 272_000);
    assert_eq!(entitled_context_windows("gpt-6.1-sol"), (272_000, 872_000));
    assert_eq!(default_tier("gpt-6.1-sol").context_window, 272_000);
    let pro = extended_tier("gpt-6.1-sol");
    assert_eq!(pro.context_window, 272_000);
    assert_eq!(pro.entitled_max_context_window, 872_000);
    assert!(pro.clamp.is_some());
    let raised = resolve(
        "gpt-6.1-sol",
        CodexContextTier::Extended,
        CodexContextOverride::raising(872_000, true),
    )
    .unwrap();
    assert_eq!(raised.context_window, 872_000);
    assert_eq!(raised.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
    assert!(raised.has_uncertain_usage);
    assert!(matches!(
        resolve(
            "gpt-6.1-sol",
            CodexContextTier::Default,
            CodexContextOverride::raising(872_000, true),
        ),
        Err(CodexContextWindowError::OverrideRequiresEntitlement { .. })
    ));
}

#[test]
fn deliberate_cap_holds_for_every_family_even_on_a_pro_plan() {
    for model_id in [
        "gpt-6-astra",
        "gpt-6.1-sol",
        "gpt-6.1-terra",
        "gpt-5.6-luna",
        "gpt-5.4",
        "gpt-5.6-sol",
        "gpt-5.6-terra",
        "gpt-5.5",
        "gpt-5.4-mini",
        "some-legacy-codex",
    ] {
        let resolved = extended_tier(model_id);
        assert_eq!(
            resolved.context_window, CODEX_CONTEXT_WINDOW_CAP,
            "{model_id} must stay on the deliberate cap for a Pro plan"
        );
        assert_eq!(
            resolved.advertised_context_window > CODEX_CONTEXT_WINDOW_CAP,
            resolved.clamp.is_some(),
            "{model_id} must report a clamp exactly when it advertised more than the cap"
        );
        if let Some(clamp) = resolved.clamp.as_ref() {
            assert_eq!(clamp.model_id, model_id);
            assert_eq!(clamp.effective_context_window, CODEX_CONTEXT_WINDOW_CAP);
            assert!(clamp.advertised_context_window > CODEX_CONTEXT_WINDOW_CAP);
        }
    }
    // The families whose advertised envelope exceeds the cap all report it.
    for model_id in ["gpt-6-astra", "gpt-5.4", "gpt-5.6-sol", "gpt-5.6-terra"] {
        assert!(
            extended_tier(model_id).clamp.is_some(),
            "{model_id} advertises more than the deliberate cap"
        );
    }
    // A family whose advertised envelope is the cap itself is not clamped.
    for model_id in ["gpt-5.5", "gpt-5.4-mini", "some-legacy-codex"] {
        assert_eq!(extended_tier(model_id).clamp, None, "{model_id}");
    }
}

#[test]
fn a_plus_plan_cannot_exceed_the_deliberate_cap_even_with_an_override() {
    let plus = CodexContextTier::Default;
    // Values inside each model's own entitlement, so the refusal is the plan
    // gate rather than the entitlement bound.
    for (model_id, requested) in [
        ("gpt-6-astra", 500_000),
        ("gpt-5.4", 500_000),
        ("gpt-5.6-sol", 300_000),
    ] {
        assert_eq!(
            default_tier(model_id).context_window,
            CODEX_CONTEXT_WINDOW_CAP,
            "{model_id} without an entitlement stays on the deliberate cap"
        );
        let error = resolve(
            model_id,
            plus,
            CodexContextOverride::raising(requested, true),
        )
        .expect_err("a non-entitled plan must not raise above the deliberate cap");
        assert!(
            matches!(
                error,
                CodexContextWindowError::OverrideRequiresEntitlement { .. }
            ),
            "{model_id}: {error}"
        );
        assert!(error.to_string().contains("Pro or ProLite"), "{error}");
    }

    // A legacy model has no entitlement above the deliberate cap at all, so the
    // same request is refused by the entitlement bound instead.
    let error = resolve(
        "legacy-codex",
        plus,
        CodexContextOverride::raising(500_000, true),
    )
    .expect_err("a legacy model is not entitled to a larger window");
    assert!(
        matches!(
            error,
            CodexContextWindowError::OverrideAboveEntitlement { .. }
        ),
        "{error}"
    );
}

#[test]
fn acknowledged_pro_override_reaches_the_entitled_maximum() {
    let unacknowledged = CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, false);
    let error = resolve("gpt-6-astra", CodexContextTier::Extended, unacknowledged)
        .expect_err("raising above the cap needs the explicit acknowledgement");
    assert!(
        matches!(
            error,
            CodexContextWindowError::OverrideRequiresAcknowledgement { .. }
        ),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.contains("double-priced"), "{message}");
    assert!(message.contains("websocket"), "{message}");

    let astra = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert_eq!(astra.context_window, CODEX_ASTRA_MAX_CONTEXT_WINDOW);
    assert_eq!(astra.context_window, 872_000);
    assert!(astra.override_applied);
    assert_eq!(astra.clamp, None, "an acknowledged override is not a clamp");

    let pro = resolve(
        "gpt-5.4",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_PRO_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert_eq!(pro.context_window, CODEX_PRO_CONTEXT_WINDOW);
    assert_eq!(pro.context_window, 1_000_000);

    // A partial raise is also allowed and lands exactly on the request.
    let partial = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(500_000, true),
    )
    .unwrap();
    assert_eq!(partial.context_window, 500_000);
}

#[test]
fn an_override_above_entitlement_fails_closed() {
    let error = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW + 1, true),
    )
    .expect_err("above entitlement must fail closed even when acknowledged");
    assert!(
        matches!(
            error,
            CodexContextWindowError::OverrideAboveEntitlement { .. }
        ),
        "{error}"
    );
    let error = resolve(
        "gpt-5.4",
        CodexContextTier::Extended,
        CodexContextOverride::raising(2_000_000, true),
    )
    .expect_err("above entitlement must fail closed even when acknowledged");
    assert!(error.to_string().contains("entitlement"), "{error}");

    let below_minimum = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(1, true),
    )
    .expect_err("an unusable window must fail closed");
    assert!(
        matches!(
            below_minimum,
            CodexContextWindowError::OverrideBelowMinimum { .. }
        ),
        "{below_minimum}"
    );

    // The refusal never silently becomes a smaller window: the deliberate cap
    // still applies, exactly as if no override had been requested.
    assert_eq!(default_tier("gpt-6-astra").context_window, 272_000);
}

#[test]
fn above_the_standard_tier_accounting_is_uncertain() {
    assert!(
        !default_tier("gpt-6-astra").has_uncertain_usage,
        "272K is the certain, published tier"
    );
    let raised = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert!(
        raised.has_uncertain_usage,
        "above 272K the whole request is double-priced, so cost cannot be claimed as exact"
    );
    assert_eq!(
        raised.uncertain_usage_operation(),
        Some("codex-context-above-272k")
    );
    assert_eq!(
        default_tier("gpt-6-astra").uncertain_usage_operation(),
        None
    );

    // Luna no longer enters the higher-priced tier without an override.
    let luna = extended_tier("gpt-5.6-luna");
    assert_eq!(luna.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert!(!luna.has_uncertain_usage);
    assert!(luna.clamp.is_some());
}

#[test]
fn output_is_never_larger_than_the_context_window() {
    for model_id in ["gpt-6-astra", "gpt-5.4", "gpt-5.6-sol", "gpt-5.6-luna"] {
        for tier in [CodexContextTier::Default, CodexContextTier::Extended] {
            for override_value in [None, Some(16_384)] {
                let user_override = match override_value {
                    Some(tokens) => CodexContextOverride::raising(tokens, true),
                    None => CodexContextOverride::NONE,
                };
                let resolved = resolve(model_id, tier, user_override).unwrap();
                assert!(
                    resolved.max_output_tokens > 0
                        && resolved.max_output_tokens <= resolved.context_window,
                    "{model_id}: {} output tokens in a {} window",
                    resolved.max_output_tokens,
                    resolved.context_window
                );
            }
        }
    }
    // Astra keeps its 128K output contract even at 872K of input envelope.
    let astra = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert_eq!(astra.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
}

#[test]
fn clamp_notice_fires_once_per_transition_and_never_without_a_clamp() {
    // The clamp belongs to the entitled (Pro/ProLite) path: that is where the
    // deliberate cap visibly reduces the advertised window.
    let clamp = extended_tier("gpt-6-astra")
        .clamp
        .expect("astra is clamped on an entitled plan");
    let mut reporter = CodexContextClampReporter::default();
    assert_eq!(reporter.observe(Some(clamp.clone())), Some(clamp.clone()));
    assert_eq!(reporter.observe(Some(clamp.clone())), None);
    assert_eq!(reporter.observe(Some(clamp.clone())), None);
    assert_eq!(reporter.observe(None), None);
    assert_eq!(reporter.observe(None), None);
    assert_eq!(reporter.observe(Some(clamp.clone())), Some(clamp.clone()));

    let other = extended_tier("gpt-5.4").clamp.expect("gpt-5.4 is clamped");
    assert_ne!(other, clamp, "each model has its own advertised window");
    assert_eq!(reporter.observe(Some(other.clone())), Some(other));

    // A session that is not clamped reports nothing, and an override that raised
    // the window is explicitly chosen rather than clamped.
    assert_eq!(reporter.observe(default_tier("gpt-6-astra").clamp), None);
    let raised = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert_eq!(reporter.observe(raised.clamp), None);

    let message = clamp.message();
    assert!(message.contains("gpt-6-astra"), "{message}");
    // Every window the note quotes is labelled, and the deliberate cap is the
    // labelled 272K figure rather than a bare number the reader must decode.
    assert!(message.contains("advertised 872K"), "{message}");
    assert!(message.contains("effective 272K"), "{message}");
    assert!(message.contains("272K Codex cap"), "{message}");
    assert!(
        message.contains("reduces the advertised window by 600K"),
        "{message}"
    );
    assert!(message.contains("double-priced"), "{message}");
    assert!(message.contains("websocket"), "{message}");
    // The in-app remedy leads; the environment variables stay the scriptable
    // alternative.
    let remedy = message.find("--codex-context-window");
    let scriptable = message.find("OCTET_CODEX_CONTEXT_WINDOW");
    assert!(remedy.is_some(), "{message}");
    assert!(
        remedy < scriptable,
        "the in-app flag must be offered before the env vars: {message}"
    );
    assert!(
        message.contains("OCTET_CODEX_CONTEXT_WINDOW_ACKNOWLEDGE_COST_CLIFF=1"),
        "{message}"
    );
    assert!(
        message.len() < 1024,
        "the notice is bounded: {}",
        message.len()
    );
}

/// The single session note is a catalog-free, effective-model-only value: it is
/// produced for one model, never per catalog entry and never per turn.
#[test]
fn one_session_note_per_effective_model_and_none_for_an_unclamped_legacy_route() {
    // A Pro plan clamped to the deliberate cap: one note, naming the reduction.
    let clamped = extended_tier("gpt-6-astra");
    let note = codex_context_session_note("gpt-6-astra", &clamped).expect("astra is clamped");
    assert!(note.contains("advertised 872K"), "{note}");
    assert!(note.contains("entitled 872K"), "{note}");
    assert!(note.contains("effective 272K"), "{note}");

    // A legacy route whose advertised window is the cap itself needs no note.
    let legacy = extended_tier("some-legacy-codex");
    assert_eq!(legacy.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert_eq!(
        codex_context_session_note("some-legacy-codex", &legacy),
        None
    );

    // A non-Codex model never reaches this policy at all; there is no window to
    // note and nothing is emitted for one.
    assert_eq!(
        codex_context_session_note("deepseek/v4.1-flash", &legacy),
        None
    );

    // The note is a pure function of the resolution: resolving the same session
    // model again yields the same single note, and the launch path calls it once.
    assert_eq!(
        codex_context_session_note("gpt-6-astra", &clamped).as_deref(),
        Some(note.as_str())
    );
}

/// A deliberately raised Luna window still reports consistent facts.
#[test]
fn the_luna_note_distinguishes_standard_defaults_from_explicit_raises() {
    let luna = default_tier("gpt-5.6-luna");
    assert_eq!(codex_context_session_note("gpt-5.6-luna", &luna), None);
    let raised = resolve(
        "gpt-5.6-luna",
        CodexContextTier::Extended,
        CodexContextOverride::raising(872_000, true),
    )
    .unwrap();
    let note = codex_context_session_note("gpt-5.6-luna", &raised).unwrap();
    assert!(note.contains("entitled 872K"), "{note}");
    assert!(note.contains("effective 872K"), "{note}");
    assert!(note.contains("above the 272K standard tier"), "{note}");
    assert!(!note.contains("reduces the advertised window"), "{note}");

    let reduced = resolve_codex_context_window(
        "gpt-5.6-luna",
        CodexContextTier::Extended,
        500_000,
        500_000,
        Some(CODEX_MAX_OUTPUT_TOKENS),
        CodexContextOverride::NONE,
    )
    .unwrap();
    assert_eq!(reduced.context_window, CODEX_CONTEXT_WINDOW_CAP);
    let note = codex_context_session_note("gpt-5.6-luna", &reduced).unwrap();
    assert!(note.contains("effective 272K"), "{note}");
    assert!(
        note.contains("reduces the advertised window by 228K"),
        "{note}"
    );
    assert!(!reduced.has_uncertain_usage);
}

/// The note is user-facing prose: no internal Rust API and no internal operation
/// identifier may leak into a transcript.
#[test]
fn the_session_note_never_names_an_internal_api_or_operation() {
    let mut variants = vec![
        codex_context_session_note("gpt-6-astra", &extended_tier("gpt-6-astra")).unwrap(),
        codex_context_session_note("gpt-5.6-luna", &extended_tier("gpt-5.6-luna")).unwrap(),
        extended_tier("gpt-6-astra").clamp.unwrap().message(),
        extended_tier("gpt-5.4").clamp.unwrap().message(),
    ];
    variants.push(
        codex_context_session_note(
            "gpt-6-astra",
            &resolve(
                "gpt-6-astra",
                CodexContextTier::Extended,
                CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    for note in variants {
        for forbidden in [
            "Session::",
            "record_usage_uncertainty",
            CODEX_ABOVE_STANDARD_TIER_OPERATION,
            "CodexContextWindow",
            "has_uncertain_usage",
        ] {
            assert!(!note.contains(forbidden), "{forbidden} leaked into: {note}");
        }
        assert!(note.starts_with("note: Codex model"), "{note}");
        assert!(note.len() < 1200, "the note stays bounded: {}", note.len());
    }
}

#[test]
fn override_parsing_is_opt_in_and_fail_closed() {
    assert_eq!(
        CodexContextOverride::parse(None, None).unwrap(),
        CodexContextOverride::NONE
    );
    assert_eq!(
        CodexContextOverride::parse(Some(""), Some("no")).unwrap(),
        CodexContextOverride::NONE
    );
    assert_eq!(
        CodexContextOverride::parse(Some(" 500_000 "), Some("TRUE")).unwrap(),
        CodexContextOverride::raising(500_000, true)
    );
    assert!(CodexContextOverride::parse(Some("many"), Some("1")).is_err());
    assert!(CodexContextOverride::parse(Some("500000"), Some("maybe")).is_err());
}

#[test]
fn the_acknowledgement_word_is_required_and_is_the_documented_wording() {
    // The wording is the contract a frontend renders before it accepts the flag.
    assert!(
        CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING.contains("2x input and 1.5x output"),
        "{CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING}"
    );
    assert!(CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING.contains("websocket"));
    assert!(CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING.contains("272K"));

    let error = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, false),
    )
    .expect_err("raising above the cap without the acknowledgement must fail closed");
    assert!(
        matches!(
            error,
            CodexContextWindowError::OverrideRequiresAcknowledgement { .. }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains(CODEX_CONTEXT_ACKNOWLEDGEMENT_WORDING),
        "the refusal must quote the exact wording a frontend renders: {error}"
    );

    // The acknowledged form of the same request lands on the entitlement.
    let granted = resolve(
        "gpt-6-astra",
        CodexContextTier::Extended,
        CodexContextOverride::raising(CODEX_ASTRA_MAX_CONTEXT_WINDOW, true),
    )
    .unwrap();
    assert_eq!(granted.context_window, CODEX_ASTRA_MAX_CONTEXT_WINDOW);
}

#[test]
fn the_above_standard_tier_operation_id_is_stable() {
    // Frontends and the bootstrap notice pass this id to
    // `Session::record_usage_uncertainty`; it is part of the public contract.
    assert_eq!(
        CODEX_ABOVE_STANDARD_TIER_OPERATION,
        "codex-context-above-272k"
    );

    // An explicitly raised Luna window retains the stable uncertainty marker.
    let luna = resolve(
        "gpt-5.6-luna",
        CodexContextTier::Extended,
        CodexContextOverride::raising(372_000, true),
    )
    .unwrap();
    assert_eq!(luna.context_window, 372_000);
    assert!(luna.override_applied);
    assert!(luna.has_uncertain_usage);
    assert_eq!(
        luna.uncertain_usage_operation(),
        Some(CODEX_ABOVE_STANDARD_TIER_OPERATION)
    );
    assert_eq!(luna.clamp, None);

    // Every family that stays on the deliberate cap owes no obligation.
    for model_id in ["gpt-6-astra", "gpt-5.4", "gpt-5.6-sol", "legacy-codex"] {
        let resolved = extended_tier(model_id);
        assert_eq!(
            resolved.context_window, CODEX_CONTEXT_WINDOW_CAP,
            "{model_id}"
        );
        assert_eq!(resolved.uncertain_usage_operation(), None, "{model_id}");
    }
}

#[test]
fn every_family_uses_the_documented_standard_cap() {
    assert_eq!(CODEX_CONTEXT_WINDOW_CAP, 272_000);
    assert_eq!(working_context_window("gpt-6-astra"), 272_000);
    assert_eq!(working_context_window("gpt-5.4"), 272_000);
    assert_eq!(working_context_window("legacy-codex"), 272_000);
    assert_eq!(working_context_window("gpt-5.6-luna"), 272_000);
    assert_eq!(CODEX_5_6_CONTEXT_WINDOW, 272_000);
    // The public notice type is constructible by every frontend.
    let notice = CodexContextClamp {
        model_id: "gpt-6-astra".to_owned(),
        advertised_context_window: 872_000,
        effective_context_window: 272_000,
    };
    assert!(notice.message().contains("gpt-6-astra"));
}
