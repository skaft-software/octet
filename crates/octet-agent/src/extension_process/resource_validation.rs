//! Fixed object-property resource descriptors and reference value validation.
use super::*;

fn invalid_descriptor(message: &str) -> ExtensionRuntimeError {
    ExtensionRuntimeError::Protocol(format!("invalid operation descriptor: {message}"))
}

fn pointer_segments(path: &str) -> Result<Vec<String>, ExtensionRuntimeError> {
    if !path.starts_with('/') || path.len() > 1024 {
        return Err(invalid_descriptor("expected bounded fixed JSON Pointer"));
    }
    path[1..]
        .split('/')
        .map(|segment| {
            let decoded = segment.replace("~1", "/").replace("~0", "~");
            if decoded.replace('~', "~0").replace('/', "~1") != segment {
                Err(invalid_descriptor("noncanonical JSON Pointer escape"))
            } else {
                Ok(decoded)
            }
        })
        .collect()
}

fn schema_at<'a>(
    schema: &'a serde_json::Value,
    path: &str,
) -> Result<&'a serde_json::Value, ExtensionRuntimeError> {
    let mut node = schema;
    for segment in pointer_segments(path)? {
        if node.get("type").and_then(|v| v.as_str()) != Some("object")
            || ["oneOf", "anyOf", "allOf", "$ref"]
                .iter()
                .any(|k| node.get(*k).is_some())
        {
            return Err(invalid_descriptor("resource paths must traverse fixed object properties, not arrays/unions/references"));
        }
        node = node
            .get("properties")
            .and_then(|p| p.get(&segment))
            .ok_or_else(|| invalid_descriptor("path not found in schema"))?;
    }
    Ok(node)
}

fn validate_resource_schema(
    schema: &serde_json::Value,
    nominal: &str,
) -> Result<(), ExtensionRuntimeError> {
    let properties = schema
        .get("properties")
        .and_then(|v| v.as_object())
        .ok_or_else(|| invalid_descriptor("resource slot must have properties"))?;
    let nominal_schema = properties.get("type");
    let constant = nominal_schema.and_then(|s| s.get("const"));
    let enumeration = nominal_schema.and_then(|s| s.get("enum"));
    if (constant.is_none() && enumeration.is_none())
        || constant.is_some_and(|v| v.as_str() != Some(nominal))
        || enumeration.is_some_and(|v| {
            v.as_array()
                .is_none_or(|values| values.len() != 1 || values[0].as_str() != Some(nominal))
        })
    {
        return Err(invalid_descriptor("resource nominal schema must constrain exactly its declared type via const or singleton enum"));
    }
    let required = schema
        .get("required")
        .and_then(|v| v.as_array())
        .map(|v| v.iter().filter_map(|v| v.as_str()).collect::<BTreeSet<_>>());
    if schema.get("type").and_then(|v| v.as_str()) != Some("object")
        || schema.get("additionalProperties") != Some(&serde_json::Value::Bool(false))
        || properties.len() != 2
        || required != Some(BTreeSet::from(["$resource", "type"]))
        || properties
            .get("$resource")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            != Some("string")
        || properties
            .get("type")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            != Some("string")
        || ["oneOf", "anyOf", "allOf", "$ref"]
            .iter()
            .any(|k| schema.get(*k).is_some())
    {
        return Err(invalid_descriptor(
            "slot must be a closed, exact ResourceRef schema of the declared nominal type",
        ));
    }
    Ok(())
}

