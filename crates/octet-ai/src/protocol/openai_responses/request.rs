//! The request builders: `build_request`, the private compact variant, the
//! raw-compact control validation, the session-affinity headers, and the tool,
//! reasoning and text mapping all three share.
//!
//! This is separate because everything in it fails before a byte leaves the
//! process, which is a different failure model from the decode half: no
//! response parsing, no stream state, and no fixture is needed to exercise it.

use crate::error::{AiError, ConfigError, DecodeError};
use crate::protocol::{
    cache_session_id, cache_session_id_for, prompt_cache_key, prompt_cache_key_for,
    HttpRequestParts,
};
use crate::types::{
    CacheRetention, OutputFormat, Protocol, ReasoningConfig, ReasoningMode, Request, ToolChoice,
    ToolDef,
};
use crate::validate::{
    normalize_request_reasoning, validate_reasoning_selection, validate_request,
};

use super::input::{map_grammar_replay, validate_async_input, validate_async_tools};
use super::wire::{
    into_wire_input, opaque_input_item, prompt_cache_options, supports_explicit_prompt_cache_mode,
    ResponsesComputerTool, ResponsesContentPart, ResponsesCustomTool, ResponsesFormat,
    ResponsesGrammarFormat, ResponsesInputItem, ResponsesReasoningConfig, ResponsesRequest,
    ResponsesTextConfig, ResponsesTool, ResponsesToolWire, COMPUTER_TOOL_NAME,
};

pub(super) fn map_responses_tools(
    model: &crate::catalog::Model,
    tools: &[ToolDef],
) -> Result<Option<Vec<ResponsesToolWire>>, AiError> {
    if tools.is_empty() || !model.spec.capabilities.tools {
        return Ok(None);
    }
    let mut mapped = Vec::with_capacity(tools.len());
    for tool in tools {
        // Grammar-constrained tools are caller-opted OpenAI `custom` tools;
        // every other tool is a strict-resolved function tool.
        if let Some(grammar) = crate::constrained_sampling::resolve_grammar(
            tool,
            crate::protocol::grammar_tools_for(model),
        )? {
            mapped.push(ResponsesToolWire::Custom(ResponsesCustomTool {
                async_execution: tool.async_execution,
                r#type: "custom",
                name: tool.name.clone(),
                description: tool.description.clone(),
                format: ResponsesGrammarFormat {
                    r#type: "grammar",
                    syntax: grammar.format.to_owned(),
                    definition: grammar.definition,
                },
            }));
            continue;
        }
        let supports_strict = crate::protocol::strict_mode_for(model);
        let (parameters, strict) =
            crate::constrained_sampling::function_tool_parameters(tool, supports_strict)?;
        mapped.push(ResponsesToolWire::Function(ResponsesTool {
            async_execution: tool.async_execution,
            r#type: "function",
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters,
            strict: supports_strict.then_some(strict),
        }));
    }
    Ok(Some(mapped))
}

pub(super) fn map_responses_lite_tools(
    model: &crate::catalog::Model,
    tools: &[ToolDef],
) -> Result<Vec<serde_json::Value>, AiError> {
    if tools.is_empty() || !model.spec.capabilities.tools {
        return Ok(Vec::new());
    }
    let tools = tools
        .iter()
        .map(|tool| {
            let (parameters, strict) =
                crate::constrained_sampling::function_tool_parameters(tool, false)?;
            let mut value = serde_json::json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "strict": strict,
                "parameters": parameters,
            });
            if tool.async_execution {
                value["async"] = true.into();
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>, AiError>>()?;
    Ok(vec![serde_json::json!({
        "type": "namespace",
        "name": "functions",
        "description": "",
        "tools": tools,
    })])
}

pub(super) fn responses_lite_prefix(
    model: &crate::catalog::Model,
    instructions: Option<&str>,
    tools: &[ToolDef],
) -> Result<Vec<crate::responses::ResponsesItem>, AiError> {
    let mut prefix = vec![opaque_input_item(ResponsesInputItem::AdditionalTools {
        role: "developer".to_owned(),
        tools: map_responses_lite_tools(model, tools)?,
    })];
    if let Some(instructions) = instructions.filter(|instructions| !instructions.is_empty()) {
        prefix.push(opaque_input_item(ResponsesInputItem::Message {
            role: "developer".to_owned(),
            content: vec![ResponsesContentPart::InputText {
                text: instructions.to_owned(),
            }],
        }));
    }
    Ok(prefix)
}

pub(super) fn responses_reasoning_effort(effort: crate::types::ReasoningEffort) -> &'static str {
    use crate::types::ReasoningEffort;

    match effort {
        ReasoningEffort::Minimal => "minimal",
        ReasoningEffort::Low => "low",
        ReasoningEffort::Medium => "medium",
        ReasoningEffort::High => "high",
        ReasoningEffort::Xhigh => "xhigh",
        ReasoningEffort::Max => "max",
        // Ultra is a host-orchestration tier; current Codex wire requests use
        // maximum model reasoning while the V2 runtime supplies delegation.
        ReasoningEffort::Ultra => "max",
    }
}

