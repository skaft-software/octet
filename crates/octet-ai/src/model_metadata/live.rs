//! Live models.dev metadata.
//!
//! The same extraction as the maintainer refresh
//! (`scripts/refresh-models-dev-pricing.py`), applied at runtime to a
//! background-fetched catalog. Every record is checked against the compiled
//! snapshot before it can replace or extend it; a record that fails a check
//! is left out, so that model keeps its built-in data. A parity test runs both
//! extractions over one fixture, so they cannot drift apart.

use std::collections::BTreeMap;

use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// Mirrors of the refresh script's membership tables. Keep them in sync with
// `scripts/refresh-models-dev-pricing.py`; the parity test covers each rule.
const UNVERIFIED_PRICING_PROVIDERS: &[&str] = &["deepseek"];
const CAPABILITY_FIELDS: &[&str] = &[
    "name",
    "limit",
    "modalities",
    "tool_call",
    "structured_output",
    "reasoning",
    "reasoning_options",
    "interleaved",
];
const UNSUPPORTED_MODEL_IDS: &[(&str, &str)] = &[("openai", "gpt-5.6")];
const TEXT_ONLY_MODEL_IDS: &[(&str, &str)] = &[
    ("baseten", "zai-org/GLM-5.2"),
    ("baseten", "zai-org/GLM-5.2-Fast"),
];
const NAME_SOURCES: &[&str] = &[
    "alibaba",
    "anthropic",
    "cohere",
    "deepreinforce",
    "deepseek",
    "google",
    "meituan",
    "meta",
    "microsoft",
    "minimax",
    "mistral",
    "moonshotai",
    "nvidia",
    "openai",
    "perplexity",
    "poolside",
    "sakana",
    "sarvam",
    "stepfun",
    "tencent",
    "thinkingmachines",
    "xai",
    "xiaomi",
    "zhipuai",
];
/// `(octet provider, models.dev provider)`, sorted by octet provider.
const PROVIDER_SOURCES: &[(&str, &str)] = &[
    ("anthropic", "anthropic"),
    ("baseten", "baseten"),
    ("cerebras", "cerebras"),
    ("deepseek", "deepseek"),
    ("fireworks", "fireworks-ai"),
    ("groq", "groq"),
    ("huggingface", "huggingface"),
    ("minimax", "minimax"),
    ("moonshotai", "moonshotai"),
    ("nvidia", "nvidia"),
    ("openai", "openai"),
    ("opencode", "opencode"),
    ("openrouter", "openrouter"),
    ("qwen-token-plan", "alibaba-token-plan"),
    ("qwen-token-plan-cn", "alibaba-token-plan-cn"),
    ("qwen-token-plan-individual", "alibaba-token-plan"),
    ("together", "togetherai"),
    ("xai", "xai"),
    ("xiaomi", "xiaomi"),
    ("zai-coding-cn", "zhipuai-coding-plan"),
];
const MODEL_REQUIRES_TOOL_CALL: &[&str] = &[
    "qwen-token-plan",
    "qwen-token-plan-cn",
    "qwen-token-plan-individual",
    "zai-coding-cn",
];
const MODEL_SKIPS_DEPRECATED: &[&str] = &["baseten"];
const EXCLUDED_MODEL_IDS: &[(&str, &str)] = &[
    ("alibaba-token-plan", "qwen3.8-max-preview"),
    ("alibaba-token-plan-cn", "qwen3.8-max-preview"),
];
const QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS: &[&str] = &[
    "deepseek-v4-flash-0731",
    "deepseek-v4-pro",
    "deepseek-v4-pro-0813",
    "glm-5.2",
    "qwen3.6-flash",
    "qwen3.7-max",
    "qwen3.7-plus",
    "qwen3.8-flash",
    "qwen3.8-max",
];
const MODEL_ALLOWLISTS: &[(&str, &[&str])] = &[(
    "qwen-token-plan-individual",
    QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS,
)];
const PRICING_FALLBACK_SOURCES: &[(&str, &str)] = &[("zai-coding-cn", "zai")];

