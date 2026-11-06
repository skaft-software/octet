//! Integer-microdollar cost accounting.

#![deny(clippy::float_arithmetic)]

use crate::error::PricingError;
use crate::types::Usage;
use serde::{Deserialize, Serialize};

/// Number of picodollars in one microdollar. Cost totals retain a remainder in
/// this finer unit so repeated small requests accumulate without float drift.
pub const PICODOLLARS_PER_MICRODOLLAR: u32 = 1_000_000;

/// Minutes in one UTC day: the period every [`PricingWindow`] repeats over.
pub const MINUTES_PER_DAY_UTC: u16 = 24 * 60;

/// Seconds in one UTC day. A request start time is minute-resolved for pricing.
const SECONDS_PER_DAY_UTC: u64 = 24 * 60 * 60;

/// Rate for tokens, represented in microdollars per 1,000,000 tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TokenRate(pub u64);

/// Pricing rules for a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pricing {
    /// Rate for prompt input tokens.
    pub input: TokenRate,
    /// Rate for generated output tokens.
    pub output: TokenRate,
    /// Rate for cached input tokens that were read.
    pub cache_read: TokenRate,
    /// Rate for input tokens that caused a cache write (5m duration).
    pub cache_write_5m: TokenRate,
    /// Rate for input tokens that caused a cache write (1h duration).
    pub cache_write_1h: Option<TokenRate>,
    /// Rate for reasoning tokens.
    pub reasoning: Option<TokenRate>,
    /// Tiered pricing rules.
    pub tiers: Vec<PricingTier>,
    /// Daily UTC windows that replace the base rates for their minutes.
    ///
    /// A window overrides the rates above; the quantity [`PricingTier`]s still
    /// apply on top, so the most specific published number wins for each rate.
    /// Requests outside every window bill the base rates. An empty schedule is
    /// every model without time-of-day prices, and it is the only form older
    /// catalogs contain, so it is not serialized at all.
    #[serde(default, skip_serializing_if = "PricingWindows::is_empty")]
    pub windows: PricingWindows,
}

/// A pricing tier with overrides.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingTier {
    /// Minimum input tokens required to qualify for this tier.
    pub min_input_tokens: u64,
    /// Override rate for prompt input tokens.
    pub input: Option<TokenRate>,
    /// Override rate for generated output tokens.
    pub output: Option<TokenRate>,
    /// Override rate for cached input tokens that were read.
    pub cache_read: Option<TokenRate>,
    /// Override rate for input tokens that caused a cache write (5m duration).
    pub cache_write_5m: Option<TokenRate>,
    /// Override rate for input tokens that caused a cache write (1h duration).
    pub cache_write_1h: Option<TokenRate>,
    /// Override rate for reasoning tokens.
    pub reasoning: Option<TokenRate>,
}

/// One repeating daily window that overrides a model's base rates.
///
/// Windows are half-open UTC minute ranges: [`Self::start_minute_utc`] is
/// included and [`Self::end_minute_utc`] is not, so an off-peak window can end
/// exactly where the next one begins. An end at or below the start wraps past
/// midnight — `start = 990` (16:30) with `end = 30` (00:30) covers 16:30–00:30
/// UTC — which is how providers publish a discount that crosses the UTC date
/// line.
///
/// A window carries the same per-rate overrides a [`PricingTier`] carries. It
/// replaces the base rate for each rate it publishes, and leaves every rate it
/// omits at the base rate; quantity tiers still apply on top.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingWindow {
    /// First minute of the window (UTC), inclusive.
    pub start_minute_utc: u16,
    /// First minute after the window (UTC), exclusive. A value lower than
    /// `start_minute_utc` wraps past midnight.
    pub end_minute_utc: u16,
    /// Override rate for prompt input tokens.
    #[serde(default)]
    pub input: Option<TokenRate>,
    /// Override rate for generated output tokens.
    #[serde(default)]
    pub output: Option<TokenRate>,
    /// Override rate for cached input tokens that were read.
    #[serde(default)]
    pub cache_read: Option<TokenRate>,
    /// Override rate for input tokens that caused a cache write (5m duration).
    #[serde(default)]
    pub cache_write_5m: Option<TokenRate>,
    /// Override rate for input tokens that caused a cache write (1h duration).
    #[serde(default)]
    pub cache_write_1h: Option<TokenRate>,
    /// Override rate for reasoning tokens.
    #[serde(default)]
    pub reasoning: Option<TokenRate>,
}

