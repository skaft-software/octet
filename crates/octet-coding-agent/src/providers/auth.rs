//! Private provider credential lifecycle.
//!
//! Only this module reads API-key environment values for catalog discovery or
//! turns an environment-variable name into an `octet_ai::Auth`. Public provider
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
}

impl fmt::Debug for EnvironmentCredential {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvironmentCredential")
            .field("variable", &self.variable)
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
        }
    }

    fn variable(&self) -> &'static str {
        self.variable
    }
}

/// Resolve the first configured environment variable declared by a provider.
/// Invalid Unicode is an actionable configuration failure; oversized values are
/// rejected by `octet_ai` before they reach a request header.
pub(crate) fn resolve_environment(
    declaration: &ProviderDeclaration,
) -> anyhow::Result<Option<EnvironmentCredential>> {
    let Some(variables) = declaration.authentication.environment_variables() else {
        return Ok(None);
    };
    for variable in variables {
        let value = match octet_ai::auth::read_bounded_env(variable) {
            Ok(value) => value,
            Err(octet_ai::ConfigError::InvalidEnv(_)) => {
                anyhow::bail!("could not read {variable}: invalid environment value")
            }
            Err(error) => return Err(error.into()),
        };
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            return Ok(Some(EnvironmentCredential { variable, value }));
        }
    }
    Ok(None)
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
mod tests {
    use super::*;
    use crate::providers::contract::{
        ProviderAuthentication, ANTHROPIC, CLOUDFLARE_AI_GATEWAY, GEMINI, OPENAI,
    };

    fn fixture_credentials(label: &str) -> octet_ai::AwsCredentials {
        octet_ai::AwsCredentials::new(format!("{label}-access"), format!("{label}-secret"), None)
            .expect("fixture AWS credentials")
    }

    #[test]
    fn aws_profile_parser_selects_only_the_requested_bounded_section() {
        let contents = "\
[default]
aws_access_key_id = default-access
aws_secret_access_key = default-secret

[profile enterprise]
aws_access_key_id = enterprise-access
aws_secret_access_key = enterprise-secret
aws_session_token = enterprise-token
ignored key = ignored
";
        let values = parse_aws_ini_section(contents, "profile enterprise").unwrap();
        assert_eq!(
            values.get("aws_access_key_id").map(String::as_str),
            Some("enterprise-access")
        );
        assert_eq!(
            values.get("aws_secret_access_key").map(String::as_str),
            Some("enterprise-secret")
        );
        assert_eq!(
            values.get("aws_session_token").map(String::as_str),
            Some("enterprise-token")
        );
        assert!(!values.contains_key("ignored key"));
    }

