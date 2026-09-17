//! Bedrock model/application-profile ARN region resolution.
//!
//! Upstream Pi resolves a Bedrock runtime region from the *model identifier*
//! first: when the selected model id is an `arn:aws[-partition]:bedrock:<region>:…`
//! inference-profile or application-inference-profile ARN, that embedded region
//! wins over `AWS_REGION`/`AWS_DEFAULT_REGION`, so a profile ARN never signs or
//! connects against a region chosen for a different service. Only when the ARN
//! carries no region does the configured region (or the standard runtime
//! endpoint host, or the `us-east-1` default) apply.
//!
//! This module owns that pure resolution plus the standard Bedrock runtime
//! endpoint shape. It performs no network, credential or environment access: a
//! host reads its own configuration and passes it in. Both the endpoint host
//! and the SigV4 signing scope must use the resolved region; signing with a
//! different region than the endpoint produces an opaque `SignatureDoesNotMatch`
//! failure, which is why one value is resolved once and returned together.

use serde::{Deserialize, Serialize};

use super::DeclarationError;

/// Fallback region used only when nothing else is configured, matching Pi's
/// non-Node fallback.
pub const DEFAULT_BEDROCK_REGION: &str = "us-east-1";

/// Maximum accepted ARN length.
pub const MAX_BEDROCK_ARN_BYTES: usize = 2048;

/// A parsed `arn:…:bedrock:…` resource.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BedrockArn {
    /// AWS partition (`aws`, `aws-us-gov`, `aws-cn`, …).
    pub partition: String,
    /// The region embedded in the ARN.
    pub region: String,
    /// The account id.
    pub account: String,
    /// Resource type, e.g. `inference-profile`.
    pub resource: String,
    /// Resource identifier after the first `/`.
    pub id: String,
}

impl BedrockArn {
    /// Whether this ARN identifies an inference profile.
    pub fn is_inference_profile(&self) -> bool {
        self.resource == "inference-profile"
    }

    /// Whether this ARN identifies an application inference profile.
    pub fn is_application_inference_profile(&self) -> bool {
        self.resource == "application-inference-profile"
    }
}

/// Whether a partition is `aws` or a documented `aws-<suffix>` family member.
fn valid_partition(value: &str) -> bool {
    value == "aws"
        || value.strip_prefix("aws-").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'+' | b'=' | b'/' | b':'))
}

/// Parse a Bedrock ARN, accepting every `arn:aws`-family partition.
///
/// Returns `None` for anything that is not exactly
/// `arn:<partition>:bedrock:<region>:<account>:<resource>[/<id>]` with
/// non-empty components, so a foundation-model id or a malformed ARN never
/// contributes a region.
pub fn parse_bedrock_arn(value: &str) -> Option<BedrockArn> {
    if value.len() > MAX_BEDROCK_ARN_BYTES {
        return None;
    }
    let mut parts = value.splitn(6, ':');
    let scheme = parts.next()?;
    let partition = parts.next()?;
    let service = parts.next()?;
    let region = parts.next()?;
    let account = parts.next()?;
    let tail = parts.next()?;
    if scheme != "arn"
        || service != "bedrock"
        || !valid_partition(partition)
        || !valid_bedrock_region(region)
        || account.is_empty()
        || !account.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let (resource, id) = tail.split_once('/')?;
    if !valid_component(resource) || !valid_component(id) {
        return None;
    }
    Some(BedrockArn {
        partition: partition.to_owned(),
        region: region.to_owned(),
        account: account.to_owned(),
        resource: resource.to_owned(),
        id: id.to_owned(),
    })
}

/// Whether a region is structurally usable for the runtime host and the SigV4
/// scope: lowercase alphanumeric plus `-`, 1–64 bytes.
pub fn valid_bedrock_region(region: &str) -> bool {
    !region.is_empty()
        && region.len() <= 64
        && region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !region.starts_with('-')
        && !region.ends_with('-')
}

/// The region pinned by a Bedrock model identifier, if it carries an ARN.
///
/// Matches Pi's `^arn:aws(?:-[a-z0-9-]+)?:bedrock:([a-z0-9-]+):` shape but
/// validates the whole ARN: a partially-formed ARN yields no region rather than
/// a guessed one.
pub fn bedrock_model_region(model_id: &str) -> Option<String> {
    parse_bedrock_arn(model_id).map(|arn| arn.region)
}