impl PricingWindow {
    /// Whether `minute_of_day` falls inside this half-open UTC window.
    pub fn contains(&self, minute_of_day: u16) -> bool {
        if self.start_minute_utc < self.end_minute_utc {
            (self.start_minute_utc..self.end_minute_utc).contains(&minute_of_day)
        } else {
            minute_of_day >= self.start_minute_utc || minute_of_day < self.end_minute_utc
        }
    }

    fn overrides(&self) -> RateOverrides {
        RateOverrides {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write_5m: self.cache_write_5m,
            cache_write_1h: self.cache_write_1h,
            reasoning: self.reasoning,
        }
    }
}

/// Why a model's daily price schedule was refused.
///
/// A schedule is validated once, when it is loaded, so every later cost
/// calculation can select a window without re-checking it. A catalog whose
/// windows do not describe a single tariff per minute fails to load instead of
/// being billed at whichever rate happened to be listed first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PricingWindowDefect {
    /// A window names a minute outside the UTC day.
    MinuteOutOfRange {
        /// Index of the offending window in the published list.
        position: usize,
        /// The window's start minute.
        start: u16,
        /// The window's end minute.
        end: u16,
    },
    /// A window starts where it ends, so it covers either no minute at all or
    /// the whole day. Neither can be billed unambiguously.
    ZeroLength {
        /// Index of the offending window in the published list.
        position: usize,
        /// The shared start and end minute.
        minute: u16,
    },
    /// Two windows claim the same minute, so they publish two tariffs for it.
    Overlap {
        /// Index of the earlier window in the published list.
        first: usize,
        /// Index of the later window in the published list.
        second: usize,
        /// The first minute both windows claim.
        minute: u16,
    },
}

impl std::fmt::Display for PricingWindowDefect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MinuteOutOfRange { position, start, end } => write!(
                formatter,
                "pricing window {position} at minute {start}..{end} is outside the {MINUTES_PER_DAY_UTC}-minute UTC day"
            ),
            Self::ZeroLength { position, minute } => write!(
                formatter,
                "pricing window {position} starts and ends at minute {minute} UTC"
            ),
            Self::Overlap {
                first,
                second,
                minute,
            } => write!(
                formatter,
                "pricing windows {first} and {second} both claim minute {minute} UTC"
            ),
        }
    }
}

impl std::error::Error for PricingWindowDefect {}

/// A model's daily price schedule: the validated list of [`PricingWindow`]s.
///
/// Empty means the model has no time-of-day prices, so every request bills the
/// base rates. A schedule is validated exactly once, by [`Self::new`] or by
/// deserialization, and holds no overlaps afterwards; cost accounting therefore
/// selects at most one window for a request start minute and never guesses
/// between competing quotes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct PricingWindows(Vec<PricingWindow>);

