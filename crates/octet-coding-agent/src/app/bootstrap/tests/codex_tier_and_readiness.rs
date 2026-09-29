//! Codex tier and readiness
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;
use crate::codex_context::CodexContextClampReporter;

#[test]
fn codex_context_tier_follows_the_plan_entitlement() {
    use crate::auth::codex::ChatGptPlan;
    for (plan, tier) in [
        (Some(ChatGptPlan::Pro), CodexContextTier::Extended),
        (Some(ChatGptPlan::ProLite), CodexContextTier::Extended),
        (Some(ChatGptPlan::Plus), CodexContextTier::Default),
        (Some(ChatGptPlan::Free), CodexContextTier::Default),
        (Some(ChatGptPlan::Team), CodexContextTier::Default),
        (
            Some(ChatGptPlan::Unknown("future-tier".into())),
            CodexContextTier::Default,
        ),
        (None, CodexContextTier::Default),
    ] {
        assert_eq!(codex_context_tier(plan.as_ref()), tier, "{plan:?}");
    }
}

pub(in crate::app::bootstrap) fn codex_discovered_model(
    model_id: &str,
    default_context_window: u64,
    max_context_window: u64,
    max_output_tokens: u64,
) -> DiscoveredCodexModel {
    DiscoveredCodexModel {
        id: model_id.to_owned(),
        display_name: None,
        reasoning_options: codex_fallback_reasoning_options(model_id),
        context_window: default_context_window,
        default_context_window,
        max_context_window,
        max_output_tokens,
        min_effort: codex_min_effort(model_id),
        max_effort: codex_max_effort(model_id),
        reasoning_effort_updates: false,
        responses_lite: false,
        agent_delegation: None,
    }
}

#[test]
fn codex_registration_keeps_the_deliberate_cap_and_reports_it_once() {
    let astra = codex_discovered_model("gpt-6-astra", 872_000, 872_000, 128_000);
    let mut reporter = CodexContextClampReporter::default();
    let resolution = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::NONE,
    );
    assert_eq!(resolution.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert_eq!(resolution.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
    assert!(!resolution.override_applied);
    // The deliberate cap keeps accounting on the standard published tier; the
    // clamp must still be visible rather than silent.
    assert!(!resolution.has_uncertain_usage);
    let clamp = resolution.clamp.clone().expect("astra is clamped");
    assert_eq!(clamp.advertised_context_window, 872_000);
    assert_eq!(clamp.effective_context_window, CODEX_CONTEXT_WINDOW_CAP);

    // Report through the same boundary the registration loop uses.
    assert_eq!(
        reporter.observe(resolution.clamp.clone()),
        Some(clamp.clone())
    );
    assert_eq!(reporter.observe(resolution.clamp.clone()), None);
    assert_eq!(reporter.observe(None), None);
    assert_eq!(reporter.observe(resolution.clamp), Some(clamp));
}

#[test]
fn codex_registration_applies_only_an_acknowledged_entitled_override() {
    let astra = codex_discovered_model("gpt-6-astra", 872_000, 872_000, 128_000);

    // A Plus-style session cannot raise the window even when acknowledged.
    let refused = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Default,
        CodexContextOverride::raising(500_000, true),
    );
    assert_eq!(refused.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert!(!refused.override_applied);

    // An unacknowledged Pro request is refused too.
    let refused = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::raising(500_000, false),
    );
    assert_eq!(refused.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert!(!refused.override_applied);

    // Above entitlement is refused rather than silently clamped.
    let refused = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::raising(872_001, true),
    );
    assert_eq!(refused.context_window, CODEX_CONTEXT_WINDOW_CAP);

    // The acknowledged Pro request raises to the model's entitlement.
    let raised = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::raising(872_000, true),
    );
    assert_eq!(raised.context_window, CODEX_ASTRA_MAX_CONTEXT_WINDOW);
    assert!(raised.override_applied);
    assert!(raised.clamp.is_none());
    assert!(raised.has_uncertain_usage);
    assert_eq!(
        raised.uncertain_usage_operation(),
        Some("codex-context-above-272k")
    );
    assert_eq!(raised.max_output_tokens, CODEX_MAX_OUTPUT_TOKENS);
}

