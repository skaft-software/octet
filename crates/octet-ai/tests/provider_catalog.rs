//! Provider-only catalog construction and inspection are local and deterministic.
use octet_ai::providers::codex::CodexCatalog;

#[test]
fn fallback_is_not_confirmed_availability() {
    let catalog = CodexCatalog::fallback();
    assert!(!catalog.is_account_inventory());
    assert!(!catalog.models().is_empty());
}

#[test]
fn authoritative_inventory_omits_unavailable_models_and_narrows_capabilities() {
    let catalog = CodexCatalog::from_inventory(&serde_json::json!({"models": [{
        "slug": "gpt-6-luna", "input_modalities": ["text"],
        "supported_reasoning_levels": ["low", "high"], "default_reasoning_level": "low",
        "context_window": 64000, "max_output_tokens": 4000
    }]}))
    .unwrap();
    assert!(catalog.is_account_inventory());
    assert_eq!(catalog.models().len(), 1);
    assert_eq!(catalog.models()[0].id, "gpt-6-luna");
    assert_eq!(catalog.models()[0].context_window, 64000);
    assert!(!catalog.models()[0]
        .input_modalities
        .contains(octet_ai::Modality::Image));
    assert!(catalog.models().iter().all(|m| m.id != "gpt-6.1-sol"));
}
