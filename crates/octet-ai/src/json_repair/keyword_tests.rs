use super::*;
use crate::types::{ToolArgumentValidation, ToolDef};
use serde_json::json;

#[test]
fn pi_schema_keywords_enforce_constraints() {
    let cases = [
        (
            json!({"type":"string","format":"uuid"}),
            json!("053dc2bc-237c-4c08-b4de-77b5dbd05fa2"),
            json!("bad"),
        ),
        (
            json!({"type":"number","multipleOf":0.1}),
            json!(0.3),
            json!(0.31),
        ),
        (
            json!({"type":"object","patternProperties":{"^x":{"type":"integer"}},"additionalProperties":false}),
            json!({"xyz":2}),
            json!({"xyz":"bad"}),
        ),
        (
            json!({"type":"array","items":[{"type":"string"}],"additionalItems":false}),
            json!(["x"]),
            json!(["x", 2]),
        ),
        (
            json!({"type":"array","prefixItems":[{"const":"x"}],"items":{"type":"integer"}}),
            json!(["x", 2]),
            json!(["x", "bad"]),
        ),
        (
            json!({"contains":{"type":"integer"},"minContains":2,"maxContains":2}),
            json!([1, "x", 2]),
            json!([1, "x"]),
        ),
        (
            json!({"if":{"required":["a"]},"then":{"required":["b"]},"else":{"not":{"required":["c"]}}}),
            json!({"a":1,"b":2}),
            json!({"a":1}),
        ),
        (
            json!({"dependentRequired":{"a":["b"]}}),
            json!({"a":1,"b":2}),
            json!({"a":1}),
        ),
        (
            json!({"dependencies":{"a":["b"]}}),
            json!({"a":1,"b":2}),
            json!({"a":1}),
        ),
        (
            json!({"dependentSchemas":{"a":{"required":["b"]}}}),
            json!({"a":1,"b":2}),
            json!({"a":1}),
        ),
        (
            json!({"propertyNames":{"pattern":"^[a-z]+$"}}),
            json!({"abc":1}),
            json!({"ABC":1}),
        ),
        (
            json!({"$defs":{"word":{"type":"string"}},"properties":{"a":{"$ref":"#/$defs/word"}}}),
            json!({"a":"yes"}),
            json!({"a":1}),
        ),
        (
            json!({"allOf":[{"properties":{"a":true}}],"unevaluatedProperties":false}),
            json!({"a":1}),
            json!({"b":1}),
        ),
        (
            json!({"anyOf":[{"prefixItems":[true]}],"unevaluatedItems":false}),
            json!([1]),
            json!([1, 2]),
        ),
        (json!({"properties":{"a":false}}), json!({}), json!({"a":1})),
    ];
    for (schema, good, bad) in cases {
        let tools = [ToolDef {
            name: "probe".into(),
            description: "Probe".into(),
            parameters: schema.clone(),
            constrained_sampling: None,
            async_execution: false,
        }];
        validate_tool_definitions(&tools).unwrap_or_else(|e| panic!("{schema}: {e}"));
        assert_eq!(
            validate_tool_arguments("probe", &good, &tools).unwrap(),
            ToolArgumentValidation::Valid,
            "{schema}: {good}"
        );
        assert_eq!(
            validate_tool_arguments("probe", &bad, &tools).unwrap(),
            ToolArgumentValidation::SchemaMismatch,
            "{schema}: {bad}"
        );
        assert_eq!(tools[0].parameters, schema);
    }
}

#[test]
fn pi_schema_default_formats_match_pinned_typebox_cases() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("format_cases.json")).unwrap();
    for case in cases {
        let tools = [ToolDef {
            name: "probe".into(),
            description: "Probe".into(),
            parameters: json!({"type":"string","format":case["format"]}),
            constrained_sampling: None,
            async_execution: false,
        }];
        validate_tool_definitions(&tools).unwrap();
        for pair in case["values"].as_array().unwrap() {
            let outcome = validate_tool_arguments("probe", &pair[0], &tools).unwrap();
            assert_eq!(
                outcome == ToolArgumentValidation::Valid,
                pair[1].as_bool().unwrap(),
                "{}: {}",
                case["format"],
                pair[0]
            );
        }
    }
}