/// The region encoded in a standard Bedrock runtime endpoint host.
pub fn bedrock_endpoint_region(base_url: &url::Url) -> Option<String> {
    let host = base_url.host_str()?.to_ascii_lowercase();
    let rest = host
        .strip_prefix("bedrock-runtime.")
        .or_else(|| host.strip_prefix("bedrock-runtime-fips."))?;
    let region = rest
        .strip_suffix(".amazonaws.com")
        .or_else(|| rest.strip_suffix(".amazonaws.com.cn"))?;
    valid_bedrock_region(region).then(|| region.to_owned())
}

/// Resolve the effective runtime region: ARN-embedded region, then the
/// caller-configured region, then the standard endpoint's own region, then
/// [`DEFAULT_BEDROCK_REGION`].
pub fn resolve_bedrock_region(
    configured_region: Option<&str>,
    model_id: &str,
    base_url: &url::Url,
) -> Option<String> {
    if let Some(region) = bedrock_model_region(model_id) {
        return Some(region);
    }
    if let Some(region) = configured_region.filter(|region| valid_bedrock_region(region)) {
        return Some(region.to_owned());
    }
    bedrock_endpoint_region(base_url).or_else(|| Some(DEFAULT_BEDROCK_REGION.to_owned()))
}

/// Whether an ARN selects the GovCloud partition.
pub fn bedrock_arn_is_government(model_id: &str) -> bool {
    parse_bedrock_arn(model_id).is_some_and(|arn| {
        arn.partition.contains("us-gov") || arn.region.starts_with("us-gov-")
    })
}

/// Standard runtime host for a region, preserving a `.cn` suffix for the
/// `aws-cn` partition.
pub fn bedrock_runtime_host(region: &str, china_partition: bool) -> String {
    if china_partition {
        format!("bedrock-runtime.{region}.amazonaws.com.cn")
    } else {
        format!("bedrock-runtime.{region}.amazonaws.com")
    }
}

