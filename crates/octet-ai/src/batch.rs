//! OpenRouter's asynchronous Batch API data types.
//!
//! Batch inference is intentionally separate from [`crate::Request`] and the
//! synchronous agent loop. OpenRouter accepts the native body for one of its
//! supported API shapes, so callers provide each request body as JSON and use
//! the endpoint/model fields to keep one batch homogeneous.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// OpenRouter API shapes accepted by the Batch API.
pub const OPENROUTER_BATCH_ENDPOINTS: &[&str] = &[
    "/v1/chat/completions",
    "/v1/responses",
    "/v1/messages",
    "/v1/embeddings",
];

/// Public statuses accepted by OpenRouter's batch-list `status` filter.
pub const OPENROUTER_BATCH_LIST_STATUSES: &[&str] = &[
    "validating",
    "in_progress",
    "completed",
    "failed",
    "expired",
    "cancelled",
];

/// One native request inside an OpenRouter batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchRequestItem {
    /// Caller-supplied identifier, unique within one batch.
    pub custom_id: String,
    /// Native JSON request body for the batch's endpoint.
    pub body: serde_json::Value,
}

impl OpenRouterBatchRequestItem {
    /// Creates a batch item from an identifier and native JSON body.
    pub fn new(custom_id: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            custom_id: custom_id.into(),
            body,
        }
    }
}

/// Submission envelope for OpenRouter's Batch API.
///
/// Field declaration order is deliberate: OpenRouter stream-parses large
/// submissions and requires `endpoint`, then `model`, then `requests`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchRequest {
    /// API shape used by every request in the batch.
    pub endpoint: String,
    /// OpenRouter model slug applied to every request.
    pub model: String,
    /// Non-empty request items.
    pub requests: Vec<OpenRouterBatchRequestItem>,
}

impl OpenRouterBatchRequest {
    /// Creates a batch submission envelope.
    pub fn new(
        endpoint: impl Into<String>,
        model: impl Into<String>,
        requests: Vec<OpenRouterBatchRequestItem>,
    ) -> Self {
        Self {
            endpoint: endpoint.into(),
            model: model.into(),
            requests,
        }
    }

    /// Validates the constraints that can be checked before network I/O.
    pub fn validate(&self) -> Result<(), BatchError> {
        if !OPENROUTER_BATCH_ENDPOINTS.contains(&self.endpoint.as_str()) {
            return Err(BatchError::InvalidEndpoint(self.endpoint.clone()));
        }
        if self.model.trim().is_empty() {
            return Err(BatchError::EmptyModel);
        }
        if self.requests.is_empty() {
            return Err(BatchError::EmptyRequests);
        }

        let mut ids = HashSet::with_capacity(self.requests.len());
        for item in &self.requests {
            if item.custom_id.trim().is_empty() {
                return Err(BatchError::InvalidCustomId(item.custom_id.clone()));
            }
            if !ids.insert(&item.custom_id) {
                return Err(BatchError::DuplicateCustomId(item.custom_id.clone()));
            }
            if !item.body.is_object() {
                return Err(BatchError::RequestBodyNotObject(item.custom_id.clone()));
            }
            if let Some(model) = item.body.get("model").and_then(serde_json::Value::as_str) {
                if model != self.model {
                    return Err(BatchError::ModelMismatch {
                        custom_id: item.custom_id.clone(),
                        model: model.to_owned(),
                    });
                }
            }
        }
        Ok(())
    }
}

/// Validation failures for a Batch API request or query.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// The endpoint is not one of OpenRouter's supported batch shapes.
    #[error("unsupported OpenRouter batch endpoint: {0:?}")]
    InvalidEndpoint(String),
    /// The batch-level model was empty.
    #[error("OpenRouter batch model must not be empty")]
    EmptyModel,
    /// The batch contained no requests.
    #[error("OpenRouter batch requests must not be empty")]
    EmptyRequests,
    /// A custom id was empty or whitespace-only.
    #[error("OpenRouter batch custom_id is empty: {0:?}")]
    InvalidCustomId(String),
    /// Two request items used the same custom id.
    #[error("duplicate OpenRouter batch custom_id: {0:?}")]
    DuplicateCustomId(String),
    /// A request body was not a JSON object.
    #[error("OpenRouter batch body for custom_id {0:?} must be a JSON object")]
    RequestBodyNotObject(String),
    /// A per-request model disagreed with the batch-level model.
    #[error("OpenRouter batch body for custom_id {custom_id:?} uses model {model:?}, which does not match the batch model")]
    ModelMismatch {
        /// Request item whose body disagreed.
        custom_id: String,
        /// Model found in that request body.
        model: String,
    },
    /// A batch id was empty or contained path-control characters.
    #[error("invalid OpenRouter batch id: {0:?}")]
    InvalidBatchId(String),
    /// A list limit was outside OpenRouter's documented range.
    #[error("OpenRouter batch list limit must be between 1 and 100, got {0}")]
    InvalidLimit(u32),
    /// A list status was not accepted by OpenRouter.
    #[error("unsupported OpenRouter batch list status: {0:?}")]
    InvalidStatus(String),
    /// The endpoint passed to an OpenRouter-only operation was not OpenRouter.
    #[error("OpenRouter Batch API requires the openrouter endpoint, got {0:?}")]
    UnsupportedProvider(String),
}