#[test]
fn codex_registration_names_above_tier_accounting_for_every_raised_route() {
    // The recorded session note branches on `uncertain_usage_operation`, so this
    // asserts the same trigger the registration loop uses. The documented 372K
    // luna window is above the standard tier without any override, while the
    // deliberate 272K cap owes no obligation.
    let luna = codex_discovered_model(
        "gpt-5.6-luna",
        CODEX_5_6_CONTEXT_WINDOW,
        CODEX_5_6_CONTEXT_WINDOW,
        128_000,
    );
    let resolution = codex_context_resolve_for_registration(
        &luna,
        CodexContextTier::Default,
        CodexContextOverride::NONE,
    );
    assert_eq!(resolution.context_window, CODEX_5_6_CONTEXT_WINDOW);
    assert!(!resolution.override_applied, "luna needs no override");
    assert!(
        resolution.clamp.is_none(),
        "372K luna is the documented window"
    );
    assert!(
        resolution.has_uncertain_usage,
        "above 272K the whole request is metered differently"
    );
    assert_eq!(
        resolution.uncertain_usage_operation(),
        Some("codex-context-above-272k")
    );

    let astra = codex_discovered_model("gpt-6-astra", 872_000, 872_000, 128_000);
    let capped = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::NONE,
    );
    assert_eq!(capped.context_window, CODEX_CONTEXT_WINDOW_CAP);
    assert_eq!(capped.uncertain_usage_operation(), None);
    assert!(capped.clamp.is_some(), "the cap stays visible");

    // An acknowledged raise turns the same route into an uncertain one.
    let raised = codex_context_resolve_for_registration(
        &astra,
        CodexContextTier::Extended,
        CodexContextOverride::raising(500_000, true),
    );
    assert_eq!(raised.context_window, 500_000);
    assert_eq!(
        raised.uncertain_usage_operation(),
        Some("codex-context-above-272k")
    );
}

// ---------------------------------------------------------------------------
// Startup readiness (wiring12b, wave 11): a proven selection initializes only
// its own route before the first turn; unrelated providers cannot delay it.
// Every proof below is a consultation/request counter or a typed decision —
// none of them depends on a wall-clock threshold.
// ---------------------------------------------------------------------------

#[test]
fn readiness_plan_narrows_only_a_proven_route() {
    let directory = tempfile::tempdir().unwrap();

    // An explicit selection names its own builtin route.
    let explicit = config(directory.path(), Some("codex/gpt-6-astra"));
    assert_eq!(catalog_readiness(&explicit).route_ids(), vec!["codex"]);

    // Un-namespaced ids may belong to any route (Codex's historical
    // compatibility ids, a custom OpenAI-compatible registry, an extension
    // provider) and must stay on the fleet plan; a real namespace that is not
    // the selected one must not change that.
    assert!(catalog_readiness(&config(directory.path(), Some("gpt-5.6-sol"))).is_fleet());
    assert!(catalog_readiness(&config(directory.path(), Some("custom/probe"))).is_fleet());
    assert!(catalog_readiness(&config(directory.path(), Some("acme-extension/worker"))).is_fleet());
    assert_eq!(
        catalog_readiness(&config(directory.path(), Some("deepseek/deepseek-v4-pro"))).route_ids(),
        vec!["deepseek"]
    );

    // Model-less setup and the picker enumerate every provider.
    assert!(catalog_readiness(&config(directory.path(), None)).is_fleet());

    // A resumed session without an explicit selection may carry provenance only
    // the session file knows, so readiness stays full. An explicit selection is
    // authoritative over that provenance.
    for resume in [
        ResumeSelector::Resume(Some("session-a".into())),
        ResumeSelector::Continue,
        ResumeSelector::Fork(Some("session-a".into())),
    ] {
        let mut resumed = config(directory.path(), None);
        resumed.resume = resume.clone();
        assert!(
            catalog_readiness(&resumed).is_fleet(),
            "restored provenance {resume:?} must keep the fleet plan"
        );

        let mut overridden = config(directory.path(), Some("codex/gpt-6-astra"));
        overridden.resume = resume;
        assert_eq!(
            catalog_readiness(&overridden).route_ids(),
            vec!["codex"],
            "an explicit selection is authoritative over restored provenance"
        );
    }
}

