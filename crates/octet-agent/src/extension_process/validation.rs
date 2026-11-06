//! Tool definition, output schema and structured content validation.

use super::*;

pub(super) struct SchemaByteBudget(pub(super) usize);

impl Write for SchemaByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_sub(bytes.len()).ok_or_else(|| {
            std::io::Error::other("tool catalog aggregate schema byte limit exceeded")
        })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(super) fn validate_tool_definitions(
    tools: &[ToolDefinition],
    api_version: &str,
) -> Result<(), ExtensionRuntimeError> {
    if tools.len() > MAX_DYNAMIC_EXTENSION_TOOLS {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "tool catalog contains {} tools; limit is {MAX_DYNAMIC_EXTENSION_TOOLS}",
            tools.len()
        )));
    }
    let mut schema_budget = SchemaByteBudget(MAX_TOOL_CATALOG_SCHEMA_BYTES);
    for tool in tools {
        validate_tool_prompt_metadata(tool, api_version)?;
        if tool.prompt_snippet.is_some() || !tool.prompt_guidelines.is_empty() {
            serde_json::to_writer(
                &mut schema_budget,
                &(&tool.prompt_snippet, &tool.prompt_guidelines),
            )
            .map_err(|_| {
                ExtensionRuntimeError::Protocol(
                    "tool catalog aggregate schema byte limit exceeded".into(),
                )
            })?;
        }
        if let Some(operation) = &tool.operation {
            serde_json::to_writer(&mut schema_budget, operation).map_err(|_| {
                ExtensionRuntimeError::Protocol(
                    "tool catalog aggregate schema byte limit exceeded".into(),
                )
            })?;
        }
        if let Some(sampling) = &tool.constrained_sampling {
            serde_json::to_writer(&mut schema_budget, sampling).map_err(|_| {
                ExtensionRuntimeError::Protocol(format!(
                    "tool catalog aggregate schema bytes exceed {MAX_TOOL_CATALOG_SCHEMA_BYTES}"
                ))
            })?;
        }
        for schema in std::iter::once(&tool.parameters).chain(tool.output_schema.iter()) {
            serde_json::to_writer(&mut schema_budget, schema).map_err(|_| {
                ExtensionRuntimeError::Protocol(format!(
                    "tool catalog aggregate schema bytes exceed {MAX_TOOL_CATALOG_SCHEMA_BYTES}"
                ))
            })?;
        }
    }
    let mut names = BTreeSet::new();
    let mut operation_ids = BTreeSet::new();
    for tool in tools {
        validate_operation_definition(tool, api_version)?;
        if let Some(operation) = &tool.operation {
            if !operation_ids.insert(&operation.id) {
                return Err(ExtensionRuntimeError::Protocol(
                    "duplicate operation id".into(),
                ));
            }
        }
        if !names.insert(tool.name.clone()) {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool catalog contains duplicate `{}`",
                tool.name
            )));
        }
        validate_identifier("tool", &tool.name, true)?;
        if tool.description.trim().is_empty() {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool `{}` has an empty description",
                tool.name
            )));
        }
        if !tool.parameters.is_object() {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool `{}` parameters must be a JSON Schema object",
                tool.name
            )));
        }
        validate_composition_tool_definition(tool, api_version)?;
        if let Some(schema) = &tool.output_schema {
            if !matches!(
                api_version,
                EXTENSION_API_VERSION_0_2 | EXTENSION_API_VERSION_0_3 | EXTENSION_API_VERSION_0_4
            ) {
                return Err(ExtensionRuntimeError::Protocol(format!(
                    "API 0.1 tool `{}` cannot declare output_schema",
                    tool.name
                )));
            }
            validate_output_schema_definition(schema).map_err(|message| {
                ExtensionRuntimeError::Protocol(format!(
                    "tool `{}` has invalid output_schema: {message}",
                    tool.name
                ))
            })?;
        }
    }
    Ok(())
}