/// Query parameters for listing OpenRouter batches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenRouterBatchListOptions {
    /// Maximum number of batches to return, from 1 through 100.
    pub limit: Option<u32>,
    /// Cursor returned as the previous page's `last_id`.
    pub after: Option<String>,
    /// Status filters. OpenRouter accepts this parameter repeatedly.
    pub statuses: Vec<String>,
    /// Include batches created strictly after this Unix timestamp or ISO-8601 value.
    pub created_after: Option<String>,
    /// Include batches created strictly before this Unix timestamp or ISO-8601 value.
    pub created_before: Option<String>,
}

impl OpenRouterBatchListOptions {
    /// Validates list parameters before network I/O.
    pub fn validate(&self) -> Result<(), BatchError> {
        if let Some(limit) = self.limit {
            if !(1..=100).contains(&limit) {
                return Err(BatchError::InvalidLimit(limit));
            }
        }
        if let Some(after) = self.after.as_deref() {
            validate_batch_id(after)?;
        }
        for status in &self.statuses {
            if !OPENROUTER_BATCH_LIST_STATUSES.contains(&status.as_str()) {
                return Err(BatchError::InvalidStatus(status.clone()));
            }
        }
        Ok(())
    }
}

/// Counts of requests in a batch.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct OpenRouterBatchRequestCounts {
    /// Number of submitted requests.
    pub total: u64,
    /// Number of requests with a response.
    pub completed: u64,
    /// Number of requests with an error.
    pub failed: u64,
}

/// Aggregate usage reported for a completed batch.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchUsage {
    /// Prompt tokens billed at the standard input bucket.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Generated completion tokens.
    #[serde(default)]
    pub completion_tokens: u64,
    /// Total tokens processed.
    #[serde(default)]
    pub total_tokens: u64,
    /// Provider-reported charge. This remains JSON-typed so decimal money is
    /// not rounded through a floating-point Rust representation.
    #[serde(default)]
    pub cost: Option<serde_json::Value>,
    /// Whether the batch used a configured provider key through BYOK.
    #[serde(default)]
    pub is_byok: Option<bool>,
}

/// Successful response envelope for one completed batch item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchResponse {
    /// HTTP-like status returned for the individual request.
    pub status_code: u16,
    /// OpenRouter request identifier, when present.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Native response body for the selected API shape.
    pub body: serde_json::Value,
}

/// One result mapped to an input by `custom_id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchResult {
    /// Optional provider result identifier.
    #[serde(default)]
    pub id: Option<String>,
    /// Input item's caller-supplied identifier.
    pub custom_id: String,
    /// Successful native response, when the item succeeded.
    #[serde(default)]
    pub response: Option<OpenRouterBatchResponse>,
    /// Provider error payload, when the item failed.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

/// Batch metadata and, for completed batches, inline results.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatch {
    /// Batch identifier used by retrieval calls.
    pub id: String,
    /// Provider object type, normally `batch`.
    pub object: String,
    /// API shape used by this batch.
    pub endpoint: String,
    /// OpenRouter model slug used by this batch.
    pub model: String,
    /// Completion window, currently `24h`.
    pub completion_window: String,
    /// Current provider status.
    pub status: String,
    /// Unix timestamp when the batch was created.
    pub created_at: u64,
    /// Unix timestamp when the batch reached a final state.
    #[serde(default)]
    pub finalized_at: Option<u64>,
    /// Request progress counts.
    pub request_counts: OpenRouterBatchRequestCounts,
    /// Aggregate token usage, available after completion.
    #[serde(default)]
    pub usage: Option<OpenRouterBatchUsage>,
    /// Inline results, available only after completion.
    #[serde(default)]
    pub results: Option<Vec<OpenRouterBatchResult>>,
    /// Batch-level provider error, when present.
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

impl OpenRouterBatch {
    /// Returns whether OpenRouter reports a terminal batch status.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "completed" | "failed" | "expired" | "cancelled"
        )
    }
}

/// Paginated list response from OpenRouter.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenRouterBatchList {
    /// Provider object type, normally `list`.
    pub object: String,
    /// Batch metadata, newest first.
    pub data: Vec<OpenRouterBatch>,
    /// First id in this page, when present.
    #[serde(default)]
    pub first_id: Option<String>,
    /// Last id in this page, when present.
    #[serde(default)]
    pub last_id: Option<String>,
    /// Whether another page is available.
    #[serde(default)]
    pub has_more: bool,
}

/// Validates a batch id before it is placed in a URL path.
pub(crate) fn validate_batch_id(id: &str) -> Result<(), BatchError> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(BatchError::InvalidBatchId(id.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
}