#[test]
fn readiness_plan_keeps_configured_compaction_routes() {
    let directory = tempfile::tempdir().unwrap();

    let mut same_route = config(directory.path(), Some("codex/gpt-6-astra"));
    same_route.compaction.compact_model = Some(ModelId("codex/gpt-5.6-sol".into()));
    assert_eq!(catalog_readiness(&same_route).route_ids(), vec!["codex"]);

    let mut other_route = config(directory.path(), Some("codex/gpt-6-astra"));
    other_route.compaction.compact_model = Some(ModelId("deepseek/deepseek-v4-pro".into()));
    assert_eq!(
        catalog_readiness(&other_route).route_ids(),
        vec!["codex", "deepseek"],
        "the configured compaction route must exist before the first turn"
    );

    // An un-namespaced compaction id cannot be proven to be a builtin route, and
    // `build_app` fails closed when it does not resolve: ambiguity stays fleet
    // rather than silently dropping the route.
    let mut unproven = config(directory.path(), Some("codex/gpt-6-astra"));
    unproven.compaction.compact_model = Some(ModelId("cheap-compactor".into()));
    assert!(catalog_readiness(&unproven).is_fleet());
}

/// `bedrock` is the reported startup blocker: its AWS credential chain (including
/// EC2 instance metadata) is *always* treated as configured, so the fleet plan
/// spawns and joins it on every launch. The declaration-consultation counter is
/// the exact request count: a Codex route must not touch it at all.
#[test]
fn narrowed_readiness_never_consults_an_unrelated_provider() {
    let directory = tempfile::tempdir().unwrap();
    let plan = catalog_readiness(&config(directory.path(), Some("codex/gpt-6-astra")));
    assert_eq!(plan.route_ids(), vec!["codex"]);
    assert!(plan.includes(crate::providers::CODEX.id));
    assert!(
        !plan.includes("bedrock"),
        "the AWS credential chain is not part of a Codex route"
    );
    assert!(
        CatalogReadiness::Fleet.includes("bedrock"),
        "the fleet plan still initializes the AWS route"
    );

    reset_readiness_declarations_consulted();
    let (_catalog, _notes) = model_catalog_for_readiness(false, &plan).unwrap();
    assert_eq!(
        readiness_declarations_consulted(),
        vec![crate::providers::CODEX.id],
        "exactly the selected route may be consulted; an unrelated configured provider must not be"
    );
}

/// A fresh, account- and plan-matched cache is already validated inside its
/// freshness window: it must serve dynamic capability with zero discovery
/// requests. The closure is the request counter.
#[test]
fn a_fresh_codex_cache_serves_dynamic_capabilities_without_a_discovery_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();
    let cached = CodexDiscovery {
        claims: claims.clone(),
        models: codex_models_from_response(
            &serde_json::json!({
                "models": [{
                    "slug": "cached-account-model",
                    "context_window": 196_000,
                    "max_output_tokens": 24_000,
                    "use_responses_lite": true,
                    "multi_agent_version": "v2",
                    "supported_reasoning_levels": ["high", "ultra"]
                }]
            }),
            claims.plan.as_ref(),
        )
        .unwrap(),
    };
    save_codex_model_cache(&store, &cached).unwrap();

    let discovery_requests = std::cell::Cell::new(0_u32);
    let (models, source) = codex_inventory_models(&store, &claims, false, false, |_store| {
        discovery_requests.set(discovery_requests.get() + 1);
        anyhow::bail!("a fresh account- and plan-matched cache must not trigger discovery")
    });
    assert_eq!(source, CodexInventorySource::FreshCache);
    assert_eq!(discovery_requests.get(), 0, "zero inventory requests");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "cached-account-model");
    assert_eq!(models[0].context_window, 196_000);
    assert!(
        models[0].responses_lite,
        "a validated fresh cache keeps its dynamic capability"
    );
    assert_eq!(models[0].agent_delegation, Some(AgentDelegation::V2));
}