/// A live price may fall at most this many times below the snapshot's.
const MAX_PRICE_DROP: u64 = 10;
/// Above $100,000 per million tokens a rate is treated as malformed.
const MAX_RATE_MICRODOLLARS: u64 = 100_000_000_000;
/// Context or output limits beyond this are treated as malformed. Zero is a
/// legitimate upstream value (audio and image models), as is an output limit
/// above the context limit; the compiled snapshot carries both.
const MAX_TOKEN_LIMIT: u64 = 100_000_000;
const MAX_NAME_BYTES: usize = 256;

/// Per-million-token rates in microdollars, as in the compiled snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePricing {
    /// Input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache reads (zero when unpublished).
    pub cache_read: u64,
    /// Five-minute cache writes (zero when unpublished).
    pub cache_write_5m: u64,
    /// Reasoning tokens, when priced separately.
    pub reasoning: Option<u64>,
}

/// Validated models.dev metadata, keyed exactly like the compiled snapshot:
/// lowercase `provider/model` for names and pricing, and the exact
/// `provider/model` route for capability records.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveModelMetadata {
    /// Display names by lowercase `provider/model`.
    pub names: BTreeMap<String, String>,
    /// Checked rates by lowercase `provider/model`.
    pub pricing: BTreeMap<String, LivePricing>,
    /// Checked capability records by exact `provider/model` route, each as the
    /// compact JSON of the record that passed its checks. One record is decoded
    /// per lookup, exactly like the compiled snapshot: retaining a decoded value
    /// tree for every record cost about 3 MiB of private memory for a 0.42 MB
    /// cache (measured 2026-10-07).
    pub capabilities: BTreeMap<String, String>,
}

impl LiveModelMetadata {
    /// Whether every map is empty.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.pricing.is_empty() && self.capabilities.is_empty()
    }
}

/// Extract and check live metadata from one fetched models.dev `api.json` body.
///
/// Returns the accepted records and one `key: reason` line per record left
/// out. A body that is not an object, or that yields no records at all, is an
/// error: callers keep whatever metadata they already had.
///
/// Only what this extraction can read is materialized: the providers it names,
/// and within them only the fields it reads. The published catalog carries more
/// than two hundred providers and thousands of models, and building one
/// `serde_json::Value` tree for all of it peaked near 60 MiB for a 5 MiB body
/// (`--profile release` probe, 2026-10-07). The retained metadata is unchanged.
pub fn live_metadata_from_models_dev(
    body: &[u8],
) -> Result<(LiveModelMetadata, Vec<String>), String> {
    let catalog = serde_json::from_slice::<WantedCatalog>(body)
        .map_err(|error| error.to_string())?
        .0;
    let (extracted, mut rejected) = extract(&catalog)?;
    let mut accepted = LiveModelMetadata {
        names: extracted.names,
        ..LiveModelMetadata::default()
    };
    for (key, live) in extracted.pricing {
        match check_pricing(&live, super::lookup_pricing_rates(&key)) {
            Ok(()) => {
                accepted.pricing.insert(key, live);
            }
            Err(reason) => rejected.push(format!("{key}: {reason}")),
        }
    }
    for (key, record) in extracted.capabilities {
        let value: Value = serde_json::from_str(&record)
            .expect("a record produced by the extraction is valid JSON");
        match check_capabilities(&value) {
            Ok(()) => {
                accepted.capabilities.insert(key, record);
            }
            Err(reason) => rejected.push(format!("{key}: {reason}")),
        }
    }
    if accepted.is_empty() {
        return Err("models.dev catalog yielded no usable records".to_owned());
    }
    rejected.sort();
    Ok((accepted, rejected))
}

/// Whether one models.dev provider id can contribute a record this extraction
/// reads: a display-name source, a pricing or capability source, or a pricing
/// fallback. Everything else is skipped while deserializing.
fn needed_provider(id: &str) -> bool {
    NAME_SOURCES.contains(&id)
        || PROVIDER_SOURCES.iter().any(|(_, source)| *source == id)
        || PRICING_FALLBACK_SOURCES
            .iter()
            .any(|(_, fallback)| *fallback == id)
}

/// One model's fields that this extraction can read, and nothing else.
///
/// The published catalog carries far more per model — provider metadata,
/// playground links, release dates, knowledge cutoffs — and building a value
/// tree for all of it is what made one refresh peak near 60 MiB. Every field
/// stays a [`Value`] so a differently shaped field is ignored rather than
/// failing the refresh.
#[derive(Deserialize, Default)]
struct ModelRecord {
    name: Option<Value>,
    status: Option<Value>,
    tool_call: Option<Value>,
    cost: Option<Value>,
    limit: Option<Value>,
    modalities: Option<Value>,
    structured_output: Option<Value>,
    reasoning: Option<Value>,
    #[serde(rename = "reasoning_options")]
    reasoning_options: Option<Value>,
    interleaved: Option<Value>,
}

