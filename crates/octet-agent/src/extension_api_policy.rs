//! Hand-maintained executable-extension version eligibility.
//!
//! Preparatory ownership split only: the live versions remain eligible until
//! their runtime/install cutover. Historical receipt management is a separate
//! concern and must be decoupled before narrowing bundle eligibility.

/// Whether a manifest's exact API version is currently eligible to run.
pub fn runtime_supports_api_version(version: &str) -> bool {
    matches!(version, "0.1" | "0.2" | "0.3" | "0.4")
}

/// Whether an exact API version is currently eligible for bundle installation.
pub fn bundle_supports_api_version(version: &str) -> bool {
    matches!(version, "0.2" | "0.3" | "0.4")
}