pub(super) fn map_responses_reasoning(
    model: &crate::catalog::Model,
    reasoning: &ReasoningConfig,
    _reasoning_mode: ReasoningMode,
) -> Option<ResponsesReasoningConfig> {
    let cap = model.spec.capabilities.reasoning.as_ref()?;
    let effort = if *reasoning == ReasoningConfig::Effort(crate::types::ReasoningEffort::Ultra)
        && model.spec.capabilities.agent_delegation == Some(crate::types::AgentDelegation::V2)
    {
        // Ultra's exact advertised choice still requires the existing V2 gate;
        // the model half of that orchestration contract remains max.
        Some(responses_reasoning_effort(crate::types::ReasoningEffort::Ultra).to_owned())
    } else {
        cap.wire_value(reasoning).filter(|value| value != "default")
    };
    let context = model
        .spec
        .capabilities
        .responses_lite
        .then_some("all_turns");
    (effort.is_some() || context.is_some()).then_some(ResponsesReasoningConfig {
        effort,
        context,
        summary: "auto",
    })
}

pub(super) fn map_responses_text(
    model: &crate::catalog::Model,
    output_format: &OutputFormat,
) -> Option<ResponsesTextConfig> {
    let text_format = match output_format {
        OutputFormat::Text => None,
        _ if !model.spec.capabilities.structured_output => None,
        OutputFormat::JsonObject => Some(ResponsesFormat::JsonObject),
        OutputFormat::JsonSchema(schema) => Some(ResponsesFormat::JsonSchema {
            name: schema.name.clone(),
            description: schema.description.clone(),
            schema: schema.schema.clone(),
            strict: schema.strict,
        }),
    };
    let verbosity = model
        .endpoint
        .runtime
        .responses_profile
        .uses_low_verbosity()
        .then_some("low");
    (text_format.is_some() || verbosity.is_some()).then_some(ResponsesTextConfig {
        format: text_format,
        verbosity,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_compact_request(
    model: &crate::catalog::Model,
    mut input: crate::responses::ResponsesInput,
    instructions: Option<String>,
    tools: &[ToolDef],
    reasoning: &ReasoningConfig,
    reasoning_mode: ReasoningMode,
    output_format: &OutputFormat,
    cache_retention: CacheRetention,
    session_id: Option<&str>,
) -> Result<crate::responses::ResponsesCompactRequest, AiError> {
    crate::catalog::validate_model_spec(&model.spec)?;
    if model.spec.protocol != Protocol::OpenAiResponses {
        return Err(crate::error::UnsupportedError::ResponsesOptions.into());
    }
    if reasoning_mode == ReasoningMode::Pro {
        return Err(crate::error::UnsupportedError::ReasoningMode.into());
    }
    validate_reasoning_selection(reasoning, &model.spec.capabilities, model.spec.protocol)?;
    crate::responses::validate_responses_input(model, &input, reasoning, true)?;
    validate_async_tools(model, tools)?;
    // The private ChatGPT Codex compact route accepts the same active tool and
    // generation controls as normal Responses calls. Public OpenAI compact
    // currently exposes a narrower schema and may reject these extra fields.
    let responses_lite = model.spec.capabilities.responses_lite;
    let rich_codex_schema = model
        .endpoint
        .runtime
        .responses_profile
        .supports_rich_compact_schema()
        || model.spec.cache.session_affinity_format
            == Some(crate::types::SessionAffinityFormat::Codex)
        || responses_lite;
    let mapped_tools = if responses_lite || !rich_codex_schema {
        None
    } else {
        map_responses_tools(model, tools)?.map(|tools| {
            tools
                .into_iter()
                .map(|tool| serde_json::to_value(tool).expect("Responses tool serializes"))
                .collect()
        })
    };
    let parallel_tool_calls = if responses_lite {
        // The internal Responses Lite route requires an explicit false even
        // when the model otherwise advertises parallel tool-call support.
        Some(false)
    } else {
        mapped_tools
            .as_ref()
            .map(|_| model.spec.capabilities.parallel_tool_calls)
    };
    let (input, instructions) = if responses_lite {
        input.strip_image_details_for_responses_lite();
        let mut items = responses_lite_prefix(model, instructions.as_deref(), tools)?;
        items.extend(input.into_items());
        (crate::responses::ResponsesInput::new(items), None)
    } else {
        (input, instructions)
    };
    let reasoning = rich_codex_schema
        .then(|| map_responses_reasoning(model, reasoning, reasoning_mode))
        .flatten()
        .map(|config| serde_json::to_value(config).expect("Responses reasoning serializes"));
    let text = rich_codex_schema
        .then(|| map_responses_text(model, output_format))
        .flatten()
        .map(|config| serde_json::to_value(config).expect("Responses text serializes"));
    Ok(crate::responses::ResponsesCompactRequest {
        model: model.spec.api_name.clone(),
        input,
        instructions,
        parallel_tool_calls,
        tools: mapped_tools,
        reasoning,
        text,
        prompt_cache_key: prompt_cache_key_for(cache_retention, session_id),
        session_id: cache_session_id_for(cache_retention, session_id).map(str::to_owned),
    })
}

/// Validate the public raw compact DTO before credentials or transport. An
/// omitted control is provider-default intent, not an explicit Off request.
pub(crate) fn validate_compact_reasoning(
    model: &crate::catalog::Model,
    reasoning: Option<&serde_json::Value>,
) -> Result<(), AiError> {
    let Some(reasoning) = reasoning else {
        return Ok(());
    };
    let unsupported = || AiError::Unsupported(crate::error::UnsupportedError::Reasoning);
    let object = reasoning.as_object().ok_or_else(unsupported)?;
    if object.contains_key("mode") {
        return Err(crate::error::UnsupportedError::ReasoningMode.into());
    }
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "effort" | "summary" | "context"))
    {
        return Err(unsupported());
    }
    let cap = model
        .spec
        .capabilities
        .reasoning
        .as_ref()
        .ok_or_else(unsupported)?;
    if let Some(effort) = object.get("effort") {
        let effort = effort.as_str().ok_or_else(unsupported)?;
        // Compare actual wire spellings, including the V2-only Ultra -> max
        // mapping. Raw `ultra` is never a substitute for that typed gate.
        let supported = cap.choices().iter().any(|choice| {
            validate_reasoning_selection(choice, &model.spec.capabilities, model.spec.protocol)
                .is_ok()
                && map_responses_reasoning(model, choice, ReasoningMode::Standard)
                    .and_then(|r| r.effort)
                    .as_deref()
                    == Some(effort)
        });
        if !supported {
            return Err(unsupported());
        }
    }
    if object
        .get("summary")
        .is_some_and(|value| !matches!(value.as_str(), Some("auto" | "concise" | "detailed")))
    {
        return Err(unsupported());
    }
    if object.get("context").is_some_and(|value| {
        !model.spec.capabilities.responses_lite || value.as_str() != Some("all_turns")
    }) {
        return Err(unsupported());
    }
    Ok(())
}

pub(crate) fn responses_affinity_headers(
    model: &crate::catalog::Model,
    session_id: Option<&str>,
) -> Result<http::HeaderMap, AiError> {
    let mut headers = http::HeaderMap::new();
    if model.spec.capabilities.responses_lite {
        headers.insert(
            http::HeaderName::from_static("x-openai-internal-codex-responses-lite"),
            http::HeaderValue::from_static("true"),
        );
    }
    let Some(session_id) = session_id.filter(|id| !id.is_empty()) else {
        return Ok(headers);
    };
    let value = http::HeaderValue::from_str(session_id)
        .map_err(|_| ConfigError::InvalidHeader("session affinity".into()))?;
    match model.spec.cache.session_affinity_format {
        Some(crate::types::SessionAffinityFormat::OpenRouter) => {
            headers.insert(http::HeaderName::from_static("x-session-id"), value);
        }
        // Mistral is a Chat route; accept the configuration here without
        // applying a Chat-only header to a Responses request.
        Some(crate::types::SessionAffinityFormat::Mistral) => {}
        Some(crate::types::SessionAffinityFormat::Codex) => {
            headers.insert(http::HeaderName::from_static("session-id"), value.clone());
            headers.insert(http::HeaderName::from_static("x-client-request-id"), value);
        }
        Some(crate::types::SessionAffinityFormat::OpenAiNoSession) => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            headers.insert(http::HeaderName::from_static("x-session-affinity"), value);
        }
        Some(crate::types::SessionAffinityFormat::OpenAi) => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            headers.insert(
                http::HeaderName::from_static("x-session-affinity"),
                value.clone(),
            );
            if model.spec.cache.send_session_id_header {
                headers.insert(http::HeaderName::from_static("session_id"), value);
            }
        }
        None => {
            headers.insert(
                http::HeaderName::from_static("x-client-request-id"),
                value.clone(),
            );
            if model.spec.cache.send_session_id_header {
                headers.insert(http::HeaderName::from_static("session_id"), value);
            }
        }
    }
    Ok(headers)
}

