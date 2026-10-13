//! Private provider credential lifecycle.
//!
//! Only this module resolves API-key environment/store values for discovery or
//! turns their private source into an `octet_ai::Auth`. Public provider
//! definitions expose setup labels and variable names, never this value type.

use std::fmt;

use octet_ai::Auth;

#[cfg(test)]
use super::contract::ProviderDiagnostic;
use super::contract::{EndpointAuthPresentation, ProviderDeclaration, ProviderRoute};

/// A resolved API-key credential confined to provider catalog registration.
pub(crate) struct EnvironmentCredential {
    variable: &'static str,
    value: String,
    source: CredentialSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialSource {
    Environment,
    Stored,
}

impl fmt::Debug for EnvironmentCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvironmentCredential")
            .field("variable", &self.variable)
            .field("source", &self.source)
            .field("value", &"<redacted>")
            .finish()
    }
}

impl EnvironmentCredential {
    /// The private value used only for a provider's inventory request.
    pub(crate) fn value(&self) -> &str {
        &self.value
    }

    pub(crate) fn for_test(variable: &'static str, value: impl Into<String>) -> Self {
        Self {
            variable,
            value: value.into(),
            source: CredentialSource::Environment,
        }
    }

    fn variable(&self) -> &'static str {
        self.variable
    }
}

/// Resolve declared environment variables first, then the owner-private API-key
/// store for providers supported by one-key setup. Environment errors fail closed
/// rather than silently changing identity; configured environment sources retain
/// their per-request lookup semantics. No environment values are changed.
pub(crate) fn resolve_environment(
    declaration: &ProviderDeclaration,
) -> anyhow::Result<Option<EnvironmentCredential>> {
    resolve_environment_with(
        declaration,
        octet_ai::auth::read_bounded_env,
        |provider_id| {
            crate::provider_setup::BuiltinApiKeyStore::default_store()?
                .load(provider_id)
                .map_err(Into::into)
        },
    )
}

