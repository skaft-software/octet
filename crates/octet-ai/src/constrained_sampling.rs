//! Provider-side constrained sampling for tool definitions.
//!
//! Several APIs implement a tool "strict" mode as JSON-schema constrained
//! sampling. Their accepted schema subset is narrower than the general JSON
//! Schema dialect, so a schema that merely declines to opt in is not the same
//! as one that the provider can enforce. This module mirrors Pi's
//! `constrained-sampling` helper:
//!
//! * [`make_strict_json_schema`] rewrites a schema into the strict subset or
//!   reports why it cannot.
//! * [`resolve_json_schema_strict`] decides whether to request strict mode from
//!   a [`ToolDef`] and the route's support, honoring `prefer`/`require`.
//! * [`resolve_grammar`] extracts an OpenAI `custom` tool grammar definition.
//!
//! A `require` constraint that cannot be satisfied is a request error. It is
//! never silently relaxed to unconstrained sampling.

use serde_json::{json, Map, Value};

use crate::error::{AiError, UnsupportedError};
use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, ToolDef};

/// JSON-schema keywords that the strict subset cannot represent.
const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

/// Reason a schema is outside the strict constrained-sampling subset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictSchemaError {
    message: String,
}

impl StrictSchemaError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Human-readable reason.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl std::fmt::Display for StrictSchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StrictSchemaError {}

/// Convert a tool parameter schema to the strict subset expected by provider
/// constrained sampling.
///
/// The input is never mutated; the returned schema is a fresh value.
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, StrictSchemaError> {
    let mut cloned = schema.clone();
    make_node_strict(&mut cloned)?;
    if cloned.get("type").and_then(Value::as_str) != Some("object") {
        return Err(StrictSchemaError::new("root schema must have type object"));
    }
    Ok(cloned)
}

/// Return `strict`-rewritten parameters for a tool, or the original parameters
/// when strict mode is not requested.
pub fn get_json_schema_tool_parameters(
    tool: &ToolDef,
    strict: bool,
) -> Result<Value, StrictSchemaError> {
    if strict {
        make_strict_json_schema(&tool.parameters)
    } else {
        Ok(tool.parameters.clone())
    }
}

/// Resolve the wire parameters and `strict` flag for a function tool on a route
/// that supports strict JSON-schema constrained sampling.
///
/// Returns `(parameters, strict)`. `strict` is `true` only when the caller asked
/// for strict JSON-schema sampling and the route can enforce the rewritten
/// schema; a `require` request that cannot be honored is an error, never a
/// silent downgrade.
pub fn function_tool_parameters(
    tool: &ToolDef,
    supports_strict_mode: bool,
) -> Result<(Value, bool), AiError> {
    let strict = resolve_json_schema_strict(tool, supports_strict_mode)?;
    let parameters =
        get_json_schema_tool_parameters(tool, strict == Some(true)).map_err(|error| {
            AiError::from(UnsupportedError::ConstrainedSampling(format!(
                "Tool \"{}\" cannot use JSON-schema constrained sampling: {}",
                tool.name, error
            )))
        })?;
    Ok((parameters, strict == Some(true)))
}

/// Decide whether to request strict JSON-schema sampling for a tool.
///
/// Returns `Ok(None)` when the tool made no JSON-schema request, or when the
/// route cannot express it and the tool only asked to `prefer` strict mode.
/// A `require` request that cannot be honored is an error.
pub fn resolve_json_schema_strict(
    tool: &ToolDef,
    supports_strict_mode: bool,
) -> Result<Option<bool>, AiError> {
    let strict = match &tool.constrained_sampling {
        Some(ConstrainedSampling::JsonSchema { strict }) => *strict,
        _ => return Ok(None),
    };
    if supports_strict_mode {
        return match make_strict_json_schema(&tool.parameters) {
            Ok(_) => Ok(Some(true)),
            Err(error) => {
                if strict == ConstrainedSamplingStrict::Require {
                    Err(UnsupportedError::ConstrainedSampling(format!(
                        "Tool \"{}\" requires JSON-schema constrained sampling, but {}",
                        tool.name, error
                    ))
                    .into())
                } else {
                    Ok(None)
                }
            }
        };
    }
    if strict == ConstrainedSamplingStrict::Require {
        return Err(UnsupportedError::ConstrainedSampling(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported on this route",
            tool.name
        ))
        .into());
    }
    Ok(None)
}

/// Wire encoding selected for a grammar-constrained tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    /// `lark` or `regex`.
    pub format: &'static str,
    /// Grammar definition text.
    pub definition: String,
    /// Schema property carrying the generated input string.
    pub input_property: String,
}

