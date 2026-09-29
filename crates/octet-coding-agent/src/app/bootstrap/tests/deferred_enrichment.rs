//! Deferred enrichment
//!
//! Part of the `app::bootstrap` test suite; see `tests/mod.rs` for how the
//! suite is organised and why it is split this way.
use super::support::*;
use super::*;

/// Deferring provider inventories must stay an enrichment concern, not an
/// extension-lifecycle change: a narrowed launch starts no extension host, and
/// enriching twice rebuilds the catalog at most once with no duplicate provider
/// registration and no second activation. Extension startup itself is unchanged
/// (`activate_eager` already fans out with `join_all`); this pins that readiness
/// and enrichment never enter that path.
///
/// The configuration-level narrowing is asserted on `catalog_readiness` itself:
/// a unit-test build never reads an ambient Codex credential
/// (`register_codex_catalog` skips the ambient HOME in tests), so the narrowed
/// catalog cannot resolve `codex/gpt-6-astra` and `bootstrap` completes the
/// launch with the fleet catalog through its documented repair rule. The wait
/// that narrowing actually removes is proven by the consultation counters in
/// `narrowed_readiness_never_consults_an_unrelated_provider`.

#[test]
fn deferred_enrichment_does_not_start_extensions_or_duplicate_providers() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = config(directory.path(), Some("codex/gpt-6-astra"));
    config.extension_paths = vec![directory.path().join("extensions")];
    config.enabled_extensions = vec!["fixture-extension".into()];

    assert_eq!(
        catalog_readiness(&config).route_ids(),
        vec!["codex"],
        "the launch decision itself is the narrowed Codex route"
    );

    let mut boot = bootstrap(config).unwrap();
    assert!(
        boot.readiness_plan().is_fleet(),
        "an unresolvable selection is completed by the fleet repair, never failed"
    );
    assert!(
        boot.prestarted_extensions.borrow().is_none(),
        "readiness must not start an extension host"
    );

    boot.enrich_catalog().unwrap();
    assert!(boot.readiness_plan().is_fleet());
    assert!(
        boot.prestarted_extensions.borrow().is_none(),
        "enrichment must not start an extension host either"
    );
    let enriched_models = boot.catalog.models().count();
    assert!(enriched_models > 0);

    // Idempotent: a second enrichment is a no-op, so a surface may call it
    // freely without duplicating a provider registration or re-running startup.
    boot.enrich_catalog().unwrap();
    assert_eq!(boot.catalog.models().count(), enriched_models);
}

/// The consumer seam for a narrowed launch: a surface that enumerates every
/// route calls `App::enrich_catalog` first. The call must move the plan to the
/// fleet, keep the active model resolvable, and be a true no-op on the second
/// call so a picker can call it freely.
#[test]
fn the_app_enrichment_seam_is_idempotent_and_keeps_the_active_model() {
    let directory = tempfile::tempdir().unwrap();
    let effective = ModelId("gpt-4o-mini".into());
    let boot = bootstrap(config(directory.path(), Some("gpt-4o-mini"))).unwrap();
    let launch = LaunchSelection {
        model: effective.clone(),
        session: SessionSelection::CreateNew(directory.path().join("enrich.jsonl")),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
    };
    let mut app = build_app(boot, launch, "system".into()).unwrap();

    // Represent the state a deferred plan leaves in the App: the fleet catalog
    // is already complete here, so the observable contract is that the
    // completing call changes nothing, disturbs nothing, and never repeats.
    app.readiness = CatalogReadiness::Routes(vec!["codex"]);
    let before = app.catalog.models().count();
    app.enrich_catalog().unwrap();
    assert!(app.readiness.is_fleet(), "enrichment completes the plan");
    assert!(
        app.catalog.resolve(&effective).is_ok(),
        "the active model stays resolvable after enrichment"
    );
    let enriched = app.catalog.models().count();
    assert_eq!(enriched, before, "no model is dropped or duplicated");

    app.enrich_catalog().unwrap();
    assert_eq!(
        app.catalog.models().count(),
        enriched,
        "a second enrichment is a no-op"
    );
    assert!(app.catalog.resolve(&effective).is_ok());
}

#[test]
fn delegation_completes_deferred_catalog_only_for_a_live_service() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path(), Some("codex/gpt-6-astra"));
    let mut catalog = ModelCatalog::default();
    let mut readiness = CatalogReadiness::Routes(vec!["codex"]);
    let mut notes = CodexContextNotes::default();
    complete_delegation_catalog(false, &config, &mut catalog, &mut readiness, &mut notes).unwrap();
    assert!(!readiness.is_fleet());
    assert_eq!(catalog.models().count(), 0);
    complete_delegation_catalog(true, &config, &mut catalog, &mut readiness, &mut notes).unwrap();
    assert!(readiness.is_fleet());
    assert!(catalog
        .resolve(&ModelId("claude-sonnet-4-6".into()))
        .is_ok());
    assert!(catalog.resolve(&ModelId("gpt-4o-mini".into())).is_ok());
    let count = catalog.models().count();
    complete_delegation_catalog(true, &config, &mut catalog, &mut readiness, &mut notes).unwrap();
    assert_eq!(catalog.models().count(), count);
}