pub(crate) fn resolve_environment_with(
    declaration: &ProviderDeclaration,
    mut read_environment: impl FnMut(&str) -> Result<Option<String>, octet_ai::ConfigError>,
    read_stored: impl FnOnce(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<Option<EnvironmentCredential>> {
    let Some(variables) = declaration.authentication.environment_variables() else {
        return Ok(None);
    };
    for variable in variables {
        let value = match read_environment(variable) {
            Ok(value) => value,
            Err(octet_ai::ConfigError::InvalidEnv(_)) => {
                anyhow::bail!("could not read {variable}: invalid environment value")
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            return Ok(Some(EnvironmentCredential {
                variable,
                value,
                source: CredentialSource::Environment,
            }));
        }
    }
    let Some(provider) = crate::provider_setup::builtin_api_key_providers()
        .into_iter()
        .find(|provider| provider.id == declaration.id)
    else {
        return Ok(None);
    };
    Ok(
        read_stored(provider.id)?.map(|value| EnvironmentCredential {
            variable: provider.credential_variable,
            value,
            source: CredentialSource::Stored,
        }),
    )
}

/// Whether a credential variable carries a bearer token rather than the
/// provider's ordinary API key.
///
/// Anthropic OAuth/subscription tokens must be sent as `Authorization: Bearer`
/// even on routes whose default presentation is a custom API-key header. This
/// keys on the credential *variable*, never on a provider name, so a new
/// provider reusing these variables inherits the behavior without a branch. The
/// variable list itself is the shared `octet_ai` credential-alias declaration,
/// not a second copy.
fn bearer_token_variable(variable: &str) -> bool {
    octet_ai::ANTHROPIC_BEARER_TOKEN_VARIABLES.contains(&variable)
}

/// Build an endpoint auth strategy without copying the credential into a public
/// provider contract.
pub(crate) fn environment_auth(
    route: &ProviderRoute,
    credential: &EnvironmentCredential,
) -> anyhow::Result<Auth> {
    if credential.source == CredentialSource::Stored {
        if bearer_token_variable(credential.variable())
            || route.auth_presentation == EndpointAuthPresentation::Bearer
        {
            return Ok(Auth::bearer(credential.value().to_owned()));
        }
        // Discovery and inference must share the declaration's exact native
        // header presentation. Each supported presentation supplies one header.
        let headers = environment_discovery_headers(route, credential)?;
        let (name, value) = headers
            .iter()
            .next()
            .expect("one declared credential header");
        return Ok(Auth::header(name.clone(), value.to_str()?.to_owned()));
    }
    if bearer_token_variable(credential.variable()) {
        return Ok(Auth::bearer_env(credential.variable()));
    }
    match route.auth_presentation {
        EndpointAuthPresentation::Bearer => Ok(Auth::bearer_env(credential.variable())),
        EndpointAuthPresentation::ApiKeyHeader => Ok(Auth::header_env(
            http::HeaderName::from_static("x-api-key"),
            credential.variable(),
        )),
        EndpointAuthPresentation::CloudflareAiGateway => Ok(Auth::header_bearer_env(
            http::HeaderName::from_static("cf-aig-authorization"),
            credential.variable(),
        )),
        EndpointAuthPresentation::Header(name) => Ok(Auth::header_env(
            http::HeaderName::from_bytes(name.as_bytes())?,
            credential.variable(),
        )),
        EndpointAuthPresentation::GoogleApiKeyHeader => Ok(Auth::header_env(
            http::HeaderName::from_static("x-goog-api-key"),
            credential.variable(),
        )),
        EndpointAuthPresentation::AwsSigV4 | EndpointAuthPresentation::Dynamic => {
            anyhow::bail!("environment provider declaration has an invalid credential presentation")
        }
    }
}

/// Build private discovery headers for an environment-authenticated route.
///
/// The resolved value remains inside the auth lifecycle; callers receive only
/// a sensitive request header map for the immediate inventory request.
pub(crate) fn environment_discovery_headers(
    route: &ProviderRoute,
    credential: &EnvironmentCredential,
) -> anyhow::Result<http::HeaderMap> {
    let mut headers = http::HeaderMap::new();
    if bearer_token_variable(credential.variable()) {
        let mut value = http::HeaderValue::from_str(&format!("Bearer {}", credential.value()))?;
        value.set_sensitive(true);
        headers.insert(http::header::AUTHORIZATION, value);
        return Ok(headers);
    }
    let (name, value) = match route.auth_presentation {
        EndpointAuthPresentation::Bearer => (
            http::header::AUTHORIZATION,
            format!("Bearer {}", credential.value()),
        ),
        EndpointAuthPresentation::ApiKeyHeader => (
            http::HeaderName::from_static("x-api-key"),
            credential.value().to_owned(),
        ),
        EndpointAuthPresentation::CloudflareAiGateway => (
            http::HeaderName::from_static("cf-aig-authorization"),
            format!("Bearer {}", credential.value()),
        ),
        EndpointAuthPresentation::Header(name) => (
            http::HeaderName::from_bytes(name.as_bytes())?,
            credential.value().to_owned(),
        ),
        EndpointAuthPresentation::GoogleApiKeyHeader => (
            http::HeaderName::from_static("x-goog-api-key"),
            credential.value().to_owned(),
        ),
        EndpointAuthPresentation::AwsSigV4 | EndpointAuthPresentation::Dynamic => {
            anyhow::bail!("environment provider declaration has an invalid credential presentation")
        }
    };
    let mut value = http::HeaderValue::from_str(&value)?;
    value.set_sensitive(true);
    headers.insert(name, value);
    Ok(headers)
}

/// Standard Bedrock API-key variable.
///
/// AWS documents `AWS_BEARER_TOKEN_BEDROCK` as an alternative to SigV4 for the
/// Bedrock Runtime: the token is presented as `Authorization: Bearer <token>`
/// and the request is not signed. It is checked first so an operator who
/// configured an API key neither depends on nor pays for the SigV4 chain.
const AWS_BEDROCK_BEARER_VARIABLE: &str = "AWS_BEARER_TOKEN_BEDROCK";

/// Return the Bedrock request auth strategy.
///
/// Precedence is the documented Bedrock order: a configured Bedrock API key is
/// presented as a bearer token, and otherwise the bounded AWS credential chain
/// is confirmed to have a usable source before SigV4 signing is selected. The
/// signer resolves the chain again on each request, allowing ECS/EC2 metadata
/// credentials to rotate without leaking the private source into provider
/// declarations.
pub(crate) fn aws_bedrock_auth(region: &str) -> anyhow::Result<Option<Auth>> {
    aws_bedrock_auth_with(region, optional_bounded_env, resolve_aws_credentials)
}

fn aws_bedrock_auth_with<R, C>(
    region: &str,
    mut read_env: R,
    resolve_credentials: C,
) -> anyhow::Result<Option<Auth>>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
    C: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
{
    // A whitespace-only value cannot authenticate, so it is treated as absent
    // rather than disabling the working SigV4 chain for a typo.
    if read_env(AWS_BEDROCK_BEARER_VARIABLE)?.is_some_and(|token| !token.trim().is_empty()) {
        return Ok(Some(Auth::bearer_env(AWS_BEDROCK_BEARER_VARIABLE)));
    }
    if resolve_credentials()?.is_none() {
        return Ok(None);
    }
    Ok(Some(Auth::request_signer(std::sync::Arc::new(
        AwsBedrockSigner::new(region.to_owned()),
    ))))
}

/// Resolve the regional Bedrock Runtime endpoint from bounded configuration.
pub(crate) fn aws_bedrock_base_url(region: &str) -> anyhow::Result<url::Url> {
    if let Some(override_url) = optional_bounded_env("OCTET_BEDROCK_ENDPOINT")? {
        let mut url = url::Url::parse(&override_url)
            .map_err(|_| anyhow::anyhow!("invalid OCTET_BEDROCK_ENDPOINT"))?;
        if !matches!(url.scheme(), "https" | "http")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            anyhow::bail!("invalid OCTET_BEDROCK_ENDPOINT");
        }
        if !url.path().ends_with('/') {
            let path = format!("{}/", url.path());
            url.set_path(&path);
        }
        return Ok(url);
    }
    let domain = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    url::Url::parse(&format!("https://bedrock-runtime.{region}.{domain}/"))
        .map_err(|_| anyhow::anyhow!("invalid AWS region"))
}

/// Resolve the Bedrock region from product and standard AWS environment setup.
pub(crate) fn aws_bedrock_region() -> anyhow::Result<String> {
    for variable in ["OCTET_BEDROCK_REGION", "AWS_REGION", "AWS_DEFAULT_REGION"] {
        if let Some(region) = optional_bounded_env(variable)? {
            return checked_aws_region(region, variable);
        }
    }
    if let Some(region) = aws_profile_value("region", true)? {
        return checked_aws_region(region, "AWS profile region");
    }
    Ok("us-east-1".to_owned())
}

type AwsCredentialsResolver =
    std::sync::Arc<dyn Fn() -> anyhow::Result<Option<octet_ai::AwsCredentials>> + Send + Sync>;

struct AwsBedrockSigner {
    region: String,
    resolver: AwsCredentialsResolver,
}

impl fmt::Debug for AwsBedrockSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AwsBedrockSigner")
            .field("region", &self.region)
            .finish_non_exhaustive()
    }
}

impl AwsBedrockSigner {
    fn new(region: String) -> Self {
        Self {
            region,
            resolver: std::sync::Arc::new(resolve_aws_credentials),
        }
    }
}

#[async_trait::async_trait]
impl octet_ai::RequestSigner for AwsBedrockSigner {
    async fn sign(
        &self,
        request: &octet_ai::SigningRequest,
    ) -> Result<octet_ai::SignedRequestHeaders, octet_ai::AuthError> {
        let resolver = self.resolver.clone();
        let credentials = tokio::task::spawn_blocking(move || resolver())
            .await
            .map_err(|_| octet_ai::AuthError::Resolve)?
            .map_err(|_| octet_ai::AuthError::Resolve)?
            .ok_or(octet_ai::AuthError::Resolve)?;
        let signer = octet_ai::AwsSigV4Signer::new(credentials, self.region.clone(), "bedrock")?;
        octet_ai::RequestSigner::sign(&signer, request).await
    }
}

const MAX_AWS_PROFILE_BYTES: usize = 64 * 1024;
const MAX_AWS_METADATA_BYTES: usize = 64 * 1024;
const AWS_METADATA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
/// Total per-attempt bound for the single web-identity STS exchange.
///
/// A web-identity role is an explicit configuration statement, so unlike the
/// heuristic metadata probe this may talk to a real regional service; the bound
/// is still a hard cap with no retries.
const AWS_STS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Upper bound on the web-identity token read from the token file. OIDC tokens
/// are a few KiB; anything larger is not a token and must not be uploaded.
const MAX_AWS_WEB_IDENTITY_TOKEN_BYTES: u64 = 64 * 1024;

