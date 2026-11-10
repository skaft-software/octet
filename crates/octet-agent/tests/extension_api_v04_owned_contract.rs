//! Temporary old/new oracles plus independent assertions for the 0.4-owned data.
//! The oracles can be retired with 0.3; the literal 0.4 assertions must remain.

use octet_agent::{
    extension_api_policy as policy, extension_api_v03 as old, extension_api_v04 as api,
};
use serde::Serialize;
use serde_json::{json, Value};

fn normalized<T: Serialize, E>(
    result: Result<T, E>,
    error: impl FnOnce(E) -> (i64, String),
) -> Result<Value, (i64, String)> {
    result
        .map(|value| serde_json::to_value(value).unwrap())
        .map_err(error)
}

// Include malformed fields at every record level, not just top-level examples.
fn hostile_variants(value: &Value) -> Vec<Value> {
    let mut result = vec![
        value.clone(),
        Value::Null,
        json!(false),
        json!(1),
        json!([]),
        json!({}),
    ];
    if let Some(object) = value.as_object() {
        let mut unknown = value.clone();
        unknown["unexpected"] = json!("refuse");
        result.push(unknown);
        for (key, entry) in object {
            let mut missing = value.clone();
            missing.as_object_mut().unwrap().remove(key);
            result.push(missing);
            let variants = if entry.is_object() {
                hostile_variants(entry)
            } else if let Some(items) = entry.as_array() {
                let mut variants = vec![json!([]), json!(null), json!(true), json!([null])];
                if let Some(first) = items.first() {
                    variants.extend(
                        hostile_variants(first)
                            .into_iter()
                            .map(|item| json!([item])),
                    );
                    variants.push(json!(vec![first.clone(); 257]));
                }
                variants
            } else {
                vec![
                    json!(null),
                    json!(true),
                    json!(-1),
                    json!(0),
                    json!(1.5),
                    json!([]),
                    json!({}),
                    json!(""),
                    json!("é".repeat(129)),
                    json!(9_007_199_254_740_992_u64),
                ]
            };
            for replacement in variants {
                let mut hostile = value.clone();
                hostile[key] = replacement;
                result.push(hostile);
            }
        }
    }
    result
}

fn provider() -> Value {
    json!({"id":"local-provider", "label":"Local provider", "auth":{"kind":"none"}})
}

fn model() -> Value {
    json!({
        "id":"local-model", "api_name":"local/model", "protocol":"openai_chat",
        "context_window":8192, "max_output_tokens":1024,
        "capabilities":{"tools":true,"parallel_tool_calls":false,"structured_output":true,"reasoning":false}
    })
}

macro_rules! equivalent {
    ($parser:ident, $inputs:expr) => {{
        let inputs = $inputs;
        for input in &inputs {
            let expected = normalized(old::$parser(input.clone()), |e| (e.code, e.message));
            let actual = normalized(api::$parser(input.clone()), |e| (e.code, e.message));
            assert_eq!(actual, expected, "{}: {input}", stringify!($parser));
        }
        inputs.len()
    }};
}

#[test]
fn provider_records_match_the_previous_validators_on_hostile_inputs() {
    let mut cases = 0;
    cases += equivalent!(parse_provider_definition, hostile_variants(&provider()));
    cases += equivalent!(parse_provider_model_definition, hostile_variants(&model()));
    let registration = json!({"provider":provider(), "models":[model()]});
    cases += equivalent!(
        parse_provider_register_params,
        hostile_variants(&registration)
    );
    cases += equivalent!(
        parse_provider_update_params,
        hostile_variants(&registration)
    );
    cases += equivalent!(
        parse_provider_unregister_params,
        hostile_variants(&json!({"provider_id":"local-provider"}))
    );
    cases += equivalent!(
        parse_provider_catalog_complete_params,
        hostile_variants(&json!({}))
    );
    cases += equivalent!(
        parse_provider_catalog_result,
        hostile_variants(
            &json!({"revision":7,"provider_ids":["local-provider"],"model_ids":["local-provider/local-model"]})
        )
    );
    cases += equivalent!(
        parse_provider_authorization_request,
        hostile_variants(
            &json!({"provider_id":"local-provider","action":"authorize","interactive":false,"scopes":["models.read"]})
        )
    );
    cases += equivalent!(
        parse_provider_authorization_result,
        hostile_variants(&json!({"status":"ready","lease":"opaque-lease"}))
    );
    cases += equivalent!(
        parse_provider_stream_request,
        hostile_variants(
            &json!({"stream_id":"provider-1-0","provider_id":"local-provider","model_id":"local-model","request":{"max_tokens":1024},"authorization_lease":"opaque-lease"})
        )
    );
    cases += equivalent!(
        parse_provider_stream_accepted,
        hostile_variants(&json!({"stream_id":"provider-1-0","accepted":true,"reason":"ready"}))
    );
    cases += equivalent!(
        parse_provider_stream_event,
        hostile_variants(
            &json!({"stream_id":"provider-1-0","sequence":0,"kind":"text_delta","payload":{"index":0,"delta":"Hello"}})
        )
    );
    cases += equivalent!(
        parse_provider_stream_cancel_params,
        hostile_variants(&json!({"stream_id":"provider-1-0","reason":"caller dropped stream"}))
    );
    println!(
        "compared {cases} provider parse/serialize outcomes including exact error codes/messages"
    );
}