/// Builds the OpenAI Responses HTTP request parts.
pub(crate) fn build_request(
    model: &crate::catalog::Model,
    req: &Request,
) -> Result<HttpRequestParts, AiError> {
    // 1. Normalize model-gated reasoning, then run validation.
    let defaults = crate::protocol::preset::request_defaults(model, req)?;
    let req = normalize_request_reasoning(&defaults, &model.spec.capabilities);
    let mut effective_capabilities = model.spec.capabilities.clone();
    effective_capabilities.responses_features = model.responses_features();
    let diagnostics = validate_request(
        &req,
        &effective_capabilities,
        &model.spec.limits,
        Protocol::OpenAiResponses,
        &model.spec.id,
        req.compatibility,
    )?;
    if req
        .responses
        .as_ref()
        .is_some_and(|options| options.input.is_some() && options.previous_response_id.is_some())
    {
        return Err(ConfigError::Parse(
            "Responses raw input and previous_response_id cannot be used together".to_owned(),
        )
        .into());
    }

    // 4. Map tools & tool_choice
    let grammar_tools = crate::protocol::grammar_tools_for(model);
    let responses_lite = model.spec.capabilities.responses_lite;
    let mut tools_opt = if responses_lite {
        None
    } else {
        map_responses_tools(model, &req.tools)?
    };

    // 4b. Declared computer-use tool. The declaration is endpoint-gated data,
    // never a provider-name branch: a route whose profile does not declare the
    // tool fails closed instead of silently dropping the caller's declaration.
    // Responses Lite cannot carry tools at all, so it fails closed too.
    let computer_use = req
        .responses
        .as_ref()
        .and_then(|options| options.computer_use);
    if let Some(tool) = computer_use {
        if responses_lite {
            return Err(ConfigError::Parse(
                "computer use cannot be declared on a Responses Lite route".to_owned(),
            )
            .into());
        }
        if !model
            .endpoint
            .runtime
            .responses_profile
            .accepts_computer_use()
        {
            return Err(crate::error::UnsupportedError::ComputerUse.into());
        }
        tools_opt
            .get_or_insert_with(Vec::new)
            .push(ResponsesToolWire::Computer(ResponsesComputerTool {
                r#type: COMPUTER_TOOL_NAME,
                display_width: tool.display_width,
                display_height: tool.display_height,
                environment: tool.environment.wire_value(),
            }));
    }

    let tool_choice_opt = if !model.spec.capabilities.tools {
        None
    } else {
        match &req.tool_choice {
            ToolChoice::Auto => Some(serde_json::Value::String("auto".to_string())),
            ToolChoice::Required => Some(serde_json::Value::String("required".to_string())),
            ToolChoice::None => Some(serde_json::Value::String("none".to_string())),
            ToolChoice::Named(name) => Some(serde_json::json!({
                "type": if crate::protocol::grammar::input_property(&req.tools, name, grammar_tools)?.is_some() { "custom" } else { "function" },
                "name": name
            })),
        }
    };

    // 5. Reasoning Configuration
    let reasoning_opt = map_responses_reasoning(model, &req.reasoning, req.reasoning_mode);

    // 6. Text / Output Format Config
    //
    // Design §7: a Lossy structured-output downgrade must actually drop the
    // capability from the wire request, not just emit a diagnostic. Strict mode
    // has already returned `Err` in `validate_request` above, so an unsupported
    // format only reaches here under Lossy — in which case we serialize plain
    // text (`text` omitted) rather than send a `text.format` the model lacks.
    let text_opt = map_responses_text(model, &req.output_format);

    // 7. Request Encrypted Reasoning
    let include = if model.spec.capabilities.reasoning.is_some() {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        vec![]
    };

    // Only forward an explicit caller cap. The Responses API treats this as
    // optional, and the ChatGPT Codex backend rejects it outright
    // (`{"detail":"Unsupported parameter: max_output_tokens"}`), so we never
    // synthesize a default from the local capacity limit. Subscription
    // endpoints that reject this parameter select omission through runtime
    // metadata rather than a codec-side provider identity check.
    let max_output_tokens = crate::effective_output_token_cap(model, req.max_output_tokens);

    let responses_options = req.responses.as_ref();
    // Codex `service_tier`: a declared endpoint capability, never a provider
    // identity. A route whose profile does not declare the field fails closed
    // instead of silently dropping a caller's billing-changing control.
    let service_tier = responses_options.and_then(|options| options.service_tier);
    if service_tier.is_some()
        && !model
            .endpoint
            .runtime
            .responses_profile
            .accepts_service_tier()
    {
        return Err(crate::error::UnsupportedError::ServiceTier.into());
    }
    let raw_input = responses_options.and_then(|options| options.input.as_ref());
    let refresh_instructions = raw_input
        .is_some_and(crate::responses::ResponsesInput::contains_compaction)
        .then(|| req.system.clone())
        .flatten();
    // Opaque replay is authoritative: do not encode canonical history only to
    // discard it when a raw input is present.
    let mut input = raw_input.cloned().unwrap_or_else(|| {
        crate::responses::encode_canonical_responses_input(
            model,
            req.system.as_deref(),
            &req.messages,
            req.compatibility,
        )
    });
    crate::responses::validate_responses_input(model, &input, &req.reasoning, false)?;
    if input.contains_configuration_updates()
        && responses_options
            .and_then(|options| options.context_management.as_ref())
            .is_some()
    {
        return Err(ConfigError::Parse(
            "configuration updates cannot be combined with automatic context management".into(),
        )
        .into());
    }
    validate_async_input(model, &input, &req.tools)?;
    let instructions = if responses_lite {
        input.strip_image_details_for_responses_lite();
        let mut items = responses_lite_prefix(model, refresh_instructions.as_deref(), &req.tools)?;
        items.extend(input.into_items());
        input = crate::responses::ResponsesInput::new(items);
        None
    } else {
        refresh_instructions
    };
    let mut wire_input = into_wire_input(input);
    if !responses_lite {
        map_grammar_replay(
            &mut wire_input,
            &req,
            raw_input.is_none(),
            crate::protocol::grammar_tools_for(model),
        )?;
    }
    let explicit_cache_mode = supports_explicit_prompt_cache_mode(model);
    let responses_req = ResponsesRequest {
        model: model.spec.api_name.clone(),
        input: wire_input,
        instructions,
        previous_response_id: responses_options
            .and_then(|options| options.previous_response_id.clone()),
        context_management: responses_options
            .and_then(|options| options.context_management.clone()),
        tools: tools_opt,
        tool_choice: tool_choice_opt,
        // The internal Responses Lite route requires an explicit false even
        // when the model otherwise advertises parallel tool-call support.
        parallel_tool_calls: if responses_lite {
            Some(false)
        } else {
            (!req.tools.is_empty() && model.spec.capabilities.tools)
                .then_some(model.spec.capabilities.parallel_tool_calls)
        },
        max_output_tokens,
        temperature: req.temperature,
        reasoning: reasoning_opt,
        text: text_opt,
        service_tier,
        prompt_cache_key: prompt_cache_key(&req),
        prompt_cache_retention: (req.cache_retention == CacheRetention::Long
            && model.spec.cache.supports_long_retention
            && !explicit_cache_mode)
            .then_some("24h"),
        prompt_cache_options: prompt_cache_options(model, req.cache_retention),
        include,
        store: responses_options.is_some_and(|options| options.store),
        stream: true,
    };

    let mut body = responses_req.into_json()?;
    crate::protocol::preset::sampling(model, &req, &mut body)?;
    let body_bytes =
        serde_json::to_vec(&body).map_err(|e| AiError::Decode(DecodeError::Json(e.to_string())))?;

    let url = crate::protocol::endpoint_url(&model.endpoint.base_url, "responses")?;

    let mut headers = responses_affinity_headers(model, cache_session_id(&req))?;
    crate::protocol::add_opencode_session_header(model, &req, &mut headers)?;

    Ok(HttpRequestParts {
        url,
        headers,
        body: bytes::Bytes::from(body_bytes),
        streaming: true,
        diagnostics,
    })
}
