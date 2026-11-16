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
        if tenths.is_multiple_of(10) {
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

/// Display-only, three-significant-digit rounding, with ties rounded up.
/// Integer nanoseconds avoid floating-point drift and preserve stored precision.
/// Only the returned string allocates; trailing fractional zeroes are omitted.
pub fn format_duration(duration: Duration) -> String {
    const UNITS: [(u128, &str); 6] = [
        (1, "ns"),
        (1_000, "µs"),
        (1_000_000, "ms"),
        (1_000_000_000, "s"),
        (60_000_000_000, "min"),
        (3_600_000_000_000, "h"),
    ];
    let nanos = duration.as_nanos();
    for (index, &(unit, suffix)) in UNITS.iter().enumerate() {
        if index + 1 < UNITS.len() && nanos >= UNITS[index + 1].0 {
            continue;
        }
        let whole = nanos / unit;
        let decimals = if whole < 10 {
            2
        } else if whole < 100 {
            1
        } else {
            0
        };
        let scale = 10u128.pow(decimals);
        let mut quantum = 1;
        while whole / quantum >= 1_000 {
            quantum *= 10;
        }
        let divisor = unit * quantum;
        let rounded = (nanos * scale + divisor / 2) / divisor * quantum;
        // Rounding may promote into the next unit (including seconds/minutes).
        if index + 1 < UNITS.len() && rounded * unit >= UNITS[index + 1].0 * scale {
            continue;
        }
        let integer = rounded / scale;
        let mut fraction = rounded % scale;
        let mut width = decimals as usize;
        if fraction == 0 {
            return format!("{integer}{suffix}");
        }
        while fraction.is_multiple_of(10) {
            fraction /= 10;
            width -= 1;
        }
        return format!("{integer}.{fraction:0width$}{suffix}");
    }
    unreachable!("hours is the final unit")
}

#[cfg(test)]
mod duration_tests {
    use super::*;

    #[test]
    fn precision_and_transitions() {
        for (nanos, expected) in [
            (0, "0ns"),
            (1, "1ns"),
            (842, "842ns"),
            (999, "999ns"),
            (1_000, "1µs"),
            (1_234, "1.23µs"),
            (9_995, "10µs"),
            (99_950, "100µs"),
            (842_000, "842µs"),
            (999_499, "999µs"),
            (999_500, "1ms"),
            (1_234_000, "1.23ms"),
            (12_345_000, "12.3ms"),
            (147_600_000, "148ms"),
            (999_500_000, "1s"),
            (1_234_000_000, "1.23s"),
            (59_949_999_999, "59.9s"),
            (59_950_000_000, "1min"),
            (60_000_000_000, "1min"),
            (90_000_000_000, "1.5min"),
            (3_597_000_000_000, "1h"),
            (3_600_000_000_000, "1h"),
        ] {
            assert_eq!(
                format_duration(Duration::from_nanos(nanos)),
                expected,
                "{nanos}ns"
            );
        }
    }

    #[test]
    fn large_durations_and_stored_microseconds() {
        assert_eq!(format_duration(Duration::MAX), "5120000000000000h");
        assert_eq!(format_duration(Duration::from_secs(4_444_444)), "1230h");
        let measured = Duration::from_micros(1_234_567);
        assert_eq!(format_duration(measured), "1.23s");
        assert_eq!(measured.as_micros(), 1_234_567);
    }
}