pub(super) fn validate_tool_definitions_for_protocol(
    tools: &[ToolDefinition],
    protocol: &ExtensionNegotiatedProtocol,
) -> Result<(), ExtensionRuntimeError> {
    validate_tool_definitions(tools, &protocol.version)?;
    for tool in tools {
        if (tool.prompt_snippet.is_some() || !tool.prompt_guidelines.is_empty())
            && !protocol.supports(EXTENSION_FEATURE_TOOL_PROMPT_METADATA)
        {
            return Err(ExtensionRuntimeError::Protocol(
                "tool prompt metadata requires negotiated tool_prompt_metadata_v1".into(),
            ));
        }
        if protocol.supports(EXTENSION_FEATURE_BULK_OBJECTS_V1) {
            for schema in std::iter::once(&tool.parameters).chain(tool.output_schema.iter()) {
                validate_bulk_schema(schema)?;
            }
        }
        if let Some(operation) = &tool.operation {
            if !protocol.supports(EXTENSION_FEATURE_OPERATION_DESCRIPTORS_V1)
                || ((!operation.resource_inputs.is_empty()
                    || !operation.resource_outputs.is_empty())
                    && !protocol.supports(EXTENSION_FEATURE_RESOURCE_REFS_V1))
            {
                return Err(resource_error("unsupported_feature"));
            }
        }
    }
    if tools.iter().any(|tool| tool.composition.is_some())
        && !protocol.supports(EXTENSION_FEATURE_TOOL_COMPOSITION)
    {
        return Err(ExtensionRuntimeError::Protocol(
            "tool composition requires negotiated tool_composition_v1".into(),
        ));
    }
    Ok(())
}

fn validate_tool_prompt_metadata(
    tool: &ToolDefinition,
    api_version: &str,
) -> Result<(), ExtensionRuntimeError> {
    let invalid = |reason: &str| {
        ExtensionRuntimeError::Protocol(format!("tool `{}` prompt metadata: {reason}", tool.name))
    };
    if tool.prompt_snippet.is_none() && tool.prompt_guidelines.is_empty() {
        return Ok(());
    }
    if api_version != EXTENSION_API_VERSION_0_4 {
        return Err(invalid("requires API 0.4"));
    }
    if tool.prompt_guidelines.len() > 16 {
        return Err(invalid("at most 16 guidelines are allowed"));
    }
    for text in tool.prompt_snippet.iter().chain(&tool.prompt_guidelines) {
        if text.len() > 1024 {
            return Err(invalid("each text must be at most 1024 UTF-8 bytes"));
        }
        if text
            .chars()
            .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
        {
            return Err(invalid("text contains a control character"));
        }
    }
    Ok(())
}

fn validate_composition_tool_definition(
    tool: &ToolDefinition,
    api_version: &str,
) -> Result<(), ExtensionRuntimeError> {
    let invalid =
        |reason: &str| ExtensionRuntimeError::Protocol(format!("tool `{}`: {reason}", tool.name));
    if (tool.composition.is_some() || tool.constrained_sampling.is_some())
        && api_version != EXTENSION_API_VERSION_0_4
    {
        return Err(invalid(
            "composition and constrained_sampling require API 0.4",
        ));
    }
    if tool
        .composition
        .as_ref()
        .is_some_and(|config| config.inline_budget > 16_000)
    {
        return Err(invalid("composition inline_budget must be at most 16000"));
    }
    if let Some(octet_ai::ConstrainedSampling::Grammar { variants }) = &tool.constrained_sampling {
        let grammars = [
            variants.openai_lark.as_deref(),
            variants.openai_regex.as_deref(),
        ];
        if !grammars
            .iter()
            .flatten()
            .any(|grammar| !grammar.trim().is_empty())
        {
            return Err(invalid(
                "grammar constrained sampling needs a supported non-empty variant",
            ));
        }
        if grammars
            .iter()
            .flatten()
            .map(|grammar| grammar.len())
            .sum::<usize>()
            > 64 * 1024
        {
            return Err(invalid("grammar constrained sampling exceeds 64 KiB"));
        }
        let definition = ToolDef {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
            constrained_sampling: tool.constrained_sampling.clone(),
            async_execution: false,
        };
        octet_ai::constrained_sampling::infer_grammar_input_property(&definition)
            .map_err(|reason| invalid(&reason))?;
    }
    Ok(())
}

pub(super) fn ensure_same_contributions(
    kind: &str,
    declared: &[String],
    initialized: &[String],
) -> Result<(), ExtensionRuntimeError> {
    let declared_set = declared.iter().collect::<BTreeSet<_>>();
    let initialized_set = initialized.iter().collect::<BTreeSet<_>>();
    if declared_set == initialized_set
        && declared_set.len() == declared.len()
        && initialized_set.len() == initialized.len()
    {
        Ok(())
    } else {
        Err(ExtensionRuntimeError::Protocol(format!(
            "initialized {kind} do not match manifest declarations"
        )))
    }
}