fn resolve_aws_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    resolve_aws_credentials_with(
        aws_environment_credentials,
        aws_web_identity_credentials,
        aws_profile_credentials,
        aws_metadata_credentials,
    )
}

/// Resolve the bounded AWS credential chain in documented order: environment
/// keys, web identity, the selected shared profile, then the indicated metadata
/// sources.
fn resolve_aws_credentials_with<E, W, P, M>(
    environment: E,
    web_identity: W,
    profile: P,
    metadata: M,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>>
where
    E: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
    W: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
    P: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
    M: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
{
    if let Some(credentials) = environment()? {
        return Ok(Some(credentials));
    }
    if let Some(credentials) = web_identity()? {
        return Ok(Some(credentials));
    }
    if let Some(credentials) = profile()? {
        return Ok(Some(credentials));
    }
    metadata()
}

fn aws_environment_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    aws_environment_credentials_with(optional_bounded_env)
}

fn aws_environment_credentials_with(
    mut read_env: impl FnMut(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let access_key_id = read_env("AWS_ACCESS_KEY_ID")?;
    let secret_access_key = read_env("AWS_SECRET_ACCESS_KEY")?;
    let session_token = read_env("AWS_SESSION_TOKEN")?;
    match (access_key_id, secret_access_key) {
        (None, None) => Ok(None),
        (Some(access_key_id), Some(secret_access_key)) => {
            aws_credentials(access_key_id, secret_access_key, session_token).map(Some)
        }
        _ => {
            anyhow::bail!("AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY must be configured together")
        }
    }
}

fn aws_profile_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let Some(values) = aws_profile_values(false)? else {
        return Ok(None);
    };
    aws_profile_credentials_from_values(&values)
}

fn aws_profile_credentials_from_values(
    values: &std::collections::BTreeMap<String, String>,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let access_key_id = values.get("aws_access_key_id").cloned();
    let secret_access_key = values.get("aws_secret_access_key").cloned();
    let session_token = values
        .get("aws_session_token")
        .cloned()
        .or_else(|| values.get("aws_security_token").cloned());
    match (access_key_id, secret_access_key) {
        (None, None) => Ok(None),
        (Some(access_key_id), Some(secret_access_key)) => {
            aws_credentials(access_key_id, secret_access_key, session_token).map(Some)
        }
        _ => anyhow::bail!("AWS profile has incomplete static credentials"),
    }
}

fn aws_credentials(
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
) -> anyhow::Result<octet_ai::AwsCredentials> {
    octet_ai::AwsCredentials::new(
        access_key_id,
        secret_access_key,
        session_token.map(octet_ai::Secret::from),
    )
    .map_err(|_| anyhow::anyhow!("AWS credential source is invalid"))
}

fn optional_bounded_env(variable: &str) -> anyhow::Result<Option<String>> {
    octet_ai::auth::read_bounded_env(variable).map_err(|error| match error {
        octet_ai::ConfigError::InvalidEnv(_) => {
            anyhow::anyhow!("could not read {variable}: invalid environment value")
        }
        other => other.into(),
    })
}

fn checked_aws_region(region: String, source: &str) -> anyhow::Result<String> {
    if region.len() > 128
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        anyhow::bail!("invalid {source}");
    }
    Ok(region)
}

fn aws_profile_name() -> anyhow::Result<String> {
    let profile = optional_bounded_env("AWS_PROFILE")?.unwrap_or_else(|| "default".to_owned());
    if profile.is_empty()
        || profile.len() > 128
        || !profile
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("invalid AWS_PROFILE");
    }
    Ok(profile)
}

fn aws_profile_credentials_path() -> anyhow::Result<Option<std::path::PathBuf>> {
    if let Some(path) = optional_bounded_env("AWS_SHARED_CREDENTIALS_FILE")? {
        return Ok(Some(std::path::PathBuf::from(path)));
    }
    Ok(dirs::home_dir().map(|home| home.join(".aws").join("credentials")))
}

fn aws_config_path() -> anyhow::Result<Option<std::path::PathBuf>> {
    if let Some(path) = optional_bounded_env("AWS_CONFIG_FILE")? {
        return Ok(Some(std::path::PathBuf::from(path)));
    }
    Ok(dirs::home_dir().map(|home| home.join(".aws").join("config")))
}

fn aws_profile_value(key: &str, config_file: bool) -> anyhow::Result<Option<String>> {
    let values = if config_file {
        aws_profile_config_values()?
    } else {
        aws_profile_values(false)?
    };
    Ok(values.and_then(|values| values.get(key).cloned()))
}

fn aws_profile_values(
    config_file: bool,
) -> anyhow::Result<Option<std::collections::BTreeMap<String, String>>> {
    let path = if config_file {
        aws_config_path()?
    } else {
        aws_profile_credentials_path()?
    };
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(contents) = read_bounded_aws_profile(&path)? else {
        return Ok(None);
    };
    let profile = aws_profile_name()?;
    let section = if config_file && profile != "default" {
        format!("profile {profile}")
    } else {
        profile
    };
    Ok(Some(parse_aws_ini_section(&contents, &section)?))
}

fn aws_profile_config_values() -> anyhow::Result<Option<std::collections::BTreeMap<String, String>>>
{
    aws_profile_values(true)
}

fn read_bounded_aws_profile(path: &std::path::Path) -> anyhow::Result<Option<String>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.file_type().is_file() {
        anyhow::bail!("AWS profile source is not a regular file");
    }
    if metadata.len() > MAX_AWS_PROFILE_BYTES as u64 {
        anyhow::bail!("AWS profile source exceeds the byte limit");
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_AWS_PROFILE_BYTES {
        anyhow::bail!("AWS profile source exceeds the byte limit");
    }
    String::from_utf8(bytes).map(Some).map_err(Into::into)
}

fn parse_aws_ini_section(
    contents: &str,
    wanted_section: &str,
) -> anyhow::Result<std::collections::BTreeMap<String, String>> {
    let mut selected = false;
    let mut values = std::collections::BTreeMap::new();
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            selected = section.trim() == wanted_section;
            continue;
        }
        if !selected {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key.len() <= 128
            && value.len() <= octet_ai::auth::MAX_ENV_VALUE_BYTES
            && key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            && !value.is_empty()
            && !value.chars().any(char::is_control)
        {
            values.insert(key, value.to_owned());
        }
    }
    Ok(values)
}

