#![allow(missing_docs)]

//! Request-credential resolution for a subscription provider.
//!
//! Refresh-token rotation is the dangerous part: the old token usually stops
//! working the moment a new one is issued. Two octet processes that both decide
//! to refresh can therefore spend the same token and leave the user unable to
//! sign in again. The resolver therefore takes an in-process mutex *and* the
//! store's cross-process advisory lock, and re-reads the credential after
//! acquiring the lock so a peer that already refreshed is believed rather than
//! raced.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use octet_ai::{AuthError, CredentialResolver, CredentialScheme, ResolvedCredential, Secret};
use tokio::sync::Mutex;

use super::flow::{RefreshMode, SubscriptionFlow};
use super::store::{OAuthStore, RefreshLock, StoredCredential};

/// A credential resolver bound to one provider's flow and credential file.
pub(crate) struct SubscriptionResolver {
    flow: Arc<dyn SubscriptionFlow>,
    store: OAuthStore,
    http: reqwest::Client,
    /// Serializes refreshes inside this process so concurrent requests do not
    /// stampede the token endpoint. Combined with the cross-process lock below
    /// this is a proper double-checked lock.
    refresh_lock: Mutex<()>,
    /// Bound on the cross-process wait, kept as a field so tests can inject a
    /// short deadline instead of depending on the production one.
    refresh_lock_wait: Duration,
}

impl std::fmt::Debug for SubscriptionResolver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SubscriptionResolver")
            .field("provider", &self.flow.label())
            .field("store", &self.store)
            .finish()
    }
}

