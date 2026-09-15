//! Host request policy and inline provider-route construction.
//!
//! This module is the security boundary for workspace/session configuration,
//! tool effects, route validation, credentials, headers, and request bounds.
//! Transport only supplies decoded DTOs; agent orchestration consumes the
//! policy-approved configuration produced here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use octet_ai::{
    Auth, CacheRetention, Capabilities, Endpoint, EndpointId, Modality, ModalitySet, ModelLimits,
    ModelSpec, OpenAiChatReasoningMode, Protocol, ReasoningCapability, ReasoningControl,
    ReasoningEffort,
};
use sha2::{Digest as _, Sha256};

use super::media::{audio_format, audio_format_name};
use super::protocol::{
    HostInputModality, MediaInput, RunRequest, MAX_API_KEY_BYTES, MAX_AUDIO_COUNT,
    MAX_CUSTOM_HEADERS, MAX_CUSTOM_HEADER_BYTES, MAX_HISTORY_BYTES, MAX_HISTORY_MESSAGES,
    MAX_IMAGE_COUNT, MAX_MEDIA_COUNT, MAX_PROMPT_BYTES, MAX_PROMPT_DISPLAY_BYTES,
};
use crate::config::{
    ColorMode, CompactionPolicy, Config, Mode, MouseMode, ResumeSelector, SandboxPolicy, ToolPolicy,
};

pub(crate) fn canonicalize_with_missing_tail(path: &Path) -> std::io::Result<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = existing.file_name().ok_or(error)?;
                missing.push(name.to_os_string());
                existing = existing.parent().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "path has no existing ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn host_config(request: &RunRequest) -> anyhow::Result<Config> {
    let workspace = request
        .workspace
        .canonicalize()
        .with_context(|| format!("workspace {} is unavailable", request.workspace.display()))?;
    let invocation_cwd = request
        .working_dir
        .as_deref()
        .unwrap_or(&workspace)
        .canonicalize()
        .with_context(|| "working directory is unavailable")?;
    if !invocation_cwd.starts_with(&workspace) {
        anyhow::bail!("working directory must stay inside the workspace");
    }
    let requested_session_dir = request
        .session_dir
        .clone()
        .unwrap_or_else(|| workspace.join(".octet/sessions"));
    let session_dir = if requested_session_dir.is_absolute() {
        requested_session_dir
    } else {
        workspace.join(requested_session_dir)
    };
    if session_dir.components().any(|component| {
        matches!(
            component,
            std::path::Component::CurDir | std::path::Component::ParentDir
        )
    }) {
        anyhow::bail!("session directory must not contain '.' or '..' components");
    }
    let session_dir =
        canonicalize_with_missing_tail(&session_dir).context("session directory is unavailable")?;
    if !request.allow_external_paths && !session_dir.starts_with(&workspace) {
        anyhow::bail!(
            "session directory must stay inside the workspace unless allow_external_paths is enabled"
        );
    }
    // Protocol v1 is fixed to Controlled. Host-supplied paths may use the
    // request boundary above, but model-controlled file tools must remain
    // workspace-relative under that effect policy.
    let mut sandbox = SandboxPolicy {
        allow_external_paths: false,
        policy_provenance: octet_agent::ToolPolicyProvenance::all(
            octet_agent::PolicyValueSource::HostRequest,
        ),
        ..SandboxPolicy::default()
    };
    if !request.allow_file_mutation {
        sandbox.allow_edit = false;
        sandbox.allow_write = false;
        sandbox.allow_process = false;
        sandbox.allow_shell = false;
    }
    let tools = match &request.tools {
        Some(tools) => ToolPolicy::only(tools.clone())?,
        None => ToolPolicy::default(),
    };
    Ok(Config {
        workspace,
        invocation_cwd,
        model: Some(octet_ai::ModelId(request.model.clone())),
        model_explicit: true,
        reasoning: request
            .reasoning
            .as_deref()
            .map(crate::config::parse_reasoning)
            .transpose()?,
        reasoning_explicit: request.reasoning.is_some(),
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        reasoning_mode_explicit: true,
        cache_retention: CacheRetention::Short,
        effect_policy: octet_agent::EffectPolicy::Controlled,
        sandbox,
        theme: None,
        system_prompt: request.system_prompt.clone(),
        theme_paths: Vec::new(),
        color: ColorMode::Never,
        mouse: MouseMode::Off,
        plain: true,
        show_images: false,
        session_dir,
        compaction: CompactionPolicy::default(),
        max_cost_microdollars: request.max_cost_microdollars,
        cost_warning_microdollars: None,
        max_turns: request.max_turns,
        show_reasoning_in_print: false,
        initial_prompt: None,
        prompt_template: None,
        debug_prompt: false,
        prompt_paths: request.prompt_paths.clone(),
        mode: Mode::Print {
            prompt: request.prompt.clone(),
        },
        resume: ResumeSelector::New,
        skill_paths: request.skill_paths.clone(),
        extension_paths: request.extension_paths.clone(),
        enabled_extensions: request.enabled_extensions.clone(),
        extension_activation_overridden: true,
        trusted_extensions: request.trusted_extensions.clone(),
        invocation_trusted_extensions: Vec::new(),
        experimental_streamable_http_mcp: false,
        extension_flag_values: Default::default(),
        tools,
        telemetry: None,
        context_files: request.context_files,
        offline: request.offline,
        workspace_trusted: true,
    })
}

