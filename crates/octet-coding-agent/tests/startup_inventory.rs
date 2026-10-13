//! The cold-cache provider inventory must not sit in front of the first turn.
//!
//! `GET /v1/models` is the only way to learn a discovery-only model id, so a
//! cold cache used to block the launch (42/42 measured cells, median 373 ms).
//! These tests stall the loopback endpoint far past the discovery timeout and
//! run the real `<binary> --print`: the first turn must go out anyway, and an
//! explicitly named model that only discovery knows must still resolve.

#![cfg(unix)]
#![allow(missing_docs)]

mod deepseek_loopback;

use std::time::Duration;

use deepseek_loopback::{fixture, ALWAYS_REGISTERED_MODEL, DISCOVERY_ONLY_MODEL};

#[tokio::test]
async fn cold_inventory_never_delays_the_first_turn() {
    let endpoint = fixture(true, None).await;
    // The always-registered alias resolves without any inventory, so the launch
    // must not wait for the stalled endpoint before its first request.
    let (elapsed, bodies) = endpoint.run(ALWAYS_REGISTERED_MODEL, &[]).await;
    assert!(
        elapsed < Duration::from_secs(10),
        "the first turn waited on the inventory: {elapsed:?}"
    );
    assert_eq!(bodies.len(), 1);
}

#[tokio::test]
async fn the_first_chat_request_precedes_a_cold_inventory_refresh() {
    let endpoint = fixture(true, None).await;
    let (_, bodies) = endpoint.run(ALWAYS_REGISTERED_MODEL, &[]).await;
    assert_eq!(bodies.len(), 1);
    let requests = endpoint.server.received_requests().await.unwrap();
    let first = requests
        .first()
        .expect("provider should receive the first turn");
    assert_eq!(
        first.url.path(),
        "/v1/chat/completions",
        "startup inventory GET ran before the first turn"
    );
}

#[tokio::test]
async fn cold_inventory_still_serves_an_explicit_discovery_only_model() {
    let endpoint = fixture(true, None).await;
    let (elapsed, bodies) = endpoint.run(DISCOVERY_ONLY_MODEL, &[]).await;
    assert!(
        elapsed < Duration::from_secs(10),
        "the first turn waited on the inventory: {elapsed:?}"
    );
    assert_eq!(bodies[0]["model"], "deepseek-flash");
}