pub(super) fn contributions_compatible(
    established: &ExtensionContributions,
    replacement: &ExtensionContributions,
    dynamic_tools: bool,
) -> bool {
    (dynamic_tools || established.tools == replacement.tools)
        && established.commands == replacement.commands
        && established.shortcuts == replacement.shortcuts
        && established.hooks == replacement.hooks
        && established.context == replacement.context
        && established.ui == replacement.ui
        && established.tool_renderers == replacement.tool_renderers
        && established.notifications == replacement.notifications
        && established.confirmations == replacement.confirmations
}

pub(super) fn validate_output_schema_definition(schema: &serde_json::Value) -> Result<(), String> {
    octet_ai::validate_json_schema_definition(schema).map_err(|error| error.to_string())
}

pub(super) fn validate_structured_content(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), String> {
    match octet_ai::validate_json_schema_value(schema, value).map_err(|error| error.to_string())? {
        true => Ok(()),
        false => Err("structured content does not match output schema".into()),
    }
}

pub(super) fn decode_tool_call_output(
    connection: &ProcessConnection,
    definition: &ToolDefinition,
    artifact_owner: Option<&str>,
    value: serde_json::Value,
) -> Result<ToolCallOutput, ExtensionRuntimeError> {
    if read_std_lock(&connection.protocol).version == EXTENSION_API_VERSION_0_3 {
        let result = api_v03::parse_tool_call_result(value).map_err(api_v03_protocol_error)?;
        api_v03::validate_tool_call_result(&result).map_err(api_v03_protocol_error)?;
        let parts = result
            .content
            .into_iter()
            .map(|part| match part {
                api_v03::ContentPart::Text { text } => Ok(ToolOutputContentPart::Text(text)),
                api_v03::ContentPart::Image { .. } | api_v03::ContentPart::Audio { .. } => {
                    Err(ExtensionRuntimeError::Protocol(
                        "API 0.3 image and audio content parts are deferred".into(),
                    ))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let structured_content = match result.structured_content {
            api_v03::Presence::Absent => None,
            api_v03::Presence::Null => Some(serde_json::Value::Null),
            api_v03::Presence::Value(value) => Some(value),
        };
        let metadata = result.metadata;
        let native = ToolOutput::from_content_parts(parts)
            .try_with_details(structured_content.clone(), metadata.clone())
            .map_err(|error| {
                ExtensionRuntimeError::Protocol(format!(
                    "tool `{}` returned invalid API 0.3 output details: {error}",
                    definition.name
                ))
            })?;
        return Ok(ToolCallOutput {
            content: native.text.clone(),
            is_error: result.is_error,
            metadata: metadata.unwrap_or(serde_json::Value::Null),
            structured_content,
            native_output: Some(native),
        });
    }
    decode_retained_tool_output(
        &read_std_lock(&connection.protocol).clone(),
        &connection.artifact_store,
        connection.generation,
        definition,
        artifact_owner,
        value,
    )
}

/// Partial results share final-result artifact authority and detail budgets, but
/// deliberately do not require a completed output matching the final schema.
pub(super) fn decode_progress_output(
    state: &ProtocolReadState,
    artifact_owner: Option<&str>,
    value: serde_json::Value,
) -> Result<ToolOutput, ExtensionRuntimeError> {
    let definition: ToolDefinition = serde_json::from_value(serde_json::json!({
        "name":"partial_result", "description":"Partial result", "parameters":{}
    }))
    .expect("internal partial-result definition is valid");
    let output = decode_retained_tool_output(
        &read_std_lock(&state.protocol).clone(),
        &state.artifact_store,
        state.generation,
        &definition,
        artifact_owner,
        value,
    )?;
    let is_error = output.is_error;
    Ok(output.into_native()?.with_is_error(is_error))
}

fn decode_retained_tool_output(
    protocol: &ExtensionNegotiatedProtocol,
    artifact_store: &ArtifactStore,
    generation: u64,
    definition: &ToolDefinition,
    artifact_owner: Option<&str>,
    value: serde_json::Value,
) -> Result<ToolCallOutput, ExtensionRuntimeError> {
    let wire: ToolCallOutputWire = serde_json::from_value(value).map_err(|error| {
        ExtensionRuntimeError::Protocol(format!(
            "invalid `{}` response for tool `{}`: {error}",
            methods::TOOL_CALL,
            definition.name
        ))
    })?;
    let structured_content = wire.structured_content.into_option();
    if protocol.version == EXTENSION_API_VERSION_0_1 {
        if structured_content
            .as_ref()
            .is_some_and(|structured| !structured.is_null())
        {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "API 0.1 tool `{}` returned unsupported structured_content",
                definition.name
            )));
        }
        let content = wire.content.as_str().ok_or_else(|| {
            ExtensionRuntimeError::Protocol(format!(
                "API 0.1 tool `{}` content must be a string",
                definition.name
            ))
        })?;
        let native = ToolOutput::new(content);
        return Ok(ToolCallOutput {
            content: content.to_owned(),
            is_error: wire.is_error,
            metadata: wire.metadata,
            structured_content: None,
            native_output: Some(native),
        });
    }

    if !protocol.supports(EXTENSION_FEATURE_CONTENT_PARTS) {
        return Err(ExtensionRuntimeError::Protocol(
            "API 0.2 tool result arrived without content_parts negotiation".into(),
        ));
    }
    // Admit the machine-readable fields through the native byte/depth/node
    // bounds before running extension-supplied schema logic over them.
    ToolOutput::new("")
        .try_with_details(structured_content.clone(), Some(wire.metadata.clone()))
        .map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "tool `{}` returned invalid output details: {error}",
                definition.name
            ))
        })?;
    let parts: Vec<ExtensionToolContentPart> =
        serde_json::from_value(wire.content).map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "API 0.2 tool `{}` content must be an array of typed parts: {error}",
                definition.name
            ))
        })?;
    if parts.len() > MAX_EXTENSION_RESULT_CONTENT_PARTS {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "API 0.2 tool `{}` returned {} content parts; limit is {MAX_EXTENSION_RESULT_CONTENT_PARTS}",
            definition.name,
            parts.len()
        )));
    }

    let mut native_parts = Vec::with_capacity(parts.len());
    let mut referenced_media_bytes = 0_u64;
    for part in parts {
        match part {
            ExtensionToolContentPart::Text { text } => {
                native_parts.push(ToolOutputContentPart::Text(text));
            }
            ExtensionToolContentPart::Image {
                artifact_id,
                mime_type,
                _alt: _,
            } => {
                require_artifact_feature(protocol, &definition.name)?;
                let artifact_owner = artifact_owner.ok_or_else(|| {
                    ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` returned an artifact without a host-owned session context",
                        definition.name
                    ))
                })?;
                let artifact_id: ArtifactId = serde_json::from_value(serde_json::Value::String(
                    artifact_id,
                ))
                .map_err(|error| {
                    ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` returned invalid artifact ID: {error}",
                        definition.name
                    ))
                })?;
                let resolved = artifact_store
                    .resolve_artifact_for_owner(generation, artifact_owner, &artifact_id)
                    .map_err(|error| {
                        ExtensionRuntimeError::Protocol(format!(
                            "tool `{}` returned unavailable artifact: {error}",
                            definition.name
                        ))
                    })?;
                if resolved.artifact.mime_type != mime_type {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` artifact MIME `{mime_type}` does not match verified `{}`",
                        definition.name, resolved.artifact.mime_type
                    )));
                }
                if !matches!(resolved.media, Media::Image(_)) {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` declared a non-image artifact as image",
                        definition.name
                    )));
                }
                referenced_media_bytes = referenced_media_bytes
                    .checked_add(resolved.artifact.size)
                    .ok_or_else(|| {
                        ExtensionRuntimeError::Protocol(format!(
                            "tool `{}` media reference byte count overflowed",
                            definition.name
                        ))
                    })?;
                if referenced_media_bytes > MAX_EXTENSION_RESULT_MEDIA_BYTES {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` referenced {referenced_media_bytes} aggregate media bytes; limit is {MAX_EXTENSION_RESULT_MEDIA_BYTES}",
                        definition.name
                    )));
                }
                native_parts.push(ToolOutputContentPart::Media(resolved.media));
            }
            ExtensionToolContentPart::Audio {
                artifact_id,
                mime_type,
                transcript,
            } => {
                require_artifact_feature(protocol, &definition.name)?;
                let artifact_owner = artifact_owner.ok_or_else(|| {
                    ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` returned an artifact without a host-owned session context",
                        definition.name
                    ))
                })?;
                let artifact_id: ArtifactId = serde_json::from_value(serde_json::Value::String(
                    artifact_id,
                ))
                .map_err(|error| {
                    ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` returned invalid artifact ID: {error}",
                        definition.name
                    ))
                })?;
                let mut resolved = artifact_store
                    .resolve_artifact_for_owner(generation, artifact_owner, &artifact_id)
                    .map_err(|error| {
                        ExtensionRuntimeError::Protocol(format!(
                            "tool `{}` returned unavailable artifact: {error}",
                            definition.name
                        ))
                    })?;
                if resolved.artifact.mime_type != mime_type {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` artifact MIME `{mime_type}` does not match verified `{}`",
                        definition.name, resolved.artifact.mime_type
                    )));
                }
                let Media::Audio(audio) = &mut resolved.media else {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` declared a non-audio artifact as audio",
                        definition.name
                    )));
                };
                referenced_media_bytes = referenced_media_bytes
                    .checked_add(resolved.artifact.size)
                    .ok_or_else(|| {
                        ExtensionRuntimeError::Protocol(format!(
                            "tool `{}` media reference byte count overflowed",
                            definition.name
                        ))
                    })?;
                if referenced_media_bytes > MAX_EXTENSION_RESULT_MEDIA_BYTES {
                    return Err(ExtensionRuntimeError::Protocol(format!(
                        "tool `{}` referenced {referenced_media_bytes} aggregate media bytes; limit is {MAX_EXTENSION_RESULT_MEDIA_BYTES}",
                        definition.name
                    )));
                }
                audio.transcript = transcript;
                native_parts.push(ToolOutputContentPart::Media(resolved.media));
            }
        }
    }

    match (&definition.output_schema, &structured_content) {
        (Some(schema), Some(structured)) => {
            validate_structured_content(schema, structured).map_err(|message| {
                ExtensionRuntimeError::Protocol(format!(
                    "tool `{}` structured_content failed output_schema: {message}",
                    definition.name
                ))
            })?;
        }
        (Some(_), None) if !wire.is_error => {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool `{}` declared output_schema but omitted structured_content",
                definition.name
            )));
        }
        (Some(_), None) => {}
        // Pi permits structuredContent without an output schema. Only tools
        // declaring a schema expose it as a programmatic composition value.
        (None, _) => {}
    }
    crate::extension_diagnostics::validate_metadata(&wire.metadata)
        .map_err(ExtensionRuntimeError::Protocol)?;
    let native = ToolOutput::from_content_parts(native_parts)
        .try_with_details(structured_content.clone(), Some(wire.metadata.clone()))
        .map_err(|error| {
            ExtensionRuntimeError::Protocol(format!(
                "tool `{}` returned invalid output details: {error}",
                definition.name
            ))
        })?;
    Ok(ToolCallOutput {
        content: native.text.clone(),
        is_error: wire.is_error,
        metadata: wire.metadata,
        structured_content,
        native_output: Some(native),
    })
}

pub(super) fn require_artifact_feature(
    protocol: &ExtensionNegotiatedProtocol,
    tool_name: &str,
) -> Result<(), ExtensionRuntimeError> {
    if protocol.supports(EXTENSION_FEATURE_ARTIFACTS) {
        Ok(())
    } else {
        Err(ExtensionRuntimeError::Protocol(format!(
            "tool `{tool_name}` returned an artifact without artifacts negotiation"
        )))
    }
}

#[cfg(test)]
mod schema_keyword_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pi_output_schema_value_budget_is_fatal_without_echoing_payloads() {
        let schema = json!({"type":"array","items":{"type":"string"}});
        validate_output_schema_definition(&schema).unwrap();
        let value = json!(vec!["private-output-value"; 4096]);
        let error = validate_structured_content(&schema, &value).unwrap_err();
        assert!(error.contains("work limit"));
        assert!(!error.contains("private-output-value"));
    }

    #[test]
    fn pi_output_schema_keywords_share_input_validator_and_preserve_constraints() {
        let schema = json!({"type":"object","properties":{"runId":{"type":"string","format":"uuid"},"step":{"type":"number","multipleOf":0.5},"fields":{"type":"object","patternProperties":{"^.*$":{"type":"integer"}},"additionalProperties":false}},"required":["runId","step","fields"]});
        validate_output_schema_definition(&schema).unwrap();
        let value =
            json!({"runId":"053dc2bc-237c-4c08-b4de-77b5dbd05fa2","step":1.5,"fields":{"a":2}});
        validate_structured_content(&schema, &value).unwrap();
        for (name, bad) in [
            ("runId", json!("bad")),
            ("step", json!(1.25)),
            ("fields", json!({"a":"bad"})),
        ] {
            let mut invalid = value.clone();
            invalid[name] = bad;
            assert!(validate_structured_content(&schema, &invalid).is_err());
        }
        assert!(validate_output_schema_definition(&json!({"patternProperties":{"[":{}}})).is_err());
        assert!(validate_output_schema_definition(&json!({"unknownConstraint":true})).is_err());
    }
}