#[test]
fn pi_schema_references_numeric_equality_and_composed_annotations() {
    for (schema, value, accepted) in [
        (
            json!({"$id":"urn:root","$defs":{"n":{"$anchor":"n","type":"integer"}},"properties":{"a":{"$ref":"#n"}}}),
            json!({"a":1}),
            true,
        ),
        (
            json!({"$defs":{"n":{"$id":"urn:number","type":"integer"}},"properties":{"a":{"$ref":"urn:number"}}}),
            json!({"a":"no"}),
            false,
        ),
        (
            json!({"anyOf":[{"properties":{"a":true}},{"properties":{"b":true}}],"unevaluatedProperties":false}),
            json!({"a":1,"b":2}),
            true,
        ),
        (
            json!({"contains":{"type":"integer"},"unevaluatedItems":false}),
            json!([1, "x"]),
            false,
        ),
        (json!({"uniqueItems":true}), json!([1, 1.0]), false),
        (json!({"type":"integer"}), json!(1.0), true),
        (json!({"const":{"n":1}}), json!({"n":1.0}), true),
        (
            json!({"const":9007199254740992_u64}),
            json!(9007199254740993_u64),
            false,
        ),
        (
            json!({"const":9007199254740992.0_f64}),
            json!(9007199254740993_u64),
            false,
        ),
        (
            json!({"const":18446744073709551616.0_f64}),
            json!(u64::MAX),
            false,
        ),
        (
            json!({"uniqueItems":true}),
            json!([9007199254740992_u64, 9007199254740993_u64]),
            true,
        ),
        (
            json!({"contentSchema":false,"readOnly":true,"$vocabulary":{"urn:annotation":false}}),
            json!({}),
            true,
        ),
    ] {
        let tools = [ToolDef {
            name: "probe".into(),
            description: "Probe".into(),
            parameters: schema.clone(),
            constrained_sampling: None,
            async_execution: false,
        }];
        validate_tool_definitions(&tools).unwrap();
        assert_eq!(
            validate_tool_arguments("probe", &value, &tools).unwrap()
                == ToolArgumentValidation::Valid,
            accepted,
            "{schema}: {value}"
        );
    }
    for schema in [
        json!({"$ref":"https://example.invalid/missing"}),
        json!({"$ref":"#"}),
    ] {
        let tools = [ToolDef {
            name: "probe".into(),
            description: "Probe".into(),
            parameters: schema,
            constrained_sampling: None,
            async_execution: false,
        }];
        assert!(validate_tool_arguments("probe", &json!({}), &tools).is_err());
    }
}

#[test]
fn pi_provider_schema_serialization_preserves_constraints_and_strict_refuses_unsupported_keywords()
{
    use crate::constrained_sampling::function_tool_parameters;
    use crate::types::{ConstrainedSampling, ConstrainedSamplingStrict};
    for schema in [
        json!({"type":"object","patternProperties":{"^x":{"multipleOf":0.5,"format":"unknown"}}}),
        json!({"type":"object","dependentRequired":{"a":["b"]}}),
        json!({"type":"object","properties":{"a":{"unevaluatedItems":false}}}),
        json!({"type":"object","properties":{"a":{"items":[true],"additionalItems":false}}}),
        json!({"type":"object","$dynamicRef":"#root"}),
    ] {
        let mut tool = ToolDef {
            name: "probe".into(),
            description: "Probe".into(),
            parameters: schema.clone(),
            constrained_sampling: None,
            async_execution: false,
        };
        assert_eq!(
            function_tool_parameters(&tool, true).unwrap(),
            (schema.clone(), false)
        );
        tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
            strict: ConstrainedSamplingStrict::Prefer,
        });
        assert_eq!(
            function_tool_parameters(&tool, true).unwrap(),
            (schema.clone(), false)
        );
        tool.constrained_sampling = Some(ConstrainedSampling::JsonSchema {
            strict: ConstrainedSamplingStrict::Require,
        });
        assert!(function_tool_parameters(&tool, true).is_err());
        assert_eq!(tool.parameters, schema);
    }
}
