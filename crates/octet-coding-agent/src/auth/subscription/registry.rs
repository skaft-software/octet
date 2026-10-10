#![allow(missing_docs)]

//! One place that knows every subscription login octet supports.
//!
//! `--login`, `--logout`, first-run onboarding, and the `/login` slash command
//! all resolve their provider through this table, so a new provider is added in
//! exactly one place and cannot be reachable from one surface but missing from
//! another.

use std::sync::Arc;

use super::flow::SubscriptionFlow;
use crate::auth::{kimi::KimiFlow, meta::MetaFlow, openrouter::OpenRouterFlow, xai::XaiFlow};

/// One entry in the subscription-login table.
struct Entry {
    /// Selectors accepted on the command line, lowercased.
    aliases: &'static [&'static str],
    flow: fn() -> Arc<dyn SubscriptionFlow>,
}

/// Every subscription login, in the order onboarding presents them.
const ENTRIES: &[Entry] = &[
    Entry {
        aliases: &["grok", "xai-subscription", "supergrok"],
        flow: || Arc::new(XaiFlow),
    },
    Entry {
        aliases: &["kimi", "kimi-coding-subscription", "kimi-code"],
        flow: || Arc::new(KimiFlow),
    },
    Entry {
        aliases: &["meta", "meta-subscription", "muse"],
        flow: || Arc::new(MetaFlow),
    },
    Entry {
        aliases: &["openrouter", "openrouter-oauth"],
        flow: || Arc::new(OpenRouterFlow),
    },
];

/// Every supported subscription login, for menus and diagnostics.
pub(crate) fn all() -> Vec<Arc<dyn SubscriptionFlow>> {
    ENTRIES.iter().map(|entry| (entry.flow)()).collect()
}

/// Resolve a `--login` / `--logout` selector to its flow.
pub(crate) fn resolve(selector: &str) -> Option<Arc<dyn SubscriptionFlow>> {
    let requested = selector.trim().to_ascii_lowercase();
    ENTRIES
        .iter()
        .find(|entry| entry.aliases.iter().any(|alias| *alias == requested))
        .map(|entry| (entry.flow)())
}

/// Every accepted selector, used by the tests that assert the table is total.
#[cfg(test)]
pub(crate) fn supported_selectors() -> Vec<&'static str> {
    let mut selectors: Vec<&'static str> = ENTRIES
        .iter()
        .flat_map(|entry| entry.aliases.iter().copied())
        .collect();
    selectors.sort_unstable();
    selectors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_advertised_selector_resolves_to_its_provider() {
        for selector in supported_selectors() {
            let flow = resolve(selector).unwrap_or_else(|| panic!("{selector} must resolve"));
            assert!(
                flow.login() == selector
                    || ENTRIES
                        .iter()
                        .any(|entry| entry.aliases.contains(&selector)),
                "{selector} resolved to {}",
                flow.login()
            );
        }
    }

    #[test]
    fn selectors_are_matched_case_insensitively_and_ignore_surrounding_space() {
        assert_eq!(resolve("  GROK ").unwrap().login(), "grok");
        assert_eq!(resolve("Kimi-Code").unwrap().login(), "kimi");
        assert!(resolve("codex").is_none(), "codex has its own module");
        assert!(resolve("copilot").is_none(), "copilot has its own module");
        assert!(resolve("").is_none());
    }

    #[test]
    fn every_flow_declares_a_provider_declaration_and_a_distinct_credential_file() {
        let flows = all();
        assert_eq!(flows.len(), ENTRIES.len());
        let mut providers: Vec<&str> = Vec::new();
        let mut logins: Vec<&str> = Vec::new();
        let mut endpoints: Vec<&str> = Vec::new();
        for flow in &flows {
            providers.push(flow.provider_id());
            logins.push(flow.login());
            endpoints.push(flow.endpoint_id());
            assert!(!flow.label().is_empty(), "{} needs a label", flow.login());
            assert!(flow.refresh_skew_secs() < 86_400, "{}", flow.login());
            assert!(flow.fallback_token_lifetime_secs() > 0, "{}", flow.login());
        }
        assert_eq!(
            providers
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            providers.len()
        );
        assert_eq!(
            logins
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            logins.len()
        );
        assert_eq!(
            endpoints
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            endpoints.len()
        );
    }

    #[test]
    fn the_default_credential_path_is_derived_from_the_login_selector() {
        for flow in all() {
            let path = super::super::store::default_path(flow.login());
            assert!(
                path.ends_with(format!("{}.json", flow.login())),
                "{} -> {path:?}",
                flow.login()
            );
        }
    }
}
