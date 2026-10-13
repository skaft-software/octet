//! Unit tests for `crate::constrained_sampling`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::constrained_sampling`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;
use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict, GrammarVariants, ToolDef};

fn tool(name: &str, parameters: Value, sampling: Option<ConstrainedSampling>) -> ToolDef {
    ToolDef {
        async_execution: false,
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
    let schema = json!({"type": "object", "properties": {"x": {"type": "string"}}, "oneOf": []});
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