fn aws_metadata_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let inputs = aws_metadata_activation_inputs()?;
    let activation = aws_metadata_activation_from(&inputs);
    aws_metadata_credentials_with(
        activation,
        inputs.profile_metadata_service_endpoint,
        optional_bounded_env,
        metadata_credentials_from_url,
        ec2_metadata_credentials,
    )
}

/// Probe the AWS metadata credential sources for `activation`.
///
/// A `Disabled` activation returns before any client is built, so an
/// unrelated-provider launch pays neither a metadata request nor its bounded
/// timeout. This is the whole point of the activation rule: instance/container
/// metadata credentials may exist, but "might exist" is not a reason to probe
/// an unrelated cloud environment on every start.
///
/// `profile_endpoint` is the `ec2_metadata_service_endpoint` of the effective
/// AWS profile: an indication that the host pinned IMDS is only actionable if
/// the probe actually goes to that endpoint, so it is the last target choice
/// after the two environment override names.
fn aws_metadata_credentials_with<R, F, C>(
    activation: AwsMetadataActivation,
    profile_endpoint: Option<String>,
    mut read_env: R,
    fetch_container: F,
    fetch_ec2: C,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
    F: Fn(&url::Url, bool) -> anyhow::Result<octet_ai::AwsCredentials>,
    C: Fn(&url::Url) -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
{
    if matches!(activation, AwsMetadataActivation::Disabled(_)) {
        return Ok(None);
    }
    if let Some(url) = ecs_metadata_url_with(&mut read_env)? {
        return fetch_container(&url, true).map(Some);
    }
    let base = aws_metadata_service_endpoint_with(&mut read_env, profile_endpoint.as_deref())?;
    fetch_ec2(&base)
}

/// Product opt-in variable that allows the AWS metadata credential sources.
///
/// EC2 instance metadata and ECS container credentials are the two AWS sources
/// with no local configuration marker, so octet cannot tell "this machine is an
/// instance with a role" from "this laptop will simply time out". The rule below
/// therefore requires a positive local indication before a probe; this variable
/// is the explicit opt-in for an EC2 user whose provider configuration has no
/// other marker.
pub(crate) const AWS_METADATA_OPT_IN_VARIABLE: &str = "OCTET_AWS_METADATA_CREDENTIALS";

/// Default EC2 instance metadata service endpoint (IMDS).
const AWS_EC2_METADATA_ENDPOINT: &str = "http://169.254.169.254/";

/// Endpoint override variables, in precedence order: the AWS-standard name
/// first (what the AWS CLI and SDKs document), then octet's earlier alias.
const AWS_METADATA_ENDPOINT_VARIABLES: [&str; 2] = [
    "AWS_EC2_METADATA_SERVICE_ENDPOINT",
    "AWS_METADATA_SERVICE_ENDPOINT",
];

/// Endpoint *mode* override variables, in the same precedence order.
const AWS_METADATA_ENDPOINT_MODE_VARIABLES: [&str; 2] = [
    "AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE",
    "AWS_METADATA_SERVICE_ENDPOINT_MODE",
];

/// First variable in `variables` that is set to a non-empty value.
fn first_set_variable<R>(read_env: &mut R, variables: &[&str]) -> anyhow::Result<Option<String>>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
{
    for variable in variables {
        if let Some(value) = read_env(variable)? {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

/// DMI (SMBIOS) markers that identify an Amazon EC2 guest without a network
/// round trip.
///
/// The AWS SDKs read the same local files to decide "is this an EC2 instance"
/// before they ever contact IMDS. Four small sysfs reads cost microseconds, and
/// the check fails closed: on a laptop the files do not exist and on another
/// hypervisor they name that vendor, so nothing is probed and no timeout is paid.
const AWS_EC2_DMI_MARKERS: [&str; 4] = [
    "/sys/class/dmi/id/sys_vendor",
    "/sys/class/dmi/id/board_vendor",
    "/sys/class/dmi/id/product_name",
    "/sys/class/dmi/id/bios_vendor",
];

/// The DMI vendor string every EC2 instance reports.
const AWS_EC2_DMI_VENDOR: &str = "Amazon EC2";

/// Bound on a single DMI marker read: each is one short line, and an oversized
/// or non-UTF-8 file is not evidence of an EC2 instance.
const MAX_AWS_DMI_BYTES: u64 = 256;

/// The EC2 instance identity reported by the local DMI markers, if any.
///
/// This is deliberately a *filesystem* read rather than a metadata request: it
/// is the cheap local statement that lets a genuine EC2 instance keep its
/// instance-profile credentials, without making every unrelated laptop pay the
/// metadata timeout. `root` is a parameter (not an environment variable) so the
/// detection is directly testable against a fixture tree.
fn aws_instance_identity_from(root: &std::path::Path) -> Option<String> {
    AWS_EC2_DMI_MARKERS.iter().find_map(|marker| {
        read_dmi_marker(&root.join(marker.trim_start_matches('/')))
            .filter(|value| value.eq_ignore_ascii_case(AWS_EC2_DMI_VENDOR))
    })
}

fn read_dmi_marker(path: &std::path::Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_file() {
        return None;
    }
    // sysfs attributes report a page-sized st_size (usually 4096) even when
    // their actual contents are one short line. Bound the read itself, not the
    // advertised size, or genuine EC2 DMI markers are always discarded.
    read_dmi_marker_from(std::fs::File::open(path).ok()?)
}

fn read_dmi_marker_from(reader: impl std::io::Read) -> Option<String> {
    use std::io::Read;

    let mut bytes = Vec::new();
    reader
        .take(MAX_AWS_DMI_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_AWS_DMI_BYTES as usize {
        return None;
    }
    let value = String::from_utf8(bytes).ok()?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// The local evidence that made the metadata sources eligible, in evaluation
/// order. Kept as data so the rule is testable and so a diagnostic can name the
/// reason without reading the environment again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AwsMetadataIndication {
    /// `AWS_EC2_METADATA_DISABLED` is explicitly `false`: the standard AWS opt-in.
    ExplicitlyNotDisabled,
    /// `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI`/`_FULL_URI` is set. ECS, EKS and
    /// similar platforms set this only inside a container that has credentials.
    ContainerCredentialsUri,
    /// `AWS_EC2_METADATA_SERVICE_ENDPOINT`/`_MODE` is set (or octet's older
    /// `AWS_METADATA_SERVICE_ENDPOINT`/`_MODE` alias): the host pinned the IMDS
    /// endpoint, which only an EC2-shaped environment does.
    MetadataServiceEndpoint,
    /// `OCTET_AWS_METADATA_CREDENTIALS` truthy: the operator's explicit opt-in.
    ProductOptIn,
    /// The effective AWS profile declares instance/container metadata as its
    /// credential source (`credential_source = Ec2InstanceMetadata|EcsContainer`).
    ProfileCredentialSource,
    /// The local DMI markers name `Amazon EC2`: this host *is* an EC2 instance,
    /// so its instance profile is worth a probe. Checked last because it is an
    /// environment observation rather than an explicit configuration statement,
    /// and by that point every explicit reason has already been reported.
    Ec2InstanceIdentity,
}

/// Why the metadata probe stays closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AwsMetadataSuppression {
    /// `AWS_EC2_METADATA_DISABLED` (or the opt-in) is explicitly off.
    ExplicitlyDisabled,
    /// The variable holds an unrecognized value; unknown state stays closed.
    UnrecognizedDisableSetting,
    /// Nothing local indicates a metadata source. This is the ordinary
    /// unrelated-provider launch (for example a Codex user on a laptop).
    Unindicated,
}

/// Whether the AWS metadata credential sources may be probed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AwsMetadataActivation {
    /// A local indication was found; the bounded probe may run.
    Enabled(AwsMetadataIndication),
    /// No probe is attempted, for the recorded reason.
    Disabled(AwsMetadataSuppression),
}

/// The already-read inputs of the activation rule.
///
/// Keeping these as plain data (rather than reading the environment inside the
/// decision) is what makes the rule a pure, directly testable function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AwsMetadataActivationInputs {
    /// Raw `AWS_EC2_METADATA_DISABLED` value.
    pub(crate) ec2_metadata_disabled: Option<String>,
    /// Raw `AWS_CONTAINER_CREDENTIALS_RELATIVE_URI` value.
    pub(crate) container_credentials_relative_uri: Option<String>,
    /// Raw `AWS_CONTAINER_CREDENTIALS_FULL_URI` value.
    pub(crate) container_credentials_full_uri: Option<String>,
    /// Raw `AWS_EC2_METADATA_SERVICE_ENDPOINT` value, falling back to octet's
    /// older `AWS_METADATA_SERVICE_ENDPOINT` alias.
    pub(crate) metadata_service_endpoint: Option<String>,
    /// Raw `AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE` value, same fallback.
    pub(crate) metadata_service_endpoint_mode: Option<String>,
    /// Raw `OCTET_AWS_METADATA_CREDENTIALS` value.
    pub(crate) product_opt_in: Option<String>,
    /// `credential_source` from the effective AWS profile, already lowercased.
    pub(crate) profile_credential_source: Option<String>,
    /// `ec2_metadata_service_endpoint` from the effective AWS profile. It is an
    /// indication *and*, when no environment override is set, the probe target.
    pub(crate) profile_metadata_service_endpoint: Option<String>,
    /// Local DMI instance identity (`sys_vendor` and friends) when it names an
    /// Amazon EC2 guest. A filesystem read, never a metadata request.
    pub(crate) instance_identity: Option<String>,
}

/// The activation rule: a pure function of the local AWS environment.
///
/// Order matters and is chosen so that an explicit off always wins, and so that
/// every *enabling* input is a positive local statement that this machine has
/// metadata credentials. `AWS_PROFILE`, `AWS_CONFIG_FILE` and
/// `AWS_SHARED_CREDENTIALS_FILE` presence alone is deliberately **not** an
/// indication: a laptop user with any AWS profile would otherwise pay the probe,
/// which is exactly the ~1s startup penalty this rule removes. A profile only
/// enables the probe when it *declares* metadata as its credential source.
pub(crate) fn aws_metadata_activation_from(
    inputs: &AwsMetadataActivationInputs,
) -> AwsMetadataActivation {
    if let Some(value) = &inputs.ec2_metadata_disabled {
        if value.eq_ignore_ascii_case("true") {
            return AwsMetadataActivation::Disabled(AwsMetadataSuppression::ExplicitlyDisabled);
        }
        if value.eq_ignore_ascii_case("false") {
            return AwsMetadataActivation::Enabled(AwsMetadataIndication::ExplicitlyNotDisabled);
        }
        // Unknown state stays closed rather than probing on a typo.
        return AwsMetadataActivation::Disabled(AwsMetadataSuppression::UnrecognizedDisableSetting);
    }
    if let Some(value) = &inputs.product_opt_in {
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => {
                return AwsMetadataActivation::Enabled(AwsMetadataIndication::ProductOptIn);
            }
            "0" | "false" | "no" | "off" => {
                return AwsMetadataActivation::Disabled(AwsMetadataSuppression::ExplicitlyDisabled);
            }
            _ => {
                return AwsMetadataActivation::Disabled(
                    AwsMetadataSuppression::UnrecognizedDisableSetting,
                );
            }
        }
    }
    if inputs.container_credentials_relative_uri.is_some()
        || inputs.container_credentials_full_uri.is_some()
    {
        return AwsMetadataActivation::Enabled(AwsMetadataIndication::ContainerCredentialsUri);
    }
    if inputs.metadata_service_endpoint.is_some() || inputs.metadata_service_endpoint_mode.is_some()
    {
        return AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint);
    }
    if inputs.profile_metadata_service_endpoint.is_some() {
        return AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint);
    }
    if inputs
        .profile_credential_source
        .as_deref()
        .is_some_and(|source| {
            matches!(
                source.trim().to_ascii_lowercase().as_str(),
                "ec2instancemetadata" | "ecscontainer"
            )
        })
    {
        return AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource);
    }
    if inputs
        .instance_identity
        .as_deref()
        .is_some_and(|identity| identity.trim().eq_ignore_ascii_case(AWS_EC2_DMI_VENDOR))
    {
        return AwsMetadataActivation::Enabled(AwsMetadataIndication::Ec2InstanceIdentity);
    }
    AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
}