/// A cache that is stale, bound to another account, or structurally invalid must
/// never be trusted. Discovery runs exactly once; when it also fails, the
/// conservative fallback keeps the plan's entitlement window and carries no
/// dynamic capability and no unadvertised Ultra.
#[test]
fn an_unusable_codex_cache_discovers_once_and_never_serves_dynamic_capabilities() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();
    let live = CodexDiscovery {
        claims: claims.clone(),
        models: codex_models_from_response(
            &serde_json::json!({
                "models": [{
                    "slug": "live-account-model",
                    "context_window": 300_000,
                    "max_output_tokens": 32_000,
                    "use_responses_lite": true,
                    "multi_agent_version": "v2",
                    "supported_reasoning_levels": ["low", "ultra"]
                }]
            }),
            claims.plan.as_ref(),
        )
        .unwrap(),
    };
    let mut cached = live.clone();
    cached.models[0].id = "cached-account-model".to_owned();
    cached.models[0].context_window = 196_000;
    cached.models[0].default_context_window = 196_000;
    save_codex_model_cache(&store, &cached).unwrap();

    let cache_path = {
        let stem = path.file_stem().and_then(|value| value.to_str()).unwrap();
        path.with_file_name(format!("{stem}-models.json"))
    };

    // Stale: outside the freshness window the cache is skipped and discovery
    // runs exactly once, and the live inventory (not the cache) is registered.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&cache_path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::SystemTime::now()
                    .checked_sub(CODEX_MODEL_CACHE_REFRESH_INTERVAL + Duration::from_secs(1))
                    .unwrap(),
            ),
        )
        .unwrap();
    let discovery_requests = std::cell::Cell::new(0_u32);
    let (models, source) = codex_inventory_models(&store, &claims, false, false, {
        let live = live.clone();
        |_store| {
            discovery_requests.set(discovery_requests.get() + 1);
            Ok(live)
        }
    });
    assert_eq!(source, CodexInventorySource::OnlineDiscovery);
    assert_eq!(discovery_requests.get(), 1, "exactly one discovery request");
    assert_eq!(models[0].id, "live-account-model");

    // Account mismatch: a mismatched account binding is refused without a
    // request to the network *as a cache*; discovery still seeds the cache.
    let mut mismatched = live.clone();
    mismatched.models[0].id = "other-account-model".to_owned();
    let mut mismatched_bytes = serde_json::to_value(CodexModelCache {
        version: CODEX_MODEL_CACHE_VERSION,
        account_id: "acct_other".into(),
        plan: codex_plan_cache_key(&claims).map(str::to_owned),
        models: mismatched.models.clone(),
    })
    .unwrap();
    mismatched_bytes["models"][0]["context_window"] = serde_json::json!(300_000);
    store
        .save_model_cache(&serde_json::to_vec(&mismatched_bytes).unwrap())
        .unwrap();
    let discovery_requests = std::cell::Cell::new(0_u32);
    let (models, source) = codex_inventory_models(&store, &claims, false, false, {
        let live = live.clone();
        |_store| {
            discovery_requests.set(discovery_requests.get() + 1);
            Ok(live)
        }
    });
    assert_eq!(source, CodexInventorySource::OnlineDiscovery);
    assert_eq!(discovery_requests.get(), 1);
    assert_eq!(models[0].id, "live-account-model");

    // Invalid: a structurally broken cache is refused, discovery runs once, and
    // its failure falls back to the conservative catalog — never to the broken
    // cache's dynamic capability.
    let mut invalid = cached.clone();
    invalid.models[0].context_window = 0;
    save_codex_model_cache(&store, &invalid).unwrap();
    let discovery_requests = std::cell::Cell::new(0_u32);
    let (models, source) = codex_inventory_models(&store, &claims, false, false, |_store| {
        discovery_requests.set(discovery_requests.get() + 1);
        anyhow::bail!("live discovery is unavailable in this test")
    });
    assert_eq!(source, CodexInventorySource::ConservativeFallback);
    assert_eq!(discovery_requests.get(), 1);
    assert!(
        models.iter().any(|model| model.id == "gpt-5.6-luna"),
        "the fallback catalog keeps the plan's entitlement models"
    );
    let luna = models
        .iter()
        .find(|model| model.id == "gpt-5.6-luna")
        .unwrap();
    assert_eq!(
        luna.context_window, CODEX_5_6_CONTEXT_WINDOW,
        "the plan's entitlement window survives the fallback"
    );
    for model in &models {
        assert!(!model.responses_lite, "{}", model.id);
        assert_eq!(model.agent_delegation, None, "{}", model.id);
        assert_ne!(
            model.max_effort,
            octet_ai::ReasoningEffort::Ultra,
            "{}",
            model.id
        );
    }
}