#[test]
fn optional_provider_fields_preserve_absence_and_reject_null() {
    for field in ["subject", "scopes"] {
        let mut value = provider();
        value["auth"][field] = Value::Null;
        equivalent!(parse_provider_definition, vec![value.clone()]);
        assert!(api::parse_provider_definition(value).is_err());
    }
    let absent = api::parse_provider_model_definition(model()).unwrap();
    assert!(serde_json::to_value(absent)
        .unwrap()
        .get("display_name")
        .is_none());
    let mut null = model();
    null["display_name"] = Value::Null;
    equivalent!(parse_provider_model_definition, vec![null.clone()]);
    assert_eq!(
        api::parse_provider_model_definition(null)
            .unwrap_err()
            .message,
        "invalid params: display_name must not be null"
    );
    for lease in [None, Some(Value::Null), Some(json!("lease"))] {
        let mut value = json!({"status":"ready"});
        if let Some(lease) = lease {
            value["lease"] = lease;
        }
        equivalent!(parse_provider_authorization_result, vec![value]);
    }
}

#[test]
fn provider_bounds_are_inclusive_and_errors_unchanged() {
    for size in [63, 64, 65] {
        let value = json!({"provider_id":"p".repeat(size)});
        equivalent!(parse_provider_unregister_params, vec![value.clone()]);
        assert_eq!(
            api::parse_provider_unregister_params(value).is_ok(),
            size <= 64
        );
    }
    for size in [127, 128, 129] {
        let mut value = provider();
        value["label"] = json!("x".repeat(size));
        equivalent!(parse_provider_definition, vec![value.clone()]);
        assert_eq!(api::parse_provider_definition(value).is_ok(), size <= 128);
    }
    for count in [0, 255, 256, 257] {
        let value = json!({"provider":provider(),"models":vec![model(); count]});
        equivalent!(parse_provider_register_params, vec![value.clone()]);
        assert_eq!(
            api::parse_provider_register_params(value).is_ok(),
            count <= 256
        );
    }
    for count in [31, 32, 33] {
        let value = json!({"provider_id":"local-provider","action":"authorize","interactive":false,"scopes":vec!["read";count]});
        equivalent!(parse_provider_authorization_request, vec![value.clone()]);
        assert_eq!(
            api::parse_provider_authorization_request(value).is_ok(),
            count <= 32
        );
    }
    for size in [255, 256, 257] {
        let value = json!({"status":"ready","lease":"x".repeat(size)});
        equivalent!(parse_provider_authorization_result, vec![value.clone()]);
        assert_eq!(
            api::parse_provider_authorization_result(value).is_ok(),
            size <= 256
        );
    }
    let mut value = model();
    value["id"] = json!("x".repeat(65));
    value["protocol"] = json!("bad");
    equivalent!(parse_provider_model_definition, vec![value.clone()]);
    let error = api::parse_provider_model_definition(value).unwrap_err();
    assert_eq!(
        (error.code, error.message.as_str()),
        (
            -32012,
            "extension resource exhausted: id exceeds byte bound"
        )
    );
}

#[test]
fn provider_portable_json_preserves_number_depth_and_encoded_byte_rules() {
    for value in [
        json!(null),
        json!({"z":1,"a":"é\n"}),
        json!(-9_007_199_254_740_991_i64),
        json!(9_007_199_254_740_991_u64),
        json!(-9_007_199_254_740_992_i64),
        json!(9_007_199_254_740_992_u64),
        json!(0.5),
        json!(1.0),
    ] {
        assert_eq!(
            api::portable_json(&value).map_err(|e| (e.code, e.message)),
            old::canonical_json(&value).map_err(|e| (e.code, e.message)),
        );
    }
    for depth in [31, 32, 33] {
        let mut value = json!(0);
        for _ in 0..depth {
            value = json!([value]);
        }
        assert_eq!(api::portable_json(&value).is_ok(), depth <= 32);
        assert_eq!(
            api::portable_json(&value).map_err(|e| (e.code, e.message)),
            old::canonical_json(&value).map_err(|e| (e.code, e.message))
        );
    }
    for size in [65_535, 65_536, 65_537] {
        let overhead = serde_json::to_vec(&json!({"delta":""})).unwrap().len();
        let value = json!({"delta":"x".repeat(size - overhead)});
        let old_bytes = old::canonical_json(&value).unwrap();
        let new_bytes = api::portable_json(&value).unwrap();
        assert_eq!(new_bytes, old_bytes);
        assert_eq!(new_bytes.len(), size);
        assert_eq!(
            new_bytes.len() <= api::MAX_PROVIDER_STREAM_EVENT_BYTES,
            old_bytes.len() <= old::MAX_PROVIDER_STREAM_EVENT_BYTES
        );
    }
}