impl PricingWindows {
    /// Validates `windows` into a schedule that prices every minute once.
    ///
    /// # Errors
    ///
    /// Returns the first [`PricingWindowDefect`]: a minute outside the UTC day,
    /// a window that starts where it ends, or two windows that claim the same
    /// minute.
    pub fn new(windows: Vec<PricingWindow>) -> Result<Self, PricingWindowDefect> {
        let mut claimed = [None; MINUTES_PER_DAY_UTC as usize];
        for (position, window) in windows.iter().enumerate() {
            if window.start_minute_utc >= MINUTES_PER_DAY_UTC
                || window.end_minute_utc >= MINUTES_PER_DAY_UTC
            {
                return Err(PricingWindowDefect::MinuteOutOfRange {
                    position,
                    start: window.start_minute_utc,
                    end: window.end_minute_utc,
                });
            }
            if window.start_minute_utc == window.end_minute_utc {
                return Err(PricingWindowDefect::ZeroLength {
                    position,
                    minute: window.start_minute_utc,
                });
            }
            for minute in 0..MINUTES_PER_DAY_UTC {
                if !window.contains(minute) {
                    continue;
                }
                if let Some(first) = claimed[usize::from(minute)] {
                    return Err(PricingWindowDefect::Overlap {
                        first,
                        second: position,
                        minute,
                    });
                }
                claimed[usize::from(minute)] = Some(position);
            }
        }
        Ok(Self(windows))
    }

    /// Whether the model has no time-of-day prices.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The windows in published order.
    pub fn as_slice(&self) -> &[PricingWindow] {
        &self.0
    }

    /// The window containing `minute_of_day` (UTC), when the schedule has one.
    pub fn window_at(&self, minute_of_day: u16) -> Option<&PricingWindow> {
        self.0
            .iter()
            .find(|window| window.contains(minute_of_day))
    }
}

impl<'de> Deserialize<'de> for PricingWindows {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let windows = Vec::<PricingWindow>::deserialize(deserializer)?;
        Self::new(windows).map_err(serde::de::Error::custom)
    }
}

/// Cost calculation result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Cost for prompt input tokens.
    pub input: u64,
    /// Cost for generated output tokens excluding reasoning tokens.
    pub output: u64,
    /// Cost for generated reasoning tokens. This is a subset of output tokens.
    #[serde(default)]
    pub reasoning: u64,
    /// Cost for cached input tokens that were read.
    pub cache_read: u64,
    /// Cost for input tokens that caused a cache write.
    pub cache_write: u64,
    /// Whole-microdollar portion of the total request cost. It is extracted
    /// only after summing exact category numerators, so it can exceed the sum
    /// of individually floored category fields by a few microdollars.
    pub total: u64,
    /// Remaining picodollars after extracting `total`. Always less than one
    /// microdollar; callers can carry it across requests without using floats.
    #[serde(default)]
    pub total_picodollars_remainder: u32,
}

struct ActiveRates {
    input: TokenRate,
    output: TokenRate,
    cache_read: TokenRate,
    cache_write_5m: TokenRate,
    cache_write_1h: Option<TokenRate>,
    reasoning: Option<TokenRate>,
}

/// Rate overrides that a daily window or a quantity tier applies on top of the
/// rates selected so far. Absent overrides keep the rate they inherit.
#[derive(Clone, Copy)]
struct RateOverrides {
    input: Option<TokenRate>,
    output: Option<TokenRate>,
    cache_read: Option<TokenRate>,
    cache_write_5m: Option<TokenRate>,
    cache_write_1h: Option<TokenRate>,
    reasoning: Option<TokenRate>,
}

impl RateOverrides {
    fn apply(self, rates: &mut ActiveRates) {
        if let Some(rate) = self.input {
            rates.input = rate;
        }
        if let Some(rate) = self.output {
            rates.output = rate;
        }
        if let Some(rate) = self.cache_read {
            rates.cache_read = rate;
        }
        if let Some(rate) = self.cache_write_5m {
            rates.cache_write_5m = rate;
        }
        if let Some(rate) = self.cache_write_1h {
            rates.cache_write_1h = Some(rate);
        }
        if let Some(rate) = self.reasoning {
            rates.reasoning = Some(rate);
        }
    }
}

impl PricingTier {
    fn overrides(&self) -> RateOverrides {
        RateOverrides {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write_5m: self.cache_write_5m,
            cache_write_1h: self.cache_write_1h,
            reasoning: self.reasoning,
        }
    }
}

fn apply_tier(tier: &PricingTier, input_bucket: u64, rates: &mut ActiveRates) {
    if input_bucket < tier.min_input_tokens {
        return;
    }
    tier.overrides().apply(rates);
}

