//! Test suite for the extension host (`crate::extensions`).
//!
//! These were a single 4,039-line inline `mod tests` block. They are a
//! directory now because the block covered eight unrelated host boundaries —
//! trust, terminals, shortcuts, context contribution, presentation, lifecycle
//! delivery, MCP, and the shared fixtures they all need — and a single file that
//! size cannot be reviewed. Each child module owns one boundary; `support.rs`
//! owns the fixtures more than one of them needs.
//!
//! `hook_tests`, `bus_tests`, `lifecycle_tests` and `ui_transport_tests` stay
//! siblings of this directory: they are separate suites for the extension bus,
//! the prompt/response hooks, lifecycle observation, and UI transport, and they
//! are reached by `#[cfg(test)] #[path = ...]` declarations in `extensions.rs`.

use super::*;

// `bus_tests`, `hook_tests`, `lifecycle_tests` and `ui_transport_tests` build a
// full `Config` through this helper at their existing `super::tests::` path, so
// the item stays visible to the whole `extensions` module and is re-exported
// here rather than at each of its four call sites.
#[cfg(unix)]
pub(in crate::extensions) use support::executable_extension_config;

mod context_composition_and_diagnostics;
mod context_contributions;
mod dynamic_shortcuts;
mod event_drain_and_reload;
mod host_request_ownership;
mod notification_output;
mod presentation_state;
mod shell_chrome_snapshots;
mod status_and_mcp_policy;
mod telemetry_and_host_state;
mod terminal_grants;
mod trust_and_preflight;

// Every remaining group drives a real extension process, so none of them exists
// off unix. Gating the declarations keeps an empty module — and its imports —
// out of the Windows build instead of leaving dead declarations behind.
#[cfg(unix)]
mod confirmation_handlers;
#[cfg(unix)]
mod deferred_startup;
#[cfg(unix)]
mod executable_process_startup;
#[cfg(unix)]
mod late_provider_registration;
#[cfg(unix)]
mod mcp_bridge_wire;
#[cfg(unix)]
mod provider_credentials;
#[cfg(unix)]
mod support;
#[cfg(unix)]
mod turn_lifecycle_delivery;
