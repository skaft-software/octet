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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDisplayMetadata {
    pub name: String,
    pub compact_names: Vec<String>,
}

impl ModelDisplayMetadata {
    pub fn resolve(spec: &ModelSpec) -> Self {
        let name =
            resolve_model_display_name(spec.display_name.as_deref(), &spec.id.0, &spec.api_name);
        let compact_names = model_display_name_variants(&name);
        Self {
            name,
            compact_names,
        }
    }
}

/// Resolve the stable, human-facing model identity once when model metadata
/// changes. Renderers receive this result and never inspect provider IDs.
pub fn resolve_model_display_name(
    configured: Option<&str>,
    canonical_id: &str,
    provider_name: &str,
) -> String {
    if let Some(configured) = configured.map(str::trim).filter(|name| !name.is_empty()) {
        return configured.to_owned();
    }
    if let Some(registry_name) = octet_ai::model_metadata::model_display_name(canonical_id)
        .or_else(|| octet_ai::model_metadata::model_display_name(provider_name))
    {
        return registry_name.to_owned();
    }
    // Custom-endpoint configuration historically encoded an explicit label in
    // the canonical suffix. Honor it only when it is visibly a human label;
    // machine IDs and repository paths continue through the conservative
    // derivation below.
    if let Some(custom_name) = canonical_id.strip_prefix("custom/") {
        let custom_name = custom_name.trim();
        if custom_name.contains(char::is_whitespace)
            && !custom_name.contains('/')
            && custom_name != provider_name
        {
            return custom_name.to_owned();
        }
    }
    // A provider-supplied value containing ordinary words is likely a real
    // label. Machine IDs, paths, and artifact names go through the conservative
    // canonical derivation below instead.
    let provider_name = provider_name.trim();
    if provider_name.contains(char::is_whitespace)
        && !provider_name.contains('/')
        && !provider_name.eq_ignore_ascii_case(canonical_id)
    {
        return provider_name.to_owned();
    }
    let derived = derive_model_display_name(provider_name);
    if !provider_name.is_empty() && derived != provider_name {
        derived
    } else {
        derive_model_display_name(canonical_id)
    }
}

/// Conservative fallback for canonical IDs. Only recognized model families
/// are normalized; unfamiliar IDs are returned byte-for-byte (apart from
/// surrounding whitespace) rather than guessed at.
pub fn derive_model_display_name(canonical_id: &str) -> String {
    let original = canonical_id.trim();
    if original.is_empty() {
        return "model".to_owned();
    }

    let mut candidate = original;
    let mut recognized_prefix = false;
    for prefix in [
        "custom/",
        "openai/",
        "anthropic/",
        "deepseek/",
        "openrouter/",
        "models/",
    ] {
        if let Some(rest) = candidate.strip_prefix(prefix) {
            candidate = rest;
            recognized_prefix = true;
            break;
        }
    }
    let leaf = candidate.rsplit('/').next().unwrap_or(candidate);
    let family = model_family(leaf);
    if family.is_none() {
        return original.to_owned();
    }
    if candidate.contains('/') && !recognized_prefix {
        // `owner/model` may itself be a meaningful unfamiliar registry ID.
        return original.to_owned();
    }

    let mut words = leaf
        .trim_end_matches(|character: char| character == '.' || character.is_whitespace())
        .split(['-', '_', ' '])
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    while words.last().is_some_and(|word| artifact_suffix(word)) {
        words.pop();
    }
    // Quantization tags such as `Q4_K_M` are split into several words by the
    // normal tokenizer. Remove the group only when it forms a recognized
    // trailing artifact suffix on an already-recognized model family.
    if let Some(group_start) = trailing_quantization_group(&words) {
        words.truncate(group_start);
    }
    if words
        .last()
        .is_some_and(|word| word.eq_ignore_ascii_case("mtp"))
        && leaf.to_ascii_lowercase().contains("gguf")
    {
        words.pop();
    }
    if words.is_empty() {
        return original.to_owned();
    }

    match family.expect("checked above") {
        ModelFamily::Gpt => format_gpt(&words),
        ModelFamily::Qwen => format_known_words("Qwen", &words, true),
        ModelFamily::Claude => format_claude(&words),
        ModelFamily::DeepSeek => format_known_words("DeepSeek", &words, false),
        ModelFamily::Llama => format_known_words("Llama", &words, true),
        ModelFamily::Gemini => format_known_words("Gemini", &words, false),
        ModelFamily::Mistral => format_known_words("Mistral", &words, true),
    }
}

#[derive(Clone, Copy)]
enum ModelFamily {
    Gpt,
    Qwen,
    Claude,
    DeepSeek,
    Llama,
    Gemini,
    Mistral,
}

fn model_family(value: &str) -> Option<ModelFamily> {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("gpt-") || lower.starts_with("gpt_") {
        Some(ModelFamily::Gpt)
    } else if lower.starts_with("qwen") {
        Some(ModelFamily::Qwen)
    } else if lower.starts_with("claude-") || lower.starts_with("claude_") {
        Some(ModelFamily::Claude)
    } else if lower.starts_with("deepseek") {
        Some(ModelFamily::DeepSeek)
    } else if lower.starts_with("llama") {
        Some(ModelFamily::Llama)
    } else if lower.starts_with("gemini") {
        Some(ModelFamily::Gemini)
    } else if lower.starts_with("mistral") || lower.starts_with("mixtral") {
        Some(ModelFamily::Mistral)
    } else {
        None
    }
}