/// Resolve a grammar-constrained tool for an OpenAI-compatible route.
///
/// Returns `Ok(None)` when the tool made no grammar request or the route does
/// not support OpenAI grammar tools. A grammar request with no usable variant
/// or an invalid schema is always an error: there is no weaker fallback.
pub fn resolve_grammar(
    tool: &ToolDef,
    supports_openai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, AiError> {
    let variants = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Grammar { variants }) => variants,
        _ => return Ok(None),
    };
    if !supports_openai_grammar_tools {
        return Ok(None);
    }
    let lark = variants
        .openai_lark
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let regex = variants
        .openai_regex
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let (format, definition) = match (lark, regex) {
        (Some(lark), _) => ("lark", lark),
        (None, Some(regex)) => ("regex", regex),
        (None, None) => {
            return Err(UnsupportedError::ConstrainedSampling(format!(
                "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided",
                tool.name
            ))
            .into())
        }
    };
    let input_property = infer_grammar_input_property(tool).map_err(|reason| {
        AiError::from(UnsupportedError::ConstrainedSampling(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {reason}",
            tool.name
        )))
    })?;
    Ok(Some(GrammarConstrainedSampling {
        format,
        definition: definition.to_owned(),
        input_property,
    }))
}

/// Grammar tools carry their generated text in exactly one required string
/// property. Infer it from the tool schema, mirroring Pi.
pub fn infer_grammar_input_property(tool: &ToolDef) -> Result<String, String> {
    let schema = tool.parameters.as_object().ok_or_else(|| {
        "grammar constrained sampling requires an object parameter schema".to_owned()
    })?;
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("grammar constrained sampling requires an object parameter schema".to_owned());
    }
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            "grammar constrained sampling requires exactly one required string property".to_owned()
        })?;
    if required.len() != 1 {
        return Err(
            "grammar constrained sampling requires exactly one required string property".to_owned(),
        );
    }
    let input_property = required[0]
        .as_str()
        .ok_or_else(|| {
            "grammar constrained sampling requires exactly one required string property".to_owned()
        })?
        .to_owned();
    let property = schema
        .get("properties")
        .and_then(Value::as_object)
        .and_then(|properties| properties.get(&input_property))
        .ok_or_else(|| {
            format!("grammar constrained sampling requires a properties entry for {input_property}")
        })?;
    if property.get("type").and_then(Value::as_str) != Some("string") {
        return Err(format!(
            "grammar constrained sampling property {input_property} must have type string"
        ));
    }
    Ok(input_property)
}

fn structured_schema(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    types.contains(&"object")
        || types.contains(&"array")
        || object.contains_key("properties")
        || object.contains_key("items")
}

fn schema_allows_null(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("type").and_then(Value::as_str) == Some("null") {
        return true;
    }
    if let Some(Value::Array(types)) = object.get("type") {
        if types.iter().any(|t| t.as_str() == Some("null")) {
            return true;
        }
    }
    if matches!(object.get("const"), Some(Value::Null)) {
        return true;
    }
    if let Some(Value::Array(values)) = object.get("enum") {
        if values.iter().any(|v| v.is_null()) {
            return true;
        }
    }
    if let Some(Value::Array(variants)) = object.get("anyOf") {
        return variants.iter().any(schema_allows_null);
    }
    false
}

