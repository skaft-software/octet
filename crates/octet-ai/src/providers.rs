//! Provider declarations and exact route composition. Construction is local only.
#![allow(missing_docs)]
pub mod codex;
pub mod codex_context;
pub mod compatibility;
pub mod contract;
pub mod headers;
pub mod inventory_metadata;
pub mod models;
pub mod pricing;
pub mod reasoning;

/// Credential-free definitions; does not initialize providers or access storage.
pub fn builtin_provider_definitions() -> Vec<contract::ProviderDefinition> {
    contract::ALL_PROVIDER_DECLARATIONS
        .iter()
        .map(contract::ProviderDeclaration::definition)
        .collect()
}
