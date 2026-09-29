//! Bootstrap test suite for `app::bootstrap`, split by subsystem.
//!
//! The suite was a single 7,706-line `tests.rs`; each group below owns one
//! cohesive area of bootstrap so a change to one area does not churn the
//! whole file, and so a failure points at a named module.

#[cfg(test)]
mod support;

#[cfg(test)]
mod codex_registration;
#[cfg(test)]
mod codex_tier_and_readiness;
#[cfg(test)]
mod compaction_and_model_selection;
#[cfg(test)]
mod custom_model_inventory;
#[cfg(test)]
mod custom_registry_and_cache;
#[cfg(test)]
mod deferred_enrichment;
#[cfg(test)]
mod discovery_and_capability;
#[cfg(test)]
mod inventory_cache;
#[cfg(test)]
mod launch_and_resume;
#[cfg(test)]
mod openrouter_discovery;
#[cfg(test)]
mod openrouter_offline_catalog;
#[cfg(test)]
mod pinned_metadata;
#[cfg(test)]
mod reasoning_ingress_and_loopback;
#[cfg(test)]
mod rebuild_and_fork;
#[cfg(test)]
mod tool_registry;

// `codex_discovered_model` is a shared codex fixture that the sibling
// `codex_context_note_regression_tests` module in `app::bootstrap` also
// builds codex expectations with, so it stays reachable at the old path.
#[cfg(test)]
pub(super) use codex_tier_and_readiness::codex_discovered_model;

use super::*;
use crate::codex_context::{
    CODEX_5_6_CONTEXT_WINDOW, CODEX_CONTEXT_WINDOW_CAP, CODEX_LEGACY_CONTEXT_WINDOW,
};
