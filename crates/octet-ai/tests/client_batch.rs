#![allow(missing_docs)]

use std::time::Duration;

use octet_ai::{
    AiClient, Auth, Endpoint, EndpointId, EndpointTransport, OpenRouterBatchListOptions,
    OpenRouterBatchRequest, OpenRouterBatchRequestItem,
};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn endpoint(base_url: String) -> Endpoint {
    Endpoint {
        id: EndpointId("openrouter".into()),
        base_url: url::Url::parse(&format!("{base_url}/api/v1/")).unwrap(),
        auth: Auth::bearer("batch-secret"),
        default_headers: http::HeaderMap::new(),
        transport: EndpointTransport::Http,
        runtime: octet_ai::RequestRuntime::default(),
        timeout: Duration::from_secs(2),
    }
}

fn response_json(status: &str) -> serde_json::Value {
    serde_json::json!({
        "id": "batch_test",
        "object": "batch",
        "endpoint": "/v1/chat/completions",
        "model": "openai/gpt-4o",
        "completion_window": "24h",
        "status": status,
        "created_at": 1,
        "request_counts": {"total": 1, "completed": 0, "failed": 0}
    })
}

fn request() -> OpenRouterBatchRequest {
    OpenRouterBatchRequest::new(
        "/v1/chat/completions",
        "openai/gpt-4o",
        vec![OpenRouterBatchRequestItem::new(
            "one",
            serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
        )],
    )
}

#[tokio::test]
async fn submit_uses_openrouter_beta_route_and_native_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/beta/batches"))
        .and(header("authorization", "Bearer batch-secret"))
        .and(header("content-type", "application/json"))
        .and(body_json(serde_json::json!({
            "endpoint": "/v1/chat/completions",
            "model": "openai/gpt-4o",
            "requests": [{
                "custom_id": "one",
                "body": {"messages": [{"role": "user", "content": "hello"}]}
            }]
        })))
        .respond_with(
            ResponseTemplate::new(202)
                .set_body_json(response_json("validating"))
                .insert_header("x-request-id", "request_test"),
        )
        .expect(1)
        .mount(&server)
        .await;

    let client = AiClient::with_http_client(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    );
    let batch = client
        .submit_openrouter_batch(&endpoint(server.uri()), request())
        .await
        .unwrap();
    assert_eq!(batch.id, "batch_test");
    assert_eq!(batch.status, "validating");
}

#[tokio::test]
async fn get_decodes_completed_results_without_assuming_a_result_id() {
    let server = MockServer::start().await;
    let mut response = response_json("completed");
    response["finalized_at"] = serde_json::json!(2);
    response["request_counts"] = serde_json::json!({
        "total": 1,
        "completed": 1,
        "failed": 0
    });
    response["results"] = serde_json::json!([{
        "custom_id": "one",
        "response": {
            "status_code": 200,
            "request_id": "req_result",
            "body": {"id": "chatcmpl_test"}
        },
        "error": null
    }]);

    Mock::given(method("GET"))
        .and(path("/api/beta/batches/batch_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(&server)
        .await;

    // Use a custom client and endpoint to preserve the same URL derivation
    // without making a network call outside the mock.
    let client = AiClient::with_http_client(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    );
    let batch = client
        .get_openrouter_batch(&endpoint(server.uri()), "batch_test")
        .await
        .unwrap();
    let results = batch.results.unwrap();
    assert_eq!(results[0].custom_id, "one");
    assert_eq!(results[0].id, None);
    assert_eq!(results[0].response.as_ref().unwrap().status_code, 200);
}

#[tokio::test]
async fn list_encodes_cursor_and_repeated_status_filters() {
    let server = MockServer::start().await;
    let response = serde_json::json!({
        "object": "list",
        "data": [],
        "first_id": null,
        "last_id": null,
        "has_more": false
    });
    Mock::given(method("GET"))
        .and(path("/api/beta/batches"))
        .and(query_param("limit", "10"))
        .and(query_param("after", "batch_previous"))
        .and(query_param("status", "completed"))
        .and(query_param("status", "failed"))
        .and(query_param("created_after", "1700000000"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .expect(1)
        .mount(&server)
        .await;

    let client = AiClient::with_http_client(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
    );
    let list = client
        .list_openrouter_batches(
            &endpoint(server.uri()),
            &OpenRouterBatchListOptions {
                limit: Some(10),
                after: Some("batch_previous".into()),
                statuses: vec!["completed".into(), "failed".into()],
                created_after: Some("1700000000".into()),
                created_before: None,
            },
        )
        .await
        .unwrap();
    assert!(list.data.is_empty());
}
