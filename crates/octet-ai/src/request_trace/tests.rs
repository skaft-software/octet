use super::*;

fn data_url(payload: &str) -> String {
    format!("data:image/png;base64,{payload}")
}

fn computer_use_history() -> Value {
    json!({
        "model": "gpt-6-luna",
        "store": false,
        "previous_response_id": "resp_SECRET_CONTINUATION",
        "instructions": "SECRET system prompt",
        "tools": [{"type": "function", "name": "SECRET_TOOL"}],
        "input": [
            {"role": "user", "content": [{"type": "input_text", "text": "SECRET user prompt"}]},
            {"type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC_SECRET_ONE"},
            {"type": "function_call", "call_id": "c1", "name": "window", "arguments": "{\"SECRET\":1}"},
            {"type": "function_call_output", "call_id": "c1", "output": [
                {"type": "input_text", "text": "SECRET window text"},
                {"type": "input_image", "image_url": data_url("AAAABBBBCCCC")}
            ]},
            {"type": "reasoning", "id": "rs_2", "summary": []},
            {"type": "function_call", "call_id": "c2", "name": "window", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c2", "output": [
                {"type": "input_image", "image_url": "https://example.test/SECRET.png"},
                {"type": "input_image", "file_id": "file_SECRET"}
            ]},
            {"type": "computer_call", "call_id": "c3", "action": {"type": "click"}},
            {"type": "computer_call_output", "call_id": "c3",
             "output": {"type": "computer_screenshot", "image_url": data_url("DDDDEEEE")}},
            {"type": "computer_call_output", "call_id": "c4",
             "output": {"type": "computer_screenshot"}},
            {"type": "reasoning", "id": "rs_3", "summary": [], "encrypted_content": "ENC_SECRET_TWO"}
        ]
    })
}

#[test]
fn shape_counts_items_images_and_reasoning() {
    let shape = Value::Object(request_shape(&computer_use_history()));
    assert_eq!(shape["input_items"], 11);
    assert_eq!(shape["item_types"]["message"], 1);
    assert_eq!(shape["item_types"]["reasoning"], 3);
    assert_eq!(shape["item_types"]["function_call"], 2);
    assert_eq!(shape["item_types"]["function_call_output"], 2);
    assert_eq!(shape["item_types"]["computer_call"], 1);
    assert_eq!(shape["item_types"]["computer_call_output"], 2);
    assert_eq!(shape["trailing_item_type"], "reasoning");
    assert_eq!(shape["previous_response_id"], true);
    assert_eq!(shape["store"], false);
    assert_eq!(shape["array_lengths"]["tools"], 1);
    assert_eq!(shape["array_lengths"]["input"], 11);

    let images = &shape["images"];
    assert_eq!(images["count"], 4);
    assert_eq!(images["inline"], 2);
    assert_eq!(images["url"], 1);
    assert_eq!(images["file_id"], 1);
    assert_eq!(
        images["inline_wire_bytes"],
        (data_url("AAAABBBBCCCC").len() + data_url("DDDDEEEE").len()) as u64
    );
    assert_eq!(
        images["largest_inline_wire_bytes"],
        data_url("AAAABBBBCCCC").len() as u64
    );

    let reasoning = &shape["reasoning"];
    assert_eq!(reasoning["items"], 3);
    assert_eq!(reasoning["with_encrypted_content"], 2);
    assert_eq!(reasoning["without_encrypted_content"], 1);
    assert_eq!(
        reasoning["encrypted_content_chars"],
        ("ENC_SECRET_ONE".len() + "ENC_SECRET_TWO".len()) as u64
    );
    // A screenshot-less computer_call_output is the shape the codec emits when
    // an inline image was dropped; it is surfaced rather than hidden.
    assert_eq!(shape["computer_call_outputs_without_image"], 1);
}

#[test]
fn shape_records_absence_of_a_continuation_id() {
    let shape = Value::Object(request_shape(
        &json!({"input": [], "previous_response_id": null}),
    ));
    assert_eq!(shape["previous_response_id"], false);
    assert_eq!(shape["input_items"], 0);
    assert_eq!(shape["trailing_item_type"], "none");
}

#[test]
fn trace_lines_never_contain_request_content() {
    let path = std::env::temp_dir().join(format!(
        "octet-request-trace-{}-{}.jsonl",
        std::process::id(),
        now_ms()
    ));
    let env = BTreeMap::from([(
        REQUEST_TRACE_ENV.to_owned(),
        path.to_string_lossy().into_owned(),
    )]);
    let trace = RequestTrace::from_env(&env).expect("trace enabled");
    trace.request("websocket", "gpt-6-luna", &computer_use_history());
    trace.error_response(400, Some("req_abc-123 $%!"));

    let written = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    for secret in [
        "SECRET",
        "resp_",
        "AAAABBBB",
        "DDDDEEEE",
        "ENC_",
        "example.test",
        "file_SECRET",
        "base64",
        "rs_1",
        "call_id",
    ] {
        assert!(!written.contains(secret), "{secret} leaked: {written}");
    }
    let lines: Vec<Value> = written
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["event"], "request");
    assert_eq!(lines[0]["transport"], "websocket");
    assert_eq!(lines[0]["model"], "gpt-6-luna");
    assert_eq!(lines[1]["event"], "response_error");
    assert_eq!(lines[1]["status"], 400);
    assert_eq!(lines[0]["id"], lines[1]["id"]);
    assert_eq!(lines[1]["provider_request_id"], "req_abc-123");
}

#[test]
fn trace_is_off_without_the_environment_flag() {
    let blank = BTreeMap::from([(REQUEST_TRACE_ENV.to_owned(), "  ".to_owned())]);
    assert!(RequestTrace::from_env(&blank).is_none());
}