impl SubscriptionResolver {
    pub(crate) fn new(flow: Arc<dyn SubscriptionFlow>, store: OAuthStore) -> Self {
        Self {
            flow,
            store,
            http: super::wire::http_client(),
            refresh_lock: Mutex::new(()),
            refresh_lock_wait: super::store::REFRESH_LOCK_WAIT,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_refresh_lock_wait(
        flow: Arc<dyn SubscriptionFlow>,
        store: OAuthStore,
        refresh_lock_wait: Duration,
    ) -> Self {
        Self {
            refresh_lock_wait,
            ..Self::new(flow, store)
        }
    }

    /// The action a user should take when this provider has no credential.
    pub(crate) fn login_command(&self) -> String {
        format!("octet --login {}", self.flow.login())
    }

    /// Load a non-expired credential, refreshing it if necessary.
    pub(crate) async fn load_valid(&self) -> Result<StoredCredential> {
        let stored = self.store.load()?.ok_or_else(|| {
            anyhow!(
                "not signed in to {}; run `{}`",
                self.flow.label(),
                self.login_command()
            )
        })?;
        if stored.is_fresh(self.flow.refresh_skew_secs()) {
            return Ok(stored);
        }

        // Near or after expiry: serialize tasks in this resolver, then serialize
        // rotation across octet processes using the same store. The file is
        // re-read after both waits because another owner may have persisted a
        // fresh token in the meantime.
        //
        // The cross-process wait is bounded *inside* the blocking worker rather
        // than by dropping this future: the worker always returns within the
        // deadline, so a contended lock can neither wedge startup nor leave an
        // indefinitely blocked lock worker behind. A timed-out acquisition holds
        // nothing and rotates nothing, so the credential file is left exactly as
        // its other owner had it.
        let _task_guard = self.refresh_lock.lock().await;
        let lock_store = self.store.clone();
        let refresh_lock_wait = self.refresh_lock_wait;
        let process_guard =
            tokio::task::spawn_blocking(move || lock_store.lock_refresh_within(refresh_lock_wait))
                .await
                .context("refresh-lock worker failed")??;
        let refreshed = self.refresh_while_locked(&process_guard).await;
        process_guard.finish_with(refreshed)
    }

    async fn refresh_while_locked(&self, lock: &RefreshLock) -> Result<StoredCredential> {
        let stored = self.store.load_while_refresh_locked(lock)?.ok_or_else(|| {
            anyhow!(
                "{} credential was removed during refresh",
                self.flow.label()
            )
        })?;
        if stored.is_fresh(self.flow.refresh_skew_secs()) {
            return Ok(stored);
        }
        // Every mode except `Never` spends the stored refresh token to get a new
        // access token. An absent one cannot be renewed, and reporting that
        // here is far more useful than sending an empty `refresh_token` and
        // surfacing the provider's opaque rejection of it.
        if self.flow.refresh_mode() != RefreshMode::Never && !stored.has_refresh_token() {
            bail!(
                "{} credential cannot be renewed and has expired; run `{}`",
                self.flow.label(),
                self.login_command()
            );
        }
        let refreshed = self.flow.refresh(&self.http, &stored).await?;
        if refreshed.access_token == stored.access_token {
            // An endpoint that answers a refresh with the credential it was
            // given has not renewed anything. Persisting the "refreshed" record
            // would reset the expiry on a token the provider has already
            // revoked, turning one failed request into a refresh on every
            // subsequent request, forever.
            bail!(
                "{} token endpoint did not renew the credential; run `{}`",
                self.flow.label(),
                self.login_command()
            );
        }
        self.store.save_while_refresh_locked(&refreshed, lock)?;
        Ok(refreshed)
    }

    /// Resolve the credential, plus every header inference needs beyond the
    /// bearer token.
    pub(crate) async fn resolved(&self) -> Result<ResolvedCredential> {
        let stored = self.load_valid().await?;
        Ok(ResolvedCredential {
            scheme: CredentialScheme::Bearer,
            value: Secret::from(self.flow.bearer(&stored)?),
            extra_headers: self.provider_headers(&stored)?,
        })
    }

    /// Headers for an authenticated inventory request, plus the fingerprint that
    /// scopes a cached inventory to this credential.
    ///
    /// The token is materialized inside this module and handed straight to the
    /// HTTP layer. Only its hash crosses the boundary, so a cache entry can be
    /// invalidated when the account changes without the raw token ever leaving.
    pub(crate) async fn discovery_headers(&self) -> Result<(http::HeaderMap, String)> {
        let stored = self.load_valid().await?;
        let bearer = self.flow.bearer(&stored)?;
        let mut headers = self.provider_headers(&stored)?;
        // Inserted last so a provider header can never displace the credential.
        let mut authorization = http::HeaderValue::from_str(&format!("Bearer {bearer}"))?;
        authorization.set_sensitive(true);
        headers.insert(http::header::AUTHORIZATION, authorization);
        Ok((headers, fingerprint(&bearer)))
    }

    /// The non-credential headers a provider requires on its own routes.
    fn provider_headers(&self, stored: &StoredCredential) -> Result<http::HeaderMap> {
        let mut extra_headers = http::HeaderMap::new();
        for (name, value) in self.flow.request_headers(stored) {
            // A provider-supplied header is still untrusted input; refuse one
            // that cannot be a legal header value rather than dropping it
            // silently and sending a request the user did not expect.
            let header_name = http::HeaderName::from_static(name);
            let header_value = http::HeaderValue::from_str(&value).map_err(|_| {
                anyhow!("{} produced an unusable request header", self.flow.label())
            })?;
            extra_headers.insert(header_name, header_value);
        }
        Ok(extra_headers)
    }
}

/// Hash a credential so a cache entry can be scoped to it without storing it.
fn fingerprint(credential: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // A fixed namespace keeps this digest distinct from any other use of the
    // default hasher in the same process.
    b"octet-subscription-credential-fingerprint-v1".hash(&mut hasher);
    credential.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[async_trait]
impl CredentialResolver for SubscriptionResolver {
    async fn resolve(&self) -> std::result::Result<ResolvedCredential, AuthError> {
        // `AuthError` deliberately carries no detail: a token-response failure
        // can contain the credential itself. The actionable
        // "run `octet --login <provider>`" guidance is attached by the catalog
        // registration path, which knows nothing secret.
        self.resolved()
            .await
            .map_err(|error| classification(&error))
    }
}

/// Classify a resolution failure without leaking its message.
///
/// Only a positively identified *pre-send* network failure is reported as
/// `Unavailable`, so a caller can distinguish "octet could not reach the
/// provider" from "the credential is not usable". A token-response body failure
/// may already follow a successful rotation and must stay indeterminate.
fn classification(error: &anyhow::Error) -> AuthError {
    if let Some(request) = error.downcast_ref::<reqwest::Error>() {
        if request.is_connect() {
            let network_failure = request.is_timeout()
                || error.chain().any(|cause| {
                    cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
                        matches!(
                            io.kind(),
                            std::io::ErrorKind::ConnectionRefused
                                | std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                                | std::io::ErrorKind::NetworkDown
                                | std::io::ErrorKind::NetworkUnreachable
                                | std::io::ErrorKind::HostUnreachable
                                | std::io::ErrorKind::TimedOut
                        )
                    })
                });
            if network_failure {
                return AuthError::Unavailable;
            }
        }
    }
    AuthError::Resolve
}
#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::auth::subscription::flow::reconcile;
    use crate::auth::subscription::store::StoredCredential;
    use crate::auth::subscription::wire::{post_for_status, Encoding, Tokens};

