//! Bounded GitHub.com wire operations. No response/error body is diagnostic data.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderName, HeaderValue};
use octet_ai::{Capabilities, ModalitySet, ModelLimits, Protocol};
use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::providers::{
    CopilotAvailabilityError as Error, CopilotCredentialScheme, CopilotDeviceLogin,
    CopilotDynamicHeader, CopilotEndpoint, CopilotModel, CopilotSession,
};

pub(super) const VERIFICATION_URI: &str = "https://github.com/login/device";
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const DEVICE_URL: &str = "https://github.com/login/device/code";
const OAUTH_URL: &str = "https://github.com/login/oauth/access_token";
const EXCHANGE_URL: &str = "https://api.github.com/copilot_internal/v2/token";
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_AUTH_BYTES: usize = 64 * 1024;
const MAX_MODELS_BYTES: usize = 1024 * 1024;
const EDITOR_VERSION: &str = concat!("octet/", env!("CARGO_PKG_VERSION"));

pub(super) struct Client {
    http: reqwest::Client,
    device_url: url::Url,
    oauth_url: url::Url,
    exchange_url: url::Url,
    #[cfg(test)]
    mock_origin: Option<url::Url>,
}

impl Client {
    pub(super) fn new() -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(HTTP_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .user_agent(EDITOR_VERSION)
            .build()
            .map_err(|_| anyhow::anyhow!("GitHub Copilot HTTP client is unavailable"))?;
        Ok(Self {
            http,
            device_url: url::Url::parse(DEVICE_URL).expect("static device URL"),
            oauth_url: url::Url::parse(OAUTH_URL).expect("static OAuth URL"),
            exchange_url: url::Url::parse(EXCHANGE_URL).expect("static exchange URL"),
            #[cfg(test)]
            mock_origin: None,
        })
    }

    // Loopback authority exists only in a test build, never in production config,
    // environment variables, persisted credentials, or a token response.
    #[cfg(test)]
    pub(super) fn mock(origin: &str) -> Self {
        let origin = url::Url::parse(origin).unwrap();
        assert_eq!(origin.scheme(), "http");
        assert!(origin
            .host_str()
            .unwrap()
            .parse::<std::net::IpAddr>()
            .unwrap()
            .is_loopback());
        Self {
            http: reqwest::Client::builder()
                .no_proxy()
                .timeout(HTTP_TIMEOUT)
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            device_url: origin.join("login/device/code").unwrap(),
            oauth_url: origin.join("login/oauth/access_token").unwrap(),
            exchange_url: origin.join("copilot_internal/v2/token").unwrap(),
            mock_origin: Some(origin),
        }
    }

    pub(super) async fn begin(&self) -> Result<Device, Error> {
        let response: DeviceResponse = bounded_json(
            self.http
                .post(self.device_url.clone())
                .header("accept", "application/json")
                .form(&[("client_id", CLIENT_ID), ("scope", "read:user")]),
            MAX_AUTH_BYTES,
        )
        .await
        .map_err(|_| Error::InvalidDeviceLogin)?;
        if response.verification_uri != VERIFICATION_URI
            || !super::store::valid_token(&response.device_code)
            || response.user_code.contains(&response.device_code)
        {
            return Err(Error::InvalidDeviceLogin);
        }
        let display = CopilotDeviceLogin::new(
            url::Url::parse(VERIFICATION_URI).expect("static verification URL"),
            response.user_code,
            Duration::from_secs(response.expires_in),
            Duration::from_secs(response.interval),
        )?;
        Ok(Device {
            code: response.device_code,
            display,
        })
    }