#[test]
fn a_route_readiness_worker_that_settles_in_time_is_joined_and_reported() {
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let value = run_route_readiness(
        "unit route readiness",
        Duration::from_secs(5),
        cancel,
        |_stop| -> anyhow::Result<u32> { Ok(7) },
    )
    .unwrap();
    assert_eq!(value, 7);
}

/// A deadline must be typed, bounded, and must *signal* cancellation: without
/// the signal the worker's own check would never fire and the credential step
/// could still start an inventory request the launch no longer wants.
#[test]
fn a_timed_out_route_readiness_envelope_cancels_and_settles_the_worker() {
    let (settled_tx, settled_rx) = std::sync::mpsc::channel();
    let error = run_route_readiness(
        "unit route readiness",
        Duration::from_millis(20),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        move |stop| -> anyhow::Result<()> {
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = settled_tx.send(());
            anyhow::bail!("cancelled before the inventory request")
        },
    )
    .unwrap_err();
    let timeout = error
        .downcast_ref::<RouteReadinessTimeout>()
        .expect("a deadline is reported as a typed, bounded timeout");
    assert_eq!(timeout.phase, "unit route readiness");
    assert_eq!(timeout.waited, Duration::from_millis(20));
    assert!(
        timeout.to_string().contains("did not finish within 20 ms"),
        "{timeout}"
    );
    settled_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the timed-out worker must observe cancellation and settle");
}

/// The refresh lock is the one wait that cannot be cancelled mid-acquisition
/// (it only ever uses a bounded non-blocking attempt). A timed-out worker that
/// holds it must therefore settle and release it, with no worker left behind.
#[test]
fn a_timed_out_route_readiness_worker_releases_the_refresh_lock() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let (holding_tx, holding_rx) = std::sync::mpsc::channel();
    let (settled_tx, settled_rx) = std::sync::mpsc::channel();
    let error = run_route_readiness(
        "unit lock holder",
        Duration::from_millis(20),
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        {
            let store = store.clone();
            move |stop| -> anyhow::Result<()> {
                let guard = store.lock_refresh_within(Duration::from_secs(1))?;
                holding_tx.send(()).unwrap();
                while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                drop(guard);
                let _ = settled_tx.send(());
                anyhow::bail!("cancelled with the refresh lock released")
            }
        },
    )
    .unwrap_err();
    assert!(
        error.downcast_ref::<RouteReadinessTimeout>().is_some(),
        "{error:#}"
    );
    holding_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the worker must acquire the refresh lock before the deadline");
    settled_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the timed-out worker must settle instead of staying blocked");

    // The lock is free again: an acquisition that succeeds well inside a
    // generous deadline proves the timed-out worker released it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match store.lock_refresh_within(Duration::from_millis(10)) {
            Ok(guard) => {
                drop(guard);
                break;
            }
            Err(error) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
                let _ = error;
            }
            Err(error) => panic!("the refresh lock must be released: {error:#}"),
        }
    }
}

