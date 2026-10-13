use super::*;
use crate::types::{ToolArgumentValidation, ToolDef};
use serde_json::{json, Value};

fn tool(parameters: Value) -> ToolDef {
    ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "fleet_probe".to_owned(),
        description: "Fleet schema probe".to_owned(),
        parameters,
    }
}

fn pattern_tool(pattern: Value) -> ToolDef {
    tool(json!({
        "type": "object",
        "properties": {"key": {"type": "string", "pattern": pattern}},
        "required": ["key"]
    }))
}

#[test]
fn pi_fleet_pattern_profile_matches_typebox_golden_cases() {
    // The adapter tests also admit these exact patterns and check every value
    // with the pinned TypeBox compiler. Rust must enforce the same constraint.
    let cases: Vec<Value> = serde_json::from_str(include_str!("pattern_cases.json")).unwrap();
    for case in cases {
        let tools = [pattern_tool(case["pattern"].clone())];
        let schema = tools[0].parameters.clone();
        let admission = validate_tool_definitions(&tools);
        assert_eq!(
            admission.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{case}"
        );
        if case["accepted"] == true {
            for (key, accepted) in case["values"]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| (pair[0].clone(), pair[1].as_bool().unwrap()))
            {
                let outcome =
                    validate_tool_arguments("fleet_probe", &json!({"key": key}), &tools).unwrap();
                assert_eq!(
                    outcome,
                    if accepted {
                        ToolArgumentValidation::Valid
                    } else {
                        ToolArgumentValidation::SchemaMismatch
                    },
                    "{case}: {key}"
                );
            }
        } else {
            assert!(admission.unwrap_err().to_string().contains("pattern"));
        }
        assert_eq!(schema, tools[0].parameters);
    }
}

#[test]
fn pi_fleet_pattern_lane_key_bounds_and_non_string_semantics() {
    let tools = [pattern_tool(json!("^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$"))];
    for (length, expected) in [
        (128, ToolArgumentValidation::Valid),
        (129, ToolArgumentValidation::SchemaMismatch),
        (16_384, ToolArgumentValidation::SchemaMismatch),
    ] {
        assert_eq!(
            validate_tool_arguments("fleet_probe", &json!({"key": "a".repeat(length)}), &tools)
                .unwrap(),
            expected
        );
    }
    // JSON Schema's pattern applies only to strings; it does not imply type.
    let tools = [tool(json!({"type": "object", "properties": {
        "key": {"pattern": "^a$"}
    }}))];
    for key in [json!(123), json!(null), json!(false)] {
        assert_eq!(
            validate_tool_arguments("fleet_probe", &json!({"key": key}), &tools).unwrap(),
            ToolArgumentValidation::Valid
        );
    }
}

#[test]
fn pi_fleet_pattern_admission_and_matching_are_bounded() {
    for pattern in [
        json!(null),
        json!(true),
        json!(123),
        json!({}),
        json!(format!("^{}$", "a".repeat(1023))),
    ] {
        assert!(validate_tool_definitions(&[pattern_tool(pattern)]).is_err());
    }
    let tools = [pattern_tool(json!("^a{1024}$"))];
    validate_tool_definitions(&tools).unwrap();
    assert_eq!(
        validate_tool_arguments("fleet_probe", &json!({"key": "a".repeat(1024)}), &tools).unwrap(),
        ToolArgumentValidation::Valid
    );
    let tools = [pattern_tool(json!(format!("^{}$", "a{0,16}".repeat(16))))];
    validate_tool_definitions(&tools).unwrap();
    let error = validate_tool_arguments("fleet_probe", &json!({"key": "a".repeat(256)}), &tools)
        .unwrap_err();
    assert!(error.to_string().contains("validation work limit exceeded"));
}

#[test]
fn pi_fleet_deprecated_is_preserved_boolean_annotation_not_constraint() {
    let schema = json!({"type": "object", "properties": {"acceptance": {"anyOf": [
        {"type": "string", "enum": ["checked"]},
        {"type": "string", "enum": ["reviewed"], "deprecated": true}
    ]}}, "deprecated": false});
    let tools = [tool(schema.clone())];
    validate_tool_definitions(&tools).unwrap();
    assert_eq!(
        validate_tool_arguments("fleet_probe", &json!({"acceptance": "reviewed"}), &tools).unwrap(),
        ToolArgumentValidation::Valid
    );
    assert_eq!(
        validate_tool_arguments("fleet_probe", &json!({"acceptance": "other"}), &tools).unwrap(),
        ToolArgumentValidation::SchemaMismatch
    );
    assert_eq!(tools[0].parameters, schema);
    for deprecated in [json!(null), json!("true"), json!(0), json!({})] {
        assert!(validate_tool_definitions(&[tool(
            json!({"type": "object", "deprecated": deprecated})
        )])
        .unwrap_err()
        .to_string()
        .contains("deprecated must be boolean"));
    }
}
