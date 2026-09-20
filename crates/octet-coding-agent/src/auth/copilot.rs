//! Coding-host GitHub Copilot authentication, separate from the embedding seam.
//!
//! Only the GitHub OAuth token is persisted. Device state and short-lived
//! inference sessions stay here, never in catalog metadata or diagnostics.

#[path = "copilot/store.rs"]
mod store;
#[path = "copilot/wire.rs"]
mod wire;

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::Mutex;
use tokio::time::Instant;

use crate::providers::{
    CopilotAvailabilityError as Error, CopilotDeviceLogin, CopilotDeviceLoginStatus, CopilotHost,
    CopilotModel, CopilotProvider, CopilotSession,
};

pub use store::{default_path, CredentialStore};

/// Run GitHub.com's device flow; headless mode never launches a browser.
pub async fn login(store: &CredentialStore, headless: bool) -> Result<()> {
    let host = CopilotCodingHost::new(store.clone())?;
    login_with_host(&host, headless).await
}

/// Drive the existing host seam with bounded, control-safe device presentation.
/// Useful to embedding UIs and deterministic hosts; the host owns persistence.
pub async fn login_with_host(host: &dyn CopilotHost, headless: bool) -> Result<()> {
    let device = host.begin_device_login().await?;
    if device.verification_uri().as_str() != wire::VERIFICATION_URI {
        return Err(Error::InvalidDeviceLogin.into());
    }
    let code = crate::output::table_field(device.user_code(), crate::output::stdout_is_terminal());
    crate::output::stdout_multiline(format!(
        "Open this URL and enter the code shown below:\n\n  {}\n\n  Code: {code}\n\nWaiting for authorization…",
        wire::VERIFICATION_URI,
    ));
    if !headless {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener)
            .arg(wire::VERIFICATION_URI)
            .spawn();
    }
    let deadline = Instant::now() + device.expires_in();
    loop {
        let status = tokio::time::timeout_at(deadline, async {
            tokio::time::sleep(device.poll_interval()).await;
            host.poll_device_login().await
        })
        .await
        .map_err(|_| Error::DeviceAuthorizationExpired)??;
        match status {
            CopilotDeviceLoginStatus::Pending => {}
            CopilotDeviceLoginStatus::Authorized => {
                crate::output::stdout_line("Signed in to GitHub Copilot.");
                return Ok(());
            }
            CopilotDeviceLoginStatus::Expired => {
                return Err(Error::DeviceAuthorizationExpired.into())
            }
            CopilotDeviceLoginStatus::Denied => return Err(Error::DeviceAuthorizationDenied.into()),
        }
    }
}

/// Remove only the selected Copilot credential. No network or remote revocation.
/// An embedding caller must also discard its old catalog/resolvers on logout.
pub async fn logout(store: &CredentialStore) -> Result<()> {
    store.delete()?;
    crate::output::stdout_line("Signed out of GitHub Copilot.");
    Ok(())
}

/// Register authenticated models from the default store. Offline is a strict
/// no-I/O fast path, not permission to import or advertise a cached inventory.
pub async fn register_available_models(
    catalog: &mut octet_ai::ModelCatalog,
    offline: bool,
) -> Result<()> {
    if offline {
        return Ok(());
    }
    let store = CredentialStore::new(default_path()?);
    register_available_models_with_store(catalog, &store, offline).await
}

/// Explicit-store variant for isolated hosts; missing credentials make no request
/// and contribute no models/endpoints. Existing caller catalog entries are kept.
pub async fn register_available_models_with_store(
    catalog: &mut octet_ai::ModelCatalog,
    store: &CredentialStore,
    offline: bool,
) -> Result<()> {
    if offline || !store.is_configured()? {
        return Ok(());
    }
    let host = Arc::new(CopilotCodingHost::new(store.clone())?);
    host.register_available_models(catalog, false).await
}

/// Synchronous product-catalog boundary, including callers already inside Tokio.
/// Offline and missing credentials return before constructing a runtime/client.
/// This never starts device login or persists credentials.
pub(crate) fn register_available_models_blocking(
    catalog: &mut octet_ai::ModelCatalog,
    offline: bool,
) -> Result<()> {
    if offline {
        return Ok(());
    }
    let store = CredentialStore::new(default_path()?);
    if !store.is_configured()? {
        return Ok(());
    }
    let host = Arc::new(CopilotCodingHost::new(store)?);
    register_available_models_with_host_blocking(catalog, host, false)
}

pub(crate) fn register_available_models_with_host_blocking(
    catalog: &mut octet_ai::ModelCatalog,
    host: Arc<CopilotCodingHost>,
    offline: bool,
) -> Result<()> {
    if offline {
        return Ok(());
    }
    // Match the existing synchronous catalog's discovery boundary without a
    // nested block_on on the caller's executor or a detached catalog writer.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("octet-copilot-discovery".into())
            .spawn_scoped(scope, move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| {
                        anyhow::anyhow!("GitHub Copilot discovery runtime is unavailable")
                    })?;
                runtime.block_on(host.register_available_models(catalog, false))
            })
            .map_err(|_| anyhow::anyhow!("GitHub Copilot discovery thread is unavailable"))?
            .join()
            .map_err(|_| anyhow::anyhow!("GitHub Copilot discovery thread failed"))?
    })
}