/// The strongest form of "an unrelated provider cannot delay readiness": the
/// AWS credential chain is poisoned so that *entering* it fails closed. A
/// Codex-only plan must complete with the selected route named and zero
/// consultations of the unrelated provider — a request counter plus a
/// fail-closed probe, never a wall-clock threshold.
#[test]
fn a_poisoned_unrelated_provider_is_never_entered_by_a_narrowed_plan() {
    let directory = tempfile::tempdir().unwrap();
    let plan = catalog_readiness(&config(directory.path(), Some("codex/gpt-6-astra")));
    assert_eq!(plan.route_ids(), vec!["codex"]);
    assert!(plan.includes(crate::providers::CODEX.id));

    let bedrock = BUILTIN_PROVIDER_DECLARATIONS
        .iter()
        .find(|declaration| declaration.id == "bedrock")
        .expect("the AWS declaration is a generated builtin");
    let poison = ForbiddenReadinessConsultation::new(bedrock.id);
    // The probe is live: entering the poisoned declaration would panic, so a
    // narrowed plan that consulted it could not pass this test.
    assert!(
        std::panic::catch_unwind(|| declaration_is_configured(bedrock)).is_err(),
        "the fail-closed probe must fire when the declaration is actually entered"
    );

    reset_readiness_declarations_consulted();
    let (_catalog, _notes) = model_catalog_for_readiness(false, &plan).unwrap();
    drop(poison);
    let consulted = readiness_declarations_consulted();
    assert!(
        consulted.contains(&crate::providers::CODEX.id),
        "the selected route is consulted: {consulted:?}"
    );
    assert!(
        !consulted.contains(&"bedrock"),
        "an unrelated configured provider must never be entered: {consulted:?}"
    );
}

/// The unit-test (fixture) registration path must not buy determinism by
/// ignoring a fresh, account- and plan-matched cache: the cache still decides
/// the inventory, reduced to the conservative contract because it cannot be
/// revalidated online, and the discovery closure is never called.
#[test]
fn a_fixture_registration_honours_a_fresh_cache_without_a_discovery_request() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("fixture-codex.json");
    write_codex_credential(&path, false, "plus");
    let store = crate::auth::codex::CredentialStore::new(&path);
    let claims = crate::auth::codex::usable_subscription_claims(&store)
        .unwrap()
        .unwrap();

    let discovery_requests = std::cell::Cell::new(0_u32);
    // No cache yet: the checked-in catalog, labelled as the fixture path.
    let (models, source) = codex_inventory_models(&store, &claims, false, true, |_store| {
        discovery_requests.set(discovery_requests.get() + 1);
        anyhow::bail!("the fixture path must never discover")
    });
    assert_eq!(source, CodexInventorySource::Fixture);
    assert_eq!(discovery_requests.get(), 0);
    assert!(models.iter().any(|model| model.id == "gpt-5.6-sol"));

    let cached = CodexDiscovery {
        claims: claims.clone(),
        models: codex_models_from_response(
            &serde_json::json!({
                "models": [{
                    "slug": "cached-fixture-model",
                    "context_window": 196_000,
                    "max_output_tokens": 24_000,
                    "use_responses_lite": true,
                    "multi_agent_version": "v2",
                    "supported_reasoning_levels": ["high", "ultra"]
                }]
            }),
            claims.plan.as_ref(),
        )
        .unwrap(),
    };
    save_codex_model_cache(&store, &cached).unwrap();

    let (models, source) = codex_inventory_models(&store, &claims, false, true, |_store| {
        discovery_requests.set(discovery_requests.get() + 1);
        anyhow::bail!("the fixture path must never discover")
    });
    assert_eq!(source, CodexInventorySource::FreshCache);
    assert_eq!(discovery_requests.get(), 0, "no inventory request");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "cached-fixture-model");
    assert!(
        !models[0].responses_lite,
        "the fixture path reduces a cache it cannot revalidate"
    );
    assert_eq!(models[0].agent_delegation, None);
    assert_ne!(models[0].max_effort, octet_ai::ReasoningEffort::Ultra);
}

