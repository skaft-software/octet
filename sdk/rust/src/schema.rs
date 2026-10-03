//! The deliberately bounded input-schema subset shared by all native frontends.
use serde_json::{Map, Value};

use crate::{Error, MAX_TEXT_BYTES};

const KEYWORDS: &[&str] = &[
    "$schema",
    "title",
    "description",
    "default",
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "anyOf",
    "allOf",
    "oneOf",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
];

pub(crate) fn generated<T: schemars::JsonSchema>() -> Result<Value, Error> {
    let mut settings = schemars::gen::SchemaSettings::draft07();
    settings.inline_subschemas = true;
    let root = settings.into_generator().into_root_schema_for::<T>();
    let mut value = serde_json::to_value(root).map_err(|_| Error::internal())?;
    // Schemars' numeric formats are descriptive, not constraints in the host's
    // schema subset. Rust deserialization still enforces the concrete width.
    strip_numeric_formats(&mut value);
    definition(&value)?;
    Ok(value)
}

fn strip_numeric_formats(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        if object.get("type").is_some_and(|kind| {
            let numeric = |v: &Value| matches!(v.as_str(), Some("integer" | "number"));
            numeric(kind)
                || kind
                    .as_array()
                    .is_some_and(|types| types.iter().any(numeric))
        }) {
            object.remove("format");
        }
        if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
            for child in properties.values_mut() {
                strip_numeric_formats(child);
            }
        }
        if let Some(items) = object.get_mut("items") {
            strip_numeric_formats(items);
        }
        if let Some(additional) = object.get_mut("additionalProperties") {
            if additional.is_object() {
                strip_numeric_formats(additional);
            }
        }
        for key in ["anyOf", "allOf", "oneOf"] {
            if let Some(branches) = object.get_mut(key).and_then(Value::as_array_mut) {
                for child in branches {
                    strip_numeric_formats(child);
                }
            }
        }
    }
}

pub(crate) fn definition(schema: &Value) -> Result<(), Error> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err(Error::invalid("tool input must be an object schema"));
    }
    if serde_json::to_vec(schema)
        .map_err(|_| Error::internal())?
        .len()
        > 64 * 1024
    {
        return Err(Error::invalid("input schema exceeds 64 KiB"));
    }
    let mut budget = 4096;
    visit(schema, 0, &mut budget)
}

fn visit(schema: &Value, depth: usize, budget: &mut usize) -> Result<(), Error> {
    if depth > 32 || *budget == 0 {
        return Err(Error::invalid("schema complexity exceeds native bounds"));
    }
    *budget -= 1;
    let object = schema
        .as_object()
        .ok_or_else(|| Error::invalid("schema nodes must be objects"))?;
    for (key, value) in object {
        if !KEYWORDS.contains(&key.as_str()) {
            return Err(Error::invalid(format!(
                "unsupported native schema keyword: {key}"
            )));
        }
        match key.as_str() {
            "$schema" | "title" | "description" if !value.is_string() => {
                return Err(Error::invalid("schema label must be a string"))
            }
            "type" => {
                let valid = |v: &Value| {
                    v.as_str().is_some_and(|s| {
                        matches!(
                            s,
                            "object"
                                | "array"
                                | "string"
                                | "integer"
                                | "number"
                                | "boolean"
                                | "null"
                        )
                    })
                };
                if !valid(value)
                    && !value
                        .as_array()
                        .is_some_and(|a| !a.is_empty() && a.iter().all(valid))
                {
                    return Err(Error::invalid("invalid schema type"));
                }
            }
            "properties" => {
                let props = value
                    .as_object()
                    .ok_or_else(|| Error::invalid("properties must be an object"))?;
                for (name, child) in props {
                    if name.len() > 256 {
                        return Err(Error::invalid("property name exceeds 256 bytes"));
                    }
                    visit(child, depth + 1, budget)?;
                }
            }
            "required" => {
                let names = value
                    .as_array()
                    .ok_or_else(|| Error::invalid("required must be an array"))?;
                let mut unique = std::collections::BTreeSet::new();
                for name in names {
                    let name = name
                        .as_str()
                        .ok_or_else(|| Error::invalid("required names must be strings"))?;
                    if !unique.insert(name) {
                        return Err(Error::invalid("duplicate required name"));
                    }
                }
            }
            "additionalProperties" if value.is_boolean() => {}
            "additionalProperties" | "items" => visit(value, depth + 1, budget)?,
            "anyOf" | "allOf" | "oneOf" => {
                let branches = value
                    .as_array()
                    .filter(|a| !a.is_empty())
                    .ok_or_else(|| Error::invalid("schema branches must be a nonempty array"))?;
                for child in branches {
                    visit(child, depth + 1, budget)?;
                }
            }
            "enum" if !value.as_array().is_some_and(|a| !a.is_empty()) => {
                return Err(Error::invalid("enum must be a nonempty array"))
            }
            "minimum" | "maximum" if !value.is_number() => {
                return Err(Error::invalid("numeric bound must be a number"))
            }
            "minLength" | "maxLength" | "minItems" | "maxItems" if value.as_u64().is_none() => {
                return Err(Error::invalid("length bound must be unsigned"))
            }
            _ => {}
        }
    }
    Ok(())
}

pub(crate) fn arguments(schema: &Value, value: &Value) -> Result<(), Error> {
    // Both values have already passed depth/node bounds, and schema syntax was
    // checked at registration. This validator also protects direct process callers.
    validate(schema.as_object().unwrap(), value)
}