/// Validate the production inference authority without making a request.
#[cfg(test)]
pub fn validate_inference_endpoint(
    value: &str,
) -> Result<crate::providers::CopilotEndpoint, Error> {
    wire::validate_inference_endpoint(value)
}

/// The coding host's implementation of the existing credential-safe Copilot seam.
/// No environment endpoints, legacy credentials, or background refresh tasks.
pub struct CopilotCodingHost {
    store: CredentialStore,
    client: wire::Client,
    device: Mutex<Option<ActiveDevice>>,
    state: Mutex<SessionState>,
}

struct ActiveDevice {
    device: wire::Device,
    snapshot: store::Snapshot,
    deadline: Instant,
    next_poll: Instant,
    interval: Duration,
}

#[derive(Default)]
struct SessionState {
    credential_bytes: Option<Vec<u8>>,
    origin: Option<url::Url>,
    origin_invalidated: bool,
    session: Option<wire::Session>,
}

impl fmt::Debug for CopilotCodingHost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CopilotCodingHost")
            .field("state", &"<private>")
            .finish()
    }
}

impl CopilotCodingHost {
    /// Construct a host without reading credentials or sending requests.
    pub fn new(store: CredentialStore) -> Result<Self> {
        Ok(Self::with_client(store, wire::Client::new()?))
    }