fn artifact_suffix(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower == "gguf"
        || lower == "awq"
        || lower == "gptq"
        || lower == "safetensors"
        || lower == "autoround"
        || lower == "onnx"
        || lower == "mlx"
        || lower == "instruct"
        || lower == "chat"
        || lower == "responses"
        || precision_suffix(&lower)
        || (lower.len() == 8
            && (lower.starts_with("19") || lower.starts_with("20"))
            && lower.chars().all(|character| character.is_ascii_digit()))
        || (lower.starts_with('q')
            && lower[1..].chars().all(|character| {
                character.is_ascii_digit() || matches!(character, 'k' | 'm' | 's' | 'l' | '_')
            }))
}

fn precision_suffix(value: &str) -> bool {
    ["int", "uint", "fp", "bf", "f"].iter().any(|prefix| {
        value
            .strip_prefix(prefix)
            .is_some_and(|bits| !bits.is_empty() && bits.chars().all(|c| c.is_ascii_digit()))
    })
}

fn trailing_quantization_group(words: &[String]) -> Option<usize> {
    let start = words.iter().rposition(|word| {
        let lower = word.to_ascii_lowercase();
        let digits = lower.strip_prefix('q').or_else(|| lower.strip_prefix("iq"));
        digits.is_some_and(|digits| {
            !digits.is_empty() && digits.chars().all(|character| character.is_ascii_digit())
        })
    })?;
    let suffix = &words[start + 1..];
    (!suffix.is_empty()
        && suffix.len() <= 3
        && suffix.iter().all(|word| {
            let lower = word.to_ascii_lowercase();
            lower.chars().all(|character| character.is_ascii_digit())
                || matches!(lower.as_str(), "k" | "m" | "s" | "l")
        }))
    .then_some(start)
}

fn format_gpt(words: &[String]) -> String {
    let Some(version) = words.get(1) else {
        return "GPT".to_owned();
    };
    let mut output = format!("GPT-{version}");
    for word in words.iter().skip(2) {
        output.push(' ');
        output.push_str(&display_word(word));
    }
    output
}

fn format_claude(words: &[String]) -> String {
    let mut output = vec!["Claude".to_owned()];
    let mut index = 1;
    while index < words.len() {
        if index + 1 < words.len()
            && words[index]
                .chars()
                .all(|character| character.is_ascii_digit())
            && words[index + 1]
                .chars()
                .all(|character| character.is_ascii_digit())
        {
            output.push(format!("{}.{}", words[index], words[index + 1]));
            index += 2;
        } else {
            output.push(display_word(&words[index]));
            index += 1;
        }
    }
    output.join(" ")
}

fn format_known_words(label: &str, words: &[String], drop_artifacts: bool) -> String {
    let mut output = Vec::with_capacity(words.len());
    let first = &words[0];
    let lower_label = label.to_ascii_lowercase();
    if first.to_ascii_lowercase() == lower_label {
        output.push(label.to_owned());
    } else if first.to_ascii_lowercase().starts_with(&lower_label) {
        output.push(format!("{label}{}", &first[label.len()..]));
    } else {
        output.push(display_word(first));
    }
    for word in words.iter().skip(1) {
        if drop_artifacts && artifact_suffix(word) {
            continue;
        }
        output.push(display_word(word));
    }
    output.join(" ")
}

fn display_word(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    if lower.len() > 1
        && lower.ends_with('b')
        && lower[..lower.len() - 1]
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
    {
        return format!("{}B", &word[..word.len() - 1]);
    }
    if lower.starts_with('a')
        && lower.ends_with('b')
        && lower[1..lower.len() - 1]
            .chars()
            .all(|character| character.is_ascii_digit())
    {
        return word.to_ascii_uppercase();
    }
    if lower.starts_with('v')
        && lower[1..]
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.')
    {
        return format!("V{}", &word[1..]);
    }
    let mut characters = word.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    format!("{}{}", first.to_uppercase(), characters.as_str())
}

pub fn model_display_name_variants(name: &str) -> Vec<String> {
    let words = name.split_whitespace().collect::<Vec<_>>();
    let mut variants = Vec::with_capacity(3);
    for count in [words.len(), words.len().min(2), words.len().min(1)] {
        if count == 0 {
            continue;
        }
        let candidate = words[..count].join(" ");
        if variants.last() != Some(&candidate) {
            variants.push(candidate);
        }
    }
    if variants.is_empty() {
        variants.push("model".to_owned());
    }
    variants
}

/// Only a catalogue-owned provider prefix may be omitted in quiet chrome.
/// Configured names and unfamiliar aliases remain verbatim, even if they
/// contain a colon or happen to begin with a known provider's name.
pub fn footer_model_name<'a>(name: &'a str, canonical_id: &str) -> &'a str {
    if octet_ai::model_metadata::model_display_name(canonical_id) != Some(name) {
        return name;
    }
    let derived = derive_model_display_name(canonical_id);
    for prefix in ["Anthropic: ", "OpenAI: ", "Google: ", "DeepSeek: "] {
        if let Some(short) = name.strip_prefix(prefix) {
            if short == derived {
                return short;
            }
        }
    }
    name
}