fn make_node_strict(value: &mut Value) -> Result<(), StrictSchemaError> {
    let object: &mut Map<String, Value> = match value.as_object_mut() {
        Some(object) => object,
        None => return Err(StrictSchemaError::new("boolean schemas are unsupported")),
    };
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        if object.contains_key(*key) {
            return Err(StrictSchemaError::new(format!(
                "{key} schemas are unsupported"
            )));
        }
    }

    if let Some(any_of) = object.get_mut("anyOf") {
        let variants = match any_of.as_array_mut() {
            Some(variants) if !variants.is_empty() => variants,
            _ => {
                return Err(StrictSchemaError::new(
                    "anyOf must contain at least one schema",
                ))
            }
        };
        for variant in variants.iter_mut() {
            if structured_schema(variant) {
                return Err(StrictSchemaError::new(
                    "object and array unions are unsupported",
                ));
            }
            make_node_strict(variant)?;
        }
    }

    if let Some(items) = object.get_mut("items") {
        if items.is_array() {
            return Err(StrictSchemaError::new("tuple schemas are unsupported"));
        }
        make_node_strict(items)?;
    }

    let is_object_schema = object.get("type").and_then(Value::as_str) == Some("object");
    if object.contains_key("properties") && !is_object_schema {
        return Err(StrictSchemaError::new("properties require type object"));
    }
    if !is_object_schema {
        return Ok(());
    }
    if let Some(additional) = object.get("additionalProperties") {
        if additional != &Value::Bool(false) {
            return Err(StrictSchemaError::new(
                "schema-valued or true additionalProperties is unsupported",
            ));
        }
    }

    // Collect property names up front so the required set and each property can
    // be validated independently of map iteration order.
    let property_names: Vec<String> = match object.get("properties") {
        Some(Value::Object(properties)) => properties.keys().cloned().collect(),
        Some(_) => {
            return Err(StrictSchemaError::new(
                "object properties must be a schema map",
            ))
        }
        None => Vec::new(),
    };
    if let Some(required) = object.get("required") {
        match required.as_array() {
            Some(entries) if entries.iter().all(|entry| entry.is_string()) => {}
            _ => {
                return Err(StrictSchemaError::new(
                    "object required must be a string array",
                ))
            }
        }
    }
    let required: std::collections::BTreeSet<String> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for key in &required {
        if !property_names.contains(key) {
            return Err(StrictSchemaError::new(
                "required contains an unknown property",
            ));
        }
    }

    if let Some(Value::Object(properties)) = object.get_mut("properties") {
        for (key, property) in properties.iter_mut() {
            make_node_strict(property)?;
            if !required.contains(key) && !schema_allows_null(property) {
                let original = property.take();
                *property = json!({ "anyOf": [original, { "type": "null" }] });
            }
        }
    }

    object.insert(
        "required".to_owned(),
        Value::Array(property_names.into_iter().map(Value::String).collect()),
    );
    object.insert("additionalProperties".to_owned(), Value::Bool(false));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, GrammarVariants, ToolDef};

    fn tool(name: &str, parameters: Value, sampling: Option<ConstrainedSampling>) -> ToolDef {
        ToolDef {
            name: name.to_owned(),
            description: String::new(),
            parameters,
            constrained_sampling: sampling,
        }
    }

    #[test]
    fn strict_schema_closes_objects_and_nullifies_optional_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "city": {"type": "string"},
                "note": {"type": "string"},
                "tag": {"type": ["string", "null"]}
            },
            "required": ["city"]
        });
        let strict = make_strict_json_schema(&schema).unwrap();
        assert_eq!(strict["required"], json!(["city", "note", "tag"]));
        assert_eq!(strict["additionalProperties"], json!(false));
        // A non-nullable optional property gains an explicit null alternative.
        assert_eq!(
            strict["properties"]["note"],
            json!({"anyOf": [{"type": "string"}, {"type": "null"}]})
        );
        // An already-nullable optional property is left untouched.
        assert_eq!(
            strict["properties"]["tag"],
            json!({"type": ["string", "null"]})
        );
        // The required property is unchanged.
        assert_eq!(strict["properties"]["city"], json!({"type": "string"}));
    }

    #[test]
    fn strict_schema_rejects_unsupported_keywords() {
        let schema =
            json!({"type": "object", "properties": {"x": {"type": "string"}}, "oneOf": []});
        assert!(make_strict_json_schema(&schema).is_err());
        let nested = json!({"type": "object", "properties": {"x": {"$ref": "#/y"}}});
        assert!(make_strict_json_schema(&nested).is_err());
    }

    #[test]
    fn require_fails_when_route_unsupported_but_prefer_is_omitted() {
        let prefer = tool(
            "t",
            json!({"type": "string"}),
            Some(ConstrainedSampling::JsonSchema {
                strict: ConstrainedSamplingStrict::Prefer,
            }),
        );
        assert_eq!(resolve_json_schema_strict(&prefer, false).unwrap(), None);
        let require = tool(
            "t",
            json!({"type": "string"}),
            Some(ConstrainedSampling::JsonSchema {
                strict: ConstrainedSamplingStrict::Require,
            }),
        );
        assert!(resolve_json_schema_strict(&require, false).is_err());
    }

    #[test]
    fn grammar_uses_lark_then_regex_and_infers_input_property() {
        let tool_def = tool(
            "grammar",
            json!({
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"]
            }),
            Some(ConstrainedSampling::Grammar {
                variants: GrammarVariants {
                    openai_lark: None,
                    openai_regex: Some("(?s).*".to_owned()),
                },
            }),
        );
        let resolved = resolve_grammar(&tool_def, true).unwrap().unwrap();
        assert_eq!(resolved.format, "regex");
        assert_eq!(resolved.input_property, "input");
        assert!(resolve_grammar(&tool_def, false).unwrap().is_none());
    }
}