#[test]
fn fixed_host_errors_preserve_all_codes_messages_and_data_presence() {
    let expected = [
        ("parse_error", -32700, "parse error"),
        ("invalid_request", -32600, "invalid request"),
        ("unknown_method", -32601, "unknown or unnegotiated method"),
        ("invalid_params", -32602, "invalid params"),
        ("internal_error", -32603, "internal error"),
        ("version_mismatch", -32010, "extension API version mismatch"),
        (
            "capability_mismatch",
            -32011,
            "extension capability mismatch",
        ),
        ("resource_exhausted", -32012, "extension resource exhausted"),
        ("request_cancelled", -32800, "request cancelled"),
    ];
    assert_eq!(api::ERRORS.len(), expected.len());
    for (name, code, message) in expected {
        for data in [
            None,
            Some(Value::Null),
            Some(json!({"reason":"unavailable"})),
            Some(json!(1.5)),
        ] {
            let actual = normalized(api::error_object(name, data.clone()), |e| {
                (e.code, e.message)
            });
            assert_eq!(
                actual,
                normalized(old::error_object(name, data.clone()), |e| (
                    e.code, e.message
                ))
            );
            if let Ok(value) = actual {
                assert_eq!(value["code"], code);
                assert_eq!(value["message"], message);
                assert_eq!(value.get("data"), data.as_ref());
            }
        }
    }
    assert_eq!(
        normalized(api::error_object("missing", None), |e| (e.code, e.message)),
        normalized(old::error_object("missing", None), |e| (e.code, e.message))
    );
}

#[test]
fn error_object_round_trips_preserve_absent_null_and_hostile_validation() {
    for value in hostile_variants(
        &json!({"code":-32603,"message":"internal error","data":{"reason":"failed"}}),
    ) {
        equivalent!(parse_error_object, vec![value.clone()]);
        let actual: Result<api::ErrorObject, _> = serde_json::from_value(value.clone());
        let expected: Result<old::ErrorObject, _> = serde_json::from_value(value);
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(
                api::validate_error_object(&actual).map_err(|e| (e.code, e.message)),
                old::validate_error_object(&expected).map_err(|e| (e.code, e.message))
            );
        }
    }
    for value in [
        json!({"code":-32603,"message":"internal error"}),
        json!({"code":-32603,"message":"internal error","data":null}),
    ] {
        let parsed: api::ErrorObject = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    }
}

#[test]
fn initialize_flag_shape_keeps_serde_semantics_without_adding_a_parser() {
    for value in [
        json!({"name":"enabled","value":true}),
        json!({"name":"count","value":-9_007_199_254_740_991_i64}),
        json!({"name":"label","value":"é"}),
        json!({"name":"flag","value":null}),
        json!({"name":"flag"}),
        json!({"value":false}),
        json!({"name":"flag","value":true,"unknown":1}),
    ] {
        let actual: Result<api::InitializeFlagValue, _> = serde_json::from_value(value.clone());
        let expected: Result<old::InitializeFlagValue, _> = serde_json::from_value(value.clone());
        assert_eq!(
            actual.as_ref().err().map(ToString::to_string),
            expected.as_ref().err().map(ToString::to_string)
        );
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(
                serde_json::to_string(&actual).unwrap(),
                serde_json::to_string(&expected).unwrap()
            );
        }
    }
}

