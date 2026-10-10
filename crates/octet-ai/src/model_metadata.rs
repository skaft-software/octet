//! Human-facing model metadata generated from the models.dev canonical catalog.
//!
//! The build script consumes checked-in models.dev snapshots. Runtime code
//! uses binary searches over generated static data (decoding only a selected
//! capability record), so pricing remains
//! deterministic and available in offline builds.
//!
//! A host may install live metadata ([`install_live_metadata`]) extracted from
//! a newer models.dev catalog and checked record by record against this
//! snapshot ([`live_metadata_from_models_dev`]). Every lookup then prefers a
//! live record and falls back to the compiled snapshot for anything the live
//! catalog lacks or failed to justify.

use std::collections::BTreeMap;
use std::sync::{Arc, PoisonError, RwLock};

use crate::pricing::{Pricing, TokenRate};

mod live;
pub use live::{live_metadata_from_models_dev, LiveModelMetadata, LivePricing};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/models_dev_names.rs"));
    include!(concat!(env!("OUT_DIR"), "/models_dev_pricing.rs"));
    include!(concat!(env!("OUT_DIR"), "/models_dev_capabilities.rs"));
}

fn lookup(table: &'static [(&'static str, &'static str)], key: &str) -> Option<&'static str> {
    table
        .binary_search_by(|(candidate, _)| candidate.cmp(&key))
        .ok()
        .map(|index| table[index].1)
}

/// Installed live metadata plus its derived unique-leaf name aliases.
struct LiveOverlay {
    metadata: LiveModelMetadata,
    aliases: BTreeMap<String, String>,
}

impl LiveOverlay {
    fn new(metadata: LiveModelMetadata) -> Self {
        let mut leaves: BTreeMap<&str, usize> = BTreeMap::new();
        for key in metadata.names.keys() {
            *leaves.entry(leaf(key)).or_default() += 1;
        }
        let aliases = metadata
            .names
            .iter()
            .filter(|(key, _)| leaves.get(leaf(key)) == Some(&1))
            .map(|(key, name)| (leaf(key).to_owned(), name.clone()))
            .collect();
        Self { metadata, aliases }
    }
}

static LIVE: RwLock<Option<Arc<LiveOverlay>>> = RwLock::new(None);

/// Make checked live metadata authoritative for every later lookup, falling
/// back to the compiled snapshot per record.
///
/// Replacing the overlay releases the previous allocation. Lookups return
/// owned values and never retain a borrowed live record across replacement.
pub fn install_live_metadata(metadata: LiveModelMetadata) {
    *LIVE.write().unwrap_or_else(PoisonError::into_inner) =
        Some(Arc::new(LiveOverlay::new(metadata)));
}

/// Whether live models.dev metadata is installed in this process.
pub fn live_metadata_installed() -> bool {
    live_overlay().is_some()
}

fn live_overlay() -> Option<Arc<LiveOverlay>> {
    LIVE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

fn leaf(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or(key)
}

fn lookup_key(key: &str) -> Option<String> {
    lookup_key_in(live_overlay().as_deref(), key).map(str::to_owned)
}

fn lookup_key_in<'a>(overlay: Option<&'a LiveOverlay>, key: &str) -> Option<&'a str> {
    overlay
        .and_then(|overlay| overlay.metadata.names.get(key))
        .map(String::as_str)
        .or_else(|| lookup(generated::MODEL_NAMES, key))
        .or_else(|| {
            overlay
                .and_then(|overlay| overlay.aliases.get(leaf(key)))
                .map(String::as_str)
        })
        .or_else(|| lookup(generated::MODEL_NAME_ALIASES, leaf(key)))
}

/// The compiled snapshot's raw rates for one lowercase `provider/model` key.
fn lookup_pricing_rates(key: &str) -> Option<LivePricing> {
    generated::MODEL_PRICING
        .binary_search_by(|(candidate, ..)| candidate.cmp(&key))
        .ok()
        .map(|index| {
            let (_, input, output, cache_read, cache_write_5m, reasoning) =
                generated::MODEL_PRICING[index];
            LivePricing {
                input,
                output,
                cache_read,
                cache_write_5m,
                reasoning,
            }
        })
}

fn pricing_from_rates(rates: LivePricing) -> Pricing {
    Pricing {
        input: TokenRate(rates.input),
        output: TokenRate(rates.output),
        cache_read: TokenRate(rates.cache_read),
        cache_write_5m: TokenRate(rates.cache_write_5m),
        // `cost_of` applies Anthropic's documented 1-hour cache-write
        // default (2x input) when this provider-specific field is absent.
        cache_write_1h: None,
        reasoning: rates.reasoning.map(TokenRate),
        tiers: Vec::new(),
    }
}

fn lookup_pricing_in(overlay: Option<&LiveOverlay>, key: &str) -> Option<Pricing> {
    overlay
        .and_then(|overlay| overlay.metadata.pricing.get(key).copied())
        .or_else(|| lookup_pricing_rates(key))
        .map(pricing_from_rates)
}

/// Return models.dev pricing for a provider/model route: an installed live
/// record when one passed its checks, otherwise the checked-in snapshot's.
///
/// The key is provider-scoped because an aggregator can charge a different
/// rate for the same upstream model. Rates are represented as microdollars per
/// million tokens.
pub fn model_pricing(provider_id: &str, model_id: &str) -> Option<Pricing> {
    let provider = provider_id.trim().to_ascii_lowercase();
    let model = model_id.trim().to_ascii_lowercase();
    if provider.is_empty() || model.is_empty() {
        return None;
    }
    let key = format!("{provider}/{model}");
    lookup_pricing_in(live_overlay().as_deref(), &key)
}

/// Return models.dev source assertions for an exact built-in provider/model
/// route, preferring an installed live record over the pinned snapshot.
///
/// The snapshot's binary-search index decodes only the selected record, never
/// the entire snapshot. Callers must preserve endpoint assertions and constrain reasoning
/// controls to a known provider wire profile. No leaf aliases or inventory are
/// inferred here; Codex and custom endpoints have no entry in this index.
pub fn model_capability_metadata(provider_id: &str, model_id: &str) -> Option<serde_json::Value> {
    capability_metadata_in(
        live_overlay().as_deref(),
        &format!("{provider_id}/{model_id}"),
    )
}

fn capability_metadata_in(overlay: Option<&LiveOverlay>, key: &str) -> Option<serde_json::Value> {
    overlay
        .and_then(|overlay| overlay.metadata.capabilities.get(key))
        .map(|record| {
            serde_json::from_str(record).expect("a checked capability record is valid JSON")
        })
        .or_else(|| {
            lookup(generated::MODEL_CAPABILITIES, key)
                .map(|raw| serde_json::from_str(raw).expect("build-validated metadata JSON"))
        })
}

/// Return the models.dev display name for a canonical or uniquely identifiable
/// model ID.
///
/// Exact canonical IDs win. Bare model names are accepted only when their leaf
/// is unique in the generated catalog. The historical `custom/` registry prefix
/// is ignored, but repository/artifact suffixes are not guessed here; callers
/// can apply a conservative fallback for models absent from models.dev.
pub fn model_display_name(id: &str) -> Option<String> {
    let normalized = id.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    lookup_key(&normalized).or_else(|| normalized.strip_prefix("custom/").and_then(lookup_key))
}

#[cfg(test)]
mod tests;
