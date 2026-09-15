use super::*;

#[test]
fn ids_are_bounded_and_cannot_be_paths() {
    assert!(valid_protocol_id("request-1:turn_2"));
    assert!(!valid_protocol_id("../session"));
    assert!(!valid_protocol_id("has/slash"));
    assert!(!valid_protocol_id(""));
    assert!(!valid_protocol_id(&"x".repeat(MAX_ID_BYTES + 1)));
}

#[test]
fn request_objects_reject_unknown_fields() {
    let mut request = serde_json::json!({
        "protocol_version": PROTOCOL_VERSION,
        "request_id": "request",
        "command": "run",
        "run_id": "run",
        "workspace": ".",
        "model": "test-model",
        "prompt": "hello",
        "allow_file_mutations": true,
    });
    let error = parse_request(request.to_string().as_bytes()).unwrap_err();
    assert!(error.contains("allow_file_mutations"));

    request
        .as_object_mut()
        .unwrap()
        .remove("allow_file_mutations");
    request["experimental_streamable_http_mcp"] = serde_json::json!(true);
    let error = parse_request(request.to_string().as_bytes()).unwrap_err();
    assert!(error.contains("experimental_streamable_http_mcp"));

    request
        .as_object_mut()
        .unwrap()
        .remove("experimental_streamable_http_mcp");
    request["history"] = serde_json::json!([{
        "role": "user",
        "text": "hello",
        "unexpected": true,
    }]);
    let error = parse_request(request.to_string().as_bytes()).unwrap_err();
    assert!(error.contains("unexpected"));
}