/// The UTC minute of the day a request started in, for window selection.
fn minute_of_day_utc(unix_seconds: u64) -> u16 {
    // A minute is `< 1440`, so the narrowing is lossless.
    ((unix_seconds % SECONDS_PER_DAY_UTC) / 60) as u16
}

/// Calculates the cost of a request based on pricing rules and usage counters.
///
/// Catalog-validated pricing tiers take the hot path without cloning or
/// sorting. Public callers can still pass an unsorted [`Pricing`]; that slow
/// fallback preserves the original order-independent behavior.
///
/// This form has no request start time, so it never selects a daily
/// [`PricingWindow`]: it bills the base rates plus the quantity tiers. That is
/// the published standard tariff, which is the safe upper bound for a hard cost
/// ceiling. Use [`cost_at`] whenever the request start time is known.
pub fn cost_of(pricing: &Pricing, usage: &Usage) -> Result<Cost, PricingError> {
    cost_of_ratio(pricing, usage, None, 1, 1)
        .map(|cost| cost.expect("integer catalog rates produce whole picodollars"))
}

/// Calculates the cost of a request that started at `started_at_unix_seconds`.
///
/// The start time — never the moment the response completed — selects the daily
/// [`PricingWindow`] that replaces the base rates for its UTC minute; quantity
/// tiers still apply on top of it, and a request outside every window bills the
/// base rates. Passing the start time is what makes a peak/off-peak tariff bill
/// the tariff that was in force when the request was made.
pub fn cost_at(
    pricing: &Pricing,
    usage: &Usage,
    started_at_unix_seconds: u64,
) -> Result<Cost, PricingError> {
    cost_of_ratio(pricing, usage, Some(started_at_unix_seconds), 1, 1)
        .map(|cost| cost.expect("integer catalog rates produce whole picodollars"))
}

/// Computes a Responses request's cost using an explicitly declared tier tariff.
///
/// The Codex runtime profile declares Pi's flex (1/2) and priority (2, or 5/2
/// for the exact API model `gpt-5.5`) schedule. No provider name or URL is guessed.
/// A Codex `default` echo retains requested flex/priority; other terminal echoes
/// override the request. Absent echoes fall back to the requested tier.
///
/// Unknown tiers, unresolved `auto`, and premium/discounted tiers on undeclared
/// profiles return `None`, not a fictional catalog-rate cost. Sub-picodollar
/// totals that cannot fit [`Cost`] also return `None` rather than rounding down.
/// Hosts can use this same function with no echo for request-budget estimates.
/// Usage buckets, long-context price tiers, and fractional category costs are
/// preserved before applying the integer ratio.
pub fn responses_cost_of(
    pricing: &Pricing,
    usage: &Usage,
    profile: crate::types::ResponsesRuntimeProfile,
    api_name: &str,
    requested: Option<crate::types::ServiceTier>,
    echoed: Option<&str>,
) -> Result<Option<Cost>, PricingError> {
    match responses_multiplier(profile, api_name, requested, echoed) {
        Some((numerator, denominator)) => {
            cost_of_ratio(pricing, usage, None, numerator, denominator)
        }
        None => Ok(None),
    }
}

/// Computes a Responses request's cost from the tariff in force when it started.
///
/// This is [`responses_cost_of`] with the start-time selection of [`cost_at`]: a
/// daily [`PricingWindow`] containing the request's UTC start minute replaces the
/// base rates, quantity tiers still apply on top, and the service-tier ratio is
/// preserved. The declared-tier rules, and the cases that return `None` instead
/// of a fictional cost, are exactly [`responses_cost_of`]'s.
pub fn responses_cost_at(
    pricing: &Pricing,
    usage: &Usage,
    started_at_unix_seconds: u64,
    profile: crate::types::ResponsesRuntimeProfile,
    api_name: &str,
    requested: Option<crate::types::ServiceTier>,
    echoed: Option<&str>,
) -> Result<Option<Cost>, PricingError> {
    match responses_multiplier(profile, api_name, requested, echoed) {
        Some((numerator, denominator)) => cost_of_ratio(
            pricing,
            usage,
            Some(started_at_unix_seconds),
            numerator,
            denominator,
        ),
        None => Ok(None),
    }
}