/// Rebuild the endpoint URL when a standard runtime endpoint and an ARN-embedded
/// region disagree.
///
/// A custom (non-standard) endpoint is returned unchanged: a VPC interface
/// endpoint or proxy is an explicit destination the caller chose, and moving it
/// because a model id names another region would break it. Only the standard
/// `<bedrock-runtime>.<region>.amazonaws.com` shape is re-hosted, and the ARN's
/// partition decides the host suffix.
pub fn bedrock_runtime_endpoint(
    base_url: &url::Url,
    model_id: &str,
    configured_region: Option<&str>,
) -> Result<url::Url, DeclarationError> {
    let Some(resolved) = resolve_bedrock_region(configured_region, model_id, base_url) else {
        return Err(DeclarationError::Invalid(
            "could not resolve a Bedrock runtime region".into(),
        ));
    };
    if !valid_bedrock_region(&resolved) {
        return Err(DeclarationError::Invalid(
            "invalid Bedrock runtime region".into(),
        ));
    }
    let (standard_region, china) = match bedrock_endpoint_region(base_url) {
        Some(region) => (Some(region), base_url.host_str().is_some_and(|host| host.ends_with(".cn"))),
        None => (None, false),
    };
    match standard_region {
        Some(region) if region == resolved => Ok(base_url.clone()),
        Some(_) => {
            let china = china
                || parse_bedrock_arn(model_id).is_some_and(|arn| arn.partition == "aws-cn");
            let mut url = base_url.clone();
            url.set_host(Some(&bedrock_runtime_host(&resolved, china)))
                .map_err(|_| DeclarationError::Invalid("invalid Bedrock endpoint host".into()))?;
            Ok(url)
        }
        // A custom endpoint is authoritative; the resolved region still feeds
        // the signing scope through `resolve_bedrock_region`.
        None => Ok(base_url.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(value: &str) -> url::Url {
        url::Url::parse(value).unwrap()
    }

    #[test]
    fn arn_parsing_requires_a_complete_well_formed_arn() {
        let arn = parse_bedrock_arn(
            "arn:aws:bedrock:eu-central-1:123456789012:application-inference-profile/abc123",
        )
        .unwrap();
        assert_eq!(arn.region, "eu-central-1");
        assert_eq!(arn.account, "123456789012");
        assert!(arn.is_application_inference_profile());
        assert!(!arn.is_inference_profile());
        let profile = parse_bedrock_arn(
            "arn:aws:bedrock:us-west-2:123456789012:inference-profile/us.anthropic.claude",
        )
        .unwrap();
        assert!(profile.is_inference_profile());
        let gov = parse_bedrock_arn(
            "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:inference-profile/gov.anthropic",
        )
        .unwrap();
        assert!(gov.partition.contains("us-gov"));
        for rejected in [
            "anthropic.claude-3-5-sonnet-20240620-v1:0",
            "us.anthropic.claude-3-5-sonnet-20240620-v1:0",
            "arn:aws:bedrock:eu-central-1:123456789012",
            "arn:aws:bedrock::123456789012:inference-profile/x",
            "arn:aws:bedrock:EU-CENTRAL-1:123456789012:inference-profile/x",
            "arn:aws:s3:eu-central-1:123456789012:inference-profile/x",
            "arn:aws:bedrock:eu-central-1:123456789012:inference-profile/",
            "not-an-arn",
        ] {
            assert!(parse_bedrock_arn(rejected).is_none(), "{rejected}");
        }
    }

    #[test]
    fn arn_region_wins_over_configured_and_endpoint_regions() {
        let model =
            "arn:aws:bedrock:ap-southeast-2:123456789012:application-inference-profile/profile-1";
        assert_eq!(bedrock_model_region(model).as_deref(), Some("ap-southeast-2"));
        assert_eq!(
            resolve_bedrock_region(Some("us-east-1"), model, &url("https://bedrock-runtime.us-east-1.amazonaws.com/")),
            Some("ap-southeast-2".to_owned())
        );
        // A foundation-model id uses the configured region.
        assert_eq!(
            resolve_bedrock_region(
                Some("eu-west-1"),
                "anthropic.claude-3-5-sonnet-20240620-v1:0",
                &url("https://bedrock-runtime.us-east-1.amazonaws.com/")
            ),
            Some("eu-west-1".to_owned())
        );
        // No configured region: the standard endpoint host supplies it.
        assert_eq!(
            bedrock_endpoint_region(&url("https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com/")),
            Some("us-gov-west-1".to_owned())
        );
        assert_eq!(
            resolve_bedrock_region(None, "foundation-model", &url("https://bedrock-runtime.us-west-2.amazonaws.com/")),
            Some("us-west-2".to_owned())
        );
        // Nothing configured at all keeps the documented default.
        assert_eq!(
            resolve_bedrock_region(None, "foundation-model", &url("https://bedrock.example.test/")),
            Some(DEFAULT_BEDROCK_REGION.to_owned())
        );
        assert!(bedrock_arn_is_government(
            "arn:aws-us-gov:bedrock:us-gov-west-1:123456789012:inference-profile/gov.anthropic"
        ));
    }

    #[test]
    fn standard_endpoints_follow_the_arn_region_but_custom_endpoints_do_not() {
        let standard = url("https://bedrock-runtime.us-east-1.amazonaws.com/");
        let moved = bedrock_runtime_endpoint(
            &standard,
            "arn:aws:bedrock:eu-central-1:123456789012:application-inference-profile/p",
            Some("us-east-1"),
        )
        .unwrap();
        assert_eq!(
            moved.as_str(),
            "https://bedrock-runtime.eu-central-1.amazonaws.com/"
        );
        assert_eq!(
            bedrock_runtime_endpoint(&standard, "foundation-model", Some("us-east-1"))
                .unwrap()
                .as_str(),
            standard.as_str()
        );
        let china = bedrock_runtime_endpoint(
            &url("https://bedrock-runtime.cn-north-1.amazonaws.com.cn/"),
            "arn:aws-cn:bedrock:cn-northwest-1:123456789012:application-inference-profile/p",
            None,
        )
        .unwrap();
        assert_eq!(
            china.as_str(),
            "https://bedrock-runtime.cn-northwest-1.amazonaws.com.cn/"
        );
        // A VPC/proxy endpoint stays exactly where the caller declared it.
        let custom = url("https://bedrock.internal.example/vpce/");
        assert_eq!(
            bedrock_runtime_endpoint(
                &custom,
                "arn:aws:bedrock:eu-central-1:123456789012:application-inference-profile/p",
                Some("us-east-1"),
            )
            .unwrap()
            .as_str(),
            custom.as_str()
        );
    }
}