fn resource_schema_paths(
    schema: &serde_json::Value,
    path: &str,
    fixed: bool,
    found: &mut BTreeSet<String>,
) -> Result<(), ExtensionRuntimeError> {
    if let Some(properties) = schema.get("properties").and_then(|v| v.as_object()) {
        if properties.contains_key("$resource") {
            if !fixed {
                return Err(invalid_descriptor(
                    "unsupported resource-containing schema shape",
                ));
            }
            found.insert(path.to_owned());
            return Ok(());
        }
        let fixed = fixed
            && schema.get("type").and_then(|v| v.as_str()) == Some("object")
            && !["oneOf", "anyOf", "allOf", "$ref"]
                .iter()
                .any(|k| schema.get(*k).is_some());
        for (name, value) in properties {
            resource_schema_paths(
                value,
                &format!("{path}/{}", name.replace('~', "~0").replace('/', "~1")),
                fixed,
                found,
            )?;
        }
    }
    // Schemas outside a fixed properties walk may contain plain values, but never refs.
    for keyword in [
        "items",
        "additionalProperties",
        "oneOf",
        "anyOf",
        "allOf",
        "$defs",
        "definitions",
        "patternProperties",
    ] {
        if let Some(value) = schema.get(keyword) {
            fn unsupported(value: &serde_json::Value) -> bool {
                match value {
                    serde_json::Value::Object(o) => {
                        o.contains_key("$resource") || o.values().any(unsupported)
                    }
                    serde_json::Value::Array(a) => a.iter().any(unsupported),
                    _ => false,
                }
            }
            if unsupported(value) {
                return Err(invalid_descriptor(
                    "unsupported resource-containing schema shape",
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_operation_definition(
    tool: &ToolDefinition,
    api: &str,
) -> Result<(), ExtensionRuntimeError> {
    let Some(operation) = &tool.operation else {
        return Ok(());
    };
    if api != EXTENSION_API_VERSION_0_4 {
        return Err(resource_error("unsupported_feature"));
    }
    if !validate_nominal(&operation.id) {
        return Err(invalid_descriptor("invalid operation id"));
    }
    validate_output_schema_definition(&tool.parameters).map_err(|e| invalid_descriptor(&e))?;
    let mut inputs = BTreeSet::new();
    for input in &operation.resource_inputs {
        if !validate_nominal(&input.resource_type) || !inputs.insert(input.path.clone()) {
            return Err(invalid_descriptor("invalid or duplicate input slot"));
        }
        validate_resource_schema(
            schema_at(&tool.parameters, &input.path)?,
            &input.resource_type,
        )?;
    }
    if operation
        .receiver
        .as_ref()
        .is_some_and(|p| !inputs.contains(p))
    {
        return Err(invalid_descriptor("receiver is not a declared input slot"));
    }
    let mut outputs = BTreeSet::new();
    for output in &operation.resource_outputs {
        if !validate_nominal(&output.resource_type) || !outputs.insert(output.path.clone()) {
            return Err(invalid_descriptor("invalid or duplicate output slot"));
        }
        let schema = tool
            .output_schema
            .as_ref()
            .ok_or_else(|| invalid_descriptor("resource output requires output_schema"))?;
        validate_resource_schema(schema_at(schema, &output.path)?, &output.resource_type)?;
    }
    for (schema, declared) in std::iter::once((&tool.parameters, inputs))
        .chain(tool.output_schema.as_ref().map(|s| (s, outputs)))
    {
        let mut found = BTreeSet::new();
        resource_schema_paths(schema, "", true, &mut found)?;
        if found != declared {
            return Err(invalid_descriptor(
                "resource schema slots and metadata must agree exactly",
            ));
        }
    }
    Ok(())
}

fn collect_resource_values(value: &serde_json::Value, path: &str, found: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if object.contains_key("$resource") {
                found.insert(path.to_owned());
                return;
            }
            for (key, value) in object {
                collect_resource_values(
                    value,
                    &format!("{path}/{}", key.replace('~', "~0").replace('/', "~1")),
                    found,
                );
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_resource_values(value, &format!("{path}/{index}"), found);
            }
        }
        _ => {}
    }
}

pub(super) fn resource_values<'a>(
    value: &serde_json::Value,
    slots: impl Iterator<Item = (&'a str, &'a str)>,
) -> Result<Vec<ResourceRef>, ExtensionRuntimeError> {
    let mut references = Vec::new();
    let mut declared = BTreeSet::new();
    for (path, nominal) in slots {
        if let Some(value) = value.pointer(path) {
            let reference: ResourceRef = serde_json::from_value(value.clone())
                .map_err(|_| resource_error("resource_unavailable"))?;
            if reference.resource_type != nominal {
                return Err(resource_error("resource_type_mismatch"));
            }
            declared.insert(path.to_owned());
            references.push(reference);
        }
    }
    let mut found = BTreeSet::new();
    collect_resource_values(value, "", &mut found);
    if declared != found {
        return Err(resource_error("resource_unavailable"));
    }
    Ok(references)
}

pub(super) fn operation_inputs(
    definition: &ToolDefinition,
    arguments: &serde_json::Value,
) -> Result<Vec<(ResourceRef, String)>, ExtensionRuntimeError> {
    let mut references = Vec::new();
    if let Some(operation) = &definition.operation {
        for slot in &operation.resource_inputs {
            if let Some(value) = arguments.pointer(&slot.path) {
                let reference = serde_json::from_value(value.clone())
                    .map_err(|_| resource_error("resource_unavailable"))?;
                references.push((reference, slot.resource_type.clone()));
            }
        }
    }
    Ok(references)
}

pub(super) fn operation_outputs(
    definition: &ToolDefinition,
    output: &ToolCallOutput,
) -> Result<Vec<ResourceRef>, ExtensionRuntimeError> {
    // Metadata (including diagnostics) cannot be an undeclared reference grant.
    resource_values(&output.metadata, std::iter::empty())?;
    let value = output
        .structured_content
        .as_ref()
        .unwrap_or(&serde_json::Value::Null);
    if output.is_error {
        return resource_values(value, std::iter::empty());
    }
    resource_values(
        value,
        definition
            .operation
            .iter()
            .flat_map(|o| o.resource_outputs.iter())
            .map(|s| (s.path.as_str(), s.resource_type.as_str())),
    )
}
