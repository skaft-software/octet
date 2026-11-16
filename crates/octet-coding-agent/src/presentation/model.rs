#![allow(missing_docs)]

//! Human-facing provider and model identity.
//!
//! This module converts provider/model identifiers into stable labels. It does
//! not own routing, execution, or terminal rendering.

use octet_ai::{ModelSpec, ProviderLifecycle, ProviderLifecycleState};

/// Compact provider name used by lifecycle/status presentation.
pub fn provider_status_name(canonical: &str) -> String {
    let canonical = canonical.trim();
    let friendly = match canonical.to_ascii_lowercase().as_str() {
        "openai-codex" | "codex" => Some("Codex"),
        "openai" => Some("OpenAI"),
        "anthropic" => Some("Anthropic"),
        "openrouter" => Some("OpenRouter"),
        "deepseek" => Some("DeepSeek"),
        "custom-openai" => Some("local endpoint"),
        _ => None,
    };
    friendly.unwrap_or(canonical).to_owned()
}

/// Compact transient label for an opt-in endpoint readiness update.
pub fn provider_lifecycle_label(provider: &str, lifecycle: &ProviderLifecycle) -> String {
    let provider = provider_status_name(provider);
    let label = match lifecycle.state {
        ProviderLifecycleState::Queued => format!("{provider} queued"),
        ProviderLifecycleState::Loading => format!("Loading {provider}"),
        ProviderLifecycleState::Ready => format!("{provider} ready"),
    };
    lifecycle
        .detail
        .as_deref()
        .filter(|detail| !detail.is_empty())
        .map_or(label.clone(), |detail| format!("{label} · {detail}"))
}

/// Shared model label cache for native, host-owned session and terminal surfaces.
/// Only presentation strings cross into the renderer, never endpoint credentials.
#[derive(Clone, Debug, Default)]
pub struct ModelLabels {
    labels: std::sync::Arc<std::collections::HashMap<String, String>>,
    provider_count: usize,
}

impl ModelLabels {
    pub fn from_catalog(catalog: &octet_ai::ModelCatalog) -> Self {
        let mut labels = std::collections::HashMap::new();
        let mut providers = std::collections::HashSet::new();
        for spec in catalog.models() {
            let name = ModelDisplayMetadata::resolve(spec).name;
            labels.insert(spec.id.0.clone(), name.clone());
            // Match the provider-qualified spelling accepted by ModelCatalog::resolve.
            labels
                .entry(format!("{}/{}", spec.endpoint.0, spec.id.0))
                .or_insert(name);
            providers.insert(spec.endpoint.0.clone());
        }
        Self {
            labels: std::sync::Arc::new(labels),
            provider_count: providers.len(),
        }
    }

    pub fn label(&self, id: &str) -> String {
        self.labels
            .get(id)
            .cloned()
            .unwrap_or_else(|| resolve_model_display_name(None, id, id))
    }

    pub fn provider_count(&self) -> usize {
        self.provider_count
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDisplayMetadata {
    pub name: String,
}

impl ModelDisplayMetadata {
    pub fn resolve(spec: &ModelSpec) -> Self {
        Self {
            name: resolve_model_display_name(
                spec.display_name.as_deref(),
                &spec.id.0,
                &spec.api_name,
            ),
        }
    }
}

/// One identity for every native surface: the complete API model ID, followed
/// by a distinct configured/inventory name. Native Rust owns terminal and
/// session rendering; the adapter cannot format these host-owned surfaces.
/// Pi compatibility: packages/coding-agent/src/modes/interactive/components/
/// model-selector.ts:328-334 uses ID, distinct name and provider; footer.ts:220-255
/// retains the complete ID and optionally prefixes the provider. octet also
/// preserves the distinct name outside the picker so its labels stay consistent.
pub fn resolve_model_display_name(
    configured: Option<&str>,
    canonical_id: &str,
    api_name: &str,
) -> String {
    let id = if api_name.is_empty() {
        canonical_id
    } else {
        api_name
    };
    // Pi: packages/coding-agent/src/modes/interactive/model-search.ts:13-34.
    // Names which only restate the ID's words/casing/separators add no identity.
    let words = |value: &str| {
        value
            .to_lowercase()
            .split(|ch: char| !ch.is_alphanumeric())
            .filter(|word| !word.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let id_words = words(id);
    let compact_id = id_words.concat();
    if let Some(name) = configured.map(str::trim).filter(|name| !name.is_empty()) {
        let distinct = words(name).iter().any(|word| {
            !id_words.contains(word) && (word.chars().count() == 1 || !compact_id.contains(word))
        });
        if distinct {
            return format!("{id} ({name})");
        }
    }
    id.to_owned()
}