#[test]
fn owned_bounds_and_version_policy_match_without_retiring_versions_early() {
    macro_rules! bound { ($($name:ident = $value:expr),+ $(,)?) => { $(assert_eq!(api::$name,$value); assert_eq!(api::$name,old::$name);)+ } }
    bound!(
        MAX_PORTABLE_JSON_INTEGER = 9_007_199_254_740_991,
        MAX_EXTENSION_FLAGS = 64,
        MAX_JSON_DEPTH = 32,
        MAX_JSON_RPC_ID_BYTES = 256,
        MAX_PROVIDERS = 32,
        MAX_PROVIDER_MODELS = 256,
        MAX_PROVIDER_ID_BYTES = 64,
        MAX_PROVIDER_LABEL_BYTES = 128,
        MAX_PROVIDER_AUTH_SCOPES = 32,
        MAX_PROVIDER_AUTH_LEASE_BYTES = 256,
        MAX_PROVIDER_STREAM_EVENT_BYTES = 65_536,
        MAX_PROVIDER_STREAM_EVENTS = 100_000,
        MAX_REASON_BYTES = 4096,
        MAX_SESSION_HOOK_ID_BYTES = 256
    );
    for version in [
        "0.1", "0.2", "0.3", "0.4", "0.04", "0.4.0", "0.5", " 0.4", "0.4\n", "", "garbage",
    ] {
        assert_eq!(
            policy::runtime_supports_api_version(version),
            old::runtime_supports_api_version(version)
        );
        assert_eq!(
            policy::bundle_supports_api_version(version),
            old::bundle_supports_api_version(version)
        );
    }
    assert!(policy::runtime_supports_api_version("0.4"));
    assert!(policy::bundle_supports_api_version("0.4"));
    assert!(!policy::bundle_supports_api_version("0.1"));
}

#[test]
fn session_hook_payloads_preserve_validation_serialization_and_bounds() {
    let binding = json!({"session_id":"session-1","extension_instance_id":"extension-1","process_generation":1});
    let mut count = 0;
    for payload in [
        json!({"hook":"session_start","payload":{"binding":binding}}),
        json!({"hook":"session_end","payload":{"binding":binding,"outcome":"completed","reason":"shutdown","duration_ms":123}}),
    ] {
        let parsed = api::parse_session_hook_params(payload.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), payload);
        count += equivalent!(parse_session_hook_params, hostile_variants(&payload));
    }
    for size in [255, 256, 257] {
        let value = json!({"hook":"session_start","payload":{"binding":{
            "session_id":"x".repeat(size),"extension_instance_id":"extension-1","process_generation":1
        }}});
        equivalent!(parse_session_hook_params, vec![value.clone()]);
        assert_eq!(api::parse_session_hook_params(value).is_ok(), size <= 256);
    }
    println!("compared {count} session hook parse/serialize outcomes");
}

#[test]
fn shared_session_responses_preserve_the_existing_checks_not_a_global_wire_policy() {
    let mut cases = Vec::new();
    for value in [
        json!({"jsonrpc":"2.0","id":"switch-1","result":{"session_id":"session-1"}}),
        json!({"jsonrpc":"2.0","id":1,"result":{"cancelled":true}}),
        json!({"jsonrpc":"2.0","id":"model-1","result":{"provider":"local","id":"model"}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"internal error","data":{"reason":"unavailable"}}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32012,"message":"extension resource exhausted"}}),
    ] {
        assert!(api::validate_session_response(&value).is_ok());
        cases.extend(hostile_variants(&value));
    }
    cases.extend([
        json!({"jsonrpc":"2.0","id":1,"result":{},"error":{"code":-32603,"message":"internal error"}}),
        json!({"jsonrpc":"2.0","id":1,"result":null}),
        json!({"jsonrpc":"2.0","id":1,"result":{"fraction":0.5}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32002,"message":"not_foreground_owner"}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"wrong"}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":-32603,"message":"internal error","data":null}}),
    ]);
    for size in [255, 256, 257] {
        cases.push(json!({"jsonrpc":"2.0","id":"x".repeat(size),"result":{}}));
    }
    for value in &cases {
        assert_eq!(
            api::validate_session_response(value).map_err(|e| (e.code, e.message)),
            old::parse_json_rpc_envelope(value.clone())
                .map(|_| ())
                .map_err(|e| (e.code, e.message)),
            "{value}"
        );
        assert_eq!(
            api::portable_json(value).map_err(|e| (e.code, e.message)),
            old::canonical_json(value).map_err(|e| (e.code, e.message)),
        );
    }
    println!(
        "compared {} session/model response validation and byte outcomes",
        cases.len()
    );
}

#[test]
fn session_switch_keeps_the_previous_byte_bound_including_empty_and_unicode() {
    for id in [
        String::new(),
        "x".repeat(256),
        "x".repeat(257),
        "é".repeat(128),
        "é".repeat(129),
    ] {
        let result = json!({"session_id":id});
        equivalent!(parse_session_lifecycle_result, vec![result.clone()]);
        assert_eq!(
            api::parse_session_lifecycle_result(result).is_ok(),
            id.len() <= 256
        );
        // The 0.4 request has already been decoded as a closed typed request;
        // the old one-field parser supplied only this additional byte bound.
        assert_eq!(
            id.len() <= api::MAX_JSON_RPC_ID_BYTES,
            old::parse_session_switch_params(json!({"session_id":id})).is_ok()
        );
    }
}
