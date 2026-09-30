#![allow(missing_docs)]

//! Catalog registration for subscription logins.
//!
//! The rule this module exists to enforce is narrow: a subscription provider
//! enters octet's catalog *only* when its own credential file says the user
//! signed in, and its model inventory is read with that same credential. Nothing
//! here can synthesize the authority, so an unsigned-in provider is simply
//! absent — never an empty endpoint, and never an unauthenticated request.
//!
//! It lives apart from `bootstrap.rs` so that rule is stated once rather than
//! spread across a match arm and a call site.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use octet_ai::{Auth, ModelCatalog};

use crate::auth::subscription::flow::SubscriptionFlow;
use crate::auth::subscription::registry;
use crate::auth::subscription::resolver::SubscriptionResolver;
use crate::providers::{
    register_dynamic_endpoints_at_base_url, register_static_models, ModelDiscovery,
    ProviderAuthentication, ProviderDeclaration, SubscriptionInventoryShape,
};

/// How long registration may spend on one provider's inventory.
///
/// The same envelope Codex discovery uses. A subscription inventory is a single
/// authenticated `GET /models`, so a provider that cannot answer inside this
/// budget is treated as unavailable rather than delaying the launch.
pub(crate) const INVENTORY_ENVELOPE: Duration = Duration::from_secs(10);

/// The subscription declarations octet knows how to register.
///
/// Every entry must have a flow in [`registry::resolve`], or a signed-in user
/// would be told to run a `--login` selector that does nothing.
pub(crate) const DECLARATIONS: &[&ProviderDeclaration] = &[
    &crate::providers::XAI_SUBSCRIPTION,
    &crate::providers::KIMI_CODING_SUBSCRIPTION,
    &crate::providers::META_SUBSCRIPTION,
    &crate::providers::OPENROUTER_OAUTH,
];

/// Register every subscription provider the user has signed in to.
pub(crate) fn register_all(
    catalog: &mut ModelCatalog,
    offline: bool,
    include: impl Fn(&str) -> bool,
) {
    for declaration in DECLARATIONS {
        // A narrowed readiness plan proves exactly one route; the rest stay on
        // the fleet path and are filled in later by `enrich_catalog`.
        if !include(declaration.id) {
            continue;
        }
        register_one(catalog, declaration, offline);
    }
}

/// Register one subscription provider, if the user has signed in to it.
pub(crate) fn register_one(
    catalog: &mut ModelCatalog,
    declaration: &'static ProviderDeclaration,
    offline: bool,
) {
    // Unit tests inject explicit temporary stores and must never read the
    // developer's ambient HOME.
    if cfg!(test) {
        return;
    }
    // Non-fatal by design: a stale or malformed credential must never block
    // octet startup, and a provider that is merely absent is not an error.
    let _ = crate::app::bootstrap::bootstrap_check(
        format!("{}-registration", declaration.id),
        register_signed_in(catalog, declaration, offline),
        |error| format!("warning: {} models unavailable: {error}", declaration.name),
    );
}

/// The credential a provider's login selector names, if it declares one.
pub(crate) fn login_selector(declaration: &ProviderDeclaration) -> Option<&'static str> {
    match declaration.authentication {
        ProviderAuthentication::Subscription { login } => Some(login),
        _ => None,
    }
}

/// The flow backing a subscription declaration.
pub(crate) fn flow_for(declaration: &ProviderDeclaration) -> Result<Arc<dyn SubscriptionFlow>> {
    let selector = login_selector(declaration)
        .with_context(|| format!("{} does not declare a subscription login", declaration.id))?;
    registry::resolve(selector).with_context(|| {
        format!(
            "{} declares login {selector:?}, which has no registered flow",
            declaration.id
        )
    })
}

fn register_signed_in(
    catalog: &mut ModelCatalog,
    declaration: &'static ProviderDeclaration,
    offline: bool,
) -> Result<()> {
    declaration.validate().map_err(|error| {
        anyhow::anyhow!("invalid {} provider declaration: {error}", declaration.id)
    })?;
    let flow = flow_for(declaration)?;
    // A flow that names a different endpoint than the declaration registers
    // would still authenticate correctly but send requests to the wrong
    // catalog entry, which is the kind of mismatch that only shows up as a
    // mysterious 404 later.
    let route_endpoint = declaration
        .routes
        .first()
        .map(|route| route.endpoint_id)
        .unwrap_or_default();
    if flow.endpoint_id() != route_endpoint {
        anyhow::bail!(
            "{} registers endpoint {route_endpoint:?} but its flow claims {:?}",
            declaration.id,
            flow.endpoint_id()
        );
    }
    let store = crate::auth::subscription::store_for(&flow);
    // A store that cannot even be read is treated as "not signed in" rather than
    // a failure, so the endpoint is then absent — exactly as before signing in.
    let signed_in = store.load().is_ok_and(|stored| stored.is_some());
    if !signed_in {
        return Ok(());
    }

    let base_url = declaration.resolved_base_url()?;
    let resolver = Arc::new(SubscriptionResolver::new(Arc::clone(&flow), store));
    register_dynamic_endpoints_at_base_url(
        catalog,
        declaration,
        Auth::dynamic(resolver.clone()),
        &base_url,
        crate::app::bootstrap::PROVIDER_RESPONSE_HEADER_TIMEOUT,
    )?;
    register_static_models(catalog, declaration)?;

    if let ModelDiscovery::SubscriptionInventory { shape } = declaration.model_discovery {
        register_inventory(catalog, declaration, shape, resolver, offline)?;
        warn_if_no_models(catalog, declaration);
    }
    Ok(())
}

