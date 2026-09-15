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
/// provider reusing these variables inherits the behavior without a branch.
fn bearer_token_variable(variable: &str) -> bool {
    matches!(variable, "ANTHROPIC_AUTH_TOKEN" | "ANTHROPIC_OAUTH_TOKEN")
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

/// Return a request-signing auth strategy after confirming that the bounded AWS
/// credential chain has a usable source. The signer resolves the chain again on
/// each request, allowing ECS/EC2 metadata credentials to rotate without
/// leaking the private source into provider declarations.
pub(crate) fn aws_bedrock_auth(region: &str) -> anyhow::Result<Option<Auth>> {
    if resolve_aws_credentials()?.is_none() {
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

type AwsCredentialsResolver = std::sync::Arc<
    dyn Fn() -> anyhow::Result<Option<octet_ai::AwsCredentials>> + Send + Sync,
>;

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

fn resolve_aws_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    resolve_aws_credentials_with(
        aws_environment_credentials,
        aws_profile_credentials,
        aws_metadata_credentials,
    )
}

fn resolve_aws_credentials_with<E, P, M>(
    environment: E,
    profile: P,
    metadata: M,
) -> anyhow::Result<Option<octet_ai::AwsCredentials>>
where
    E: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
    P: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
    M: FnOnce() -> anyhow::Result<Option<octet_ai::AwsCredentials>>,
{
    if let Some(credentials) = environment()? {
        return Ok(Some(credentials));
    }
    if let Some(credentials) = profile()? {
        return Ok(Some(credentials));
    }
    metadata()
}

fn aws_environment_credentials() -> anyhow::Result<Option<octet_ai::AwsCredentials>> {
    aws_environment_credentials_with(|variable| optional_bounded_env(variable))
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
    aws_metadata_credentials_with(
        aws_metadata_activation()?,
        |variable| optional_bounded_env(variable),
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
fn aws_metadata_credentials_with<R, F, C>(
    activation: AwsMetadataActivation,
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
    let base = aws_metadata_service_endpoint_with(&mut read_env)?;
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
    /// `AWS_METADATA_SERVICE_ENDPOINT`/`_MODE` is set: the host pinned the IMDS
    /// endpoint, which only an EC2-shaped environment does.
    MetadataServiceEndpoint,
    /// `OCTET_AWS_METADATA_CREDENTIALS` truthy: the operator's explicit opt-in.
    ProductOptIn,
    /// The effective AWS profile declares instance/container metadata as its
    /// credential source (`credential_source = Ec2InstanceMetadata|EcsContainer`).
    ProfileCredentialSource,
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
    /// Raw `AWS_METADATA_SERVICE_ENDPOINT` value.
    pub(crate) metadata_service_endpoint: Option<String>,
    /// Raw `AWS_METADATA_SERVICE_ENDPOINT_MODE` value.
    pub(crate) metadata_service_endpoint_mode: Option<String>,
    /// Raw `OCTET_AWS_METADATA_CREDENTIALS` value.
    pub(crate) product_opt_in: Option<String>,
    /// `credential_source` from the effective AWS profile, already lowercased.
    pub(crate) profile_credential_source: Option<String>,
    /// `ec2_metadata_service_endpoint` from the effective AWS profile.
    pub(crate) profile_metadata_service_endpoint: Option<String>,
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
                return AwsMetadataActivation::Disabled(
                    AwsMetadataSuppression::ExplicitlyDisabled,
                );
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
    AwsMetadataActivation::Disabled(AwsMetadataSuppression::Unindicated)
}

/// Read the activation inputs from the AWS environment and the effective profile.
fn aws_metadata_activation_inputs() -> anyhow::Result<AwsMetadataActivationInputs> {
    aws_metadata_activation_inputs_with(
        |variable| optional_bounded_env(variable),
        |config_file| aws_profile_values(config_file),
    )
}

fn aws_metadata_activation_inputs_with<R, P>(
    mut read_env: R,
    mut read_profile: P,
) -> anyhow::Result<AwsMetadataActivationInputs>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
    P: FnMut(bool) -> anyhow::Result<Option<std::collections::BTreeMap<String, String>>>,
{
    let profile_values = read_profile(false)?;
    let profile_config = read_profile(true)?;
    let profile_value = |values: &Option<std::collections::BTreeMap<String, String>>, key: &str| {
        values
            .as_ref()
            .and_then(|values| values.get(key))
            .cloned()
    };
    Ok(AwsMetadataActivationInputs {
        ec2_metadata_disabled: read_env("AWS_EC2_METADATA_DISABLED")?,
        container_credentials_relative_uri: read_env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")?,
        container_credentials_full_uri: read_env("AWS_CONTAINER_CREDENTIALS_FULL_URI")?,
        metadata_service_endpoint: read_env("AWS_METADATA_SERVICE_ENDPOINT")?,
        metadata_service_endpoint_mode: read_env("AWS_METADATA_SERVICE_ENDPOINT_MODE")?,
        product_opt_in: read_env(AWS_METADATA_OPT_IN_VARIABLE)?,
        profile_credential_source: profile_value(&profile_values, "credential_source"),
        profile_metadata_service_endpoint: profile_value(&profile_config, "ec2_metadata_service_endpoint"),
    })
}

fn aws_metadata_activation() -> anyhow::Result<AwsMetadataActivation> {
    Ok(aws_metadata_activation_from(&aws_metadata_activation_inputs()?))
}

/// Resolve the IMDS base URL, honoring the standard endpoint override.
///
/// The override exists so a machine (or a test) that pins IMDS can still be
/// used; it is validated fail-closed because it selects a host the signer will
/// contact.
fn aws_metadata_service_endpoint_with<R>(mut read_env: R) -> anyhow::Result<url::Url>
where
    R: FnMut(&str) -> anyhow::Result<Option<String>>,
{
    let Some(value) = read_env("AWS_METADATA_SERVICE_ENDPOINT")? else {
        return url::Url::parse(AWS_EC2_METADATA_ENDPOINT).map_err(Into::into);
    };
    checked_aws_metadata_endpoint(&value)
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
        Some(url::Host::Ipv4(address)) => address.is_loopback() || address.octets()[..2] == [169, 254],
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
        octet_ai::AwsCredentials::new(
            format!("{label}-access"),
            format!("{label}-secret"),
            None,
        )
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
        let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let metadata_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let profile_flag = profile_called.clone();
        let metadata_flag = metadata_called.clone();
        let selected = resolve_aws_credentials_with(
            || Ok(Some(fixture_credentials("environment"))),
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
            || Ok(Some(fixture_credentials("metadata"))),
        )
        .unwrap();
        assert!(selected.is_some());

        let profile_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let profile_flag = profile_called.clone();
        assert!(resolve_aws_credentials_with(
            || Err(anyhow::anyhow!("environment source failed")),
            move || {
                profile_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(Some(fixture_credentials("profile")))
            },
            || Ok(None),
        )
        .is_err());
        assert!(!profile_called.load(std::sync::atomic::Ordering::SeqCst));
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
            r#"{"AccessKeyId":"metadata\naccess","SecretAccessKey":"metadata-secret"}"#
                .to_owned(),
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
        let headers =
            environment_discovery_headers(&ANTHROPIC.routes[0], &auth_token).unwrap();
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
        let headers =
            environment_discovery_headers(&ANTHROPIC.routes[0], &api_key).unwrap();
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

    /// The ordinary case: a laptop with no AWS environment and no AWS profile.
    fn laptop_inputs() -> AwsMetadataActivationInputs {
        aws_metadata_activation_inputs_with(|_| Ok(None), |_| Ok(None)).unwrap()
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

        // An explicit off wins over every enabling input.
        let mut inputs = laptop_inputs();
        inputs.ec2_metadata_disabled = Some("true".to_owned());
        inputs.container_credentials_relative_uri = Some("/v2/credentials".to_owned());
        inputs.product_opt_in = Some("true".to_owned());
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
                Ok(if config_file { None } else { Some(profile.clone()) })
            },
        )
        .unwrap();
        assert_eq!(
            aws_metadata_activation_from(&inputs),
            AwsMetadataActivation::Enabled(AwsMetadataIndication::ProfileCredentialSource)
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
        assert!(credentials.is_none(), "an unavailable IMDS resolves to no credentials");
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn aws_metadata_service_endpoint_override_is_validated_fail_closed() {
        let default = aws_metadata_service_endpoint_with(|_| Ok(None)).unwrap();
        assert_eq!(default.as_str(), AWS_EC2_METADATA_ENDPOINT);

        for accepted in [
            "http://127.0.0.1:8080",
            "http://localhost:8080/imds",
            "http://[::1]:8080",
            "https://metadata.internal.example",
        ] {
            let endpoint = aws_metadata_service_endpoint_with(|_| Ok(Some(accepted.to_owned())))
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
                aws_metadata_service_endpoint_with(|_| Ok(Some(rejected.to_owned()))).is_err(),
                "metadata endpoint override must be rejected: {rejected}"
            );
        }
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
}