/// The declared service-tier ratio for one Responses request, or `None` when the
/// tier cannot be priced from a declaration alone.
fn responses_multiplier(
    profile: crate::types::ResponsesRuntimeProfile,
    api_name: &str,
    requested: Option<crate::types::ServiceTier>,
    echoed: Option<&str>,
) -> Option<(u128, u128)> {
    use crate::types::{ResponsesRuntimeProfile, ServiceTier};
    let tier = match echoed {
        Some("default")
            if profile == ResponsesRuntimeProfile::Codex
                && matches!(requested, Some(ServiceTier::Flex | ServiceTier::Priority)) =>
        {
            requested
        }
        Some("default") => Some(ServiceTier::Default),
        Some("flex") => Some(ServiceTier::Flex),
        Some("priority") => Some(ServiceTier::Priority),
        Some("auto") => Some(ServiceTier::Auto),
        Some(_) => return None,
        None => requested,
    };
    match tier {
        None | Some(ServiceTier::Default) => Some((1, 1)),
        Some(ServiceTier::Flex) if profile == ResponsesRuntimeProfile::Codex => Some((1, 2)),
        Some(ServiceTier::Priority) if profile == ResponsesRuntimeProfile::Codex => {
            Some(if api_name == "gpt-5.5" {
                (5, 2)
            } else {
                (2, 1)
            })
        }
        _ => None,
    }
}