/// Warn when a signed-in provider offers no models at all.
///
/// These providers are discovery-only: octet deliberately does not ship a
/// static model list for them, because the account inventory is the only
/// authoritative source of what a plan can actually call. That makes an empty
/// result ambiguous — a failed refresh looks identical to never having signed in,
/// and a user has no way to tell which happened. Say so instead.
fn warn_if_no_models(catalog: &ModelCatalog, declaration: &ProviderDeclaration) {
    let offered = catalog.models().any(|model| {
        model
            .id
            .0
            .split_once('/')
            .is_some_and(|(namespace, _)| namespace == declaration.id)
    });
    if offered {
        return;
    }
    crate::output::checked_diagnostics(
        crate::output::DiagnosticComponent::Bootstrap(format!("{}-inventory", declaration.id)),
        vec![format!(
            "warning: {} is signed in but its inventory offered no models; \
             restart online to refresh it",
            declaration.name
        )],
        true,
    );
}

/// Register a subscription provider's discovered inventory.
///
/// Offline launches keep whatever static models the declaration checked in and
/// skip the network entirely, so a signed-in subscription provider degrades to a
/// smaller model list rather than to a slow launch.
fn register_inventory(
    catalog: &mut ModelCatalog,
    declaration: &'static ProviderDeclaration,
    shape: SubscriptionInventoryShape,
    resolver: Arc<SubscriptionResolver>,
    offline: bool,
) -> Result<()> {
    if offline {
        return Ok(());
    }
    let Some(body) = fetch_inventory(declaration, shape, resolver, INVENTORY_ENVELOPE)? else {
        return Ok(());
    };
    match shape {
        SubscriptionInventoryShape::OpenAi { filter } => {
            crate::app::bootstrap::register_openai_compatible_models_from_response(
                catalog,
                declaration,
                filter,
                &body,
            )
        }
        SubscriptionInventoryShape::Anthropic { filter } => {
            crate::app::bootstrap::register_anthropic_compatible_models_from_response(
                catalog,
                declaration,
                filter,
                &body,
            )
        }
        SubscriptionInventoryShape::OpenRouter => {
            crate::app::bootstrap::register_openrouter_models_from_response(
                catalog,
                declaration,
                &body,
            )
        }
    }
}

/// Fetch one provider's inventory with the credential inference will use.
///
/// The credential step is async and the fetch is blocking, so they run in
/// separate runtimes: `reqwest::blocking` creates and drops an internal runtime
/// of its own, which must not happen while another runtime is entered on this
/// thread.
fn fetch_inventory(
    declaration: &'static ProviderDeclaration,
    shape: SubscriptionInventoryShape,
    resolver: Arc<SubscriptionResolver>,
    envelope: Duration,
) -> Result<Option<serde_json::Value>> {
    let cancel = Arc::new(AtomicBool::new(false));
    crate::app::bootstrap::run_route_readiness(
        "subscription-inventory",
        envelope,
        Arc::clone(&cancel),
        move |cancel| {
            let (mut headers, fingerprint) = resolve_discovery_headers(&resolver, &cancel)?;
            crate::app::bootstrap::add_declared_headers(&mut headers, declaration)?;
            crate::app::bootstrap::fetch_cached_subscription_inventory(
                declaration,
                shape,
                headers,
                &fingerprint,
            )
        },
    )
}

/// Resolve the credential and its request headers, then release the runtime.
fn resolve_discovery_headers(
    resolver: &SubscriptionResolver,
    cancel: &AtomicBool,
) -> Result<(http::HeaderMap, String)> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| anyhow::anyhow!("subscription inventory runtime is unavailable"))?;
    let resolved = runtime.block_on(resolver.discovery_headers());
    drop(runtime);
    let headers = resolved?;
    crate::app::bootstrap::startup_phase("subscription.credentials");
    if cancel.load(Ordering::SeqCst) {
        anyhow::bail!("subscription inventory discovery was cancelled before its request");
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::EndpointAuthPresentation;

    #[test]
    fn every_registration_target_declares_a_resolvable_login() {
        for declaration in DECLARATIONS {
            let selector = login_selector(declaration)
                .unwrap_or_else(|| panic!("{} must declare a subscription login", declaration.id));
            let flow = flow_for(declaration)
                .unwrap_or_else(|error| panic!("{}: {error:#}", declaration.id));
            assert_eq!(flow.login(), selector, "{}", declaration.id);
            assert_eq!(flow.provider_id(), declaration.id);
            assert_eq!(flow.endpoint_id(), declaration.id);
            // A dynamic route is the only presentation that can carry a
            // privately resolved credential.
            assert!(
                declaration
                    .routes
                    .iter()
                    .all(|route| route.auth_presentation == EndpointAuthPresentation::Dynamic),
                "{} must present every route dynamically",
                declaration.id
            );
        }
    }

    #[test]
    fn registration_targets_are_distinct_and_named_by_their_login_selector() {
        let ids: std::collections::HashSet<&str> = DECLARATIONS
            .iter()
            .map(|declaration| declaration.id)
            .collect();
        assert_eq!(ids.len(), DECLARATIONS.len());
        let logins: std::collections::HashSet<&str> = DECLARATIONS
            .iter()
            .filter_map(|declaration| login_selector(declaration))
            .collect();
        assert_eq!(logins.len(), DECLARATIONS.len());
    }

    #[test]
    fn an_environment_declaration_is_never_treated_as_a_subscription() {
        assert!(login_selector(&crate::providers::OPENAI).is_none());
        assert!(flow_for(&crate::providers::OPENAI).is_err());
    }
}