/// The recorded note survives catalog construction into the `App` a frontend
/// holds, is visible to an on-demand surface, is delivered at most once, and
/// survives a rebuild — while remaining something startup never renders.
#[test]
fn the_recorded_codex_note_reaches_the_app_lazily_and_survives_a_rebuild() {
    let directory = tempfile::tempdir().unwrap();
    let effective = ModelId("gpt-4o-mini".into());
    let mut boot = bootstrap(config(directory.path(), Some("gpt-4o-mini"))).unwrap();

    // Build the exact note a catalog would record for this effective model.
    let mut notes = CodexContextNotes::default();
    notes.record(
        effective.clone(),
        crate::codex_context::codex_context_session_note(
            "gpt-5.6-luna",
            &crate::codex_context::resolve_codex_context_window(
                "gpt-5.6-luna",
                crate::codex_context::CodexContextTier::Default,
                372_000,
                372_000,
                Some(128_000),
                crate::codex_context::CodexContextOverride::NONE,
            )
            .unwrap(),
        )
        .expect("372K is above the standard tier"),
    );
    boot.codex_context_notes = notes;

    let launch = LaunchSelection {
        model: effective.clone(),
        session: SessionSelection::CreateNew(directory.path().join("note.jsonl")),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
    };
    let app = build_app(boot, launch, "system".into()).unwrap();

    // On-demand surface: readable without delivering.
    let report = app
        .codex_context_report()
        .expect("the note is available")
        .to_owned();
    assert!(report.starts_with("note: Codex model"), "{report}");
    assert!(!report.contains("Session::"), "{report}");
    assert_eq!(app.codex_context_report(), Some(report.as_str()));

    // First assistant turn: delivered exactly once, then silent.
    assert_eq!(
        app.take_codex_context_note().as_deref(),
        Some(report.as_str())
    );
    assert_eq!(app.take_codex_context_note(), None);
    assert_eq!(app.take_codex_context_note(), None);
    // The peek survives delivery for diagnostics: the once-per-session latch
    // only governs delivery, never the on-demand report.
    assert_eq!(app.codex_context_report(), Some(report.as_str()));

    // The latch survives a rebuild: a delivered note stays delivered and no
    // second delivery can leak into the new App.
    let app = rebuild_app(app, None, None, None, None).unwrap();
    assert_eq!(app.take_codex_context_note(), None);
    assert_eq!(app.codex_context_report(), Some(report.as_str()));

    // /new opens a different transcript, so its first assistant turn owes its
    // own note rather than inheriting the old session's delivered latch.
    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::CreateNew(
            directory.path().join("new-note.jsonl"),
        )),
    )
    .unwrap();
    assert_eq!(
        app.take_codex_context_note().as_deref(),
        Some(report.as_str())
    );
    assert_eq!(app.take_codex_context_note(), None);

    // Reopening that same transcript is not a new note-delivery boundary.
    let current = app.agent.session().path().to_owned();
    let app = rebuild_app(
        app,
        None,
        None,
        None,
        Some(SessionSelection::OpenExisting(current)),
    )
    .unwrap();
    assert_eq!(app.take_codex_context_note(), None);
}
