#![allow(missing_docs)]

//! Subscription OAuth logins: the shared machinery behind every
//! `--login <provider>` that signs in with a paid plan rather than an API key.
//!
//! Each provider file under [`super`] contributes a small descriptor — its
//! endpoints, client id, scopes, and how its refresh token behaves — and this
//! module supplies everything that must be identical across all of them:
//! bounded, redirect-free transport; the RFC 8628 polling cadence; PKCE for
//! browser logins; owner-private credential storage; and a resolver that
//! rotates refresh tokens under both an in-process mutex and a cross-process
//! advisory lock.
//!
//! Nothing here is provider-specific, so a new provider cannot accidentally
//! skip a rule that an existing one relies on. It is also deliberately
//! independent of `octet-ai`: request credentials still resolve through the
//! existing `Auth::Dynamic` seam, with no new protocol, scheme, or header type.

pub(crate) mod device;
pub(crate) mod device_grant;
pub(crate) mod flow;
pub(crate) mod login;
pub(crate) mod pkce;
pub(crate) mod registry;
pub(crate) mod resolver;
pub(crate) mod store;
pub(crate) mod wire;

use std::sync::Arc;

use flow::SubscriptionFlow;
use store::OAuthStore;

/// The store a provider's credential lives in.
///
/// The path is derived from the login selector so a provider cannot be pointed
/// at another provider's credential file, and so `--login` and `--logout`
/// always address the same file.
pub(crate) fn store_for(flow: &Arc<dyn SubscriptionFlow>) -> OAuthStore {
    OAuthStore::new(store::default_path(flow.login()), flow.label())
}