impl ModelRecord {
    /// The display name, when the catalog gives a usable one.
    fn name(&self) -> Option<&str> {
        self.name.as_ref().and_then(Value::as_str).map(str::trim)
    }

    /// One capability field as the catalog states it, when present.
    fn field(&self, name: &str) -> Option<&Value> {
        match name {
            "name" => self.name.as_ref(),
            "limit" => self.limit.as_ref(),
            "modalities" => self.modalities.as_ref(),
            "tool_call" => self.tool_call.as_ref(),
            "structured_output" => self.structured_output.as_ref(),
            "reasoning" => self.reasoning.as_ref(),
            "reasoning_options" => self.reasoning_options.as_ref(),
            "interleaved" => self.interleaved.as_ref(),
            _ => None,
        }
    }
}

/// One provider's model map, with anything that is not an object treated as
/// empty: a provider octet does not model may change shape without failing a
/// refresh, exactly as the previous value-walking code did.
#[derive(Default)]
struct ProviderRecord {
    models: BTreeMap<String, ModelRecord>,
}

impl<'de> Deserialize<'de> for ProviderRecord {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ProviderVisitor;
        impl<'de> Visitor<'de> for ProviderVisitor {
            type Value = ProviderRecord;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a models.dev provider")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut record = ProviderRecord::default();
                while let Some(key) = map.next_key::<String>()? {
                    if key == "models" {
                        record.models = map.next_value::<TolerantModels>()?.0;
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(record)
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(ProviderRecord::default())
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(ProviderRecord::default())
            }
        }
        deserializer.deserialize_any(ProviderVisitor)
    }
}

/// A provider's models, or an empty map when the catalog states something else.
#[derive(Default)]
struct TolerantModels(BTreeMap<String, ModelRecord>);

impl<'de> Deserialize<'de> for TolerantModels {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ModelsVisitor;
        impl<'de> Visitor<'de> for ModelsVisitor {
            type Value = TolerantModels;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a models.dev model map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut models = BTreeMap::new();
                while let Some(id) = map.next_key::<String>()? {
                    let record = map.next_value::<TolerantModel>()?;
                    models.insert(id, record.0);
                }
                Ok(TolerantModels(models))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(TolerantModels::default())
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(TolerantModels::default())
            }
        }
        deserializer.deserialize_any(ModelsVisitor)
    }
}

/// One model record, or an empty record when the catalog states something else.
struct TolerantModel(ModelRecord);

impl<'de> Deserialize<'de> for TolerantModel {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        Ok(TolerantModel(match value {
            Value::Object(map) => {
                serde_json::from_value::<ModelRecord>(Value::Object(map)).unwrap_or_default()
            }
            _ => ModelRecord::default(),
        }))
    }
}

/// The models.dev provider map with only the needed providers materialized.
struct WantedCatalog(BTreeMap<String, ProviderRecord>);

impl<'de> Deserialize<'de> for WantedCatalog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WantedVisitor;
        impl<'de> Visitor<'de> for WantedVisitor {
            type Value = WantedCatalog;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a models.dev provider map")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut providers = BTreeMap::new();
                while let Some(id) = map.next_key::<String>()? {
                    if needed_provider(&id) {
                        providers.insert(id, map.next_value::<ProviderRecord>()?);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                }
                Ok(WantedCatalog(providers))
            }
        }
        deserializer.deserialize_map(WantedVisitor)
    }
}

/// The providers this extraction reads, by models.dev id.
type WantedProviders = BTreeMap<String, ProviderRecord>;