pub(crate) fn register_inline_model(
    catalog: &mut octet_ai::ModelCatalog,
    request: &RunRequest,
) -> anyhow::Result<octet_ai::ModelId> {
    let Some(raw_base_url) = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return Ok(octet_ai::ModelId(request.model.clone()));
    };
    let base_url = parse_inline_base_url(raw_base_url)?;
    let protocol = inline_protocol(request.provider_mode.as_deref())?;
    let route_digest = inline_route_digest(
        &base_url,
        protocol,
        request.provider.as_deref(),
        &request.model,
    );
    let endpoint_id = EndpointId(format!("host-inline-{route_digest}"));
    let model_id = octet_ai::ModelId(format!("host-inline/{route_digest}"));
    let auth = request
        .api_key
        .as_deref()
        .filter(|key| !key.trim().is_empty())
        .map(|key| {
            if protocol == Protocol::AnthropicMessages {
                Auth::header(http::HeaderName::from_static("x-api-key"), key)
            } else {
                Auth::bearer(key)
            }
        })
        .unwrap_or(Auth::None);
    let default_headers = build_inline_headers(&request.custom_headers)?;
    catalog.register_endpoint(Endpoint {
        id: endpoint_id.clone(),
        base_url,
        auth,
        default_headers,
        transport: octet_ai::EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: std::time::Duration::from_secs(30),
    })?;
    let context_window = request.context_window_tokens.unwrap_or(262_144).max(1);
    let max_output_tokens = request
        .max_output_tokens
        .unwrap_or(16_384)
        .max(1)
        .min(context_window);
    let mut input_modalities = ModalitySet::none();
    if request.vision {
        input_modalities = input_modalities.with(Modality::Image);
    }
    for modality in &request.input_modalities {
        input_modalities = input_modalities.with(match modality {
            HostInputModality::Image => Modality::Image,
            HostInputModality::Audio => Modality::Audio,
        });
    }
    catalog.register_model(ModelSpec {
        id: model_id.clone(),
        endpoint: endpoint_id,
        api_name: request.model.clone(),
        display_name: None,
        protocol,
        capabilities: Capabilities {
            input_modalities,
            output_modalities: ModalitySet::none(),
            tools: true,
            parallel_tool_calls: true,
            reasoning: request.supports_reasoning.then_some(ReasoningCapability {
                options: None,
                control: ReasoningControl::Effort,
                exposes_text: true,
                preserves_state: protocol == Protocol::OpenAiResponses,
                effort_budgets: None,
                openai_chat_mode: OpenAiChatReasoningMode::Standard,
                min_effort: ReasoningEffort::Minimal,
                max_effort: ReasoningEffort::Max,
            }),
            responses_lite: false,
            agent_delegation: None,
            structured_output: true,
            deferred_tool_loading: false,
        },
        limits: ModelLimits {
            context_window,
            max_output_tokens,
        },
        pricing: None,
        cache: octet_ai::CacheCompatibility::default(),
    })?;
    Ok(model_id)
}