/// Read the activation inputs from the AWS environment and the effective profile.
fn aws_metadata_activation_inputs() -> anyhow::Result<AwsMetadataActivationInputs> {
    aws_metadata_activation_inputs_with(optional_bounded_env, aws_profile_values, || {
        aws_instance_identity_from(std::path::Path::new("/"))
    })
}

fn aws_metadata_activation_inputs_with<R, P, I>(
    mut read_env: R,
    mut read_profile: P,
    read_instance_identity: I,
) -> anyhow::Result<AwsMetadataActivationInputs>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
    P: FnMut(bool) -> anyhow::Result<Option<std::collections::BTreeMap<String, String>>>,
    I: FnOnce() -> Option<String>,
{
    let profile_values = read_profile(false)?;
    let profile_config = read_profile(true)?;
    let profile_value = |values: &Option<std::collections::BTreeMap<String, String>>, key: &str| {
        values.as_ref().and_then(|values| values.get(key)).cloned()
    };
    Ok(AwsMetadataActivationInputs {
        ec2_metadata_disabled: read_env("AWS_EC2_METADATA_DISABLED")?,
        container_credentials_relative_uri: read_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")?,
        container_credentials_full_uri: read_env("AWS_CONTAINER_CREDENTIALS_FULL_URI")?,
        metadata_service_endpoint: first_set_variable(
            &mut read_env,
            &AWS_METADATA_ENDPOINT_VARIABLES,
        )?,
        metadata_service_endpoint_mode: first_set_variable(
            &mut read_env,
            &AWS_METADATA_ENDPOINT_MODE_VARIABLES,
        )?,
        product_opt_in: read_env(AWS_METADATA_OPT_IN_VARIABLE)?,
        // `credential_source` is documented for the shared *config* file
        // (`~/.aws/config`, where role-assumption profiles live); the shared
        // credentials file accepts it too, so it is read as a fallback rather
        // than ignored. Reading only the credentials file would silently
        // suppress an intentional EC2/ECS-backed Bedrock profile.
        profile_credential_source: profile_value(&profile_config, "credential_source")
            .or_else(|| profile_value(&profile_values, "credential_source")),
        profile_metadata_service_endpoint: profile_value(
            &profile_config,
            "ec2_metadata_service_endpoint",
        ),
        instance_identity: read_instance_identity(),
    })
}

