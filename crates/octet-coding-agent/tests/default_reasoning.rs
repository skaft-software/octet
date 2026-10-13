//! The default reasoning level comes from the endpoint, not from octet.
//!
//! DeepSeek's own inventory declares `effort.default_level` and the enabled
//! levels it accepts. octet ignored that field: with no user preference the
//! selection fell back to the first enabled level octet knew about, so an
//! unset preference could differ from the endpoint's declared default. These
//! tests run the real `<binary> --print` against a loopback endpoint that
//! publishes that shape and read the wire controls it sends.

#![cfg(unix)]
#![allow(missing_docs)]

mod deepseek_loopback;

use deepseek_loopback::{fixture, DISCOVERY_ONLY_MODEL};

#[tokio::test]
async fn declared_default_level_replaces_octets_own_guess() {
    let endpoint = fixture(false, Some("high")).await;
    let (_, bodies) = endpoint.run(DISCOVERY_ONLY_MODEL, &[]).await;
    assert_eq!(
        bodies[0]["reasoning_effort"], "high",
        "an unset preference must use the provider-declared default: {}",
        bodies[0]
    );
    assert_eq!(bodies[0]["thinking"]["type"], "enabled");
}

#[tokio::test]
async fn declared_default_level_outweighs_octets_pinned_guess() {
    // Same endpoint, different declaration: the cached contract must follow the
    // endpoint rather than octet's own pinned default for this model.
    let endpoint = fixture(false, Some("low")).await;
    let (_, bodies) = endpoint.run(DISCOVERY_ONLY_MODEL, &[]).await;
    assert_eq!(
        bodies[0]["reasoning_effort"], "low",
        "the endpoint's declared default must win: {}",
        bodies[0]
    );
}

#[tokio::test]
async fn explicit_reasoning_choice_still_wins() {
    let endpoint = fixture(false, Some("high")).await;
    let (_, max) = endpoint
        .run(DISCOVERY_ONLY_MODEL, &["--reasoning", "max"])
        .await;
    assert_eq!(
        max[0]["reasoning_effort"], "max",
        "the declared levels must keep max available: {}",
        max[0]
    );
    assert_eq!(max[0]["thinking"]["type"], "enabled");

    let offline_endpoint = fixture(false, Some("high")).await;
    let (_, off) = offline_endpoint
        .run(DISCOVERY_ONLY_MODEL, &["--reasoning", "off"])
        .await;
    assert!(
        off[0].get("reasoning_effort").is_none(),
        "Off must not become an effort level: {}",
        off[0]
    );
    assert_eq!(off[0]["thinking"]["type"], "disabled");
}