    #[test]
    fn aws_chain_precedence_is_ordered_without_metadata_fallback() {
        // The documented chain order: environment keys, web identity, the
        // selected profile, then the indicated metadata sources. Every source
        // after the one that resolves must stay untouched.
        let web_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let web_flag = web_called.clone();
        let profile_flag = profile_called.clone();
        let metadata_flag = metadata_called.clone();
        let selected = resolve_aws_credentials_with(
            || Ok(Some(fixture_credentials("environment"))),
            move || {
                web_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("web-identity")))
            },
            move || {
                profile_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("profile")))
            },
            move || {
                metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("metadata")))
            },
        )
        .unwrap();
        assert!(selected.is_some());
        assert!(!web_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!profile_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

        let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let profile_flag = profile_called.clone();
        let metadata_flag = metadata_called.clone();
        let selected = resolve_aws_credentials_with(
            || Ok(None),
            || Ok(Some(fixture_credentials("web-identity"))),
            move || {
                profile_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("profile")))
            },
            move || {
                metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("metadata")))
            },
        )
        .unwrap();
        assert!(selected.is_some());
        assert!(!profile_called.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

        let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let metadata_flag = metadata_called.clone();
        let selected = resolve_aws_credentials_with(
            || Ok(None),
            || Ok(None),
            || Ok(Some(fixture_credentials("profile"))),
            move || {
                metadata_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("metadata")))
            },
        )
        .unwrap();
        assert!(selected.is_some());
        assert!(!metadata_called.load(std::sync::atomic::Ordering::SeqCst));

        let selected = resolve_aws_credentials_with(
            || Ok(None),
            || Ok(None),
            || Ok(None),
            || Ok(Some(fixture_credentials("metadata"))),
        )
        .unwrap();
        assert!(selected.is_some());

        // A failing earlier source fails the chain: a later source must not be
        // silently substituted for an environment/role the operator selected.
        let web_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let web_flag = web_called.clone();
        assert!(resolve_aws_credentials_with(
            || Err(anyhow::anyhow!("environment source failed")),
            move || {
                web_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("web-identity")))
            },
            || Ok(None),
            || Ok(None),
        )
        .is_err());
        assert!(!web_called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn partial_environment_credentials_are_rejected_without_process_fallback() {
        let error = aws_environment_credentials_with(|name| {
            Ok(match name {
                "AWS_ACCESS_KEY_ID" => Some("environment-access".to_owned()),
                _ => None,
            })
        })
        .unwrap_err();
        assert!(error.to_string().contains("configured together"));
    }

    #[test]
    fn profile_credential_process_is_not_executed_or_selected() {
        let values = parse_aws_ini_section(
            "[default]\ncredential_process = touch /tmp/must-not-run\n",
            "default",
        )
        .unwrap();
        assert!(values.contains_key("credential_process"));
        assert!(aws_profile_credentials_from_values(&values)
            .unwrap()
            .is_none());

        let values = parse_aws_ini_section(
            "[default]\naws_access_key_id = profile-access\naws_secret_access_key = profile-secret\ncredential_process = touch /tmp/must-not-run\n",
            "default",
        )
        .unwrap();
        assert!(aws_profile_credentials_from_values(&values)
            .unwrap()
            .is_some());
    }

    #[test]
    fn container_metadata_urls_are_allowlisted_and_proxy_independent() {
        let relative = ecs_metadata_url_with(|name| {
            Ok(match name {
                "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI" => Some("/v2/credentials".to_owned()),
                "AWS_CONTAINER_CREDENTIALS_FULL_URI" => {
                    Some("http://example.invalid/should-not-win".to_owned())
                }
                _ => None,
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(relative.host_str(), Some("169.254.170.2"));
        assert_eq!(relative.path(), "/v2/credentials");

        let local = ecs_metadata_url_with(|name| {
            Ok(if name == "AWS_CONTAINER_CREDENTIALS_FULL_URI" {
                Some("http://localhost:1234/credentials".to_owned())
            } else {
                None
            })
        })
        .unwrap()
        .unwrap();
        assert_eq!(local.host_str(), Some("localhost"));
        assert_eq!(local.port(), Some(1234));

        for value in [
            "http://example.invalid/credentials",
            "http://user@localhost/credentials",
            "http://localhost/credentials#fragment",
            "http://[::1]/credentials",
        ] {
            let result = ecs_metadata_url_with(|name| {
                Ok(if name == "AWS_CONTAINER_CREDENTIALS_FULL_URI" {
                    Some(value.to_owned())
                } else {
                    None
                })
            });
            assert!(result.is_err(), "metadata URL must be rejected: {value}");
        }
    }

    #[tokio::test]
    async fn aws_bedrock_signer_resolves_current_credentials_for_each_request() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls_for_resolver = calls.clone();
        let resolver: AwsCredentialsResolver = std::sync::Arc::new(move || {
            let call = calls_for_resolver.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(fixture_credentials(&format!("refresh-{call}"))))
        });
        let signer = AwsBedrockSigner {
            region: "us-west-2".to_owned(),
            resolver,
        };
        let request = octet_ai::SigningRequest::new(
            http::Method::POST,
            url::Url::parse(
                "https://bedrock-runtime.us-west-2.amazonaws.com/model/example/converse-stream",
            )
            .unwrap(),
            bytes::Bytes::from_static(b"{}"),
            http::HeaderMap::new(),
        );
        octet_ai::RequestSigner::sign(&signer, &request)
            .await
            .unwrap();
        octet_ai::RequestSigner::sign(&signer, &request)
            .await
            .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn aws_metadata_credentials_are_bounded_and_require_both_key_components() {
        assert!(credentials_from_metadata_body(
            r#"{"AccessKeyId":"metadata-access","SecretAccessKey":"metadata-secret","Token":"metadata-token"}"#
                .to_owned(),
        )
        .is_ok());
        assert!(
            credentials_from_metadata_body(r#"{"AccessKeyId":"metadata-access"}"#.to_owned(),)
                .is_err()
        );
        let oversized = "x".repeat(octet_ai::auth::MAX_ENV_VALUE_BYTES + 1);
        assert!(credentials_from_metadata_body(format!(
            r#"{{"AccessKeyId":"{oversized}","SecretAccessKey":"metadata-secret"}}"#
        ))
        .is_err());
        assert!(credentials_from_metadata_body(
            r#"{"AccessKeyId":"metadata\naccess","SecretAccessKey":"metadata-secret"}"#.to_owned(),
        )
        .is_err());
    }

    #[test]
    fn aws_region_validation_rejects_untrusted_endpoint_components() {
        assert_eq!(
            checked_aws_region("eu-west-1".to_owned(), "test").unwrap(),
            "eu-west-1"
        );
        assert!(checked_aws_region("eu/west-1".to_owned(), "test").is_err());
    }

    #[test]
    fn diagnostics_never_format_a_resolved_value() {
        let credential = EnvironmentCredential {
            variable: "TEST_PROVIDER_KEY",
            value: "secret-value-must-not-appear".to_owned(),
        };
        assert!(!format!("{credential:?}").contains(&credential.value));
        assert!(missing_environment_diagnostic(&OPENAI)
            .action()
            .contains("OPENAI_API_KEY"));
    }

    #[test]
    fn generated_presentation_selects_auth_header_without_a_secret() {
        let credential = EnvironmentCredential {
            variable: "TEST_PROVIDER_KEY",
            value: "not-formatted".to_owned(),
        };
        let auth = environment_auth(&ANTHROPIC.routes[0], &credential).unwrap();
        assert!(matches!(auth, Auth::HeaderEnv { .. }));
        let gateway = environment_auth(&CLOUDFLARE_AI_GATEWAY.routes[0], &credential).unwrap();
        assert!(matches!(
            gateway,
            Auth::HeaderBearerEnv { ref name, .. }
                if name == http::HeaderName::from_static("cf-aig-authorization")
        ));
    }

    #[test]
    fn discovery_headers_are_sensitive_and_route_selected() {
        let credential = EnvironmentCredential {
            variable: "TEST_PROVIDER_KEY",
            value: "not-formatted".to_owned(),
        };
        let bearer = environment_discovery_headers(&OPENAI.routes[0], &credential).unwrap();
        assert!(bearer[http::header::AUTHORIZATION].is_sensitive());

        let api_key = environment_discovery_headers(&ANTHROPIC.routes[0], &credential).unwrap();
        assert!(api_key[http::HeaderName::from_static("x-api-key")].is_sensitive());

        let gateway =
            environment_discovery_headers(&CLOUDFLARE_AI_GATEWAY.routes[0], &credential).unwrap();
        let gateway_header = &gateway[http::HeaderName::from_static("cf-aig-authorization")];
        assert_eq!(gateway_header.to_str().unwrap(), "Bearer not-formatted");
        assert!(gateway_header.is_sensitive());

        let google = environment_discovery_headers(&GEMINI.routes[0], &credential).unwrap();
        assert!(google[http::HeaderName::from_static("x-goog-api-key")].is_sensitive());
    }

    #[test]
    fn bearer_token_aliases_override_the_route_api_key_header() {
        // An Anthropic OAuth/subscription alias must be sent as
        // `Authorization: Bearer` even though the route's default presentation
        // is the `x-api-key` header. This keys on the credential variable.
        let auth_token = EnvironmentCredential::for_test("ANTHROPIC_AUTH_TOKEN", "token-value");
        assert!(matches!(
            environment_auth(&ANTHROPIC.routes[0], &auth_token).unwrap(),
            Auth::BearerEnv { .. }
        ));
        let headers = environment_discovery_headers(&ANTHROPIC.routes[0], &auth_token).unwrap();
        let authorization = &headers[http::header::AUTHORIZATION];
        assert_eq!(authorization.to_str().unwrap(), "Bearer token-value");
        assert!(authorization.is_sensitive());
        assert!(headers
            .get(http::HeaderName::from_static("x-api-key"))
            .is_none());

        let api_key = EnvironmentCredential::for_test("ANTHROPIC_API_KEY", "key-value");
        assert!(matches!(
            environment_auth(&ANTHROPIC.routes[0], &api_key).unwrap(),
            Auth::HeaderEnv { .. }
        ));
        let headers = environment_discovery_headers(&ANTHROPIC.routes[0], &api_key).unwrap();
        assert_eq!(
            headers[http::HeaderName::from_static("x-api-key")],
            "key-value"
        );
    }

    // --- AWS metadata activation (startup latency) -------------------------
    //
    // These are the deterministic acceptance tests for the activation rule. They
    // assert *request counts*, never elapsed milliseconds: the defect was "an
    // unrelated-provider launch issues a metadata request at all", and a timing
    // threshold would pass or fail with machine load.

    /// The ordinary case: a laptop with no AWS environment, no AWS profile, and
    /// no EC2 DMI markers.
    fn laptop_inputs() -> AwsMetadataActivationInputs {
        aws_metadata_activation_inputs_with(|_| Ok(None), |_| Ok(None), || None).unwrap()
    }

    /// A loopback metadata fixture. Returns the base URL and a request counter.
    ///
    /// The server answers exactly `routes.len()` requests and then exits, so a
    /// test that asserts the counter also asserts how many requests were made.
    fn metadata_fixture(
        routes: Vec<(&'static str, &'static str, &'static str)>,
    ) -> (url::Url, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = requests.clone();
        let expected = routes.len();
        std::thread::spawn(move || {
            for _ in 0..expected {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut byte = [0_u8; 1];
                while !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => request.push(byte[0]),
                    }
                }
                let line = String::from_utf8_lossy(&request);
                let path = line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_owned();
                let (status, body) = routes
                    .iter()
                    .find(|(route, _, _)| *route == path)
                    .map(|(_, status, body)| (*status, *body))
                    .unwrap_or(("404 Not Found", ""));
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        });
        (
            url::Url::parse(&format!("http://{address}/")).unwrap(),
            requests,
        )
    }

    #[test]
    fn unrelated_provider_launch_makes_zero_aws_metadata_requests() {
        // Headline acceptance criterion. The activation rule is evaluated from
        // empty environment/profile reads, exactly like a Codex user's laptop,
        // and the stubbed metadata sources count every request they receive.
        let activation = aws_metadata_activation_from(&laptop_inputs());
        assert_eq!(
            activation,
            AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
        );

        let container_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ec2_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let container_counter = container_requests.clone();
        let ec2_counter = ec2_requests.clone();
        let credentials = aws_metadata_credentials_with(
            activation,
            None,
            |_| Ok(None),
            move |_, _| {
                container_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(fixture_credentials("container"))
            },
            move |_| {
                ec2_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("ec2")))
            },
        )
        .unwrap();

        assert!(credentials.is_none());
        assert_eq!(
            container_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no container metadata request may be issued for an unrelated provider"
        );
        assert_eq!(
            ec2_requests.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "no EC2 metadata request may be issued for an unrelated provider"
        );
    }

    #[test]
    fn disabled_activation_opens_no_connection_to_a_live_metadata_endpoint() {
        // The measured defect, asserted against a *live* loopback endpoint rather
        // than a stub closure: an unrelated-provider launch must not put a single
        // packet on the wire. The fixture is a blocking accept (so the control
        // connection is observed without racing a non-blocking accept against the
        // TCP handshake), the disabled call runs synchronously, and only then is
        // the listener drained: every connection the probe would have queued is
        // counted, and the assertion is a request count, not a timing threshold.
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = url::Url::parse(&format!("http://{address}/")).unwrap();
        let control_seen = std::sync::Arc::new(AtomicUsize::new(0));
        let observer = control_seen.clone();
        let (drain, drain_ready) = std::sync::mpsc::channel::<()>();
        let fixture = std::thread::spawn(move || {
            let _control = listener
                .accept()
                .expect("the loopback metadata fixture must be reachable");
            observer.fetch_add(1, AtomicOrdering::SeqCst);
            // Hold the listener open until the disabled call has returned, then
            // count every connection still queued behind the control connection.
            let _ = drain_ready.recv_timeout(std::time::Duration::from_secs(30));
            listener.set_nonblocking(true).unwrap();
            let mut drained = 0_usize;
            while listener.accept().is_ok() {
                drained += 1;
            }
            drained
        });
        let control_connection = std::net::TcpStream::connect(address)
            .expect("the loopback metadata fixture must be reachable");
        // Wait for the fixture's accept to observe the control connection: this
        // proves reachability before the assertion, without a timing threshold on
        // the behavior under test.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while control_seen.load(AtomicOrdering::SeqCst) == 0 {
            assert!(
                std::time::Instant::now() < deadline,
                "the control connection must be observed before the assertion runs"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(control_seen.load(AtomicOrdering::SeqCst), 1);

        let activation = aws_metadata_activation_from(&laptop_inputs());
        assert_eq!(
            activation,
            AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
        );
        let endpoint = base.to_string();
        let credentials = aws_metadata_credentials_with(
            activation,
            None,
            move |name| Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone())),
            metadata_credentials_from_url,
            ec2_metadata_credentials,
        )
        .unwrap();
        assert!(credentials.is_none());

        drop(control_connection);
        drain.send(()).unwrap();
        let after = fixture.join().unwrap();
        assert_eq!(
            after, 0,
            "a disabled activation must not connect to the metadata endpoint at all"
        );
    }

    #[tokio::test]
    async fn opt_in_activation_resolves_and_signs_with_live_metadata_credentials() {
        // The intentional path, end to end over real HTTP against the fixture:
        // token, role list, role credentials, then a Bedrock signature computed
        // from exactly those credentials.
        let (base, requests) = metadata_fixture(vec![
            ("/latest/api/token", "200 OK", "metadata-token"),
            (
                "/latest/meta-data/iam/security-credentials/",
                "200 OK",
                "fixture-role\n",
            ),
            (
                "/latest/meta-data/iam/security-credentials/fixture-role",
                "200 OK",
                r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
            ),
        ]);
        let mut inputs = laptop_inputs();
        inputs.profile_credential_source = Some("Ec2InstanceMetadata".to_owned());
        let activation = aws_metadata_activation_from(&inputs);
        assert_eq!(
            activation,
            AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource)
        );

        let endpoint = base.to_string();
        // Resolve on a blocking thread, exactly like the production signer
        // (`AwsBedrockSigner::sign` wraps its resolver in `spawn_blocking`): the
        // metadata client is a blocking client, and dropping its internal
        // runtime from inside the async test context panics.
        let credentials = tokio::task::spawn_blocking(move || {
            resolve_aws_credentials_with(
                || Ok(None),
                || Ok(None),
                || Ok(None),
                move || {
                    aws_metadata_credentials_with(
                        activation,
                        None,
                        |name| {
                            Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone()))
                        },
                        metadata_credentials_from_url,
                        ec2_metadata_credentials,
                    )
                },
            )
        })
        .await
        .expect("the blocking credential resolver must not panic")
        .unwrap()
        .expect("an indicated machine must still resolve instance credentials");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);

        let resolves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let resolves_for_signer = resolves.clone();
        let resolver: AwsCredentialsResolver = std::sync::Arc::new(move || {
            resolves_for_signer.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Some(credentials.clone()))
        });
        let signer = AwsBedrockSigner {
            region: "us-east-1".to_owned(),
            resolver,
        };
        let request = octet_ai::SigningRequest::new(
            http::Method::POST,
            url::Url::parse(
                "https://bedrock-runtime.us-east-1.amazonaws.com/model/example/converse",
            )
            .unwrap(),
            bytes::Bytes::from_static(b"{}"),
            http::HeaderMap::new(),
        );
        octet_ai::RequestSigner::sign(&signer, &request)
            .await
            .expect("metadata credentials must sign a Bedrock request");
        assert_eq!(resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn static_keys_and_a_plain_profile_never_activate_the_metadata_probe() {
        // Static keys and a profile that declares no metadata credential source
        // are the ordinary laptop launch: the rule stays closed and the chain
        // never reaches the metadata source.
        let inputs = aws_metadata_activation_inputs_with(
            |name| {
                Ok(match name {
                    "AWS_ACCESS_KEY_ID" => Some("AKIAEXAMPLE".to_owned()),
                    "AWS_SECRET_ACCESS_KEY" => Some("fixture-secret".to_owned()),
                    _ => None,
                })
            },
            |_| {
                Ok(Some(std::collections::BTreeMap::from([(
                    "region".to_owned(),
                    "us-east-1".to_owned(),
                )])))
            },
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
        );

        let metadata_calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = metadata_calls.clone();
        let credentials = resolve_aws_credentials_with(
            || Ok(Some(fixture_credentials("environment"))),
            || panic!("the web-identity source must not run once environment keys resolved"),
            || panic!("the profile source must not run once environment keys resolved"),
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("metadata")))
            },
        )
        .unwrap();
        assert!(credentials.is_some());
        assert_eq!(metadata_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn activation_rule_is_a_pure_function_of_the_local_inputs() {
        let rule = aws_metadata_activation_from;
        let enabled = AwsMetadataActivation::Enabled;
        let disabled = AwsMetadataActivation::Disabled;
        use AwsMetadataIndication as Indication;
        use AwsMetadataSuppression as Suppression;

        assert_eq!(
            rule(&laptop_inputs()),
            disabled(Suppression::Unindicated),
            "no local indication must stay closed"
        );

        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("true".to_owned());
        assert_eq!(
            rule(&inputs),
            disabled(Suppression::ExplicitlyDisabled),
            "the standard AWS disable switch is respected"
        );

        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("TRUE".to_owned());
        assert_eq!(rule(&inputs), disabled(Suppression::ExplicitlyDisabled));

        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("maybe".to_owned());
        assert_eq!(
            rule(&inputs),
            disabled(Suppression::UnrecognizedDisableSetting),
            "unknown state stays closed instead of probing"
        );

        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("false".to_owned());
        assert_eq!(rule(&inputs), enabled(Indication::ExplicitlyNotDisabled));

        for value in ["1", "true", "TRUE", "yes", "on"] {
            let mut inputs = laptop_inputs();
            inputs.product_opt_in = Some(value.to_owned());
            assert_eq!(
                rule(&inputs),
                enabled(Indication::ProductOptIn),
                "opt-in value {value} must enable the probe"
            );
        }

        for value in ["0", "false", "no", "off"] {
            let mut inputs = laptop_inputs();
            inputs.product_opt_in = Some(value.to_owned());
            assert_eq!(
                rule(&inputs),
                disabled(Suppression::ExplicitlyDisabled),
                "opt-in value {value} must keep the probe closed"
            );
        }

        for (relative, full) in [
            (Some("/v2/credentials".to_owned()), None),
            (None, Some("http://localhost:1234/credentials".to_owned())),
        ] {
            let mut inputs = laptop_inputs();
            inputs.container_credentials_relative_uri = relative;
            inputs.container_credentials_full_uri = full;
            assert_eq!(rule(&inputs), enabled(Indication::ContainerCredentialsUri));
        }

        let mut inputs = laptop_inputs();
        inputs.metadata_service_endpoint = Some("http://169.254.169.254/".to_owned());
        assert_eq!(rule(&inputs), enabled(Indication::MetadataServiceEndpoint));

        let mut inputs = laptop_inputs();
        inputs.metadata_service_endpoint_mode = Some("IPv6".to_owned());
        assert_eq!(rule(&inputs), enabled(Indication::MetadataServiceEndpoint));

        let mut inputs = laptop_inputs();
        inputs.profile_credential_source = Some("Ec2InstanceMetadata".to_owned());
        assert_eq!(rule(&inputs), enabled(Indication::ProfileCredentialSource));

        let mut inputs = laptop_inputs();
        inputs.profile_credential_source = Some("Environment".to_owned());
        assert_eq!(
            rule(&inputs),
            disabled(Suppression::Unindicated),
            "a profile that does not declare metadata is not an indication"
        );

        // A bare EC2 instance profile: no environment marker and no profile
        // statement, only the local DMI vendor. It must still resolve, and it
        // must be re-checked here rather than trusted from the reader.
        let mut inputs = laptop_inputs();
        inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
        assert_eq!(rule(&inputs), enabled(Indication::Ec2InstanceIdentity));

        let mut inputs = laptop_inputs();
        inputs.instance_identity = Some(" VMware, Inc. ".to_owned());
        assert_eq!(
            rule(&inputs),
            disabled(Suppression::Unindicated),
            "another hypervisor's DMI vendor is not an indication"
        );

        let mut inputs = laptop_inputs();
        inputs.instance_identity = Some(String::new());
        assert_eq!(rule(&inputs), disabled(Suppression::Unindicated));

        // An explicit off wins over every enabling input.
        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("true".to_owned());
        inputs.container_credentials_relative_uri = Some("/v2/credentials".to_owned());
        inputs.product_opt_in = Some("true".to_owned());
        inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
        assert_eq!(rule(&inputs), disabled(Suppression::ExplicitlyDisabled));
    }

    #[test]
    fn activation_reads_the_documented_opt_in_variable_and_profile_source() {
        let inputs = aws_metadata_activation_inputs_with(
            |name| {
                Ok(match name {
                    AWS_METADATA_OPT_IN_VARIABLE => Some("1".to_owned()),
                    _ => None,
                })
            },
            |_| Ok(None),
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::ProductOptIn)
        );

        let profile = std::collections::BTreeMap::from([(
            "credential_source".to_owned(),
            "EcsContainer".to_owned(),
        )]);
        let inputs = aws_metadata_activation_inputs_with(
            |_| Ok(None),
            move |config_file| {
                Ok(if config_file {
                    None
                } else {
                    Some(profile.clone())
                })
            },
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource)
        );
    }

    #[test]
    fn profile_credential_source_is_read_from_the_config_file_it_is_documented_in() {
        // `credential_source` is documented for `~/.aws/config` (where
        // role-assumption profiles are declared), so a profile that only exists
        // there must still activate: reading the credentials file alone silently
        // suppressed an intentional EC2/ECS-backed Bedrock profile. The
        // credentials file stays a tolerated fallback for the profiles that
        // declare it there.
        use std::collections::BTreeMap;

        let in_config = |values: BTreeMap<String, String>| {
            move |config_file: bool| {
                Ok(if config_file {
                    Some(values.clone())
                } else {
                    None
                })
            }
        };

        for source in ["Ec2InstanceMetadata", "EcsContainer", "ec2instancemetadata"] {
            let inputs = aws_metadata_activation_inputs_with(
                |_| Ok(None),
                in_config(BTreeMap::from([(
                    "credential_source".to_owned(),
                    source.to_owned(),
                )])),
                || None,
            )
            .unwrap();
            assert_eq!(
                aws_metadata_activation_from(&inputs),
                AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource),
                "credential_source = {source} in ~/.aws/config must indicate the metadata probe"
            );
        }

        let inputs = aws_metadata_activation_inputs_with(
            |_| Ok(None),
            in_config(BTreeMap::from([(
                "credential_source".to_owned(),
                "Environment".to_owned(),
            )])),
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated),
            "a config profile that declares another credential source is not an indication"
        );
    }

    #[test]
    fn indicated_metadata_probe_reaches_the_ec2_source() {
        // The intentional path: an EC2 user who opts in still resolves, and the
        // probe runs against the default IMDS endpoint.
        let mut inputs = laptop_inputs();
        inputs.product_opt_in = Some("true".to_owned());
        let activation = aws_metadata_activation_from(&inputs);
        let ec2_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_base = std::sync::Arc::new(std::sync::Mutex::new(None));
        let ec2_counter = ec2_requests.clone();
        let base_slot = seen_base.clone();
        let credentials = aws_metadata_credentials_with(
            activation,
            None,
            |_| Ok(None),
            |_, _| panic!("the container source must not run without a container URI"),
            move |base| {
                ec2_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                *base_slot.lock().unwrap() = Some(base.to_string());
                Ok(Some(fixture_credentials("ec2")))
            },
        )
        .unwrap();

        assert!(credentials.is_some(), "the opt-in must still resolve");
        assert_eq!(ec2_requests.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            seen_base.lock().unwrap().as_deref(),
            Some(AWS_EC2_METADATA_ENDPOINT)
        );
    }

    #[test]
    fn indicated_metadata_probe_prefers_the_container_uri() {
        let mut inputs = laptop_inputs();
        inputs.container_credentials_relative_uri = Some("/v2/credentials".to_owned());
        let activation = aws_metadata_activation_from(&inputs);
        let container_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen_url = std::sync::Arc::new(std::sync::Mutex::new(None));
        let container_counter = container_requests.clone();
        let url_slot = seen_url.clone();
        let credentials = aws_metadata_credentials_with(
            activation,
            None,
            |name| {
                Ok((name == "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
                    .then(|| "/v2/credentials".to_owned()))
            },
            move |url, ecs| {
                assert!(ecs, "container credentials are fetched with the ECS flag");
                container_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                *url_slot.lock().unwrap() = Some(url.to_string());
                Ok(fixture_credentials("container"))
            },
            |_| panic!("the EC2 source must not run when a container URI is configured"),
        )
        .unwrap();

        assert!(credentials.is_some());
        assert_eq!(
            container_requests.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            seen_url.lock().unwrap().as_deref(),
            Some("http://169.254.170.2/v2/credentials")
        );
    }

    #[test]
    fn ec2_metadata_flow_is_bounded_to_the_documented_requests() {
        // Functional evidence that the intentional path still works end to end:
        // token, role list, role credentials. The counter pins the request count.
        let (base, requests) = metadata_fixture(vec![
            ("/latest/api/token", "200 OK", "metadata-token"),
            (
                "/latest/meta-data/iam/security-credentials/",
                "200 OK",
                "fixture-role\n",
            ),
            (
                "/latest/meta-data/iam/security-credentials/fixture-role",
                "200 OK",
                r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
            ),
        ]);
        let credentials = ec2_metadata_credentials(&base).unwrap();
        assert!(credentials.is_some(), "a reachable IMDS must still resolve");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn an_unavailable_metadata_endpoint_costs_one_bounded_request() {
        // The probe is entered only when indicated, and when the endpoint is
        // blocked it fails fast and closed: exactly one request, no retry, and a
        // `None` that lets the unrelated provider be skipped.
        let (base, requests) = metadata_fixture(vec![("/latest/api/token", "404 Not Found", "")]);
        let credentials = ec2_metadata_credentials(&base).unwrap();
        assert!(
            credentials.is_none(),
            "an unavailable IMDS resolves to no credentials"
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn aws_metadata_service_endpoint_override_is_validated_fail_closed() {
        let default = aws_metadata_service_endpoint_with(|_| Ok(None), None).unwrap();
        assert_eq!(default.as_str(), AWS_EC2_METADATA_ENDPOINT);

        for accepted in [
            "http://127.0.0.1:8080",
            "http://localhost:8080/imds",
            "http://[::1]:8080",
            "https://metadata.internal.example",
        ] {
            let endpoint =
                aws_metadata_service_endpoint_with(|_| Ok(Some(accepted.to_owned())), None)
                    .unwrap_or_else(|error| panic!("{accepted} must be accepted: {error}"));
            assert!(endpoint.as_str().ends_with('/'), "{endpoint}");
        }

        for rejected in [
            "",
            "http://metadata.example.com",
            "http://user@169.254.169.254/",
            "http://169.254.169.254/?query=1",
            "http://169.254.169.254/#fragment",
            "ftp://169.254.169.254/",
        ] {
            assert!(
                aws_metadata_service_endpoint_with(|_| Ok(Some(rejected.to_owned())), None)
                    .is_err(),
                "metadata endpoint override must be rejected: {rejected}"
            );
            assert!(
                aws_metadata_service_endpoint_with(|_| Ok(None), Some(rejected)).is_err(),
                "a profile endpoint must be validated exactly like the environment override: {rejected}"
            );
        }
    }

    #[test]
    fn metadata_endpoint_precedence_is_standard_name_then_alias_then_profile() {
        // The AWS-standard name wins, the older alias still works, and a profile
        // that pins IMDS actually selects that endpoint (an indication that is
        // not also the target would leave the host unprobed). Every accepted
        // endpoint is normalized with a trailing slash so that the IMDS path
        // segments append instead of replacing the last component.
        let standard = aws_metadata_service_endpoint_with(
            |name| {
                Ok(match name {
                    "AWS_EC2_METADATA_SERVICE_ENDPOINT" => {
                        Some("http://127.0.0.1:1/standard".to_owned())
                    }
                    "AWS_METADATA_SERVICE_ENDPOINT" => Some("http://127.0.0.1:2/alias".to_owned()),
                    _ => None,
                })
            },
            Some("http://127.0.0.1:3/profile"),
        )
        .unwrap();
        assert_eq!(standard.as_str(), "http://127.0.0.1:1/standard/");

        let alias = aws_metadata_service_endpoint_with(
            |name| {
                Ok((name == "AWS_METADATA_SERVICE_ENDPOINT")
                    .then(|| "http://127.0.0.1:2/alias".to_owned()))
            },
            Some("http://127.0.0.1:3/profile"),
        )
        .unwrap();
        assert_eq!(alias.as_str(), "http://127.0.0.1:2/alias/");

        let profile =
            aws_metadata_service_endpoint_with(|_| Ok(None), Some("http://127.0.0.1:3/profile"))
                .unwrap();
        assert_eq!(profile.as_str(), "http://127.0.0.1:3/profile/");
    }

    #[test]
    fn the_profile_endpoint_activates_and_is_the_probe_target() {
        // End to end from the rule to the network layer: a profile that pins
        // IMDS activates the probe, and the live request lands on that pinned
        // endpoint instead of the link-local default.
        let (base, requests) = metadata_fixture(vec![
            ("/latest/api/token", "200 OK", "metadata-token"),
            (
                "/latest/meta-data/iam/security-credentials/",
                "200 OK",
                "fixture-role\n",
            ),
            (
                "/latest/meta-data/iam/security-credentials/fixture-role",
                "200 OK",
                r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
            ),
        ]);
        let inputs = aws_metadata_activation_inputs_with(
            |_| Ok(None),
            |config_file| {
                Ok(config_file.then(|| {
                    std::collections::BTreeMap::from([(
                        "ec2_metadata_service_endpoint".to_owned(),
                        base.to_string(),
                    )])
                }))
            },
            || None,
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint)
        );

        let credentials = aws_metadata_credentials_with(
            aws_metadata_activation_from(&inputs),
            inputs.profile_metadata_service_endpoint.clone(),
            |_| Ok(None),
            metadata_credentials_from_url,
            ec2_metadata_credentials,
        )
        .unwrap();
        assert!(
            credentials.is_some(),
            "the profile-pinned endpoint must be probed and must resolve"
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn activation_reads_the_standard_endpoint_variable_names() {
        for variable in [
            "AWS_EC2_METADATA_SERVICE_ENDPOINT",
            "AWS_METADATA_SERVICE_ENDPOINT",
            "AWS_EC2_METADATA_SERVICE_ENDPOINT_MODE",
            "AWS_METADATA_SERVICE_ENDPOINT_MODE",
        ] {
            let name = variable.to_owned();
            let inputs = aws_metadata_activation_inputs_with(
                move |probe| Ok((probe == name).then(|| "IPv6".to_owned())),
                |_| Ok(None),
                || None,
            )
            .unwrap();
            assert_eq!(
                aws_metadata_activation_from(&inputs),
                AwsMetadataActivation::Enabled(AwsMetadataIndication::MetadataServiceEndpoint),
                "{variable} must be an indication"
            );
        }
    }

    #[test]
    fn dmi_markers_identify_an_ec2_instance_without_touching_the_network() {
        // The local statement that keeps a bare EC2 instance-profile host
        // working. Only a marker that names Amazon EC2 counts; missing, foreign,
        // oversized, non-UTF-8, and non-file markers all fail closed.
        let dir = tempfile::tempdir().unwrap();
        let markers = dir.path().join("sys/class/dmi/id");
        std::fs::create_dir_all(&markers).unwrap();
        assert_eq!(
            aws_instance_identity_from(dir.path()),
            None,
            "an empty fixture tree is not an EC2 instance"
        );

        std::fs::write(markers.join("sys_vendor"), "VMware, Inc.\n").unwrap();
        assert_eq!(aws_instance_identity_from(dir.path()), None);

        std::fs::write(markers.join("product_name"), "Amazon EC2\n").unwrap();
        assert_eq!(
            aws_instance_identity_from(dir.path()).as_deref(),
            Some(AWS_EC2_DMI_VENDOR),
            "any DMI marker naming Amazon EC2 is enough"
        );

        let oversized = markers.join("bios_vendor");
        std::fs::write(&oversized, "x".repeat(MAX_AWS_DMI_BYTES as usize + 1)).unwrap();
        std::fs::remove_file(markers.join("sys_vendor")).unwrap();
        std::fs::remove_file(markers.join("product_name")).unwrap();
        assert_eq!(
            aws_instance_identity_from(dir.path()),
            None,
            "an oversized marker is not evidence of an instance"
        );

        std::fs::remove_file(&oversized).unwrap();
        std::fs::create_dir(&oversized).unwrap();
        assert_eq!(
            aws_instance_identity_from(dir.path()),
            None,
            "a directory is not a marker file"
        );
    }

    #[test]
    fn dmi_reads_are_bounded_by_content_not_file_size() {
        assert_eq!(
            read_dmi_marker_from(&b"Amazon EC2\n"[..]).as_deref(),
            Some(AWS_EC2_DMI_VENDOR)
        );
        assert_eq!(read_dmi_marker_from(&b"\xff"[..]), None);
        assert_eq!(read_dmi_marker_from(&b" \n"[..]), None);

        // Even a source with no EOF consumes only the bound plus one byte.
        let mut infinite = std::io::repeat(b'x');
        assert_eq!(read_dmi_marker_from(&mut infinite), None);
        let data = vec![b'x'; MAX_AWS_DMI_BYTES as usize + 16];
        let mut reader = std::io::Cursor::new(data);
        assert_eq!(read_dmi_marker_from(&mut reader), None);
        assert_eq!(reader.position(), MAX_AWS_DMI_BYTES + 1);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sysfs_dmi_page_sized_metadata_does_not_hide_a_short_marker() {
        // A real sysfs attribute has page-sized metadata, unlike a tempfile.
        // DMI is optional (containers/ARM may not expose it); no network or
        // credential sources are consulted by this regression.
        let path = std::path::Path::new(AWS_EC2_DMI_MARKERS[0]);
        let Ok(file) = std::fs::File::open(path) else {
            return;
        };
        let expected = read_dmi_marker_from(file);
        assert_eq!(read_dmi_marker(path), expected);
    }

    #[test]
    fn an_ec2_instance_identity_still_resolves_instance_credentials() {
        // End to end over real HTTP: the DMI indication activates the probe, and
        // the EC2 source resolves and counts its requests.
        let (base, requests) = metadata_fixture(vec![
            ("/latest/api/token", "200 OK", "metadata-token"),
            (
                "/latest/meta-data/iam/security-credentials/",
                "200 OK",
                "fixture-role\n",
            ),
            (
                "/latest/meta-data/iam/security-credentials/fixture-role",
                "200 OK",
                r#"{"AccessKeyId":"fixture-access","SecretAccessKey":"fixture-secret","Token":"fixture-token"}"#,
            ),
        ]);
        let mut inputs = laptop_inputs();
        inputs.instance_identity = Some(AWS_EC2_DMI_VENDOR.to_owned());
        let activation = aws_metadata_activation_from(&inputs);
        assert_eq!(
            activation,
            AwsMetadataActivation::Enabled(AwsMetadataIndication::Ec2InstanceIdentity)
        );

        let endpoint = base.to_string();
        let credentials = aws_metadata_credentials_with(
            activation,
            None,
            move |name| Ok((name == "AWS_METADATA_SERVICE_ENDPOINT").then(|| endpoint.clone())),
            metadata_credentials_from_url,
            ec2_metadata_credentials,
        )
        .unwrap();
        assert!(
            credentials.is_some(),
            "an EC2 instance identity must still resolve instance-profile credentials"
        );
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[test]
    fn anthropic_declaration_lists_bearer_aliases_before_api_key() {
        match ANTHROPIC.authentication {
            ProviderAuthentication::Environment { variables } => assert_eq!(
                variables,
                &[
                    "ANTHROPIC_AUTH_TOKEN",
                    "ANTHROPIC_OAUTH_TOKEN",
                    "ANTHROPIC_API_KEY"
                ]
            ),
            other => panic!("unexpected authentication {other:?}"),
        }
    }

    // --- Bedrock API key + web identity (roadmap row 1c.5) -----------------

    /// A loopback STS fixture that answers exactly one POST and records its
    /// form body, so a test can assert both the request and the exchange.
    fn sts_fixture(
        status: &'static str,
        body: &'static str,
    ) -> (url::Url, std::sync::Arc<std::sync::Mutex<String>>) {
        use std::io::{Read as _, Write as _};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let recorder = seen.clone();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut byte = [0_u8; 1];
            while !request.ends_with(b"\r\n\r\n") {
                match stream.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => request.push(byte[0]),
                }
            }
            let head = String::from_utf8_lossy(&request).into_owned();
            let content_length = head
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, value)| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut form = vec![0_u8; content_length];
            let _ = stream.read_exact(&mut form);
            *recorder.lock().unwrap() = String::from_utf8_lossy(&form).into_owned();
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        });
        (
            url::Url::parse(&format!("http://{address}/")).unwrap(),
            seen,
        )
    }

    const STS_SUCCESS_XML: &str =
        "<AssumeRoleWithWebIdentityResponse><AssumeRoleWithWebIdentityResult>\
<Credentials><AccessKeyId>fixture-access</AccessKeyId>\
<SecretAccessKey>fixture-secret</SecretAccessKey>\
<SessionToken>fixture-token</SessionToken>\
<Expiration>2030-01-01T00:00:00Z</Expiration></Credentials>\
</AssumeRoleWithWebIdentityResult></AssumeRoleWithWebIdentityResponse>";

    #[test]
    fn bedrock_api_key_takes_precedence_over_the_sigv4_chain() {
        let auth = aws_bedrock_auth_with(
            "us-east-1",
            |name| Ok((name == AWS_BEDROCK_BEARER_VARIABLE).then(|| "bedrock-api-key".to_owned())),
            || {
                panic!(
                    "the SigV4 credential chain must not run when a Bedrock API key is configured"
                )
            },
        )
        .unwrap()
        .expect("the configured API key must select an auth strategy");
        match auth {
            Auth::BearerEnv { var } => assert_eq!(var, AWS_BEDROCK_BEARER_VARIABLE),
            _ => panic!("a configured Bedrock API key must select bearer auth"),
        }
    }

    #[test]
    fn blank_bedrock_api_key_falls_back_to_the_sigv4_chain() {
        let resolves = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = resolves.clone();
        let auth = aws_bedrock_auth_with(
            "us-east-1",
            |name| Ok((name == AWS_BEDROCK_BEARER_VARIABLE).then(|| "   ".to_owned())),
            move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("sigv4")))
            },
        )
        .unwrap()
        .expect("the SigV4 chain must still provide an auth strategy");
        assert!(matches!(auth, Auth::RequestSigner(_)));
        assert_eq!(resolves.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn bedrock_without_any_credential_source_offers_no_auth() {
        let auth = aws_bedrock_auth_with("us-east-1", |_| Ok(None), || Ok(None)).unwrap();
        assert!(
            auth.is_none(),
            "no credential source means no auth strategy"
        );
    }

    #[test]
    fn web_identity_requires_both_standard_variables() {
        let token_only = AwsWebIdentityInputs {
            token_file: Some("/var/run/secrets/eks.amazonaws.com/serviceaccount/token".to_owned()),
            ..AwsWebIdentityInputs::default()
        };
        let error = aws_web_identity_credentials_from(
            &token_only,
            "us-east-1",
            |_| panic!("no token file may be read while the configuration is half set"),
            |_, _| panic!("no STS request may be sent while the configuration is half set"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("AWS_ROLE_ARN"), "{error}");

        let role_only = AwsWebIdentityInputs {
            role_arn: Some("arn:aws:iam::123456789012:role/octet".to_owned()),
            ..AwsWebIdentityInputs::default()
        };
        let error = aws_web_identity_credentials_from(
            &role_only,
            "us-east-1",
            |_| panic!("no token file may be read while the configuration is half set"),
            |_, _| panic!("no STS request may be sent while the configuration is half set"),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("AWS_WEB_IDENTITY_TOKEN_FILE"),
            "{error}"
        );
    }

    #[test]
    fn web_identity_without_configuration_makes_no_request() {
        let credentials = aws_web_identity_credentials_from(
            &AwsWebIdentityInputs::default(),
            "us-east-1",
            |_| panic!("no token file may be read without configuration"),
            |_, _| panic!("no STS request may be sent without configuration"),
        )
        .unwrap();
        assert!(credentials.is_none());
    }

    #[test]
    fn web_identity_posts_the_documented_sts_form_and_resolves_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "fixture-oidc-token\n").unwrap();
        let (endpoint, seen) = sts_fixture("200 OK", STS_SUCCESS_XML);
        let inputs = AwsWebIdentityInputs {
            token_file: Some(token_path.to_string_lossy().into_owned()),
            role_arn: Some("arn:aws:iam::123456789012:role/octet-web-identity".to_owned()),
            session_name: None,
            endpoint_override: Some(endpoint.to_string()),
        };

        let credentials = aws_web_identity_credentials_from(
            &inputs,
            "us-east-1",
            read_bounded_web_identity_token,
            sts_assume_role_with_web_identity,
        )
        .unwrap()
        .expect("a successful exchange must resolve credentials");
        // Signing proves the parsed credentials are structurally usable and
        // that the session token is presented as the SigV4 security token.
        let signer = octet_ai::AwsSigV4Signer::new(
            credentials,
            "us-east-1".to_owned(),
            "bedrock".to_owned(),
        )
        .expect("the STS credentials must be a valid SigV4 credential set");
        let request = octet_ai::SigningRequest::new(
            http::Method::POST,
            url::Url::parse("https://bedrock-runtime.us-east-1.amazonaws.com/model/x/converse")
                .unwrap(),
            bytes::Bytes::from_static(b"{}"),
            http::HeaderMap::new(),
        );
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(octet_ai::RequestSigner::sign(&signer, &request))
            .expect("web-identity credentials must sign a Bedrock request");

        // The exchange used the documented form and never put the token in a URL.
        let form = seen.lock().unwrap().clone();
        assert!(form.contains("Action=AssumeRoleWithWebIdentity"), "{form}");
        assert!(form.contains("Version=2011-06-15"), "{form}");
        assert!(
            form.contains("RoleArn=arn%3Aaws%3Aiam%3A%3A123456789012%3Arole%2Foctet-web-identity"),
            "{form}"
        );
        assert!(form.contains("RoleSessionName=octet"), "{form}");
        assert!(
            form.contains("WebIdentityToken=fixture-oidc-token"),
            "{form}"
        );
        assert!(
            !form.contains("fixture-token"),
            "no response token in the request"
        );
    }

    #[test]
    fn web_identity_sts_failure_is_reported_and_never_downgraded() {
        let directory = tempfile::tempdir().unwrap();
        let token_path = directory.path().join("token");
        std::fs::write(&token_path, "fixture-oidc-token").unwrap();
        let (endpoint, _) = sts_fixture(
            "403 Forbidden",
            "<ErrorResponse><Error><Code>ExpiredTokenException</Code>\
             <Message>token expired</Message></Error></ErrorResponse>",
        );
        let inputs = AwsWebIdentityInputs {
            token_file: Some(token_path.to_string_lossy().into_owned()),
            role_arn: Some("arn:aws:iam::123456789012:role/octet-web-identity".to_owned()),
            session_name: Some("octet-test".to_owned()),
            endpoint_override: Some(endpoint.to_string()),
        };
        let error = aws_web_identity_credentials_from(
            &inputs,
            "us-east-1",
            read_bounded_web_identity_token,
            sts_assume_role_with_web_identity,
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("403"), "{message}");
        assert!(message.contains("ExpiredTokenException"), "{message}");
        // Provider prose and the token itself are never echoed.
        assert!(!message.contains("token expired"), "{message}");
        assert!(!message.contains("fixture-oidc-token"), "{message}");
    }

    #[test]
    fn web_identity_token_file_is_bounded_and_never_empty() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        assert!(read_bounded_web_identity_token(&missing.to_string_lossy()).is_err());

        let empty = directory.path().join("empty");
        std::fs::write(&empty, "  \n").unwrap();
        let error = read_bounded_web_identity_token(&empty.to_string_lossy()).unwrap_err();
        assert!(error.to_string().contains("empty"), "{error}");

        let oversized = directory.path().join("oversized");
        std::fs::write(
            &oversized,
            "x".repeat(MAX_AWS_WEB_IDENTITY_TOKEN_BYTES as usize + 1),
        )
        .unwrap();
        let error = read_bounded_web_identity_token(&oversized.to_string_lossy()).unwrap_err();
        assert!(error.to_string().contains("bounded"), "{error}");

        let valid = directory.path().join("valid");
        std::fs::write(&valid, " token-value\n").unwrap();
        assert_eq!(
            read_bounded_web_identity_token(&valid.to_string_lossy()).unwrap(),
            "token-value"
        );
    }

    #[test]
    fn sts_xml_parsing_requires_every_credential_field() {
        // The full documented result parses into a usable credential set; a
        // missing field, an empty field, and an unknown XML entity all fail
        // closed instead of producing an approximate credential value.
        assert!(sts_credentials_from_xml(STS_SUCCESS_XML).is_ok());

        let missing_token =
            STS_SUCCESS_XML.replace("<SessionToken>fixture-token</SessionToken>", "");
        let error = sts_credentials_from_xml(&missing_token).unwrap_err();
        assert!(
            error.to_string().contains("incomplete credentials"),
            "{error}"
        );

        let empty_secret = STS_SUCCESS_XML.replace(
            "<SecretAccessKey>fixture-secret</SecretAccessKey>",
            "<SecretAccessKey></SecretAccessKey>",
        );
        assert!(sts_credentials_from_xml(&empty_secret).is_err());

        assert_eq!(
            xml_tag("<a>one&amp;two&lt;three&gt;</a>", "a").as_deref(),
            Some("one&two<three>")
        );
        assert_eq!(xml_tag("<a>bad&nbsp;value</a>", "a"), None);
        assert_eq!(xml_tag("<a></a>", "a"), None);
        assert_eq!(xml_tag("<b>x</b>", "a"), None);
    }

    #[test]
    fn sts_endpoint_is_regional_and_validates_overrides_fail_closed() {
        assert_eq!(
            aws_sts_endpoint("us-west-2", None).unwrap().as_str(),
            "https://sts.us-west-2.amazonaws.com/"
        );
        assert_eq!(
            aws_sts_endpoint("us-east-1", Some("http://127.0.0.1:9/"))
                .unwrap()
                .as_str(),
            "http://127.0.0.1:9/"
        );
        for rejected in [
            "http://sts.example.com/",
            "https://user@sts.example.com/",
            "https://sts.example.com/?query=1",
            "https://sts.example.com/#fragment",
            "ftp://sts.example.com/",
        ] {
            assert!(
                aws_sts_endpoint("us-east-1", Some(rejected)).is_err(),
                "STS endpoint override must be rejected: {rejected}"
            );
        }
        assert!(aws_sts_endpoint("not a region", None).is_err());
    }

    #[test]
    fn role_arn_and_session_name_are_validated_fail_closed() {
        assert!(checked_aws_role_arn("arn:aws:iam::123456789012:role/octet").is_ok());
        for rejected in ["", "role/octet", "arn:aws:iam::123:role/bad?query"] {
            assert!(checked_aws_role_arn(rejected).is_err(), "{rejected}");
        }
        for accepted in ["octet", "octet-session_1@example.com"] {
            assert!(
                checked_aws_role_session_name(accepted).is_ok(),
                "{accepted}"
            );
        }
        for rejected in ["", "a", "two words", "control\u{7}"] {
            assert!(
                checked_aws_role_session_name(rejected).is_err(),
                "{rejected:?}"
            );
        }
    }
}
