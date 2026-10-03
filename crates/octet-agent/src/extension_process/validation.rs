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
    for tool in tools {
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
    if tools.iter().any(|tool| tool.composition.is_some())
        && !protocol.supports(EXTENSION_FEATURE_TOOL_COMPOSITION)
    {
        return Err(ExtensionRuntimeError::Protocol(
            "tool composition requires negotiated tool_composition_v1".into(),
        ));
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
    fn visit(schema: &serde_json::Value, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err("schema nesting exceeds 32 levels".into());
        }
        let object = schema
            .as_object()
            .ok_or_else(|| "schema nodes must be objects".to_owned())?;
        const SUPPORTED: &[&str] = &[
            "$schema",
            "title",
            "description",
            "default",
            "examples",
            "type",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "enum",
            "const",
            "allOf",
            "anyOf",
            "oneOf",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "uniqueItems",
            "minProperties",
            "maxProperties",
        ];
        if let Some(keyword) = object
            .keys()
            .find(|keyword| !SUPPORTED.contains(&keyword.as_str()))
        {
            return Err(format!("unsupported JSON Schema keyword `{keyword}`"));
        }
        if let Some(types) = object.get("type") {
            let valid_type = |name: &str| {
                matches!(
                    name,
                    "null" | "boolean" | "object" | "array" | "number" | "integer" | "string"
                )
            };
            match types {
                serde_json::Value::String(name) if valid_type(name) => {}
                serde_json::Value::Array(names)
                    if !names.is_empty()
                        && names
                            .iter()
                            .all(|name| name.as_str().is_some_and(valid_type)) => {}
                _ => return Err("type must name one or more supported JSON types".into()),
            }
        }
        if let Some(properties) = object.get("properties") {
            for (name, property) in properties
                .as_object()
                .ok_or_else(|| "properties must be an object".to_owned())?
            {
                if name.len() > 256 {
                    return Err("property name exceeds 256 bytes".into());
                }
                visit(property, depth + 1)?;
            }
        }
        if let Some(required) = object.get("required") {
            let required = required
                .as_array()
                .ok_or_else(|| "required must be an array".to_owned())?;
            if !required.iter().all(serde_json::Value::is_string) {
                return Err("required entries must be strings".into());
            }
        }
        if let Some(additional) = object.get("additionalProperties") {
            if !additional.is_boolean() {
                visit(additional, depth + 1)?;
            }
        }
        if let Some(items) = object.get("items") {
            visit(items, depth + 1)?;
        }
        for keyword in ["allOf", "anyOf", "oneOf"] {
            if let Some(branches) = object.get(keyword) {
                let branches = branches
                    .as_array()
                    .filter(|branches| !branches.is_empty())
                    .ok_or_else(|| format!("{keyword} must be a non-empty array"))?;
                for branch in branches {
                    visit(branch, depth + 1)?;
                }
            }
        }
        if object.get("enum").is_some_and(|values| {
            !values.is_array() || values.as_array().is_some_and(Vec::is_empty)
        }) {
            return Err("enum must be a non-empty array".into());
        }
        for keyword in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
            if object.get(keyword).is_some_and(|value| !value.is_number()) {
                return Err(format!("{keyword} must be a number"));
            }
        }
        for keyword in [
            "minLength",
            "maxLength",
            "minItems",
            "maxItems",
            "minProperties",
            "maxProperties",
        ] {
            if object
                .get(keyword)
                .is_some_and(|value| value.as_u64().is_none())
            {
                return Err(format!("{keyword} must be a non-negative integer"));
            }
        }
        if object
            .get("uniqueItems")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err("uniqueItems must be boolean".into());
        }
        Ok(())
    }
    visit(schema, 0)
}

