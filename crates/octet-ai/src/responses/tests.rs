//! Unit tests for `crate::responses`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::responses`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn item(kind: &str) -> ResponsesItem {
    ResponsesItem::new(serde_json::json!({"type": kind, "unknown": {"x": 1}})).unwrap()
}

#[test]
fn items_preserve_unknown_fields_and_reject_non_objects() {
    let item = item("reasoning");
    assert_eq!(item.as_json()["unknown"]["x"], 1);
    assert!(ResponsesItem::new(serde_json::json!(["bad"])).is_err());
}

#[test]
fn responses_lite_strips_only_input_image_detail_hints() {
    let mut input = ResponsesInput::new(vec![
        ResponsesItem::new(serde_json::json!({
            "type": "message",
            "role": "user",
            "content": [
                {"type": "input_image", "image_url": "data:image/png;base64,eA==", "detail": "high", "future": true},
                {"type": "input_text", "text": "keep", "detail": "future-value"}
            ],
            "unknown": {"detail": "keep"}
        }))
        .unwrap(),
        ResponsesItem::new(serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": [
                {"type": "input_image", "image_url": "data:image/png;base64,eA==", "detail": "low"}
            ]
        }))
        .unwrap(),
    ]);

    input.strip_image_details_for_responses_lite();

    assert!(input.items()[0].as_json()["content"][0]
        .get("detail")
        .is_none());
    assert_eq!(input.items()[0].as_json()["content"][0]["future"], true);
    assert_eq!(
        input.items()[0].as_json()["content"][1]["detail"],
        "future-value"
    );
    assert_eq!(input.items()[0].as_json()["unknown"]["detail"], "keep");
    assert!(input.items()[1].as_json()["output"][0]
        .get("detail")
        .is_none());
}

#[test]
fn chaining_prunes_only_before_latest_compaction() {
    let input = ResponsesInput::new(vec![item("message"), item("compaction"), item("message")]);
    assert_eq!(
        input.prune_for_server_compaction_chaining().items().len(),
        2
    );
    let output = ResponsesOutput::new(input.into_items());
    assert_eq!(output.into_input().unwrap().items().len(), 3);
}

#[test]
fn full_replay_never_mixes_server_chaining_or_storage() {
    let options = ResponsesOptions::full_replay(ResponsesInput::new(vec![item("message")]));
    assert!(options.input.is_some());
    assert_eq!(options.previous_response_id, None);
    assert!(!options.store);
}

#[test]
fn compact_usage_preserves_reasoning_detail_beyond_the_aggregate() {
    let response: ResponsesCompactResponse = serde_json::from_value(serde_json::json!({
        "output": [{"type": "compaction", "encrypted_content": "opaque"}],
        "usage": {
            "input_tokens": 10,
            "output_tokens": 3,
            "output_tokens_details": {"reasoning_tokens": 5}
        }
    }))
    .unwrap();
    assert_eq!(response.usage.output_tokens, 8);
    assert_eq!(response.usage.reasoning_tokens, 5);
    assert_eq!(response.usage.total_tokens, 18);
}

#[test]
fn compact_output_requires_one_nonempty_encrypted_checkpoint() {
    for value in [
        serde_json::json!([]),
        serde_json::json!([{"type": "message"}]),
        serde_json::json!([{"type": "compaction"}]),
        serde_json::json!([{"type": "compaction", "encrypted_content": ""}]),
        serde_json::json!([
            {"type": "compaction", "encrypted_content": "one"},
            {"type": "compaction", "encrypted_content": "two"}
        ]),
    ] {
        let output: ResponsesOutput = serde_json::from_value(value).unwrap();
        assert!(!output.has_valid_compaction());
    }

    let output: ResponsesOutput = serde_json::from_value(serde_json::json!([
        {"type": "message", "id": "leading"},
        {"type": "compaction", "encrypted_content": "opaque"},
        {"type": "message", "id": "trailing"}
    ]))
    .unwrap();
    assert!(output.has_valid_compaction());
}

#[test]
fn projection_keeps_opaque_reasoning_with_its_own_unchanged_assistant() {
    use crate::{AssistantPart, ModelCatalog, ModelId};
    let model = ModelCatalog::builtin()
        .unwrap()
        .resolve(&ModelId("gpt-6-astra".into()))
        .unwrap();
    let assistant = |text: &str| {
        Message::Assistant(AssistantMessage {
            model: model.spec.id.clone(),
            protocol: model.spec.protocol,
            content: vec![AssistantPart::Text(text.into())],
        })
    };
    let original = vec![assistant("first"), assistant("second")];
    let output = |id: &str, text: &str| {
        ResponsesOutput::new(vec![
        ResponsesItem::new(serde_json::json!({"type":"reasoning", "id":id, "summary":[], "encrypted_content":id, "future":{"verbatim":true}})).unwrap(),
        ResponsesItem::new(serde_json::json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]})).unwrap(),
    ])
    };
    let first = output("r1", "first");
    let second = output("r2", "second");
    let replay = vec![
        ResponsesReplayItem::Output(first),
        ResponsesReplayItem::Output(second.clone()),
    ];
    for projected in [
        vec![original[1].clone()],
        vec![assistant("edited first"), original[1].clone()],
    ] {
        let input =
            encode_responses_projection(&model, Some("new system"), &projected, &original, &replay)
                .unwrap();
        let reasoning = input
            .items()
            .iter()
            .filter(|item| item.as_json()["type"] == "reasoning")
            .collect::<Vec<_>>();
        assert_eq!(reasoning, vec![&second.items()[0]]);
        assert_eq!(
            input.items()[0].as_json()["content"][0]["text"],
            "new system"
        );
    }
    // Removing every assistant cannot leave free-floating reasoning behind.
    let input = encode_responses_projection(&model, None, &[], &original, &replay).unwrap();
    assert!(input.items().is_empty());
}