pub(crate) fn parse_inline_base_url(raw: &str) -> anyhow::Result<url::Url> {
    let normalized = if raw.ends_with('/') {
        raw.to_owned()
    } else {
        format!("{raw}/")
    };
    let url = url::Url::parse(&normalized).with_context(|| "inline model base_url is invalid")?;
    if url.cannot_be_a_base()
        || !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        anyhow::bail!(
            "inline model base_url must be an absolute HTTP(S) URL without userinfo, query, or fragment"
        );
    }
    Ok(url)
}

pub(crate) fn build_inline_headers(
    headers: &HashMap<String, String>,
) -> anyhow::Result<http::HeaderMap> {
    if headers.len() > MAX_CUSTOM_HEADERS {
        anyhow::bail!("custom_headers exceeds the {MAX_CUSTOM_HEADERS}-header limit");
    }
    let total_bytes = headers.iter().try_fold(0usize, |total, (name, value)| {
        total
            .checked_add(name.len())
            .and_then(|total| total.checked_add(value.len()))
    });
    if total_bytes.is_none_or(|total| total > MAX_CUSTOM_HEADER_BYTES) {
        anyhow::bail!("custom_headers exceeds the {MAX_CUSTOM_HEADER_BYTES}-byte limit");
    }

    let mut result = http::HeaderMap::new();
    for (raw_name, raw_value) in headers {
        let name = http::HeaderName::from_bytes(raw_name.as_bytes())
            .with_context(|| format!("invalid custom header name {raw_name:?}"))?;
        if matches!(
            name.as_str(),
            "connection"
                | "content-length"
                | "host"
                | "keep-alive"
                | "proxy-authenticate"
                | "proxy-authorization"
                | "proxy-connection"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            anyhow::bail!("custom header {name} is not allowed");
        }
        let value = http::HeaderValue::from_str(raw_value)
            .with_context(|| format!("invalid value for custom header {name}"))?;
        result.insert(name, value);
    }
    Ok(result)
}

pub(crate) fn inline_protocol(provider_mode: Option<&str>) -> anyhow::Result<Protocol> {
    let mode = provider_mode
        .unwrap_or("openai-compatible")
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    match mode.as_str() {
        "" | "openai" | "openai-chat" | "openai-compatible" | "chat" => {
            Ok(Protocol::OpenAiChat)
        }
        "openai-responses" | "responses" => Ok(Protocol::OpenAiResponses),
        "anthropic" | "anthropic-compatible" | "anthropic-messages" => {
            Ok(Protocol::AnthropicMessages)
        }
        _ => anyhow::bail!(
            "unsupported provider_mode {mode:?}; use openai-compatible, openai-responses, or anthropic-messages"
        ),
    }
}

