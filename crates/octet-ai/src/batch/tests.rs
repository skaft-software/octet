//! Unit tests for `crate::batch`.
//!
//! Covers the module's observable behaviour and bounds.
//!
//! Extracted from `mod.rs` so the implementation reads as pure
//! production code and the assertions that pin it can be
//! navigated separately. This is still a child module of
//! `crate::batch`, so `use super::*` reaches exactly the
//! private items it reached while the tests were inline.

use super::*;

fn item(id: &str) -> OpenRouterBatchRequestItem {
    OpenRouterBatchRequestItem::new(id, serde_json::json!({"messages": []}))
}

#[test]
fn submission_serializes_stream_parser_fields_in_required_order() {
    let request =
        OpenRouterBatchRequest::new("/v1/chat/completions", "openai/gpt-4o", vec![item("one")]);
    request.validate().unwrap();
    let json = serde_json::to_string(&request).unwrap();
    assert!(json.find("\"endpoint\"").unwrap() < json.find("\"model\"").unwrap());
    assert!(json.find("\"model\"").unwrap() < json.find("\"requests\"").unwrap());
}

#[test]
fn submission_rejects_duplicates_and_model_mismatches() {
    let duplicate = OpenRouterBatchRequest::new(
        "/v1/chat/completions",
        "openai/gpt-4o",
        vec![item("same"), item("same")],
    );
    assert!(matches!(
        duplicate.validate(),
        Err(BatchError::DuplicateCustomId(id)) if id == "same"
    ));

    let mismatch = OpenRouterBatchRequest::new(
        "/v1/chat/completions",
        "openai/gpt-4o",
        vec![OpenRouterBatchRequestItem::new(
            "one",
            serde_json::json!({"model": "anthropic/claude"}),
        )],
    );
    assert!(matches!(
        mismatch.validate(),
        Err(BatchError::ModelMismatch { custom_id, model })
            if custom_id == "one" && model == "anthropic/claude"
    ));
}

#[test]
fn list_validation_rejects_bad_cursor_limit_and_status() {
    assert!(matches!(
        OpenRouterBatchListOptions {
            limit: Some(101),
            ..Default::default()
        }
        .validate(),
        Err(BatchError::InvalidLimit(101))
    ));
    assert!(matches!(
        OpenRouterBatchListOptions {
            after: Some("batch/one".into()),
            ..Default::default()
        }
        .validate(),
        Err(BatchError::InvalidBatchId(_))
    ));
    assert!(matches!(
        OpenRouterBatchListOptions {
            statuses: vec!["finalizing".into()],
            ..Default::default()
        }
        .validate(),
        Err(BatchError::InvalidStatus(status)) if status == "finalizing"
    ));
}

#[test]
fn terminal_statuses_are_explicit() {
    let mut batch: OpenRouterBatch = serde_json::from_value(serde_json::json!({
        "id": "batch_1",
        "object": "batch",
        "endpoint": "/v1/chat/completions",
        "model": "openai/gpt-4o",
        "completion_window": "24h",
        "status": "in_progress",
        "created_at": 1,
        "finalized_at": null,
        "request_counts": {"total": 1, "completed": 0, "failed": 0},
        "usage": null,
        "results": null,
        "error": null
    }))
    .unwrap();
    assert!(!batch.is_terminal());
    batch.status = "completed".into();
    assert!(batch.is_terminal());
}