pub(super) fn validate_structured_content(
    schema: &serde_json::Value,
    value: &serde_json::Value,
) -> Result<(), String> {
    struct ValidationBudget {
        remaining: usize,
    }

    impl ValidationBudget {
        fn consume(&mut self) -> Result<(), String> {
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or_else(|| "structured output validation budget exceeded".to_owned())?;
            Ok(())
        }
    }

    fn matches_type(value: &serde_json::Value, expected: &str) -> bool {
        match expected {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "number" => value.is_number(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "string" => value.is_string(),
            _ => false,
        }
    }

    fn visit(
        schema: &serde_json::Value,
        value: &serde_json::Value,
        path: &str,
        depth: usize,
        budget: &mut ValidationBudget,
    ) -> Result<(), String> {
        budget.consume()?;
        if depth > 32 {
            return Err(format!("{path} exceeds validation depth"));
        }
        let object = schema
            .as_object()
            .ok_or_else(|| "validated schema node is not an object".to_owned())?;
        if let Some(expected) = object.get("type") {
            let accepted = match expected {
                serde_json::Value::String(expected) => matches_type(value, expected),
                serde_json::Value::Array(expected) => expected
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .any(|expected| matches_type(value, expected)),
                _ => false,
            };
            if !accepted {
                return Err(format!("{path} does not match declared type"));
            }
        }
        if let Some(values) = object.get("enum").and_then(serde_json::Value::as_array) {
            let mut matched = false;
            for candidate in values {
                budget.consume()?;
                if candidate == value {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(format!("{path} is not one of the declared enum values"));
            }
        }
        if object
            .get("const")
            .is_some_and(|constant| constant != value)
        {
            return Err(format!("{path} does not match const"));
        }
        if let Some(branches) = object.get("allOf").and_then(serde_json::Value::as_array) {
            for branch in branches {
                visit(branch, value, path, depth + 1, budget)?;
            }
        }
        if let Some(branches) = object.get("anyOf").and_then(serde_json::Value::as_array) {
            let mut matched = false;
            for branch in branches {
                if visit(branch, value, path, depth + 1, budget).is_ok() {
                    matched = true;
                    break;
                }
            }
            if !matched {
                return Err(format!("{path} does not match anyOf"));
            }
        }
        if let Some(branches) = object.get("oneOf").and_then(serde_json::Value::as_array) {
            let mut matches = 0_u8;
            for branch in branches {
                if visit(branch, value, path, depth + 1, budget).is_ok() {
                    matches = matches.saturating_add(1);
                    if matches > 1 {
                        break;
                    }
                }
            }
            if matches != 1 {
                return Err(format!("{path} does not match exactly one oneOf branch"));
            }
        }
        if let Some(value) = value.as_object() {
            let properties = object
                .get("properties")
                .and_then(serde_json::Value::as_object);
            if let Some(required) = object.get("required").and_then(serde_json::Value::as_array) {
                for required in required.iter().filter_map(serde_json::Value::as_str) {
                    budget.consume()?;
                    if !value.contains_key(required) {
                        return Err(format!("{path}.{required} is required"));
                    }
                }
            }
            for (name, child) in value {
                budget.consume()?;
                if let Some(schema) = properties.and_then(|properties| properties.get(name)) {
                    visit(schema, child, &format!("{path}.{name}"), depth + 1, budget)?;
                } else if let Some(additional) = object.get("additionalProperties") {
                    match additional {
                        serde_json::Value::Bool(false) => {
                            return Err(format!("{path}.{name} is not allowed"));
                        }
                        serde_json::Value::Object(_) => {
                            visit(
                                additional,
                                child,
                                &format!("{path}.{name}"),
                                depth + 1,
                                budget,
                            )?;
                        }
                        _ => {}
                    }
                }
            }
            let count = value.len() as u64;
            if object
                .get("minProperties")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|minimum| count < minimum)
            {
                return Err(format!("{path} has too few properties"));
            }
            if object
                .get("maxProperties")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|maximum| count > maximum)
            {
                return Err(format!("{path} has too many properties"));
            }
        }
        if let Some(value) = value.as_array() {
            if let Some(items) = object.get("items") {
                for (index, child) in value.iter().enumerate() {
                    visit(items, child, &format!("{path}[{index}]"), depth + 1, budget)?;
                }
            }
            let count = value.len() as u64;
            if object
                .get("minItems")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|minimum| count < minimum)
            {
                return Err(format!("{path} has too few items"));
            }
            if object
                .get("maxItems")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|maximum| count > maximum)
            {
                return Err(format!("{path} has too many items"));
            }
            if object
                .get("uniqueItems")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            {
                let mut unique = HashSet::with_capacity(value.len());
                for item in value {
                    budget.consume()?;
                    let canonical = serde_json::to_vec(item)
                        .map_err(|error| format!("cannot canonicalize {path} item: {error}"))?;
                    if !unique.insert(canonical) {
                        return Err(format!("{path} contains duplicate items"));
                    }
                }
            }
        }
        if let Some(value) = value.as_str() {
            let count = value.chars().count() as u64;
            if object
                .get("minLength")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|minimum| count < minimum)
            {
                return Err(format!("{path} is shorter than minLength"));
            }
            if object
                .get("maxLength")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|maximum| count > maximum)
            {
                return Err(format!("{path} is longer than maxLength"));
            }
        }
        if let Some(number) = value.as_f64() {
            for (keyword, predicate) in [
                (
                    "minimum",
                    number
                        < object
                            .get("minimum")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(number),
                ),
                (
                    "maximum",
                    number
                        > object
                            .get("maximum")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(number),
                ),
                (
                    "exclusiveMinimum",
                    number
                        <= object
                            .get("exclusiveMinimum")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(number - 1.0),
                ),
                (
                    "exclusiveMaximum",
                    number
                        >= object
                            .get("exclusiveMaximum")
                            .and_then(serde_json::Value::as_f64)
                            .unwrap_or(number + 1.0),
                ),
            ] {
                if object.contains_key(keyword) && predicate {
                    return Err(format!("{path} violates {keyword}"));
                }
            }
        }
        Ok(())
    }
    let mut budget = ValidationBudget {
        remaining: MAX_SCHEMA_VALIDATION_STEPS,
    };
    visit(schema, value, "$", 0, &mut budget)
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
    let wire: ToolCallOutputWire = serde_json::from_value(value).map_err(|error| {
        ExtensionRuntimeError::Protocol(format!(
            "invalid `{}` response for tool `{}`: {error}",
            methods::TOOL_CALL,
            definition.name
        ))
    })?;
    let structured_content = wire.structured_content.into_option();
    let protocol = read_std_lock(&connection.protocol).clone();
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
    if parts.is_empty() {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "API 0.2 tool `{}` returned no content parts",
            definition.name
        )));
    }
    if parts.len() > MAX_EXTENSION_RESULT_CONTENT_PARTS {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "API 0.2 tool `{}` returned {} content parts; limit is {MAX_EXTENSION_RESULT_CONTENT_PARTS}",
            definition.name,
            parts.len()
        )));
    }

    let mut native_parts = Vec::with_capacity(parts.len());
    let mut saw_text = false;
    let mut referenced_media_bytes = 0_u64;
    for part in parts {
        match part {
            ExtensionToolContentPart::Text { text } => {
                saw_text = true;
                native_parts.push(ToolOutputContentPart::Text(text));
            }
            ExtensionToolContentPart::Image {
                artifact_id,
                mime_type,
                _alt: _,
            } => {
                require_artifact_feature(&protocol, &definition.name)?;
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
                let resolved = connection
                    .artifact_store
                    .resolve_artifact_for_owner(connection.generation, artifact_owner, &artifact_id)
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
                require_artifact_feature(&protocol, &definition.name)?;
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
                let mut resolved = connection
                    .artifact_store
                    .resolve_artifact_for_owner(connection.generation, artifact_owner, &artifact_id)
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
    if !saw_text {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "API 0.2 tool `{}` must include an explicit compact text part",
            definition.name
        )));
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
        (None, Some(_)) => {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "tool `{}` returned structured_content without output_schema",
                definition.name
            )));
        }
        (None, None) => {}
    }
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