    fn with_client(store: CredentialStore, client: wire::Client) -> Self {
        Self {
            store,
            client,
            device: Mutex::new(None),
            state: Mutex::new(SessionState::default()),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_mock_server(store: CredentialStore, origin: &str) -> Self {
        Self::with_client(store, wire::Client::mock(origin))
    }

    /// Bind an authenticated origin and reuse the existing provider's atomic
    /// route/model registration. No public catalog mutation precedes discovery.
    pub async fn register_available_models(
        self: &Arc<Self>,
        catalog: &mut octet_ai::ModelCatalog,
        offline: bool,
    ) -> Result<()> {
        if offline || !self.store.is_configured()? {
            return Ok(());
        }
        let endpoint = self.session(false).await?.endpoint;
        let host: Arc<dyn CopilotHost> = self.clone();
        CopilotProvider::new(host, endpoint)?
            .register_models(catalog)
            .await?;
        Ok(())
    }

    fn current_snapshot(&self) -> Result<store::Snapshot, Error> {
        self.store
            .snapshot()
            .map_err(|_| Error::TokenExchangeUnavailable)
    }

    fn check_snapshot(&self, expected: &[u8]) -> Result<store::Snapshot, Error> {
        let snapshot = self.current_snapshot()?;
        if snapshot.bytes.as_deref() != Some(expected) {
            return Err(Error::LoginRequired);
        }
        Ok(snapshot)
    }

    async fn session(&self, force_refresh: bool) -> Result<wire::Session, Error> {
        // Both direct host calls and the provider resolver are serialized. No
        // detached task can install a token after its awaiting operation is dropped.
        let mut state = self.state.lock().await;
        if state.origin_invalidated {
            return Err(Error::InvalidEndpoint);
        }
        let snapshot = self.current_snapshot()?;
        let Some(token) = snapshot
            .token()
            .map_err(|_| Error::TokenExchangeUnavailable)?
        else {
            state.session = None;
            return Err(Error::LoginRequired);
        };
        if state
            .credential_bytes
            .as_ref()
            .is_some_and(|bytes| Some(bytes) != snapshot.bytes.as_ref())
        {
            state.session = None;
            return Err(Error::LoginRequired);
        }
        if !force_refresh {
            if let Some(session) = state.session.as_ref().filter(|session| session.is_fresh()) {
                return Ok(session.clone());
            }
        }
        state.session = None;
        state.credential_bytes = snapshot.bytes;
        let replacement = self.client.exchange(&token).await.inspect_err(|error| {
            if *error == Error::InvalidEndpoint {
                state.origin_invalidated = true;
            }
        })?;
        self.check_snapshot(
            state
                .credential_bytes
                .as_deref()
                .expect("loaded credential"),
        )?;
        if state
            .origin
            .as_ref()
            .is_some_and(|origin| origin != replacement.endpoint.base_url())
        {
            state.origin_invalidated = true;
            return Err(Error::InvalidEndpoint);
        }
        state.origin = Some(replacement.endpoint.base_url().clone());
        state.session = Some(replacement.clone());
        Ok(replacement)
    }
}

#[async_trait::async_trait]
impl CopilotHost for CopilotCodingHost {
    async fn availability(&self) -> Result<(), Error> {
        let mut state = self.state.lock().await;
        // An origin rejection fences every resolver sharing this host, including
        // caches with still-fresh tokens. Only a new host may bind an authority.
        if state.origin_invalidated {
            return Err(Error::InvalidEndpoint);
        }
        let available = self.current_snapshot().and_then(|snapshot| {
            snapshot
                .token()
                .map_err(|_| Error::TokenExchangeUnavailable)?
                .ok_or(Error::LoginRequired)?;
            if state
                .credential_bytes
                .as_ref()
                .is_some_and(|bytes| Some(bytes) != snapshot.bytes.as_ref())
            {
                return Err(Error::LoginRequired);
            }
            Ok(())
        });
        if available.is_err() {
            state.session = None;
        }
        available
    }

    async fn begin_device_login(&self) -> Result<CopilotDeviceLogin, Error> {
        let mut active = self.device.lock().await;
        // Starting another flow discards this host's old device state, including
        // when the new request is cancelled or fails before it returns.
        *active = None;
        let snapshot = self.current_snapshot()?;
        let device = self.client.begin().await?;
        let display = device.display.clone();
        let now = Instant::now();
        *active = Some(ActiveDevice {
            device,
            snapshot,
            deadline: now + display.expires_in(),
            next_poll: now + display.poll_interval(),
            interval: display.poll_interval(),
        });
        Ok(display)
    }

    async fn poll_device_login(&self) -> Result<CopilotDeviceLoginStatus, Error> {
        let mut active = self.device.lock().await;
        let Some(flow) = active.as_mut() else {
            return Err(Error::LoginRequired);
        };
        let result = tokio::time::timeout_at(flow.deadline, async {
            tokio::time::sleep_until(flow.next_poll).await;
            // Reserve the next poll before transport so cancellation cannot cause
            // a caller to poll faster than the server-authorized interval.
            flow.next_poll = Instant::now() + flow.interval;
            self.client.poll(&flow.device).await
        })
        .await;
        let poll = match result {
            Ok(Ok(poll)) => poll,
            Ok(Err(error)) => {
                *active = None;
                return Err(error);
            }
            Err(_) => wire::Poll::Expired,
        };
        match poll {
            wire::Poll::Pending => Ok(CopilotDeviceLoginStatus::Pending),
            wire::Poll::SlowDown => {
                let flow = active.as_mut().expect("active device flow");
                flow.interval += Duration::from_secs(5);
                flow.next_poll = Instant::now() + flow.interval;
                Ok(CopilotDeviceLoginStatus::Pending)
            }
            wire::Poll::Authorized(token) => {
                let flow = active.take().expect("active device flow");
                // Small bounded synchronous commit: cancellation cannot leave a
                // detached save worker that resurrects a logged-out credential.
                self.store
                    .save_if_unchanged(&flow.snapshot, &token)
                    .map_err(|_| Error::TokenExchangeUnavailable)?;
                Ok(CopilotDeviceLoginStatus::Authorized)
            }
            wire::Poll::Expired => {
                *active = None;
                Ok(CopilotDeviceLoginStatus::Expired)
            }
            wire::Poll::Denied => {
                *active = None;
                Ok(CopilotDeviceLoginStatus::Denied)
            }
        }
    }

    async fn exchange(&self) -> Result<CopilotSession, Error> {
        // A resolver calls exchange again after explicit invalidation. Never
        // return the same locally cached session in that case.
        Ok(self.session(true).await?.session)
    }

    async fn refresh(&self) -> Result<CopilotSession, Error> {
        self.session(true)
            .await
            .map(|session| session.session)
            .map_err(|_| Error::TokenRefreshUnavailable)
    }

    async fn discover_models(&self) -> Result<Vec<CopilotModel>, Error> {
        let session = self.session(false).await?;
        let state = self.state.lock().await;
        let expected = state
            .credential_bytes
            .as_deref()
            .ok_or(Error::LoginRequired)?;
        let snapshot = self.check_snapshot(expected)?;
        let token = snapshot
            .token()
            .map_err(|_| Error::TokenExchangeUnavailable)?
            .ok_or(Error::LoginRequired)?;
        let models = self.client.models(&session, &token).await?;
        self.check_snapshot(expected)?;
        Ok(models)
    }
}

#[cfg(test)]
mod fixture_tests {
    use super::*;

    #[test]
    fn fixture_credentials_stay_private_and_mock_origin_is_loopback() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory
            .path()
            .canonicalize()
            .unwrap()
            .join("copilot.json");
        let store = CredentialStore::new(path);
        store.save("fixture-oauth-token").unwrap();
        let host = CopilotCodingHost::with_mock_server(store, "http://127.0.0.1:1");
        assert!(host.store.is_configured().unwrap());
        assert!(validate_inference_endpoint("https://api.githubcopilot.com/").is_ok());
        assert!(validate_inference_endpoint("http://127.0.0.1:1/").is_err());
    }
}