    const ACCESS: &str = "access-PRIVATE-SENTINEL";
    const REFRESH: &str = "refresh-PRIVATE-SENTINEL";
    const ROTATED_ACCESS: &str = "rotated-access-PRIVATE-SENTINEL";
    const ROTATED_REFRESH: &str = "rotated-refresh-PRIVATE-SENTINEL";

    /// A flow whose only network behaviour is a token refresh against a
    /// wiremock server, so the resolver's own logic is what is under test.
    #[derive(Debug)]
    struct TestFlow {
        token_url: String,
        mode: RefreshMode,
        skew: u64,
        /// When set, the refreshed access token is forced to this value, which
        /// models a token endpoint that echoes the credential back.
        echo: Option<String>,
        headers: Vec<(&'static str, String)>,
    }

    impl TestFlow {
        fn new(token_url: String) -> Self {
            Self {
                token_url,
                mode: RefreshMode::Rotating,
                skew: 0,
                echo: None,
                headers: Vec::new(),
            }
        }
    }

    #[async_trait::async_trait]
    impl SubscriptionFlow for TestFlow {
        fn provider_id(&self) -> &'static str {
            "test-subscription"
        }
        fn label(&self) -> &'static str {
            "Test Provider"
        }
        fn login(&self) -> &'static str {
            "test-provider"
        }
        fn refresh_skew_secs(&self) -> u64 {
            self.skew
        }
        fn token_encoding(&self) -> Encoding {
            Encoding::Form
        }
        fn refresh_mode(&self) -> RefreshMode {
            self.mode
        }
        fn fallback_token_lifetime_secs(&self) -> u64 {
            3600
        }
        fn request_headers(&self, _credential: &StoredCredential) -> Vec<(&'static str, String)> {
            self.headers.clone()
        }
        async fn authorize(
            &self,
            _http: &reqwest::Client,
            _headless: bool,
        ) -> Result<StoredCredential> {
            bail!("the test flow never authorizes")
        }
        async fn refresh(
            &self,
            http: &reqwest::Client,
            credential: &StoredCredential,
        ) -> Result<StoredCredential> {
            let (_, body) = post_for_status(
                http,
                &self.token_url,
                Encoding::Form,
                &[("grant_type".to_owned(), "refresh_token".to_owned())],
                "token refresh",
            )
            .await?;
            let parsed = crate::auth::subscription::wire::tokens_from_response(
                &body,
                "access_token",
                "refresh_token",
                3600,
                "token refresh",
            )?;
            let tokens = match &self.echo {
                // The endpoint answers with the token it was already given.
                Some(echo) => Tokens {
                    access: echo.clone(),
                    refresh: parsed.refresh,
                    expires_at: parsed.expires_at,
                },
                None => parsed,
            };
            reconcile(self.label(), self.mode, tokens, Some(credential))
        }
    }

    struct Fixture {
        store: OAuthStore,
        flow: Arc<dyn SubscriptionFlow>,
        // Held so the temporary credential directory outlives the store.
        _guard: tempfile::TempDir,
    }

    /// An unroutable token endpoint: any test that must not refresh uses it.
    const UNROUTABLE: &str = "http://127.0.0.1:1/token";

    fn fixture(mode: RefreshMode, skew: u64) -> Fixture {
        fixture_with(UNROUTABLE, mode, skew)
    }

    fn fixture_with(token_url: &str, mode: RefreshMode, skew: u64) -> Fixture {
        let guard = tempfile::tempdir().unwrap();
        let mut flow = TestFlow::new(token_url.to_owned());
        flow.mode = mode;
        flow.skew = skew;
        Fixture {
            store: OAuthStore::new(guard.path().join("credential.json"), "Test Provider"),
            flow: Arc::new(flow),
            _guard: guard,
        }
    }

    /// A credential expiring `offset` seconds from now.
    fn credential(offset: i64) -> StoredCredential {
        StoredCredential {
            version: crate::auth::subscription::store::CREDENTIAL_VERSION,
            access_token: ACCESS.to_owned(),
            refresh_token: REFRESH.to_owned(),
            expires_at: super::super::wire::now_unix().saturating_add_signed(offset),
            scope: None,
            account_id: None,
        }
    }

    fn resolver(fixture: &Fixture) -> SubscriptionResolver {
        SubscriptionResolver::new(Arc::clone(&fixture.flow), fixture.store.clone())
    }

    /// A wiremock token endpoint that always answers with a rotated pair.
    async fn rotating_token_endpoint() -> (wiremock::MockServer, Arc<AtomicU64>) {
        let server = wiremock::MockServer::start().await;
        let calls = Arc::new(AtomicU64::new(0));
        let counter = Arc::clone(&calls);
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(move |_request: &wiremock::Request| {
                counter.fetch_add(1, Ordering::SeqCst);
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "access_token": ROTATED_ACCESS,
                    "refresh_token": ROTATED_REFRESH,
                    "expires_in": 3600
                }))
            })
            .mount(&server)
            .await;
        (server, calls)
    }

    /// Await a resolution that is expected to fail.
    ///
    /// `ResolvedCredential` is deliberately not `Debug`, so `unwrap_err` is not
    /// available and a failure is unwrapped by hand.
    async fn expect_resolution_failure(resolver: &SubscriptionResolver) -> anyhow::Error {
        match resolver.resolved().await {
            Ok(_) => panic!("the credential was expected to fail resolution"),
            Err(error) => error,
        }
    }

    /// The bearer a resolver would actually send, read the way the HTTP layer
    /// reads it.
    async fn bearer(resolver: &SubscriptionResolver) -> String {
        let (headers, _) = resolver.discovery_headers().await.unwrap();
        headers
            .get(http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .expect("a resolved credential must carry an Authorization header")
            .to_owned()
    }

    #[tokio::test]
    async fn a_missing_credential_names_the_exact_login_command() {
        let fixture = fixture(RefreshMode::Rotating, 0);
        let error = resolver(&fixture).load_valid().await.unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("not signed in to Test Provider"),
            "{message}"
        );
        assert!(message.contains("octet --login test-provider"), "{message}");
        assert!(!message.contains(ACCESS) && !message.contains(REFRESH));
    }

    #[tokio::test]
    async fn a_credential_inside_its_skew_resolves_without_the_token_endpoint() {
        let fixture = fixture(RefreshMode::Rotating, 300);
        fixture.store.save(&credential(600)).unwrap();
        // The token endpoint is unroutable, so a resolve that succeeded proves
        // the credential was considered fresh and nothing was renewed.
        assert_eq!(
            bearer(&resolver(&fixture)).await,
            format!("Bearer {ACCESS}")
        );
    }

    #[tokio::test]
    async fn a_rotation_persists_both_new_tokens_and_a_future_deadline() {
        let (server, calls) = rotating_token_endpoint().await;
        let fixture = fixture_with(&format!("{}/token", server.uri()), RefreshMode::Rotating, 0);
        fixture.store.save(&credential(-10)).unwrap();

        assert_eq!(
            bearer(&resolver(&fixture)).await,
            format!("Bearer {ROTATED_ACCESS}")
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let stored = fixture.store.load().unwrap().unwrap();
        assert_eq!(stored.access_token, ROTATED_ACCESS);
        assert_eq!(stored.refresh_token, ROTATED_REFRESH);
        assert!(stored.expires_at > super::super::wire::now_unix());
    }

    #[tokio::test]
    async fn a_rotating_provider_that_omits_the_new_refresh_token_keeps_the_old_credential() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": ROTATED_ACCESS, "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;
        let fixture = fixture_with(&format!("{}/token", server.uri()), RefreshMode::Rotating, 0);
        fixture.store.save(&credential(-10)).unwrap();

        let error = expect_resolution_failure(&resolver(&fixture)).await;
        assert!(
            format!("{error:#}").contains("rotated refresh token"),
            "{error:#}"
        );
        // The provider has already revoked the old refresh token, so persisting
        // the new access token against it would leave a credential that can
        // never be renewed. Nothing is written.
        let stored = fixture.store.load().unwrap().unwrap();
        assert_eq!(stored.access_token, ACCESS);
        assert_eq!(stored.refresh_token, REFRESH);
    }

    #[tokio::test]
    async fn a_retaining_provider_keeps_its_refresh_token_across_a_renewal() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
                serde_json::json!({ "access_token": ROTATED_ACCESS, "expires_in": 3600 }),
            ))
            .mount(&server)
            .await;
        let fixture = fixture_with(
            &format!("{}/token", server.uri()),
            RefreshMode::Retaining,
            0,
        );
        fixture.store.save(&credential(-10)).unwrap();
        resolver(&fixture).resolved().await.unwrap();
        assert_eq!(
            fixture.store.load().unwrap().unwrap().refresh_token,
            REFRESH
        );
    }

    #[tokio::test]
    async fn a_token_endpoint_that_renews_nothing_is_refused() {
        let (server, _) = rotating_token_endpoint().await;
        let guard = tempfile::tempdir().unwrap();
        let store = OAuthStore::new(guard.path().join("credential.json"), "Test Provider");
        // An already-expired credential, so the echo check is the only thing
        // that can reject the response.
        store.save(&credential(-10)).unwrap();
        let mut flow = TestFlow::new(format!("{}/token", server.uri()));
        flow.echo = Some(ACCESS.to_owned());
        let resolver =
            SubscriptionResolver::new(Arc::new(flow) as Arc<dyn SubscriptionFlow>, store.clone());

        let error = expect_resolution_failure(&resolver).await;
        assert!(
            format!("{error:#}").contains("did not renew the credential"),
            "{error:#}"
        );
        // Accepting the echo would reset the expiry on a dead token and refresh
        // again on every single request, forever.
        assert_eq!(store.load().unwrap().unwrap().access_token, ACCESS);
    }

    #[tokio::test]
    async fn two_resolvers_over_one_store_spend_a_rotated_token_only_once() {
        let (server, calls) = rotating_token_endpoint().await;
        let fixture = fixture_with(&format!("{}/token", server.uri()), RefreshMode::Rotating, 0);
        fixture.store.save(&credential(-10)).unwrap();

        // Two independent resolvers, as two octet processes inside one test
        // binary would be. Both observe an expired credential at once; the
        // cross-process lock plus the post-lock re-read must mean only one of
        // them spends the refresh token.
        let first = resolver(&fixture);
        let second = resolver(&fixture);
        let (left, right) = tokio::join!(bearer(&first), bearer(&second));
        assert_eq!(left, format!("Bearer {ROTATED_ACCESS}"));
        assert_eq!(right, format!("Bearer {ROTATED_ACCESS}"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "a rotated refresh token must only be spent once"
        );
    }

    #[tokio::test]
    async fn an_expired_credential_with_nothing_to_renew_it_says_so() {
        for mode in [
            RefreshMode::Rotating,
            RefreshMode::Retaining,
            RefreshMode::Minting,
        ] {
            let fixture = fixture(mode, 0);
            let mut stored = credential(-10);
            // A credential that carries only an access token cannot be renewed.
            stored.refresh_token = String::new();
            fixture.store.save(&stored).unwrap();

            let error = expect_resolution_failure(&resolver(&fixture)).await;
            let message = format!("{error:#}");
            assert!(message.contains("cannot be renewed"), "{mode:?}: {message}");
            assert!(
                message.contains("octet --login test-provider"),
                "{mode:?}: {message}"
            );
            // The guard fires before any request, so an unroutable endpoint is
            // never contacted and the credential is left as its owner had it.
            assert_eq!(fixture.store.load().unwrap().unwrap().access_token, ACCESS);
        }
    }

    #[tokio::test]
    async fn a_durable_key_is_never_treated_as_needing_renewal() {
        // A `Never` provider has no refresh token by design, so the same
        // credential shape must resolve rather than being called unrenewable.
        let fixture = fixture(RefreshMode::Never, 0);
        let mut stored = credential(-10);
        stored.refresh_token = String::new();
        fixture.store.save(&stored).unwrap();
        // The stub refresh always fails, which is the point: the guard must let
        // the request through to the flow rather than pre-empting it.
        let error = expect_resolution_failure(&resolver(&fixture)).await;
        assert!(
            !format!("{error:#}").contains("cannot be renewed"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_contended_refresh_lock_fails_closed_inside_its_budget() {
        let fixture = fixture(RefreshMode::Rotating, 0);
        fixture.store.save(&credential(-10)).unwrap();
        let held = fixture.store.lock_refresh().unwrap();
        let resolver = SubscriptionResolver::with_refresh_lock_wait(
            Arc::clone(&fixture.flow),
            fixture.store.clone(),
            std::time::Duration::from_millis(50),
        );
        let started = std::time::Instant::now();
        let error = expect_resolution_failure(&resolver).await;
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let message = format!("{error:#}");
        assert!(message.contains("another octet process"), "{message}");
        // Nothing was rotated, so the credential is exactly as its owner left it.
        let stored = fixture.store.load().unwrap().unwrap();
        assert_eq!(stored.refresh_token, REFRESH);
        drop(held);
    }

    #[tokio::test]
    async fn only_a_presend_connection_failure_is_reported_as_unavailable() {
        assert!(matches!(
            classification(&anyhow::anyhow!("boom")),
            AuthError::Resolve
        ));
        let connect = reqwest::Client::new()
            .get(UNROUTABLE)
            .send()
            .await
            .expect_err("loopback port 1 must refuse");
        assert!(matches!(
            classification(&anyhow::Error::new(connect)),
            AuthError::Unavailable
        ));
    }

    #[tokio::test]
    async fn a_request_header_octet_cannot_encode_fails_loudly() {
        let mut flow = TestFlow::new(UNROUTABLE.to_owned());
        // An embedded newline cannot be a header value. Silently dropping the
        // header would send a request the provider did not expect, so the
        // resolution fails with a message naming the provider instead.
        flow.headers = vec![("x-rejected", "one\ntwo".to_owned())];
        let guard = tempfile::tempdir().unwrap();
        let store = OAuthStore::new(guard.path().join("credential.json"), "Test Provider");
        store.save(&credential(600)).unwrap();
        let resolver =
            SubscriptionResolver::new(Arc::new(flow) as Arc<dyn SubscriptionFlow>, store.clone());

        let error = expect_resolution_failure(&resolver).await;
        let message = format!("{error:#}");
        assert!(message.contains("unusable request header"), "{message}");
        assert!(message.contains("Test Provider"), "{message}");
        assert!(!message.contains(ACCESS), "{message}");
    }

    #[tokio::test]
    async fn discovery_headers_carry_the_bearer_and_hide_the_token_behind_a_fingerprint() {
        let mut flow = TestFlow::new(UNROUTABLE.to_owned());
        flow.headers = vec![("x-provider-identity", "octet-test".to_owned())];
        let guard = tempfile::tempdir().unwrap();
        let store = OAuthStore::new(guard.path().join("credential.json"), "Test Provider");
        store.save(&credential(600)).unwrap();
        let resolver =
            SubscriptionResolver::new(Arc::new(flow) as Arc<dyn SubscriptionFlow>, store);

        let (headers, fingerprint) = resolver.discovery_headers().await.unwrap();
        assert_eq!(
            headers
                .get(http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some(format!("Bearer {ACCESS}").as_str())
        );
        assert_eq!(
            headers
                .get("x-provider-identity")
                .and_then(|value| value.to_str().ok()),
            Some("octet-test")
        );
        // The fingerprint scopes a cached inventory to this account without the
        // cache file ever holding a usable secret.
        assert!(!fingerprint.contains(ACCESS), "{fingerprint}");
        assert!(!fingerprint.contains(REFRESH), "{fingerprint}");
        assert_eq!(fingerprint.len(), 16, "{fingerprint}");
    }

    #[tokio::test]
    async fn no_diagnostic_reaches_the_caller_with_a_token_in_it() {
        let fixture = fixture(RefreshMode::Rotating, 0);
        fixture.store.save(&credential(-10)).unwrap();
        let resolver = resolver(&fixture);
        // An unroutable token endpoint: the failure message must not carry the
        // refresh token it was about to send.
        let error = expect_resolution_failure(&resolver).await;
        let rendered = format!("{error:#} {error:?} {resolver:?}");
        assert!(!rendered.contains(ACCESS), "{rendered}");
        assert!(!rendered.contains(REFRESH), "{rendered}");
    }
}