fn validate(schema: &Map<String, Value>, value: &Value) -> Result<(), Error> {
    let bad = || Error::invalid("arguments do not match the registered input schema");
    if let Some(kind) = schema.get("type") {
        let matches = |kind: &Value| match kind.as_str().unwrap() {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            _ => false,
        };
        if !kind
            .as_array()
            .map_or_else(|| matches(kind), |a| a.iter().any(matches))
        {
            return Err(bad());
        }
    }
    if let Some(values) = schema.get("enum") {
        if !values.as_array().unwrap().contains(value) {
            return Err(bad());
        }
    }
    for (key, test) in [("allOf", 0), ("anyOf", 1), ("oneOf", 2)] {
        if let Some(branches) = schema.get(key) {
            let branches = branches.as_array().unwrap();
            let count = branches
                .iter()
                .filter(|s| validate(s.as_object().unwrap(), value).is_ok())
                .count();
            if match test {
                0 => count != branches.len(),
                1 => count == 0,
                _ => count != 1,
            } {
                return Err(bad());
            }
        }
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required") {
            if required
                .as_array()
                .unwrap()
                .iter()
                .any(|k| !object.contains_key(k.as_str().unwrap()))
            {
                return Err(bad());
            }
        }
        for (key, child) in object {
            if let Some(child_schema) = schema.get("properties").and_then(|p| p.get(key)) {
                validate(child_schema.as_object().unwrap(), child)?;
            } else if let Some(additional) = schema.get("additionalProperties") {
                if additional == &Value::Bool(false) {
                    return Err(bad());
                }
                if let Some(s) = additional.as_object() {
                    validate(s, child)?;
                }
            }
        }
    }
    if let Some(number) = value.as_f64() {
        if schema
            .get("minimum")
            .is_some_and(|v| number < v.as_f64().unwrap())
            || schema
                .get("maximum")
                .is_some_and(|v| number > v.as_f64().unwrap())
        {
            return Err(bad());
        }
    }
    if let Some(text) = value.as_str() {
        // JSON Schema length counts Unicode scalar values; a separate byte cap
        // bounds memory and is common to Rust and C.
        if text.len() > MAX_TEXT_BYTES {
            return Err(bad());
        }
        let len = text.chars().count() as u64;
        if schema
            .get("minLength")
            .is_some_and(|v| len < v.as_u64().unwrap())
            || schema
                .get("maxLength")
                .is_some_and(|v| len > v.as_u64().unwrap())
        {
            return Err(bad());
        }
    }
    if let Some(array) = value.as_array() {
        let len = array.len() as u64;
        if schema
            .get("minItems")
            .is_some_and(|v| len < v.as_u64().unwrap())
            || schema
                .get("maxItems")
                .is_some_and(|v| len > v.as_u64().unwrap())
        {
            return Err(bad());
        }
        if let Some(items) = schema.get("items") {
            for child in array {
                validate(items.as_object().unwrap(), child)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(deny_unknown_fields)]
    struct Input {
        name: String,
        delay: Option<u32>,
    }
    #[test]
    fn generated_schema_is_real_and_checked() {
        let schema = generated::<Input>().unwrap();
        assert_eq!(schema["properties"]["name"]["type"], "string");
        assert_eq!(schema["additionalProperties"], false);
        assert!(arguments(&schema, &json!({"name":"world"})).is_ok());
        assert!(arguments(&schema, &json!({"name":1})).is_err());
        assert!(arguments(&schema, &json!({"name":"world", "other":true})).is_err());
        let input: Input = serde_json::from_value(json!({"name":"world"})).unwrap();
        assert_eq!(input.name, "world");
        assert!(input.delay.is_none());
    }
    #[test]
    fn generated_nested_schemas_work_and_recursion_fails_explicitly() {
        #[derive(schemars::JsonSchema)]
        struct Nested {
            #[allow(dead_code)]
            value: bool,
        }
        #[derive(schemars::JsonSchema)]
        struct Composite {
            #[allow(dead_code)]
            nested: Nested,
            #[allow(dead_code)]
            values: Vec<String>,
            #[allow(dead_code)]
            optional: Option<i32>,
        }
        #[derive(schemars::JsonSchema)]
        struct Recursive {
            #[allow(dead_code)]
            next: Option<Box<Recursive>>,
        }
        let schema = generated::<Composite>().unwrap();
        assert!(arguments(
            &schema,
            &json!({"nested":{"value":true},"values":["a"],"optional":null})
        )
        .is_ok());
        assert!(arguments(&schema, &json!({"nested":{"value":1},"values":["a"]})).is_err());
        assert!(arguments(&schema, &json!({"nested":{"value":true},"values":[1]})).is_err());
        assert!(generated::<Recursive>().is_err());
    }
    #[test]
    fn constraints_and_unsupported_vocabulary() {
        let schema = json!({"type":"object","properties":{"n":{"type":"integer","minimum":0,"maximum":5},"s":{"type":"string","maxLength":2}},"required":["n"],"additionalProperties":false});
        definition(&schema).unwrap();
        assert!(arguments(&schema, &json!({"n":4,"s":"éé"})).is_ok());
        for v in [
            json!({"n":6}),
            json!({"n":true}),
            json!({}),
            json!({"n":1,"s":"abc"}),
        ] {
            assert!(arguments(&schema, &v).is_err());
        }
        assert!(definition(&json!({"type":"object","$ref":"#/definitions/X"})).is_err());
        assert!(definition(&json!({"type":"object","required":[1]})).is_err());
    }
}
