#![allow(missing_docs)]

//! Presentation formatting for prices, context sizes, and elapsed time.
//!
//! Accounting remains integer-based; this module only chooses a readable
//! representation for already-authoritative values.

use std::time::Duration;

use octet_ai::{Pricing, TokenRate};

/// Render a token rate as dollars per million tokens.
pub fn format_token_rate(rate: TokenRate) -> String {
    format!("{}/M", format_token_rate_value(rate))
}

/// Compact dollar value for side-by-side model price comparisons.
///
/// `TokenRate` is an integer microdollar rate. Rounding is done with integer
/// arithmetic so presentation cannot introduce floating-point cost drift.
pub fn format_token_rate_value(rate: TokenRate) -> String {
    const MICRODOLLARS_PER_CENT: u64 = 10_000;
    const MICRODOLLARS_PER_TEN_THOUSANDTH: u64 = 100;
    if rate.0 >= 1_000_000 {
        let cents = rate.0.saturating_add(MICRODOLLARS_PER_CENT / 2) / MICRODOLLARS_PER_CENT;
        format!("${}.{:02}", cents / 100, cents % 100)
    } else {
        let ten_thousandths = rate.0.saturating_add(MICRODOLLARS_PER_TEN_THOUSANDTH / 2)
            / MICRODOLLARS_PER_TEN_THOUSANDTH;
        let whole = ten_thousandths / 10_000;
        let fractional = ten_thousandths % 10_000;
        if fractional == 0 {
            format!("${whole}")
        } else {
            let fractional = format!("{fractional:04}");
            format!("${whole}.{}", fractional.trim_end_matches('0'))
        }
    }
}

/// Compact context-window size for status and model-selection surfaces.
pub fn compact_context_limit(value: u64) -> String {
    if value >= 1_000_000 {
        let tenths = (u128::from(value) * 10 + 500_000) / 1_000_000;
        if tenths % 10 == 0 {
            format!("{}M", tenths / 10)
        } else {
            format!("{}.{:01}M", tenths / 10, tenths % 10)
        }
    } else if value >= 1_000 {
        format!("{}K", value / 1_000)
    } else {
        value.to_string()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriceDisplay {
    #[default]
    Unknown,
    ExplicitZero,
    Priced,
}

impl PriceDisplay {
    pub fn from_pricing(pricing: Option<&Pricing>) -> Self {
        let Some(pricing) = pricing else {
            return Self::Unknown;
        };
        let base_is_zero = pricing.input.0 == 0
            && pricing.output.0 == 0
            && pricing.cache_read.0 == 0
            && pricing.cache_write_5m.0 == 0
            && pricing.cache_write_1h.is_none_or(|rate| rate.0 == 0)
            && pricing.reasoning.is_none_or(|rate| rate.0 == 0);
        let tiers_are_zero = pricing.tiers.iter().all(|tier| {
            tier.input.is_none_or(|rate| rate.0 == 0)
                && tier.output.is_none_or(|rate| rate.0 == 0)
                && tier.cache_read.is_none_or(|rate| rate.0 == 0)
                && tier.cache_write_5m.is_none_or(|rate| rate.0 == 0)
                && tier.cache_write_1h.is_none_or(|rate| rate.0 == 0)
                && tier.reasoning.is_none_or(|rate| rate.0 == 0)
        });
        if base_is_zero && tiers_are_zero {
            Self::ExplicitZero
        } else {
            Self::Priced
        }
    }
}

/// Render a short elapsed interval without floating-point conversion.
pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    if seconds < 60 {
        let tenths = seconds.saturating_mul(10).saturating_add(u64::from(
            (duration.subsec_nanos() + 50_000_000) / 100_000_000,
        ));
        format!("{}.{:01}s", tenths / 10, tenths % 10)
    } else {
        let minutes = seconds / 60;
        let remainder = seconds % 60;
        format!("{minutes}m{remainder:02}s")
    }
}
