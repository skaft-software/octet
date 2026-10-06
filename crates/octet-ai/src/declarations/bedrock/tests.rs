//! Unit tests for `crate::declarations::bedrock`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `bedrock.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::declarations::bedrock`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

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
    assert_eq!(
        bedrock_model_region(model).as_deref(),
        Some("ap-southeast-2")
    );
    assert_eq!(
        resolve_bedrock_region(
            Some("us-east-1"),
            model,
            &url("https://bedrock-runtime.us-east-1.amazonaws.com/")
        ),
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
        bedrock_endpoint_region(&url(
            "https://bedrock-runtime-fips.us-gov-west-1.amazonaws.com/"
        )),
        Some("us-gov-west-1".to_owned())
    );
    assert_eq!(
        resolve_bedrock_region(
            None,
            "foundation-model",
            &url("https://bedrock-runtime.us-west-2.amazonaws.com/")
        ),
        Some("us-west-2".to_owned())
    );
    // Nothing configured at all keeps the documented default.
    assert_eq!(
        resolve_bedrock_region(
            None,
            "foundation-model",
            &url("https://bedrock.example.test/")
        ),
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
