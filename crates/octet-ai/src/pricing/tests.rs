//! Unit tests for `crate::pricing`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::pricing`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn service_tier_scales_exact_categories_and_retains_sub_microdollars() {
    use crate::types::{ResponsesRuntimeProfile::Codex, ServiceTier};
    let pricing = Pricing {
        input: TokenRate(600_000),
        output: TokenRate(600_000),
        cache_read: TokenRate(600_000),
        cache_write_5m: TokenRate(600_000),
        cache_write_1h: Some(TokenRate(600_000)),
        reasoning: Some(TokenRate(600_000)),
        tiers: vec![],
    };
    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        total_tokens: 2,
        ..Usage::default()
    };
    let priority = responses_cost_of(
        &pricing,
        &usage,
        Codex,
        "gpt-5.4",
        Some(ServiceTier::Priority),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!((priority.input, priority.output), (1, 1));
    assert_eq!(
        (priority.total, priority.total_picodollars_remainder),
        (2, 400_000)
    );
    let flex = responses_cost_of(
        &pricing,
        &usage,
        Codex,
        "gpt-5.4",
        Some(ServiceTier::Flex),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!((flex.total, flex.total_picodollars_remainder), (0, 600_000));
    let premium = responses_cost_of(
        &pricing,
        &usage,
        Codex,
        "gpt-5.5",
        Some(ServiceTier::Priority),
        Some("default"),
    )
    .unwrap()
    .unwrap();
    assert_eq!((premium.total, premium.total_picodollars_remainder), (3, 0));
}

#[test]
fn service_tier_preserves_long_context_and_disjoint_usage_buckets() {
    use crate::types::{ResponsesRuntimeProfile::Codex, ServiceTier};
    let pricing = Pricing {
        input: TokenRate(2_000_000),
        output: TokenRate(4_000_000),
        cache_read: TokenRate(500_000),
        cache_write_5m: TokenRate(2_500_000),
        cache_write_1h: Some(TokenRate(4_000_000)),
        reasoning: Some(TokenRate(6_000_000)),
        tiers: vec![PricingTier {
            min_input_tokens: 10,
            input: Some(TokenRate(4_000_000)),
            output: Some(TokenRate(8_000_000)),
            cache_read: None,
            cache_write_5m: None,
            cache_write_1h: None,
            reasoning: None,
        }],
    };
    let usage = Usage {
        input_tokens: 8,
        cache_read_tokens: 1,
        cache_write_tokens: 1,
        cache_write_1h_tokens: 1,
        output_tokens: 2,
        reasoning_tokens: 1,
        total_tokens: 12,
    };
    // Catalog: input32 + read.5 + write4 + output8 + reasoning6 = 50.5.
    let premium = responses_cost_of(
        &pricing,
        &usage,
        Codex,
        "gpt-5.5",
        Some(ServiceTier::Priority),
        None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(premium.input, 80);
    assert_eq!(premium.cache_read, 1);
    assert_eq!(premium.cache_write, 10);
    assert_eq!(premium.output, 20);
    assert_eq!(premium.reasoning, 15);
    assert_eq!(
        (premium.total, premium.total_picodollars_remainder),
        (126, 250_000)
    );
}

#[test]
fn service_tier_unrepresentable_fraction_is_unpriced_not_rounded_down() {
    use crate::types::{ResponsesRuntimeProfile::Codex, ServiceTier};
    let pricing = Pricing {
        input: TokenRate(1),
        output: TokenRate(0),
        cache_read: TokenRate(0),
        cache_write_5m: TokenRate(0),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    };
    let usage = Usage {
        input_tokens: 1,
        total_tokens: 1,
        ..Usage::default()
    };
    assert!(responses_cost_of(
        &pricing,
        &usage,
        Codex,
        "gpt-5.5",
        Some(ServiceTier::Flex),
        None
    )
    .unwrap()
    .is_none());
    assert_eq!(
        cost_of(&pricing, &usage)
            .unwrap()
            .total_picodollars_remainder,
        1
    );
}

#[test]
fn test_cost_of_simple() {
    let pricing = Pricing {
        input: TokenRate(10), // $10 per 1M tokens ($1e-5 per token)
        output: TokenRate(20),
        cache_read: TokenRate(5),
        cache_write_5m: TokenRate(15),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    };

    let usage = Usage {
        input_tokens: 100_000,
        cache_read_tokens: 50_000,
        cache_write_tokens: 10_000,
        cache_write_1h_tokens: 0,
        output_tokens: 200_000,
        reasoning_tokens: 0,
        total_tokens: 360_000,
    };

    let cost = cost_of(&pricing, &usage).unwrap();
    // input: (100,000 * 10) / 1,000,000 = 1 microdollar
    // cache_read: (50,000 * 5) / 1,000,000 = 0 microdollars (floor-rounded)
    // cache_write: (10,000 * 15) / 1,000,000 = 0 microdollars
    // output: (200,000 * 20) / 1,000,000 = 4 microdollars
    // total: 1 + 0 + 0 + 4 = 5 microdollars
    assert_eq!(cost.input, 1);
    assert_eq!(cost.cache_read, 0);
    assert_eq!(cost.cache_write, 0);
    assert_eq!(cost.output, 4);
    assert_eq!(cost.total, 5);
}

#[test]
fn request_total_preserves_the_exact_sum_of_category_costs() {
    let pricing = Pricing {
        input: TokenRate(600_000),
        output: TokenRate(600_000),
        cache_read: TokenRate(600_000),
        cache_write_5m: TokenRate(600_000),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    };
    let usage = Usage {
        input_tokens: 1,
        output_tokens: 1,
        total_tokens: 2,
        ..Usage::default()
    };

    let cost = cost_of(&pricing, &usage).unwrap();
    assert_eq!(cost.input, 0);
    assert_eq!(cost.output, 0);
    assert_eq!(cost.total, 1);
    assert_eq!(cost.total_picodollars_remainder, 200_000);
}

#[test]
fn test_cost_of_tier_boundary() {
    let pricing = Pricing {
        input: TokenRate(100),
        output: TokenRate(200),
        cache_read: TokenRate(50),
        cache_write_5m: TokenRate(150),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![PricingTier {
            min_input_tokens: 100_000,
            input: Some(TokenRate(80)),
            output: None,
            cache_read: None,
            cache_write_5m: None,
            cache_write_1h: None,
            reasoning: None,
        }],
    };

    // 1. Just below tier boundary (99,999 input bucket tokens)
    let usage_below = Usage {
        input_tokens: 99_999,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 99_999,
    };
    let cost_below = cost_of(&pricing, &usage_below).unwrap();
    // input cost: (99,999 * 100) / 1,000_000 = 9 microdollars
    assert_eq!(cost_below.input, 9);

    // 2. Exactly at/above tier boundary (100,000 input bucket tokens)
    let usage_above = Usage {
        input_tokens: 100_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        output_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 100_000,
    };
    let cost_above = cost_of(&pricing, &usage_above).unwrap();
    // input cost: (100,000 * 80) / 1,000_000 = 8 microdollars (using tier override)
    assert_eq!(cost_above.input, 8);
}

#[test]
fn one_hour_cache_write_defaults_to_twice_input_rate() {
    let pricing = Pricing {
        input: TokenRate(3_000_000),
        output: TokenRate(15_000_000),
        cache_read: TokenRate(300_000),
        cache_write_5m: TokenRate(3_750_000),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    };
    let usage = Usage {
        input_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 1_000_000,
        cache_write_1h_tokens: 1_000_000,
        output_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 1_000_000,
    };

    let cost = cost_of(&pricing, &usage).unwrap();
    assert_eq!(cost.cache_write, 6_000_000);
    assert_eq!(cost.total, 6_000_000);
}

#[test]
fn test_cost_inconsistent_subsets() {
    let pricing = Pricing {
        input: TokenRate(10),
        output: TokenRate(20),
        cache_read: TokenRate(5),
        cache_write_5m: TokenRate(15),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    };

    // cache_write_1h_tokens > cache_write_tokens
    let usage_bad_cache = Usage {
        input_tokens: 10_000,
        cache_read_tokens: 0,
        cache_write_tokens: 100,
        cache_write_1h_tokens: 200,
        output_tokens: 0,
        reasoning_tokens: 0,
        total_tokens: 10_300,
    };
    assert!(matches!(
        cost_of(&pricing, &usage_bad_cache),
        Err(PricingError::InvalidUsageBuckets)
    ));

    // reasoning_tokens > output_tokens
    let usage_bad_reasoning = Usage {
        input_tokens: 10_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_write_1h_tokens: 0,
        output_tokens: 100,
        reasoning_tokens: 200,
        total_tokens: 10_300,
    };
    assert!(matches!(
        cost_of(&pricing, &usage_bad_reasoning),
        Err(PricingError::InvalidUsageBuckets)
    ));
}