/// Resolve the IMDS base URL, honoring the standard endpoint override.
///
/// The override exists so a machine (or a test) that pins IMDS can still be
/// used; it is validated fail-closed because it selects a host the signer will
/// contact. Precedence: `AWS_EC2_METADATA_SERVICE_ENDPOINT`, octet's older
/// `AWS_METADATA_SERVICE_ENDPOINT` alias, then the effective profile's
/// `ec2_metadata_service_endpoint` (the only target a profile can select), then
/// the link-local default.
fn aws_metadata_service_endpoint_with<R>(
    mut read_env: R,
    profile_endpoint: Option<&str>,
) -> anyhow::Result<url::Url>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
{
    if let Some(value) = first_set_variable(&mut read_env, &AWS_METADATA_ENDPOINT_VARIABLES)? {
        return checked_aws_metadata_endpoint(&value);
    }
    if let Some(value) = profile_endpoint {
        return checked_aws_metadata_endpoint(value);
    }
    url::Url::parse(AWS_EC2_METADATA_ENDPOINT).map_err(Into::into)
}

fn checked_aws_metadata_endpoint(value: &str) -> anyhow::Result<url::Url> {
    if value.is_empty() || value.len() > 2048 {
        anyhow::bail!("invalid AWS metadata service endpoint");
    }
    let mut url = url::Url::parse(value)
        .map_err(|_| anyhow::anyhow!("invalid AWS metadata service endpoint"))?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("invalid AWS metadata service endpoint");
    }
    // Plain HTTP is only acceptable where IMDS actually lives: the loopback or
    // the link-local metadata addresses. Anything else must be HTTPS.
    if url.scheme() == "http" && !aws_metadata_host_is_link_local_or_loopback(&url) {
        anyhow::bail!("invalid AWS metadata service endpoint");
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn aws_metadata_host_is_link_local_or_loopback(url: &url::Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => {
            address.is_loopback() || address.octets()[..2] == [169, 254]
        }
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

fn metadata_http_client() -> anyhow::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(AWS_METADATA_TIMEOUT)
        .timeout(AWS_METADATA_TIMEOUT)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(Into::into)
}

fn ecs_metadata_url_with(
    mut read_env: impl FnMut(&str) -> anyhow::Result<Option<String>>,
) -> anyhow::Result<Option<url::Url>> {
    if let Some(relative) = read_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")? {
        if relative.len() > 2048 || !relative.starts_with('/') || relative.contains(['\r', '\n']) {
            anyhow::bail!("invalid AWS_CONTAINER_CREDENTIALS_RELATIVE_URI");
        }
        return url::Url::parse(&format!("http://169.254.170.2{relative}"))
            .map(Some)
            .map_err(Into::into);
    }
    let Some(full) = read_env("AWS_CONTAINER_CREDENTIALS_FULL_URI")? else {
        return Ok(None);
    };
    let url = url::Url::parse(&full)
        .map_err(|_| anyhow::anyhow!("invalid AWS_CONTAINER_CREDENTIALS_FULL_URI"))?;
    let allowed_host = matches!(
        url.host_str(),
        Some("169.254.170.2" | "169.254.170.23" | "localhost")
    );
    if !matches!(url.scheme(), "http" | "https")
        || !allowed_host
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!("invalid AWS_CONTAINER_CREDENTIALS_FULL_URI");
    }
    Ok(Some(url))
}

fn metadata_credentials_from_url(
    url: &url::Url,
    ecs: bool,
) -> anyhow::Result<octet_ai::AwsCredentials> {
    let client = metadata_http_client()?;
    let mut request = client.get(url.clone());
    if ecs {
        if let Some(token) = optional_bounded_env("AWS_CONTAINER_AUTHORIZATION_TOKEN")? {
            request = request.header("Authorization", token);
        }
    }
    let response = request
        .send()
        .map_err(|_| anyhow::anyhow!("AWS metadata credential request failed"))?;
    if !response.status().is_success() {
        anyhow::bail!("AWS metadata credential request failed");
    }
    credentials_from_metadata_body(read_bounded_response(response)?)
}