    pub(super) async fn poll(&self, device: &Device) -> Result<Poll, Error> {
        let response: PollResponse = bounded_json(
            self.http
                .post(self.oauth_url.clone())
                .header("accept", "application/json")
                .form(&[
                    ("client_id", CLIENT_ID),
                    ("device_code", device.code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ]),
            MAX_AUTH_BYTES,
        )
        .await
        .map_err(|_| Error::TokenExchangeUnavailable)?;
        match response {
            PollResponse::Token {
                access_token,
                token_type,
            } if token_type.eq_ignore_ascii_case("bearer")
                && super::store::valid_token(&access_token) =>
            {
                Ok(Poll::Authorized(access_token))
            }
            PollResponse::Failure { error } => match error.as_str() {
                "authorization_pending" => Ok(Poll::Pending),
                "slow_down" => Ok(Poll::SlowDown),
                "expired_token" => Ok(Poll::Expired),
                "access_denied" => Ok(Poll::Denied),
                _ => Err(Error::TokenExchangeUnavailable),
            },
            _ => Err(Error::TokenExchangeUnavailable),
        }
    }

    pub(super) async fn exchange(&self, github_token: &str) -> Result<Session, Error> {
        let response: TokenResponse = bounded_json(
            self.http
                .get(self.exchange_url.clone())
                .header("authorization", sensitive_header("token", github_token)?)
                .header("accept", "application/json")
                .header("editor-version", EDITOR_VERSION)
                .header("editor-plugin-version", EDITOR_VERSION),
            MAX_AUTH_BYTES,
        )
        .await
        .map_err(|_| Error::TokenExchangeUnavailable)?;
        let endpoint = self.endpoint(&response.endpoints.api)?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidSession)?
            .as_secs();
        let remaining = response
            .expires_at
            .checked_sub(now)
            .ok_or(Error::InvalidSession)?;
        if remaining > 24 * 60 * 60
            || !super::store::valid_token(&response.token)
            || response.token.contains(github_token)
        {
            return Err(Error::InvalidSession);
        }
        let lifetime = Duration::from_secs(response.refresh_in.unwrap_or(remaining).min(remaining));
        // Reject already-stale sessions rather than let every request refresh.
        if lifetime <= Duration::from_secs(30) {
            return Err(Error::InvalidSession);
        }
        let authorization = sensitive_header("Bearer", &response.token)?;
        let headers = [
            ("editor-version", EDITOR_VERSION),
            ("editor-plugin-version", EDITOR_VERSION),
            ("user-agent", EDITOR_VERSION),
            ("copilot-integration-id", "vscode-chat"),
            ("openai-intent", "conversation-panel"),
        ]
        .into_iter()
        .map(|(name, value)| {
            CopilotDynamicHeader::new(
                HeaderName::from_static(name),
                HeaderValue::from_static(value),
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
        let session = CopilotSession::new(
            response.token,
            CopilotCredentialScheme::Bearer,
            headers,
            lifetime,
        )?;
        Ok(Session {
            endpoint,
            session,
            authorization,
            expires_at: tokio::time::Instant::now() + lifetime,
        })
    }

    fn endpoint(&self, value: &str) -> Result<CopilotEndpoint, Error> {
        #[cfg(test)]
        if let Some(origin) = self.mock_origin.as_ref() {
            let url = url::Url::parse(value).map_err(|_| Error::InvalidEndpoint)?;
            return if &url == origin {
                CopilotEndpoint::new(url)
            } else {
                Err(Error::InvalidEndpoint)
            };
        }
        validate_inference_endpoint(value)
    }

    pub(super) async fn models(
        &self,
        session: &Session,
        github_token: &str,
    ) -> Result<Vec<CopilotModel>, Error> {
        let url = session
            .endpoint
            .base_url()
            .join("models")
            .map_err(|_| Error::InvalidEndpoint)?;
        let inventory: Inventory = bounded_json(
            self.http
                .get(url)
                .header("authorization", session.authorization.clone())
                .header("accept", "application/json")
                .header("editor-version", EDITOR_VERSION)
                .header("editor-plugin-version", EDITOR_VERSION)
                .header("copilot-integration-id", "vscode-chat"),
            MAX_MODELS_BYTES,
        )
        .await
        .map_err(|_| Error::ModelDiscoveryUnavailable)?;
        let inference_token = session
            .authorization
            .to_str()
            .expect("validated ASCII authorization")
            .strip_prefix("Bearer ")
            .expect("host-owned Bearer authorization");
        parse_models(inventory, [github_token, inference_token])
    }
}

/// Validate the complete inference authority before attaching an inference token.
/// Enterprise custom domains and token-embedded proxy hints are not authority.
pub(super) fn validate_inference_endpoint(value: &str) -> Result<CopilotEndpoint, Error> {
    let url = url::Url::parse(value).map_err(|_| Error::InvalidEndpoint)?;
    if url.scheme() != "https"
        || url.port_or_known_default() != Some(443)
        || !matches!(
            url.host_str(),
            Some(
                "api.githubcopilot.com"
                    | "api.individual.githubcopilot.com"
                    | "api.business.githubcopilot.com"
                    | "api.enterprise.githubcopilot.com"
            )
        )
    {
        return Err(Error::InvalidEndpoint);
    }
    CopilotEndpoint::new(url)
}

fn sensitive_header(scheme: &str, token: &str) -> Result<HeaderValue, Error> {
    let mut value =
        HeaderValue::from_str(&format!("{scheme} {token}")).map_err(|_| Error::InvalidSession)?;
    value.set_sensitive(true);
    Ok(value)
}

async fn bounded_json<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
    limit: usize,
) -> Result<T, ()> {
    // The outer deadline also bounds a peer that keeps sending small chunks.
    tokio::time::timeout(HTTP_TIMEOUT, async {
        let mut response = request.send().await.map_err(|_| ())?;
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|len| len > limit as u64)
        {
            return Err(());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| ())
    })
    .await
    .map_err(|_| ())?
}

pub(super) struct Device {
    code: String,
    pub(super) display: CopilotDeviceLogin,
}

pub(super) enum Poll {
    Pending,
    SlowDown,
    Authorized(String),
    Expired,
    Denied,
}

#[derive(Clone)]
pub(super) struct Session {
    pub(super) endpoint: CopilotEndpoint,
    pub(super) session: CopilotSession,
    authorization: HeaderValue,
    expires_at: tokio::time::Instant,
}

impl Session {
    pub(super) fn is_fresh(&self) -> bool {
        self.expires_at
            .saturating_duration_since(tokio::time::Instant::now())
            > Duration::from_secs(30)
    }
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PollResponse {
    Failure {
        error: String,
    },
    Token {
        access_token: String,
        token_type: String,
    },
}

#[derive(Deserialize)]
struct TokenResponse {
    token: String,
    expires_at: u64,
    refresh_in: Option<u64>,
    endpoints: Endpoints,
}

#[derive(Deserialize)]
struct Endpoints {
    api: String,
}

#[derive(Deserialize)]
struct Inventory {
    data: Vec<serde_json::Value>,
}

fn parse_models(inventory: Inventory, secrets: [&str; 2]) -> Result<Vec<CopilotModel>, Error> {
    if inventory.data.len() > 128 {
        return Err(Error::TooManyModels);
    }
    let mut models = Vec::new();
    for value in inventory.data {
        // Validate each record before filtering, so a malformed sibling cannot
        // turn a failed inventory into a partially advertised one.
        let model: WireModel =
            serde_json::from_value(value).map_err(|_| Error::InvalidModelMetadata)?;
        if [model.id.as_str(), model.name.as_deref().unwrap_or("")]
            .iter()
            .any(|text| secrets.iter().any(|secret| text.contains(*secret)))
        {
            return Err(Error::InvalidModelMetadata);
        }
        if !model.model_picker_enabled
            || model.policy.state != "enabled"
            || model.capabilities.kind != "chat"
        {
            continue;
        }
        let protocol = if model
            .supported_endpoints
            .iter()
            .any(|route| route == "/chat/completions")
        {
            Protocol::OpenAiChat
        } else if model
            .supported_endpoints
            .iter()
            .any(|route| route == "/responses")
        {
            Protocol::OpenAiResponses
        } else {
            // In particular, do not relabel Anthropic-only models as OpenAI.
            continue;
        };
        let supports = model.capabilities.supports;
        // A bare reasoning flag is not an effort/control contract. Leave these
        // models out rather than fabricate controls or strip reasoning semantics.
        if supports.reasoning {
            continue;
        }
        let mut metadata = CopilotModel::new(
            model.id,
            protocol,
            Capabilities {
                input_modalities: ModalitySet::none(),
                output_modalities: ModalitySet::none(),
                tools: supports.tool_calls,
                parallel_tool_calls: supports.parallel_tool_calls,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            ModelLimits {
                context_window: model.capabilities.limits.max_context_window_tokens,
                max_output_tokens: model.capabilities.limits.max_output_tokens,
            },
        );
        // Vision and structured-output controls require separate wire evidence;
        // do not infer them, reasoning, or routes from a model's name.
        if let Some(name) = model.name {
            metadata = metadata.with_display_name(name);
        }
        models.push(metadata);
    }
    if models.is_empty() {
        return Err(Error::NoEligibleModels);
    }
    Ok(models)
}

#[derive(Deserialize)]
struct WireModel {
    id: String,
    name: Option<String>,
    model_picker_enabled: bool,
    policy: Policy,
    supported_endpoints: Vec<String>,
    capabilities: WireCapabilities,
}

#[derive(Deserialize)]
struct Policy {
    state: String,
}

#[derive(Deserialize)]
struct WireCapabilities {
    #[serde(rename = "type")]
    kind: String,
    supports: Supports,
    limits: Limits,
}

#[derive(Deserialize)]
struct Supports {
    #[serde(default)]
    tool_calls: bool,
    #[serde(default)]
    parallel_tool_calls: bool,
    #[serde(default)]
    reasoning: bool,
}

#[derive(Deserialize)]
struct Limits {
    max_context_window_tokens: u64,
    max_output_tokens: u64,
}