fn inline_route_digest(
    base_url: &url::Url,
    protocol: Protocol,
    provider: Option<&str>,
    model: &str,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(base_url.as_str().as_bytes());
    hasher.update([0]);
    hasher.update(format!("{protocol:?}").as_bytes());
    hasher.update([0]);
    hasher.update(provider.unwrap_or("inline").as_bytes());
    hasher.update([0]);
    hasher.update(model.as_bytes());
    hasher
        .finalize()
        .iter()
        .take(12)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn validate_inline_media_route(request: &RunRequest, protocol: Protocol) -> anyhow::Result<()> {
    let requested_audio = request
        .input_modalities
        .iter()
        .any(|modality| *modality == HostInputModality::Audio);
    let has_audio_attachment = request
        .media
        .iter()
        .any(|input| matches!(input, MediaInput::Audio { .. }));
    if protocol != Protocol::OpenAiChat && (requested_audio || has_audio_attachment) {
        anyhow::bail!(
            "native audio input is unsupported through {protocol:?}; use an OpenAI Chat Completions route with an audio-capable model (WAV or MP3 only)"
        );
    }
    if has_audio_attachment && !requested_audio {
        anyhow::bail!(
            "inline native audio attachments require input_modalities to include \"audio\""
        );
    }
    if protocol == Protocol::OpenAiChat {
        for input in &request.media {
            let MediaInput::Audio { path } = input else {
                continue;
            };
            let format = audio_format(path).with_context(|| {
                format!(
                    "native audio attachment {} must use WAV or MP3",
                    path.display()
                )
            })?;
            if !matches!(
                format,
                octet_ai::AudioFormat::Wav | octet_ai::AudioFormat::Mp3
            ) {
                anyhow::bail!(
                    "native audio attachment {} uses {}; OpenAI Chat Completions accepts only WAV or MP3",
                    path.display(),
                    audio_format_name(format),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_run_request(request: &RunRequest) -> anyhow::Result<()> {
    if request.prompt.len() > MAX_PROMPT_BYTES {
        anyhow::bail!("prompt exceeds the {MAX_PROMPT_BYTES}-byte limit");
    }
    if request.prompt_display_text.as_ref().is_some_and(|text| {
        text.len() > MAX_PROMPT_DISPLAY_BYTES
            || text
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    }) {
        anyhow::bail!(
            "prompt_display_text is malformed or exceeds the {MAX_PROMPT_DISPLAY_BYTES}-byte limit"
        );
    }
    if request.model.trim().is_empty()
        || request.model.trim() != request.model
        || request.model.len() > 512
        || request.model.chars().any(char::is_control)
    {
        anyhow::bail!("model id is empty, malformed, or too long");
    }
    if request
        .provider
        .as_ref()
        .is_some_and(|provider| !super::protocol::valid_protocol_id(provider))
    {
        anyhow::bail!("provider id is malformed or too long");
    }
    if request.api_key.as_ref().is_some_and(|api_key| {
        api_key.len() > MAX_API_KEY_BYTES
            || api_key.trim() != api_key
            || api_key.chars().any(char::is_control)
    }) {
        anyhow::bail!("api_key is malformed or exceeds the {MAX_API_KEY_BYTES}-byte limit");
    }
    if let Some(base_url) = request
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        parse_inline_base_url(base_url)?;
        let protocol = inline_protocol(request.provider_mode.as_deref())?;
        validate_inline_media_route(request, protocol)?;
        build_inline_headers(&request.custom_headers)?;
    }
    if request.history.len() > MAX_HISTORY_MESSAGES {
        anyhow::bail!("history exceeds the {MAX_HISTORY_MESSAGES}-message limit");
    }
    let history_bytes = request
        .history
        .iter()
        .try_fold(0usize, |total, message| {
            total.checked_add(message.text.len())
        })
        .ok_or_else(|| anyhow::anyhow!("history size overflow"))?;
    if history_bytes > MAX_HISTORY_BYTES {
        anyhow::bail!("history exceeds the {MAX_HISTORY_BYTES}-byte limit");
    }
    if !request.media.is_empty() && !request.image_paths.is_empty() {
        anyhow::bail!("media and legacy image_paths cannot be combined");
    }
    let (image_count, audio_count) = if request.media.is_empty() {
        (request.image_paths.len(), 0)
    } else {
        request
            .media
            .iter()
            .fold((0usize, 0usize), |(images, audio), input| match input {
                MediaInput::Image { .. } => (images + 1, audio),
                MediaInput::Audio { .. } => (images, audio + 1),
            })
    };
    let media_count = image_count
        .checked_add(audio_count)
        .ok_or_else(|| anyhow::anyhow!("media count overflow"))?;
    if media_count > MAX_MEDIA_COUNT {
        anyhow::bail!("media exceeds the {MAX_MEDIA_COUNT}-item limit");
    }
    if image_count > MAX_IMAGE_COUNT {
        anyhow::bail!("media exceeds the {MAX_IMAGE_COUNT}-image limit");
    }
    if audio_count > MAX_AUDIO_COUNT {
        anyhow::bail!("media exceeds the {MAX_AUDIO_COUNT}-audio limit");
    }
    if request.input_modalities.len() > 2
        || request
            .input_modalities
            .iter()
            .enumerate()
            .any(|(index, modality)| request.input_modalities[..index].contains(modality))
    {
        anyhow::bail!("input_modalities contains duplicate or excess values");
    }
    if request
        .system_prompt
        .as_ref()
        .is_some_and(|system| system.len() > MAX_PROMPT_BYTES)
    {
        anyhow::bail!("system prompt exceeds the {MAX_PROMPT_BYTES}-byte limit");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