fn ec2_metadata_credentials(base: &url::Url) -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let client = metadata_http_client()?;
    let token_url = base
        .join("latest/api/token")
        .map_err(|_| anyhow::anyhow!("invalid AWS metadata service endpoint"))?;
    let token_response = match client
        .put(token_url)
        .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
        .send()
    {
        Ok(response) if response.status().is_success() => response,
        Ok(_) | Err(_) => return Ok(None),
    };
    let token = read_bounded_response(token_response)?;
    if token.is_empty() || token.len() > 512 || token.chars().any(char::is_control) {
        return Ok(None);
    }
    let roles_url = base
        .join("latest/meta-data/iam/security-credentials/")
        .map_err(|_| anyhow::anyhow!("invalid AWS metadata service endpoint"))?;
    let role_response = match client
        .get(roles_url)
        .header("X-aws-ec2-metadata-token", &token)
        .send()
    {
        Ok(response) if response.status().is_success() => response,
        Ok(_) | Err(_) => return Ok(None),
    };
    let role = read_bounded_response(role_response)?;
    let role = role.lines().next().unwrap_or("").trim();
    if role.is_empty()
        || role.len() > 128
        || !role.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'-' | b'_' | b'+' | b'=' | b',' | b'.' | b'@')
        })
    {
        return Ok(None);
    }
    let url = base
        .join(&format!("latest/meta-data/iam/security-credentials/{role}"))
        .map_err(|_| anyhow::anyhow!("invalid AWS metadata service endpoint"))?;
    let response = match client
        .get(url)
        .header("X-aws-ec2-metadata-token", token)
        .send()
    {
        Ok(response) if response.status().is_success() => response,
        Ok(_) | Err(_) => return Ok(None),
    };
    credentials_from_metadata_body(read_bounded_response(response)?).map(Some)
}

fn read_bounded_response(response: reqwest::blocking::Response) -> anyhow::Result<String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    response
        .take((MAX_AWS_METADATA_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_AWS_METADATA_BYTES {
        anyhow::bail!("AWS metadata response exceeds the byte limit");
    }
    String::from_utf8(bytes).map_err(Into::into)
}

fn credentials_from_metadata_body(body: String) -> anyhow::Result<octet_ai::AwsCredentials> {
    let value: serde_json::Value = serde_json::from_str(&body)
        .map_err(|_| anyhow::anyhow!("AWS metadata returned invalid credentials"))?;
    let access_key_id = value
        .get("AccessKeyId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= octet_ai::auth::MAX_ENV_VALUE_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| anyhow::anyhow!("AWS metadata returned incomplete credentials"))?
        .to_owned();
    let secret_access_key = value
        .get("SecretAccessKey")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= octet_ai::auth::MAX_ENV_VALUE_BYTES
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(|| anyhow::anyhow!("AWS metadata returned incomplete credentials"))?
        .to_owned();
    let token = value
        .get("Token")
        .and_then(serde_json::Value::as_str)
        .filter(|value| {
            !value.is_empty()
                && value.len() <= octet_ai::auth::MAX_ENV_VALUE_BYTES
                && !value.chars().any(char::is_control)
        })
        .map(str::to_owned);
    aws_credentials(access_key_id, secret_access_key, token)
}

/// Web-identity variable names (AWS IAM Roles for Service Accounts and any
/// other OIDC issuer that writes the standard triple).
const AWS_WEB_IDENTITY_TOKEN_FILE: &str = "AWS_WEB_IDENTITY_TOKEN_FILE";
const AWS_ROLE_ARN: &str = "AWS_ROLE_ARN";
const AWS_ROLE_SESSION_NAME: &str = "AWS_ROLE_SESSION_NAME";
/// Standard AWS SDK endpoint override for STS: a private or fixture STS must be
/// selectable without changing the role/token configuration under test.
const AWS_STS_ENDPOINT_VARIABLE: &str = "AWS_ENDPOINT_URL_STS";
/// Session name used when `AWS_ROLE_SESSION_NAME` is absent.
const DEFAULT_AWS_ROLE_SESSION_NAME: &str = "octet";

/// The already-read web-identity configuration, kept as data so the decisions
/// ("configured?", "well-formed?") are directly unit-testable without a network
/// request or a mutated process environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct AwsWebIdentityInputs {
    token_file: Option<String>,
    role_arn: Option<String>,
    session_name: Option<String>,
    endpoint_override: Option<String>,
}

fn aws_web_identity_inputs_with<R>(mut read_env: R) -> anyhow::Result<AwsWebIdentityInputs>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
{
    Ok(AwsWebIdentityInputs {
        token_file: read_env(AWS_WEB_IDENTITY_TOKEN_FILE)?,
        role_arn: read_env(AWS_ROLE_ARN)?,
        session_name: read_env(AWS_ROLE_SESSION_NAME)?,
        endpoint_override: read_env(AWS_STS_ENDPOINT_VARIABLE)?,
    })
}

/// Resolve web-identity credentials through one STS `AssumeRoleWithWebIdentity`
/// exchange.
///
/// A half-configured web identity (only one of the two required variables)
/// fails closed instead of silently falling through to another identity source:
/// resolving the wrong role would sign requests for an account the operator did
/// not select.
fn aws_web_identity_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    let inputs = aws_web_identity_inputs_with(optional_bounded_env)?;
    let region = aws_bedrock_region()?;
    aws_web_identity_credentials_from(
        &inputs,
        &region,
        read_bounded_web_identity_token,
        sts_assume_role_with_web_identity,
    )
}

fn aws_web_identity_credentials_from<T, F>(
    inputs: &AwsWebIdentityInputs,
    region: &str,
    read_token_file: T,
    fetch: F,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>>
where
    T: FnOnce(&str) -> anyhow::Result<String>,
    F: FnOnce(&url::Url, &[(&'static str, String)]) -> anyhow::Result<String>,
{
    let (Some(token_file), Some(role_arn)) = (&inputs.token_file, &inputs.role_arn) else {
        if inputs.token_file.is_some() || inputs.role_arn.is_some() {
            anyhow::bail!(
                "AWS web identity requires both {AWS_ROLE_ARN} and \
                 {AWS_WEB_IDENTITY_TOKEN_FILE}; only one is set"
            );
        }
        return Ok(None);
    };
    let role_arn = checked_aws_role_arn(role_arn)?;
    let session_name = checked_aws_role_session_name(
        inputs
            .session_name
            .as_deref()
            .unwrap_or(DEFAULT_AWS_ROLE_SESSION_NAME),
    )?;
    let token = read_token_file(token_file)?;
    let endpoint = aws_sts_endpoint(region, inputs.endpoint_override.as_deref())?;
    let form = [
        ("Action", "AssumeRoleWithWebIdentity".to_owned()),
        ("Version", "2011-06-15".to_owned()),
        ("RoleArn", role_arn),
        ("RoleSessionName", session_name),
        ("WebIdentityToken", token),
    ];
    let body = fetch(&endpoint, &form)?;
    sts_credentials_from_xml(&body).map(Some)
}

fn checked_aws_role_arn(arn: &str) -> anyhow::Result<String> {
    if arn.len() > 2048
        || !arn.starts_with("arn:")
        || !arn
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'?' && byte != b'#')
    {
        anyhow::bail!("invalid {AWS_ROLE_ARN}");
    }
    Ok(arn.to_owned())
}

fn checked_aws_role_session_name(name: &str) -> anyhow::Result<String> {
    if !(2..=64).contains(&name.len())
        || name
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
        || !name.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'+' | b'=' | b',' | b'.' | b'@' | b'-' | b'_')
        })
    {
        anyhow::bail!("invalid {AWS_ROLE_SESSION_NAME}");
    }
    Ok(name.to_owned())
}

