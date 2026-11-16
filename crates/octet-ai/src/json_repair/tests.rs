//! Unit tests for `crate::json_repair`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::json_repair`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

#[test]
fn valid_json_is_canonicalized_without_semantic_changes() {
    assert_eq!(
        normalize_json_object(r#"{ "path": "src/main.rs", "n": 2 }"#).unwrap(),
        r#"{"n":2,"path":"src/main.rs"}"#
    );
}

#[test]
fn repairs_controls_invalid_escapes_trailing_commas_and_python_literals() {
    let raw = "{path:'C:\\Users\\example', lines:['a\nb',], ok:True, none:None,}";
    let value = parse_json_value(raw).unwrap();
    assert_eq!(value["path"], r"C:\Users\example");
    assert_eq!(value["lines"][0], "a\nb");
    assert_eq!(value["ok"], true);
    assert!(value["none"].is_null());
}

#[test]
fn accepts_json_code_fences() {
    assert_eq!(
        normalize_json_object("```json\n{\"path\":\"README.md\"}\n```").unwrap(),
        r#"{"path":"README.md"}"#
    );
    assert_eq!(
        normalize_json_object("{'message':'你好 🌲'}").unwrap(),
        r#"{"message":"你好 🌲"}"#
    );
}

#[test]
fn never_completes_truncated_json() {
    for raw in [r#"{"command":"rm -r"#, r#"{"path":"src"#, "{'path':'src"] {
        assert!(normalize_json_object(raw).is_err(), "accepted {raw:?}");
    }
}

#[test]
fn validates_repaired_arguments_against_tool_schema() {
    let tools = vec![crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "read".to_owned(),
        description: "Read a file".to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "integer", "minimum": 1}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    }];
    validate_tool_definitions(&tools).unwrap();
    let arguments = normalize_json_object_value("{path:'README.md', offset:1}").unwrap();
    validate_tool_arguments("read", &arguments, &tools).unwrap();

    for invalid in [
        serde_json::json!({"offset": 1}),
        serde_json::json!({"path": "README.md", "offset": 0}),
        serde_json::json!({"path": "README.md", "unexpected": true}),
        serde_json::json!({"path": 7}),
        serde_json::json!({}),
    ] {
        assert_eq!(
            validate_tool_arguments("read", &invalid, &tools).unwrap(),
            crate::types::ToolArgumentValidation::SchemaMismatch,
        );
    }
    // Unknown names are preserved for the agent dispatcher to report as a
    // tool result so the model can recover on its next turn.
    assert_eq!(
        validate_tool_arguments("no_such_tool", &arguments, &tools).unwrap(),
        crate::types::ToolArgumentValidation::UnknownTool,
    );
}

#[test]
fn rejects_ambiguous_or_unbounded_tool_schemas() {
    let duplicate = vec![
        crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "same".to_owned(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
        },
        crate::types::ToolDef {
            async_execution: false,
            constrained_sampling: None,
            name: "same".to_owned(),
            description: String::new(),
            parameters: serde_json::json!({"type": "object"}),
        },
    ];
    assert!(validate_tool_definitions(&duplicate).is_err());
    assert!(validate_tool_definitions(&[crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "unsupported".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"value": {"type": "string", "pattern": "["}}
        }),
    }])
    .is_err());
}

#[test]
fn value_validation_work_exhaustion_is_fatal_and_secret_free() {
    let tools = [crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "bounded".to_owned(),
        description: String::new(),
        parameters: serde_json::json!({
            "type": "object",
            "additionalProperties": {"type": "string"},
        }),
    }];
    validate_tool_definitions(&tools).unwrap();
    // Each property consumes work in the object and child-schema passes,
    // so this remains a fatal resource-bound failure rather than a normal
    // schema mismatch.
    let arguments = serde_json::Value::Object(
        (0..MAX_SCHEMA_NODES)
            .map(|index| {
                (
                    format!("field_{index}"),
                    serde_json::Value::String("provider-secret-value".to_owned()),
                )
            })
            .collect(),
    );
    let error = validate_tool_arguments("bounded", &arguments, &tools).unwrap_err();
    let message = error.to_string();
    assert!(message.contains("validation work limit exceeded"));
    assert!(!message.contains("provider-secret-value"));
    assert!(message.len() <= MAX_SCHEMA_ERROR_BYTES);
}

#[test]
fn bounds_schema_validation_work_and_error_text() {
    let mut schema = serde_json::json!({"type": "object"});
    for _ in 0..=MAX_SCHEMA_DEPTH {
        schema = serde_json::json!({"type": "object", "properties": {"next": schema}});
    }
    let error = validate_tool_definitions(&[crate::types::ToolDef {
        async_execution: false,
        constrained_sampling: None,
        name: "deep".to_owned(),
        description: String::new(),
        parameters: schema,
    }])
    .unwrap_err();
    let message = error.to_string();
    assert!(message.len() <= MAX_SCHEMA_ERROR_BYTES);
    assert!(message.contains("nesting") || message.contains("work limit"));
}

fn plain_tool(name: String, parameters: serde_json::Value) -> crate::types::ToolDef {
    crate::types::ToolDef {
        async_execution: false,
        name,
        description: "ordinary tool".into(),
        parameters,
        constrained_sampling: None,
    }
}

#[test]
fn many_ordinary_tool_schemas_validate_with_independent_budgets() {
    // 160 tools of ~60 schema nodes each exceed one shared 4,096-node budget
    // but each is far below the per-schema limit.
    let tools: Vec<crate::types::ToolDef> = (0..160)
        .map(|index| {
            let properties: serde_json::Map<String, serde_json::Value> = (0..20)
                .map(|field| {
                    (
                        format!("field_{field}"),
                        serde_json::json!({"type": "string", "description": "value"}),
                    )
                })
                .collect();
            plain_tool(
                format!("tool_{index}"),
                serde_json::json!({"type": "object", "properties": properties}),
            )
        })
        .collect();
    super::validate_tool_definitions(&tools).expect("independent per-tool budgets");
}

#[test]
fn one_oversized_tool_schema_still_exceeds_its_budget() {
    let properties: serde_json::Map<String, serde_json::Value> = (0..5_000)
        .map(|field| (format!("f{field}"), serde_json::json!({"type": "string"})))
        .collect();
    let tools = vec![plain_tool(
        "huge".into(),
        serde_json::json!({"type": "object", "properties": properties}),
    )];
    let error = super::validate_tool_definitions(&tools).unwrap_err();
    assert!(
        error.to_string().contains("validation work limit exceeded"),
        "{error}"
    );
}