/// The unchecked extraction, exactly as the refresh script performs it.
/// Malformed prices leave out their model instead of failing the catalog.
fn extract(catalog: &WantedProviders) -> Result<(LiveModelMetadata, Vec<String>), String> {
    let mut metadata = LiveModelMetadata::default();
    let mut rejected = Vec::new();

    for provider_id in NAME_SOURCES {
        let Some(models) = provider_models(catalog, provider_id) else {
            continue;
        };
        for (model_id, model) in models {
            if !supported_model(provider_id, model_id) {
                continue;
            }
            let Some(name) = model.name() else {
                continue;
            };
            if name.is_empty() || name.len() > MAX_NAME_BYTES {
                continue;
            }
            metadata.names.insert(
                format!("{provider_id}/{model_id}").to_lowercase(),
                name.to_owned(),
            );
        }
    }

    for (octet_provider, source_provider) in PROVIDER_SOURCES {
        let Some(models) = provider_models(catalog, source_provider) else {
            continue;
        };
        for (model_id, model) in models {
            if !supported_model(source_provider, model_id)
                || !catalog_model_included(octet_provider, source_provider, model_id, model)
            {
                continue;
            }
            if !UNVERIFIED_PRICING_PROVIDERS.contains(octet_provider) {
                if let Some(cost) =
                    pinned_cost(catalog, octet_provider, source_provider, model_id, model)
                {
                    let key = format!("{octet_provider}/{model_id}").to_lowercase();
                    match pricing_record(cost) {
                        Ok(pricing) => {
                            metadata.pricing.insert(key, pricing);
                        }
                        Err(reason) => rejected.push(format!("{key}: {reason}")),
                    }
                }
            }
            let mut record = serde_json::Map::new();
            for field in CAPABILITY_FIELDS {
                if let Some(value) = model.field(field) {
                    record.insert((*field).to_owned(), value.clone());
                }
            }
            if TEXT_ONLY_MODEL_IDS.contains(&(octet_provider, model_id.as_str())) {
                if let Some(input) = record
                    .get_mut("modalities")
                    .and_then(Value::as_object_mut)
                    .and_then(|modalities| modalities.get_mut("input"))
                    .and_then(Value::as_array_mut)
                {
                    input.retain(|item| item.as_str() != Some("image"));
                }
            }
            metadata.capabilities.insert(
                format!("{octet_provider}/{model_id}"),
                Value::Object(record).to_string(),
            );
        }
    }
    Ok((metadata, rejected))
}

fn provider_models<'a>(
    catalog: &'a WantedProviders,
    provider: &str,
) -> Option<&'a BTreeMap<String, ModelRecord>> {
    catalog.get(provider).map(|record| &record.models)
}

fn supported_model(provider_id: &str, model_id: &str) -> bool {
    !UNSUPPORTED_MODEL_IDS.contains(&(provider_id, model_id))
}

fn catalog_model_included(
    octet_provider: &str,
    source_provider: &str,
    model_id: &str,
    model: &ModelRecord,
) -> bool {
    if EXCLUDED_MODEL_IDS.contains(&(source_provider, model_id)) {
        return false;
    }
    if let Some((_, allowlist)) = MODEL_ALLOWLISTS
        .iter()
        .find(|(provider, _)| *provider == octet_provider)
    {
        if !allowlist.contains(&model_id) {
            return false;
        }
    }
    if MODEL_REQUIRES_TOOL_CALL.contains(&octet_provider)
        && model.tool_call.as_ref() != Some(&Value::Bool(true))
    {
        return false;
    }
    if MODEL_SKIPS_DEPRECATED.contains(&octet_provider)
        && model.status.as_ref().and_then(Value::as_str) == Some("deprecated")
    {
        return false;
    }
    true
}

fn has_input_and_output(cost: &Value) -> bool {
    cost.is_object()
        && cost.get("input").is_some_and(|value| !value.is_null())
        && cost.get("output").is_some_and(|value| !value.is_null())
}

fn pinned_cost<'a>(
    catalog: &'a WantedProviders,
    octet_provider: &str,
    source_provider: &str,
    model_id: &str,
    model: &'a ModelRecord,
) -> Option<&'a Value> {
    if let Some(cost) = model
        .cost
        .as_ref()
        .filter(|cost| has_input_and_output(cost))
    {
        return Some(cost);
    }
    let fallback = PRICING_FALLBACK_SOURCES
        .iter()
        .find(|(provider, _)| *provider == octet_provider)
        .map(|(_, fallback)| *fallback)?;
    if fallback == source_provider {
        return None;
    }
    provider_models(catalog, fallback)?
        .get(model_id)?
        .cost
        .as_ref()
        .filter(|cost| has_input_and_output(cost))
}