fn read_bounded_web_identity_token(path: &str) -> anyhow::Result<String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|_| anyhow::anyhow!("{AWS_WEB_IDENTITY_TOKEN_FILE} cannot be read"))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_AWS_WEB_IDENTITY_TOKEN_BYTES {
        anyhow::bail!("{AWS_WEB_IDENTITY_TOKEN_FILE} is not a bounded token file");
    }
    let bytes = std::fs::read(path)
        .map_err(|_| anyhow::anyhow!("{AWS_WEB_IDENTITY_TOKEN_FILE} cannot be read"))?;
    if bytes.len() as u64 > MAX_AWS_WEB_IDENTITY_TOKEN_BYTES {
        anyhow::bail!("{AWS_WEB_IDENTITY_TOKEN_FILE} exceeds the byte limit");
    }
    let token = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("{AWS_WEB_IDENTITY_TOKEN_FILE} is not a UTF-8 token"))?;
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("{AWS_WEB_IDENTITY_TOKEN_FILE} is empty");
    }
    Ok(token.to_owned())
}

/// STS endpoint for a region: the standard AWS SDK override wins, then the
/// regional endpoint. Every candidate is validated fail-closed (`https`, or
/// loopback `http` for a private fixture), like the other AWS endpoints here.
fn aws_sts_endpoint(region: &str, override_url: Option<&str>) -> anyhow::Result<url::Url> {
    if let Some(value) = override_url {
        let url = url::Url::parse(value)
            .map_err(|_| anyhow::anyhow!("invalid {AWS_STS_ENDPOINT_VARIABLE}"))?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || (url.scheme() == "http" && !aws_metadata_host_is_link_local_or_loopback(&url))
        {
            anyhow::bail!("invalid {AWS_STS_ENDPOINT_VARIABLE}");
        }
        return Ok(url);
    }
    let region = checked_aws_region(region.to_owned(), "AWS region")?;
    url::Url::parse(&format!("https://sts.{region}.amazonaws.com/"))
        .map_err(|_| anyhow::anyhow!("invalid AWS region"))
}

fn sts_assume_role_with_web_identity(
    endpoint: &url::Url,
    form: &[(&'static str, String)],
) -> anyhow::Result<String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(AWS_STS_TIMEOUT)
        .timeout(AWS_STS_TIMEOUT)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| anyhow::anyhow!("AWS STS client could not be created"))?;
    let response = client
        .post(endpoint.clone())
        .form(form)
        .send()
        .map_err(|_| anyhow::anyhow!("AWS STS AssumeRoleWithWebIdentity request failed"))?;
    if !response.status().is_success() {
        let status = response.status();
        let code = read_bounded_response(response)
            .ok()
            .and_then(|body| xml_tag(&body, "Code"))
            .filter(|code| code.len() <= 64 && !code.chars().any(char::is_control));
        return Err(match code {
            Some(code) => {
                anyhow::anyhow!("AWS STS AssumeRoleWithWebIdentity failed with {status}: {code}")
            }
            None => anyhow::anyhow!("AWS STS AssumeRoleWithWebIdentity failed with {status}"),
        });
    }
    read_bounded_response(response)
}

/// Extract the three credential fields from the STS XML result.
///
/// Only the documented `AssumeRoleWithWebIdentityResult` fields are read, each
/// bounded and validated, and the session token is required because the
/// resulting credentials are always temporary. The response is never logged.
fn sts_credentials_from_xml(body: &str) -> anyhow::Result<octet_ai::AwsCredentials> {
    let access_key_id = xml_tag(body, "AccessKeyId")
        .ok_or_else(|| anyhow::anyhow!("AWS STS returned incomplete credentials"))?;
    let secret_access_key = xml_tag(body, "SecretAccessKey")
        .ok_or_else(|| anyhow::anyhow!("AWS STS returned incomplete credentials"))?;
    let session_token = xml_tag(body, "SessionToken")
        .ok_or_else(|| anyhow::anyhow!("AWS STS returned incomplete credentials"))?;
    aws_credentials(access_key_id, secret_access_key, Some(session_token))
}

/// Read the first occurrence of `<tag>value</tag>` with the five predefined XML
/// entities decoded. Unknown or malformed content yields `None` (fail closed)
/// rather than an approximation of a credential value.
fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    let raw = &body[start..end];
    if raw.len() > octet_ai::auth::MAX_ENV_VALUE_BYTES {
        return None;
    }
    let mut value = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        value.push_str(&rest[..index]);
        let tail = &rest[index..];
        let (decoded, consumed) = [
            ("&amp;", '&'),
            ("&lt;", '<'),
            ("&gt;", '>'),
            ("&quot;", '"'),
            ("&apos;", '\''),
        ]
        .into_iter()
        .find_map(|(entity, decoded)| {
            tail.starts_with(entity).then_some((decoded, entity.len()))
        })?;
        value.push(decoded);
        rest = &tail[consumed..];
    }
    value.push_str(rest);
    let value = value.trim().to_owned();
    (!value.is_empty() && !value.chars().any(char::is_control)).then_some(value)
}

/// Return a credential-free diagnostic for an unavailable API-key declaration.
#[cfg(test)]
pub(crate) fn missing_environment_diagnostic(
    declaration: &ProviderDeclaration,
) -> ProviderDiagnostic {
    ProviderDiagnostic::missing_environment(&declaration.definition())
}

#[cfg(test)]
mod tests;
