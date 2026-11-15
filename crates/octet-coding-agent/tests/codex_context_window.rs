#![allow(missing_docs)]

//! Provider defaults and explicit budgets for subscription routes. Tests of the
//! retired flat cap, plan-name inference and surcharge consent have been replaced
//! by live-limit, default-window and subscription/API separation regressions.

use octet_sdk::codex_context::{
    codex_context_session_note, fallback_context_windows, resolve_codex_context_window,
    CodexContextOverride, CodexContextWindowError, CODEX_MAX_OUTPUT_TOKENS,
};

#[test]
fn documented_sol_default_is_used_without_a_surcharge_gate() {
    let window = resolve_codex_context_window(
        "gpt-6.1-sol",
        500_000,
        872_000,
        Some(128_000),
        CodexContextOverride::NONE,
    )
    .unwrap();
    assert_eq!(window.context_window, 500_000);
    assert_eq!(window.default_context_window, 500_000);
    assert_eq!(window.max_context_window, 872_000);
    assert_eq!(codex_context_session_note("gpt-6.1-sol", &window), None);
}

#[test]
fn reviewed_fallbacks_distinguish_defaults_from_maxima() {
    for (id, default, maximum) in [
        ("gpt-6.1-sol", 500_000, 872_000),
        ("gpt-6.1-terra", 500_000, 872_000),
        ("gpt-6-astra", 272_000, 872_000),
        ("gpt-6-sol", 272_000, 872_000),
        ("gpt-6-luna", 272_000, 872_000),
        ("gpt-5.6-sol", 272_000, 872_000),
        ("gpt-5.6-terra", 272_000, 872_000),
        ("gpt-5.6-luna", 272_000, 872_000),
        ("gpt-5.4", 272_000, 1_000_000),
        ("gpt-5.5", 272_000, 272_000),
        ("codex-auto-review", 272_000, 872_000),
        ("unknown-codex", 272_000, 272_000),
    ] {
        assert_eq!(fallback_context_windows(id), (default, maximum));
        let window =
            resolve_codex_context_window(id, default, maximum, None, CodexContextOverride::NONE)
                .unwrap();
        assert_eq!(window.context_window, default, "{id}");
        assert!(!window.override_applied);
        assert_eq!(codex_context_session_note(id, &window), None);
        let raised = resolve_codex_context_window(
            id,
            default,
            maximum,
            None,
            CodexContextOverride::new(maximum),
        )
        .unwrap();
        assert_eq!(raised.context_window, maximum);
        assert!(matches!(
            resolve_codex_context_window(
                id,
                default,
                maximum,
                None,
                CodexContextOverride::new(maximum + 1)
            ),
            Err(CodexContextWindowError::OverrideAboveMaximum { .. })
        ));
    }
}

#[test]
fn live_limits_override_the_family_fallback_in_both_directions() {
    for (id, default, maximum) in [
        ("gpt-6.1-sol", 200_000, 400_000),
        ("gpt-6-astra", 600_000, 900_000),
        ("new-codex-model", 500_000, 1_200_000),
    ] {
        let window =
            resolve_codex_context_window(id, default, maximum, None, CodexContextOverride::NONE)
                .unwrap();
        assert_eq!(window.context_window, default);
        let raised = resolve_codex_context_window(
            id,
            default,
            maximum,
            None,
            CodexContextOverride::new(maximum),
        )
        .unwrap();
        assert_eq!(raised.context_window, maximum);
        assert!(matches!(
            resolve_codex_context_window(
                id,
                default,
                maximum,
                None,
                CodexContextOverride::new(maximum + 1)
            ),
            Err(CodexContextWindowError::OverrideAboveMaximum { .. })
        ));
    }
}

#[test]
fn crossing_272k_requires_neither_plan_inference_nor_cost_acknowledgement() {
    for requested in [272_001, 372_000, 500_000, 872_000] {
        let window = resolve_codex_context_window(
            "gpt-6.1-sol",
            500_000,
            872_000,
            None,
            CodexContextOverride::new(requested),
        )
        .unwrap();
        assert_eq!(window.context_window, requested);
        assert!(window.override_applied);
        if let Some(note) = codex_context_session_note("gpt-6.1-sol", &window) {
            for obsolete in [
                "double-priced",
                "websocket",
                "uncertain",
                "Pro",
                "272K cap",
                "acknowledge",
            ] {
                assert!(!note.contains(obsolete), "{note}");
            }
            assert!(note.contains("provider default 500K"));
            assert!(note.contains("maximum 872K"));
        }
    }
}

#[test]
fn unusable_or_above_maximum_overrides_are_refused_not_silently_clamped() {
    for requested in [0, 1, 16_383, 872_001, u64::MAX] {
        assert!(resolve_codex_context_window(
            "gpt-6.1-sol",
            500_000,
            872_000,
            None,
            CodexContextOverride::new(requested),
        )
        .is_err());
    }
    let default = resolve_codex_context_window(
        "gpt-6.1-sol",
        500_000,
        872_000,
        None,
        CodexContextOverride::NONE,
    )
    .unwrap();
    assert_eq!(default.context_window, 500_000);
}

#[test]
fn output_is_bounded_by_effective_context_and_astras_output_contract() {
    for output in [None, Some(0), Some(64_000), Some(256_000)] {
        for budget in [16_384, 500_000, 872_000] {
            let astra = resolve_codex_context_window(
                "gpt-6-astra",
                272_000,
                872_000,
                output,
                CodexContextOverride::new(budget),
            )
            .unwrap();
            assert!(astra.max_output_tokens > 0);
            assert!(astra.max_output_tokens <= budget.min(CODEX_MAX_OUTPUT_TOKENS));
        }
    }
}

#[test]
fn override_parsing_is_optional_and_rejects_malformed_token_counts() {
    assert_eq!(
        CodexContextOverride::parse(None).unwrap(),
        CodexContextOverride::NONE
    );
    assert_eq!(
        CodexContextOverride::parse(Some(" ")).unwrap(),
        CodexContextOverride::NONE
    );
    assert_eq!(
        CodexContextOverride::parse(Some(" 500_000 ")).unwrap(),
        CodexContextOverride::new(500_000)
    );
    for invalid in ["many", "-1", "18446744073709551616"] {
        assert!(CodexContextOverride::parse(Some(invalid)).is_err());
    }
}

#[test]
fn explicit_override_note_labels_all_windows_without_internal_identifiers() {
    let window = resolve_codex_context_window(
        "gpt-6.1-sol",
        500_000,
        872_000,
        None,
        CodexContextOverride::new(600_000),
    )
    .unwrap();
    let note = codex_context_session_note("gpt-6.1-sol", &window).unwrap();
    assert!(
        note.contains("provider default 500K, maximum 872K, effective 600K"),
        "{note}"
    );
    assert!(!note.contains("::"));
    assert!(note.len() < 400);
}
