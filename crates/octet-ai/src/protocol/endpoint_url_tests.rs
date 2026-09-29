//! Unit tests for `crate::protocol`.
//!
//! Covers endpoint URL and codec-path resolution.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::protocol`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::endpoint_url;

#[test]
fn preserves_the_single_version_query_when_resolving_a_codec_path() {
    let base = url::Url::parse(
        "https://enterprise-resource.openai.azure.com/openai/?api-version=2025-04-01-preview",
    )
    .unwrap();
    let url = endpoint_url(&base, "responses").unwrap();
    assert_eq!(
        url.as_str(),
        "https://enterprise-resource.openai.azure.com/openai/responses?api-version=2025-04-01-preview"
    );
}