fn cost_of_ratio(
    pricing: &Pricing,
    usage: &Usage,
    started_at_unix_seconds: Option<u64>,
    multiplier_numerator: u128,
    multiplier_denominator: u128,
) -> Result<Option<Cost>, PricingError> {
    // 1. Check subset invariants
    if usage.cache_write_1h_tokens > usage.cache_write_tokens {
        return Err(PricingError::InvalidUsageBuckets);
    }
    if usage.reasoning_tokens > usage.output_tokens {
        return Err(PricingError::InvalidUsageBuckets);
    }

    // 2. Select active rates: the base rates, then the daily window that
    // contains the request's own start minute (UTC), then the quantity tiers.
    // A window replaces the base rate it publishes and quantity tiers still
    // apply on top, so the most specific published number wins per rate.
    let input_bucket = usage
        .input_tokens
        .checked_add(usage.cache_read_tokens)
        .ok_or(PricingError::ArithmeticOverflow)?
        .checked_add(usage.cache_write_tokens)
        .ok_or(PricingError::ArithmeticOverflow)?;

    let mut rates = ActiveRates {
        input: pricing.input,
        output: pricing.output,
        cache_read: pricing.cache_read,
        cache_write_5m: pricing.cache_write_5m,
        cache_write_1h: pricing.cache_write_1h,
        reasoning: pricing.reasoning,
    };

    if let Some(window) = started_at_unix_seconds
        .map(minute_of_day_utc)
        .and_then(|minute| pricing.windows.window_at(minute))
    {
        window.overrides().apply(&mut rates);
    }

    if pricing
        .tiers
        .windows(2)
        .all(|pair| pair[0].min_input_tokens <= pair[1].min_input_tokens)
    {
        for tier in &pricing.tiers {
            apply_tier(tier, input_bucket, &mut rates);
        }
    } else {
        let mut sorted_tiers = pricing.tiers.clone();
        sorted_tiers.sort_unstable_by_key(|tier| tier.min_input_tokens);
        for tier in &sorted_tiers {
            apply_tier(tier, input_bucket, &mut rates);
        }
    }

    // 3. Compute exact category numerators in millionths of a microdollar.
    // Individual category fields remain whole microdollars, but `total` and
    // its remainder are split only after these numerators are summed. Flooring
    // every category first systematically undercounts small requests.
    const RATE_DENOMINATOR: u128 = PICODOLLARS_PER_MICRODOLLAR as u128;
    let input_numerator = usage.input_tokens as u128 * rates.input.0 as u128;
    let cache_read_numerator = usage.cache_read_tokens as u128 * rates.cache_read.0 as u128;

    // Cache write cost: 5m and 1h separate.
    let cache_write_5m_tokens = usage.cache_write_tokens - usage.cache_write_1h_tokens;
    let cache_write_5m_numerator = cache_write_5m_tokens as u128 * rates.cache_write_5m.0 as u128;

    // Anthropic's documented one-hour write price is 2x normal input, not
    // the 5-minute write price (1.25x). Catalogs that know a provider-specific
    // rate still override this default explicitly.
    let cache_write_1h_numerator = if usage.cache_write_1h_tokens == 0 {
        0
    } else {
        let active_rate = match rates.cache_write_1h {
            Some(rate) => rate,
            None => TokenRate(
                rates
                    .input
                    .0
                    .checked_mul(2)
                    .ok_or(PricingError::ArithmeticOverflow)?,
            ),
        };
        usage.cache_write_1h_tokens as u128 * active_rate.0 as u128
    };
    let cache_write_numerator = cache_write_5m_numerator
        .checked_add(cache_write_1h_numerator)
        .ok_or(PricingError::ArithmeticOverflow)?;

    // Output cost: reasoning and regular output separate.
    let output_standard_tokens = usage.output_tokens - usage.reasoning_tokens;
    let output_standard_numerator = output_standard_tokens as u128 * rates.output.0 as u128;
    let active_reasoning_rate = rates.reasoning.unwrap_or(rates.output);
    let reasoning_numerator = usage.reasoning_tokens as u128 * active_reasoning_rate.0 as u128;
    let output_numerator = output_standard_numerator
        .checked_add(reasoning_numerator)
        .ok_or(PricingError::ArithmeticOverflow)?;

    let total_numerator = input_numerator
        .checked_add(cache_read_numerator)
        .and_then(|value| value.checked_add(cache_write_numerator))
        .and_then(|value| value.checked_add(output_numerator))
        .ok_or(PricingError::ArithmeticOverflow)?;

    // Scale exact category numerators, never their already-floored fields.
    let scale = |value: u128| {
        value
            .checked_mul(multiplier_numerator)
            .ok_or(PricingError::ArithmeticOverflow)
    };
    let input_numerator = scale(input_numerator)?;
    let cache_read_numerator = scale(cache_read_numerator)?;
    let cache_write_numerator = scale(cache_write_numerator)?;
    let output_standard_numerator = scale(output_standard_numerator)?;
    let reasoning_numerator = scale(reasoning_numerator)?;
    let total_numerator = scale(total_numerator)?;
    if total_numerator % multiplier_denominator != 0 {
        return Ok(None);
    }
    let total_numerator = total_numerator / multiplier_denominator;
    let category_denominator = RATE_DENOMINATOR * multiplier_denominator;

    // 4. Split the exact request total into whole microdollars plus a
    // picodollar remainder, then safely downcast all fields.
    let input = u64::try_from(input_numerator / category_denominator)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let cache_read = u64::try_from(cache_read_numerator / category_denominator)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let cache_write = u64::try_from(cache_write_numerator / category_denominator)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let output = u64::try_from(output_standard_numerator / category_denominator)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let reasoning = u64::try_from(reasoning_numerator / category_denominator)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let total = u64::try_from(total_numerator / RATE_DENOMINATOR)
        .map_err(|_| PricingError::ArithmeticOverflow)?;
    let total_picodollars_remainder = u32::try_from(total_numerator % RATE_DENOMINATOR)
        .map_err(|_| PricingError::ArithmeticOverflow)?;

    Ok(Some(Cost {
        input,
        output,
        cache_read,
        cache_write,
        reasoning,
        total,
        total_picodollars_remainder,
    }))
}

#[cfg(test)]
mod tests;