fn pricing_record(cost: &Value) -> Result<LivePricing, String> {
    Ok(LivePricing {
        input: microdollars(cost.get("input"))?,
        output: microdollars(cost.get("output"))?,
        cache_read: microdollars(cost.get("cache_read"))?,
        cache_write_5m: microdollars(cost.get("cache_write"))?,
        reasoning: match cost.get("reasoning") {
            None | Some(Value::Null) => None,
            value => Some(microdollars(value)?),
        },
    })
}

/// Convert a USD-per-million-token price to integer microdollars, rounding
/// half up exactly like the refresh script's `Decimal` conversion.
fn microdollars(value: Option<&Value>) -> Result<u64, String> {
    let number = match value {
        None | Some(Value::Null) => return Ok(0),
        Some(Value::Number(number)) => number.to_string(),
        Some(other) => return Err(format!("invalid price {other}")),
    };
    decimal_to_microdollars(&number).ok_or_else(|| format!("invalid price {number}"))
}

fn decimal_to_microdollars(text: &str) -> Option<u64> {
    if text.starts_with('-') {
        return None;
    }
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    let digits = format!("{whole}{fraction}");
    if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    // value = digits * 10^(exponent - fraction.len()); scale by 10^6.
    let shift = exponent
        .checked_add(6)?
        .checked_sub(i32::try_from(fraction.len()).ok()?)?;
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    if shift >= 0 {
        let shift = u32::try_from(shift).ok()?;
        let base: u128 = digits.parse().ok()?;
        let scaled = base.checked_mul(10u128.checked_pow(shift)?)?;
        return u64::try_from(scaled).ok();
    }
    let dropped = usize::try_from(shift.unsigned_abs()).ok()?;
    if dropped > digits.len() {
        return Some(0);
    }
    let (kept, rest) = digits.split_at(digits.len() - dropped);
    let mut value: u128 = if kept.is_empty() {
        0
    } else {
        kept.parse().ok()?
    };
    if rest.as_bytes().first().is_some_and(|digit| *digit >= b'5') {
        value = value.checked_add(1)?;
    }
    u64::try_from(value).ok()
}

/// A live rate replaces or extends the snapshot only when it is plausible
/// against it. Over-counting is safe for hard ceilings; under-counting is not.
fn check_pricing(live: &LivePricing, snapshot: Option<LivePricing>) -> Result<(), String> {
    let rates = [
        live.input,
        live.output,
        live.cache_read,
        live.cache_write_5m,
        live.reasoning.unwrap_or(0),
    ];
    if rates.iter().any(|rate| *rate > MAX_RATE_MICRODOLLARS) {
        return Err("price is implausibly high".to_owned());
    }
    match snapshot {
        Some(snapshot) => {
            for (name, live, built_in) in [
                ("input", live.input, snapshot.input),
                ("output", live.output, snapshot.output),
                ("cache_read", live.cache_read, snapshot.cache_read),
                ("cache_write", live.cache_write_5m, snapshot.cache_write_5m),
                // Missing reasoning prices use the output rate at billing.
                (
                    "reasoning",
                    live.reasoning.unwrap_or(live.output),
                    snapshot.reasoning.unwrap_or(snapshot.output),
                ),
            ] {
                if built_in > 0 && live == 0 {
                    return Err(format!("{name} price became zero"));
                }
                if live.saturating_mul(MAX_PRICE_DROP) < built_in {
                    return Err(format!("{name} price dropped more than {MAX_PRICE_DROP}x"));
                }
            }
            Ok(())
        }
        // A route new to octet is priced only with non-zero rates, so an
        // upstream placeholder can never make a hard ceiling treat it as free.
        None if live.input == 0 || live.output == 0 => {
            Err("a new model needs non-zero input and output prices".to_owned())
        }
        None => Ok(()),
    }
}

fn check_capabilities(record: &Value) -> Result<(), String> {
    let Some(limit) = record.get("limit") else {
        return Ok(());
    };
    if limit.is_null() {
        return Ok(());
    }
    let limit = limit
        .as_object()
        .ok_or_else(|| "limit is not an object".to_owned())?;
    for field in ["context", "input", "output"] {
        match limit.get(field) {
            None | Some(Value::Null) => {}
            Some(value) => {
                if value.as_u64().is_none_or(|tokens| tokens > MAX_TOKEN_LIMIT) {
                    return Err(format!("{field} limit is implausible"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
