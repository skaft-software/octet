#![allow(missing_docs)]

//! Agent integration tests against a deterministic scripted model.
//!
//! The scripted boundary is `octet-ai`'s real HTTP + SSE path: a wiremock server
//! replays hand-written Anthropic Messages SSE bodies in sequence, so the
//! agent exercises the exact stream-assembly and request-building code it
//! uses in production, with no live provider.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use octet_agent::{
    Agent, AgentConfig, AgentEvent, CompletionPolicy, CoreTools, EffectBroker, EffectPolicy,
    EntryId, EntryValue, ExtensionHost, FinishReason, InputPart, OutputChannel, OutputStream,
    QueueDeliveryMode, ReplaySafety, RunControl, SandboxConfig, Session, Tool, ToolCallHook,
    ToolConcurrency, ToolContext, ToolEffect, ToolError, ToolOutput, ToolPolicyDenialCode,
    UsageRecordKind, UserInput,
};
use octet_ai::{
    AiClient, AssistantMessage, AssistantPart, AudioFormat, AudioOutputOptions, AudioPayload,
    AudioVoice, Auth, Capabilities, Endpoint, EndpointId, Media, Message, Modality, ModalitySet,
    Model, ModelId, ModelLimits, ModelSpec, OutputModalities, Pricing, Protocol,
    ProviderLifecycleState, ReasoningCapability, ReasoningConfig, ReasoningControl,
    ReasoningEffortBudgets, TokenRate, ToolCall, ToolCallArgumentError, Usage, UserMessage,
    UserPart,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message as WebSocketMessage};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};

const MAX_CONNECT_ATTEMPTS_FOR_TEST: usize = 6;

// ── Scripted SSE bodies (Anthropic Messages wire shapes) ───────────────────

fn frame(event: &str, data: serde_json::Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn msg_start() -> String {
    frame(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {"id": "msg_1", "usage": {"input_tokens": 5, "output_tokens": 0}}
        }),
    )
}

fn msg_end(stop_reason: &str) -> String {
    frame(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": 3}
        }),
    ) + &frame("message_stop", serde_json::json!({"type": "message_stop"}))
}

fn text_block(index: usize, deltas: &[&str]) -> String {
    let mut s = frame(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "text", "text": ""}
        }),
    );
    for delta in deltas {
        s += &frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": index,
                "delta": {"type": "text_delta", "text": delta}
            }),
        );
    }
    s + &frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": index}),
    )
}

fn thinking_block(index: usize, text: &str) -> String {
    frame(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "thinking", "thinking": ""}
        }),
    ) + &frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "thinking_delta", "thinking": text}
        }),
    ) + &frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": index}),
    )
}

fn tool_block(index: usize, id: &str, name: &str, args: &serde_json::Value) -> String {
    frame(
        "content_block_start",
        serde_json::json!({
            "type": "content_block_start",
            "index": index,
            "content_block": {"type": "tool_use", "id": id, "name": name}
        }),
    ) + &frame(
        "content_block_delta",
        serde_json::json!({
            "type": "content_block_delta",
            "index": index,
            "delta": {"type": "input_json_delta", "partial_json": args.to_string()}
        }),
    ) + &frame(
        "content_block_stop",
        serde_json::json!({"type": "content_block_stop", "index": index}),
    )
}

/// A complete turn that answers with plain text.
fn text_turn(text: &str) -> String {
    msg_start() + &text_block(0, &[text]) + &msg_end("end_turn")
}

fn text_turn_with_stop(text: &str, stop_reason: &str) -> String {
    msg_start() + &text_block(0, &[text]) + &msg_end(stop_reason)
}

fn empty_turn() -> String {
    msg_start() + &msg_end("end_turn")
}

fn reasoning_only_turn(text: &str) -> String {
    msg_start() + &thinking_block(0, text) + &msg_end("end_turn")
}

fn openai_text_turn(text: &str) -> String {
    let text = serde_json::to_string(text).unwrap();
    format!(
        "data: {{\"id\":\"chat\",\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"content\":{text}}}}}]}}\n\ndata: {{\"id\":\"chat\",\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}}}\n\ndata: [DONE]\n\n"
    )
}

fn responses_text_turn(
    response_id: &str,
    text: &str,
    terminal_type: &str,
    opaque_marker: &str,
) -> String {
    let terminal = if terminal_type == "response.incomplete" {
        serde_json::json!({
            "type": terminal_type,
            "response": {
                "output": [{
                    "type": "message",
                    "id": format!("msg_{response_id}"),
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text, "annotations": []}],
                    "unknown_provider_field": opaque_marker,
                }],
                "incomplete_details": {"reason": "max_output_tokens"},
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
            },
        })
    } else {
        serde_json::json!({
            "type": terminal_type,
            "response": {
                "output": [{
                    "type": "message",
                    "id": format!("msg_{response_id}"),
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text, "annotations": []}],
                    "unknown_provider_field": opaque_marker,
                }],
                "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
            },
        })
    };
    [
        serde_json::json!({"type": "response.created", "response": {"id": response_id}}),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id": format!("msg_{response_id}"), "type": "message"},
        }),
        serde_json::json!({
            "type": "response.output_text.delta",
            "output_index": 0,
            "delta": text,
        }),
        serde_json::json!({"type": "response.output_text.done", "output_index": 0}),
        terminal,
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

fn responses_tool_turn(response_id: &str, call_id: &str) -> String {
    let arguments = r#"{"path":"lifecycle.txt"}"#;
    let terminal_output = serde_json::json!([
        {
            "type": "reasoning",
            "id": format!("rs_{response_id}"),
            "encrypted_content": "encrypted-reasoning-state",
            "future_reasoning_field": {"preserved": true}
        },
        {
            "type": "function_call",
            "id": format!("fc_{response_id}"),
            "call_id": call_id,
            "name": "read",
            "arguments": arguments,
            "phase": "commentary",
            "unknown_provider_field": {"preserved": true}
        }
    ]);
    [
        serde_json::json!({"type": "response.created", "response": {"id": response_id}}),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {
                "id": format!("fc_{response_id}"),
                "type": "function_call",
                "call_id": call_id,
                "name": "read"
            }
        }),
        serde_json::json!({
            "type": "response.function_call_arguments.done",
            "output_index": 0,
            "arguments": arguments
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "output": terminal_output,
                "usage": {"input_tokens": 9, "output_tokens": 3, "total_tokens": 12}
            }
        }),
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

fn openai_tool_turn(calls: &[(&str, &str, serde_json::Value)]) -> String {
    let mut body = String::new();
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let chunk = serde_json::json!({
            "id": "chat-tools",
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": index,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments.to_string(),
                        },
                    }],
                },
            }],
        });
        body += &format!("data: {chunk}\n\n");
    }
    body += "data: {\"id\":\"chat-tools\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n";
    body += "data: [DONE]\n\n";
    body
}

/// A syntactically valid stream prefix with visible output but no terminal
/// message event. Closing the HTTP body after this prefix reproduces the
/// provider/proxy disconnect that used to fail long octet runs.
fn partial_text_turn(text: &str) -> String {
    msg_start()
        + &frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": {"type": "text", "text": ""}
            }),
        )
        + &frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": text}
            }),
        )
}

/// A complete turn that requests the given tool calls.
fn tool_turn_with_stop(calls: &[(&str, &str, serde_json::Value)], stop_reason: &str) -> String {
    let mut s = msg_start();
    for (i, (id, name, args)) in calls.iter().enumerate() {
        s += &tool_block(i, id, name, args);
    }
    s + &msg_end(stop_reason)
}

fn tool_turn(calls: &[(&str, &str, serde_json::Value)]) -> String {
    tool_turn_with_stop(calls, "tool_use")
}

struct ResponsesConnectionLimitServer {
    base_url: String,
    websocket_requests: Arc<AtomicUsize>,
    http_requests: Arc<AtomicUsize>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl ResponsesConnectionLimitServer {
    async fn start() -> Self {
        Self::with_http_failures(0).await
    }

    async fn with_http_failures(http_failures: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let websocket_requests = Arc::new(AtomicUsize::new(0));
        let http_requests = Arc::new(AtomicUsize::new(0));
        let (shutdown, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let websocket_count = Arc::clone(&websocket_requests);
        let http_count = Arc::clone(&http_requests);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let websocket_count = Arc::clone(&websocket_count);
                        let http_count = Arc::clone(&http_count);
                        tokio::spawn(async move {
                            let _ = handle_responses_connection_limit(
                                stream,
                                websocket_count,
                                http_count,
                                http_failures,
                            )
                            .await;
                        });
                    }
                }
            }
        });
        Self {
            base_url: format!("http://{address}/"),
            websocket_requests,
            http_requests,
            shutdown: Some(shutdown),
            task,
        }
    }
}

impl Drop for ResponsesConnectionLimitServer {
    fn drop(&mut self) {
        self.shutdown.take();
        self.task.abort();
    }
}

async fn handle_responses_connection_limit(
    mut stream: TcpStream,
    websocket_requests: Arc<AtomicUsize>,
    http_requests: Arc<AtomicUsize>,
    http_failures: usize,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut peek = [0_u8; 4096];
    let count = stream.peek(&mut peek).await?;
    let request_head = String::from_utf8_lossy(&peek[..count]).to_ascii_lowercase();
    if request_head.contains("upgrade: websocket") {
        let mut socket = accept_async(stream).await?;
        let Some(Ok(WebSocketMessage::Text(_))) = socket.next().await else {
            return Ok(());
        };
        websocket_requests.fetch_add(1, Ordering::SeqCst);
        for event in [
            serde_json::json!({
                "type": "response.created",
                "response": {"id": "limited-response"}
            }),
            serde_json::json!({
                "type": "response.failed",
                "response": {
                    "error": {
                        "code": "websocket_connection_limit_reached",
                        "message": "Create a new websocket connection to continue."
                    }
                }
            }),
        ] {
            socket
                .send(WebSocketMessage::Text(event.to_string().into()))
                .await?;
        }
        return Ok(());
    }

    let mut request = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        if request.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let attempt = http_requests.fetch_add(1, Ordering::SeqCst);
    let completed_body = concat!(
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"http-response\"}}\n\n",
        "data: {\"type\":\"response.content_part.added\",\"output_index\":0,\"content_index\":0,\"part\":{\"type\":\"output_text\"}}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":\"recovered\"}\n\n",
        "data: {\"type\":\"response.output_text.done\",\"output_index\":0,\"content_index\":0}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"http-response\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1,\"total_tokens\":3}}}\n\n"
    );
    let failed_body =
        interrupted_responses_prefix("text") + &recovery_provider_error("server_error");
    let body = if attempt < http_failures {
        failed_body.as_str()
    } else {
        completed_body
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).await?;
    Ok(())
}

// ── Scripted server + agent harness ────────────────────────────────────────

/// Replays SSE bodies in sequence; the last body repeats once exhausted.
struct Script {
    bodies: Vec<String>,
    next: AtomicUsize,
}

impl Respond for Script {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(i)
            .or_else(|| self.bodies.last())
            .expect("script must have at least one body")
            .clone();
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header("content-type", "text/event-stream")
    }
}

/// Replays JSON or SSE responses in sequence according to each request's stream flag.
struct JsonScript {
    bodies: Vec<String>,
    next: AtomicUsize,
}

impl Respond for JsonScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        let body = self
            .bodies
            .get(i)
            .or_else(|| self.bodies.last())
            .expect("script must have at least one body")
            .clone();
        let streaming = serde_json::from_slice::<serde_json::Value>(&request.body)
            .ok()
            .and_then(|body| body.get("stream").and_then(serde_json::Value::as_bool))
            .unwrap_or(false);
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header(
                "content-type",
                if streaming {
                    "text/event-stream"
                } else {
                    "application/json"
                },
            )
    }
}

struct RetryInitialOpen {
    calls: Arc<AtomicUsize>,
}

impl Respond for RetryInitialOpen {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(503)
                .set_body_string(r#"{"error":{"message":"temporarily unavailable"}}"#)
                .insert_header("retry-after", "0")
        } else {
            ResponseTemplate::new(200)
                .set_body_string(text_turn("recovered"))
                .insert_header("content-type", "text/event-stream")
        }
    }
}

struct DelayedHeaders {
    calls: Arc<AtomicUsize>,
}

impl Respond for DelayedHeaders {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(200).set_delay(Duration::from_millis(100))
    }
}

struct FailThenSucceed {
    calls: AtomicUsize,
}

impl Respond for FailThenSucceed {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(400).set_body_string(r#"{"error":{"message":"request failed"}}"#)
        } else {
            ResponseTemplate::new(200)
                .set_body_string(text_turn("hello from the new turn"))
                .insert_header("content-type", "text/event-stream")
        }
    }
}

/// Serve one truncated HTTP body followed by a complete response. The first
/// response advertises more bytes than it sends, making reqwest surface the
/// same non-timeout body transport error seen when a provider closes an SSE
/// connection mid-generation.
async fn interrupted_body_server(partial: String, recovered: String) -> (String, Arc<AtomicUsize>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = calls.clone();
    tokio::spawn(async move {
        for (index, body) in [partial, recovered].into_iter().enumerate() {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            let (header_end, content_length) = loop {
                let read = socket.read(&mut buf).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buf[..read]);
                let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                break (header_end, content_length.unwrap_or_default());
            };
            while request.len().saturating_sub(header_end) < content_length {
                let read = socket.read(&mut buf).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
            }
            server_calls.fetch_add(1, Ordering::SeqCst);

            let declared_length = if index == 0 {
                body.len() + 128
            } else {
                body.len()
            };
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {declared_length}\r\nConnection: close\r\n\r\n"
            );
            socket.write_all(headers.as_bytes()).await.unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
            socket.shutdown().await.unwrap();
        }
    });
    (uri, calls)
}

/// Accept TLS connections and close each socket during the handshake, before
/// any HTTP request can reach the server. This deterministically exercises a
/// replay-safe connection-establishment failure.
async fn failed_tls_connect_server(attempts: usize) -> (String, Arc<AtomicUsize>) {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("https://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = calls.clone();
    tokio::spawn(async move {
        for _ in 0..attempts {
            let (socket, _) = listener.accept().await.unwrap();
            server_calls.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    (uri, calls)
}

/// Accept requests and close each socket before writing response headers. This
/// deterministically exercises an ambiguous response-header failure after the
/// provider may have accepted the POST.
async fn dropped_header_server(attempts: usize) -> (String, Arc<AtomicUsize>) {
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let server_calls = calls.clone();
    tokio::spawn(async move {
        for _ in 0..attempts {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let (body_start, content_length) = loop {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
                let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let body_start = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..body_start]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or_default();
                break (body_start, content_length);
            };
            while request.len().saturating_sub(body_start) < content_length {
                let read = socket.read(&mut buffer).await.unwrap();
                if read == 0 {
                    return;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            server_calls.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    (uri, calls)
}

struct ContextAwareScript {
    main_calls: AtomicUsize,
    reject_at: Vec<usize>,
}

struct AbortableCompactionScript {
    summary_started: Arc<std::sync::atomic::AtomicBool>,
}

struct SummaryThenSlowMain;

impl Respond for SummaryThenSlowMain {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let tools_empty = body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty);
        let response = if tools_empty {
            text_turn("authoritative usage forced this summary")
        } else {
            text_turn("normal response should not open before compaction is visible")
        };
        let template = ResponseTemplate::new(200)
            .set_body_string(response)
            .insert_header("content-type", "text/event-stream");
        if tools_empty {
            template
        } else {
            template.set_delay(Duration::from_secs(5))
        }
    }
}

impl Respond for AbortableCompactionScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let tools_empty = body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty);
        if tools_empty {
            self.summary_started.store(true, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(5))
                .set_body_string(text_turn("summary that must never commit"))
                .insert_header("content-type", "text/event-stream")
        } else {
            ResponseTemplate::new(400)
                .set_body_string(r#"{"error":{"message":"context window exceeded"}}"#)
        }
    }
}

struct LocalServerRequestSizeScript {
    main_calls: Arc<AtomicUsize>,
}

impl Respond for LocalServerRequestSizeScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let tools_empty = match body.get("tools") {
            None | Some(serde_json::Value::Null) => true,
            Some(tools) => tools.as_array().is_some_and(Vec::is_empty),
        };
        if tools_empty {
            return ResponseTemplate::new(200)
                .set_body_string(text_turn("compacted summary"))
                .insert_header("content-type", "text/event-stream");
        }
        let index = self.main_calls.fetch_add(1, Ordering::SeqCst);
        if index < 2 {
            return ResponseTemplate::new(200)
                .set_body_string(tool_turn(&[(
                    &format!("read_{index}"),
                    "read",
                    serde_json::json!({"path": "large.txt"}),
                )]))
                .insert_header("content-type", "text/event-stream");
        }
        match index {
            // Verbatim shape of a vLLM context-length rejection, including the
            // numeric code that used to veto the compaction path.
            2 => ResponseTemplate::new(400).set_body_string(
                r#"{"object":"error","message":"This model's maximum context length is 131072 tokens. However, you requested 30896 output tokens and your prompt contains at least 100177 input tokens, for a total of at least 131073 tokens. (parameter=input_tokens, value=100177)","type":"BadRequestError","param":null,"code":400}"#,
            ),
            _ => ResponseTemplate::new(200)
                .set_body_string(text_turn("recovered after compaction"))
                .insert_header("content-type", "text/event-stream"),
        }
    }
}

impl Respond for ContextAwareScript {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let tools_empty = match body.get("tools") {
            None | Some(serde_json::Value::Null) => true,
            Some(tools) => tools.as_array().is_some_and(Vec::is_empty),
        };
        if tools_empty {
            ResponseTemplate::new(200)
                .set_body_string(text_turn("compacted summary"))
                .insert_header("content-type", "text/event-stream")
        } else {
            let index = self.main_calls.fetch_add(1, Ordering::SeqCst);
            if self.reject_at.contains(&index) {
                return ResponseTemplate::new(400)
                    .set_body_string(r#"{"error":{"message":"context window exceeded"}}"#);
            }
            let body = if index < 3 {
                tool_turn(&[(
                    &format!("read_{index}"),
                    "read",
                    serde_json::json!({"path": "large.txt"}),
                )])
            } else {
                text_turn("done after compaction")
            };
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream")
        }
    }
}

fn scripted_model(uri: &str) -> Model {
    Model {
        spec: Arc::new(ModelSpec {
            id: ModelId("scripted".to_string()),
            endpoint: EndpointId("test".to_string()),
            api_name: "scripted-model".to_string(),
            display_name: None,
            protocol: Protocol::AnthropicMessages,
            capabilities: Capabilities {
                input_modalities: ModalitySet::none().with(Modality::Image),
                output_modalities: ModalitySet::none(),
                tools: true,
                parallel_tool_calls: true,
                reasoning: None,
                responses_lite: false,
                agent_delegation: None,
                structured_output: false,
                deferred_tool_loading: false,
            },
            limits: ModelLimits {
                context_window: 200_000,
                max_output_tokens: 8192,
            },
            pricing: None,
            cache: octet_ai::CacheCompatibility::default(),
            preset: Default::default(),
        }),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("test".to_string()),
            base_url: url::Url::parse(uri).unwrap(),
            auth: Auth::bearer("test-key"),
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(10),
        }),
    }
}

fn openai_multimodal_model(uri: &str) -> Model {
    let base = scripted_model(uri);
    let mut spec = (*base.spec).clone();
    spec.protocol = Protocol::OpenAiChat;
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(Endpoint {
            id: EndpointId("test".to_string()),
            base_url: url::Url::parse(&format!("{uri}/v1/")).unwrap(),
            auth: Auth::bearer("test-key"),
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(10),
        }),
    }
}

fn openai_audio_model(uri: &str) -> Model {
    let mut model = openai_multimodal_model(uri);
    let spec = Arc::make_mut(&mut model.spec);
    spec.capabilities.input_modalities = spec.capabilities.input_modalities.with(Modality::Audio);
    spec.capabilities.output_modalities = spec.capabilities.output_modalities.with(Modality::Audio);
    model
}

fn openai_audio_turn(id: &str, data: &[u8], transcript: &str) -> String {
    use base64::Engine as _;

    serde_json::json!({
        "id": id,
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "audio": {
                    "id": format!("{id}-audio"),
                    "data": base64::engine::general_purpose::STANDARD.encode(data),
                    "transcript": transcript,
                    "expires_at": 4_102_444_800_u64
                }
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 5,
            "completion_tokens": 3,
            "total_tokens": 8
        }
    })
    .to_string()
}

fn configure_audio_output(agent: &mut Agent) {
    agent.set_output_modalities(OutputModalities::TextAndAudio(AudioOutputOptions {
        format: AudioFormat::Wav,
        voice: AudioVoice::Named("alloy".to_string()),
    }));
}

fn scripted_responses_model(uri: &str) -> Model {
    let base = scripted_model(uri);
    let mut spec = (*base.spec).clone();
    spec.protocol = Protocol::OpenAiResponses;
    Model {
        spec: Arc::new(spec),
        endpoint: Arc::new(Endpoint {
            id: base.endpoint.id.clone(),
            base_url: url::Url::parse(&format!("{uri}/")).unwrap(),
            auth: Auth::bearer("test-key"),
            default_headers: http::HeaderMap::new(),
            transport: octet_ai::EndpointTransport::Http,
            runtime: octet_ai::RequestRuntime::default(),
            timeout: Duration::from_secs(10),
        }),
    }
}

fn scripted_model_for_protocol(uri: &str, protocol: Protocol) -> Model {
    match protocol {
        Protocol::OpenAiResponses => scripted_responses_model(uri),
        Protocol::OpenAiChat => openai_multimodal_model(uri),
        Protocol::AnthropicMessages => scripted_model(uri),
        Protocol::BedrockConverse | Protocol::GoogleGenerativeAi | Protocol::MistralConversations => {
            panic!("{protocol:?} requires a codec-specific provider fixture")
        }
        // `pi-messages` is the native host codec (the Pi gateway request
        // document plus its serialized assistant-message stream), not another
        // provider alias, and this target has no Pi route fixture. Asking for
        // one is a test setup error, so it is named explicitly instead of being
        // folded into a catch-all that would swallow a future protocol.
        Protocol::PiMessages => {
            panic!("{protocol:?} is the native host codec and has no scripted fixture")
        }
    }
}

fn scripted_model_with_limits(uri: &str, context_window: u64, max_output_tokens: u64) -> Model {
    let base = scripted_model(uri);
    let mut spec = (*base.spec).clone();
    spec.limits.context_window = context_window;
    spec.limits.max_output_tokens = max_output_tokens;
    Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    }
}

struct Harness {
    agent: Agent,
    server: Option<MockServer>,
    session_path: PathBuf,
    workspace: PathBuf,
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

fn build_agent(uri: &str, workspace: &Path, session_path: &Path, max_turns: Option<u64>) -> Agent {
    build_agent_from_session(
        uri,
        workspace,
        Session::create(session_path).unwrap(),
        max_turns,
    )
}

fn build_agent_from_session(
    uri: &str,
    workspace: &Path,
    session: Session,
    max_turns: Option<u64>,
) -> Agent {
    build_agent_from_session_with_model(
        scripted_model(uri),
        workspace,
        session,
        ReasoningConfig::Off,
        max_turns,
    )
}

fn build_agent_from_session_with_model(
    model: Model,
    workspace: &Path,
    session: Session,
    reasoning: ReasoningConfig,
    max_turns: Option<u64>,
) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "You are a scripted test agent.".to_string(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns,
        reasoning,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

/// A scripted Anthropic model that advertises token-budget reasoning, so a
/// non-off [`ReasoningConfig`] passes `octet-ai` validation and serializes a
/// `thinking` block onto the wire.
fn scripted_model_with_reasoning(uri: &str) -> Model {
    let base = scripted_model(uri);
    let mut spec = (*base.spec).clone();
    // The highest advertised thinking budget must leave room for an answer.
    spec.limits.max_output_tokens = 16_384;
    spec.capabilities.reasoning = Some(ReasoningCapability {
        options: None,
        control: ReasoningControl::TokenBudget,
        exposes_text: true,
        preserves_state: true,
        min_effort: octet_ai::ReasoningEffort::Minimal,
        effort_budgets: Some(ReasoningEffortBudgets {
            minimal: 1024,
            low: 2048,
            medium: 4096,
            high: 8192,
            xhigh: 8192,
            max: 8192,
        }),
        openai_chat_mode: octet_ai::OpenAiChatReasoningMode::Standard,
        max_effort: octet_ai::ReasoningEffort::High,
    });
    Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    }
}

/// Replaces a scripted model's reasoning contract with an exact advertised set
/// that intentionally omits `Off`, matching current Codex discovery metadata.
fn scripted_model_requiring_reasoning(base: Model) -> Model {
    let mut spec = (*base.spec).clone();
    spec.capabilities.reasoning = Some(ReasoningCapability {
        options: Some(octet_ai::types::ReasoningOptions {
            values: vec!["medium".into(), "high".into(), "max".into()],
            default: Some("high".into()),
        }),
        control: ReasoningControl::Effort,
        exposes_text: true,
        preserves_state: true,
        min_effort: octet_ai::ReasoningEffort::Medium,
        effort_budgets: None,
        openai_chat_mode: octet_ai::OpenAiChatReasoningMode::Standard,
        max_effort: octet_ai::ReasoningEffort::Max,
    });
    Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    }
}

/// Builds an agent bound to `model` with an explicit [`ReasoningConfig`], the
/// core tools, and a fully-enabled sandbox.
fn build_agent_with_reasoning(
    model: Model,
    session_path: &Path,
    workspace: &Path,
    reasoning: ReasoningConfig,
    max_turns: Option<u64>,
) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(session_path).unwrap(),
        system: "You are a scripted test agent.".to_string(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns,
        reasoning,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

fn build_responses_agent_from_session(
    model: Model,
    session: Session,
    workspace: &Path,
    max_turns: Option<u64>,
    system: &str,
    reasoning: ReasoningConfig,
) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: system.to_owned(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns,
        reasoning,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: Some("lifecycle-session".into()),
    })
    .unwrap()
}

/// Spins up a scripted server replaying `bodies`, then builds a reasoning-capable
/// agent with the given `reasoning` config against it.
async fn reasoning_harness(
    bodies: Vec<String>,
    reasoning: ReasoningConfig,
    reasoning_capable: bool,
) -> (
    Agent,
    MockServer,
    PathBuf,
    (tempfile::TempDir, tempfile::TempDir),
) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let model = if reasoning_capable {
        scripted_model_with_reasoning(&server.uri())
    } else {
        scripted_model(&server.uri())
    };
    let agent = build_agent_with_reasoning(model, &session_path, &workspace, reasoning, Some(8));
    (agent, server, session_path, (workspace_dir, session_dir))
}

async fn harness(bodies: Vec<String>, max_turns: Option<u64>) -> Harness {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let agent = build_agent(&server.uri(), &workspace, &session_path, max_turns);
    Harness {
        agent,
        server: Some(server),
        session_path,
        workspace,
        _dirs: (workspace_dir, session_dir),
    }
}

async fn collect(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    events
}

fn session_with_authoritative_pressure(path: &Path, total_tokens: u64) -> Session {
    session_with_authoritative_pressure_and_pricing(path, total_tokens, None)
}

fn session_with_authoritative_pressure_and_pricing(
    path: &Path,
    total_tokens: u64,
    pricing: Option<&Pricing>,
) -> Session {
    let mut session = Session::create(path).unwrap();
    let mut latest_assistant = None;
    for index in 0..5 {
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("prior user {index}"))],
            })))
            .unwrap();
        latest_assistant = Some(
            session
                .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                    content: vec![AssistantPart::Text(format!("prior answer {index}"))],
                    model: ModelId("scripted".into()),
                    protocol: Protocol::AnthropicMessages,
                })))
                .unwrap(),
        );
    }
    let usage = Usage {
        input_tokens: total_tokens.saturating_sub(1_000),
        output_tokens: 1_000,
        total_tokens,
        ..Usage::default()
    };
    // Pricing is declared when the synthetic historical response is created;
    // never retrofit a known price onto previously unpriced durable history.
    let cost = pricing.map(|pricing| octet_ai::pricing::cost_of(pricing, &usage).unwrap());
    session
        .record_assistant_usage(
            latest_assistant.unwrap(),
            EndpointId("test".into()),
            ModelId("scripted".into()),
            usage,
            cost,
        )
        .unwrap();
    session
}

/// Every started run must emit exactly one `RunFinished`, as its final event.
fn assert_single_run_finished(events: &[AgentEvent]) -> &FinishReason {
    let finishes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, AgentEvent::RunFinished { .. }).then_some(i))
        .collect();
    assert_eq!(finishes.len(), 1, "expected exactly one RunFinished");
    assert_eq!(finishes[0], events.len() - 1, "RunFinished must be last");
    match &events[events.len() - 1] {
        AgentEvent::RunFinished { reason, .. } => reason,
        _ => unreachable!(),
    }
}

async fn wire_requests(server: &MockServer) -> Vec<serde_json::Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn count_tool_results(message: &serde_json::Value) -> usize {
    message["content"]
        .as_array()
        .map(|parts| parts.iter().filter(|p| p["type"] == "tool_result").count())
        .unwrap_or(0)
}

/// The exact system prompt text a wire request carried, across the shapes the
/// Messages and Responses encoders emit (string, or a block list).
fn wire_system_text(request: &serde_json::Value) -> String {
    match request.get("system") {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Array(blocks)) => blocks
            .iter()
            .map(|block| block["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n\n"),
        Some(other) => panic!("unexpected system encoding: {other}"),
    }
}

fn request_has_no_tools(request: &serde_json::Value) -> bool {
    match request.get("tools").and_then(serde_json::Value::as_array) {
        None => true,
        Some(tools) => tools.is_empty(),
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn normal_terminal_turn_without_user_visible_content_fails_loudly() {
    // Each shape names its own cause: an empty reply, and a model that spent the
    // whole turn on reasoning without ever emitting answer text (what a local
    // thinking model does when its budget or template produces no final answer).
    for (body, expected) in [
        (empty_turn(), "no user-visible content"),
        (
            reasoning_only_turn("private trace only"),
            "reasoning but no answer text",
        ),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("messages"))
            .respond_with(Script {
                bodies: vec![body],
                next: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;

        let workspace_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().canonicalize().unwrap();
        let session_path = session_dir.path().join("session.jsonl");
        let mut agent = build_agent(&server.uri(), &workspace, &session_path, Some(4));

        let mut run = agent.prompt("return an answer").await.unwrap();
        let events = collect(&mut run).await;
        match assert_single_run_finished(&events) {
            FinishReason::Failed(error) => {
                assert!(
                    error.to_string().contains(expected),
                    "expected {expected:?}, got: {error}"
                );
            }
            other => panic!("empty terminal response must fail, got {other:?}"),
        }
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::TurnFinished { .. })),
            "an empty turn must not be presented as a completed model turn"
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn retryable_initial_stream_open_is_retried_by_the_agent() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(RetryInitialOpen {
            calls: calls.clone(),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&server.uri(), &workspace, &session_path, Some(4));

    let mut run = agent.prompt("retry safely").await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let retries = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ProviderRetry { .. }))
        .count();
    assert_eq!(retries, 1, "the retry must be visible while it happens");
    let starts = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::TurnStarted))
        .count();
    assert_eq!(
        starts, 2,
        "each physical provider attempt needs its own timing lifecycle"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn response_header_timeout_is_terminal_without_replaying_post() {
    for protocol in [
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
    ] {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .respond_with(DelayedHeaders {
                calls: calls.clone(),
            })
            .mount(&server)
            .await;

        let workspace_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().canonicalize().unwrap();
        let session_path = session_dir.path().join("session.jsonl");
        let mut model = scripted_model_for_protocol(&server.uri(), protocol);
        Arc::make_mut(&mut model.endpoint).timeout = Duration::from_millis(10);
        let mut agent = build_agent_with_reasoning(
            model,
            &session_path,
            &workspace,
            ReasoningConfig::Off,
            Some(4),
        );

        let mut run = agent.prompt("do not replay a slow POST").await.unwrap();
        let events = collect(&mut run).await;
        assert!(
            matches!(assert_single_run_finished(&events), FinishReason::Failed(_)),
            "{protocol:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })),
            "{protocol:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{protocol:?}");
        let FinishReason::Failed(error) = assert_single_run_finished(&events) else {
            unreachable!("failure was asserted above")
        };
        assert!(
            error
                .to_string()
                .contains("timed out waiting for response headers"),
            "{protocol:?}: {error}"
        );
    }
}

#[tokio::test]
async fn incomplete_responses_output_survives_continuation_and_restart() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                responses_text_turn(
                    "resp_partial",
                    "partial",
                    "response.incomplete",
                    "partial-opaque",
                ),
                responses_text_turn(
                    "resp_final",
                    "finished",
                    "response.completed",
                    "final-opaque",
                ),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let model = scripted_responses_model(&server.uri());
    let endpoint = model.endpoint.id.clone();
    let model_id = model.spec.id.clone();
    let mut agent = build_agent_with_reasoning(
        model,
        &session_path,
        &workspace,
        ReasoningConfig::Off,
        Some(4),
    );

    let output = agent.complete("continue exactly").await.unwrap();
    assert!(
        output.text.contains("finished"),
        "the corrective continuation must finish: {}",
        output.text
    );

    let requests = wire_requests(&server).await;
    assert_eq!(
        requests.len(),
        2,
        "max-token output must trigger one continuation"
    );
    let replayed = serde_json::to_string(&requests[1]["input"]).unwrap();
    assert!(
        replayed.contains("partial-opaque"),
        "the continuation request lost the incomplete terminal output: {replayed}"
    );
    assert!(
        replayed.contains("previous response was truncated"),
        "the continuation instruction is missing: {replayed}"
    );
    assert_eq!(requests[1]["store"], false);
    assert!(requests[1].get("previous_response_id").is_none());

    drop(agent);
    let reopened = Session::open(&session_path).unwrap();
    let replay = reopened
        .responses_replay_items(&endpoint, &model_id)
        .unwrap()
        .expect("every Responses assistant turn must retain an exact sidecar");
    let markers: Vec<_> = replay
        .iter()
        .filter_map(|item| match item {
            octet_ai::responses::ResponsesReplayItem::Output(output) => output
                .items()
                .first()
                .and_then(|item| item.as_json()["unknown_provider_field"].as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(markers, vec!["partial-opaque", "final-opaque"]);
}

#[tokio::test]
async fn responses_restart_compact_and_post_checkpoint_replay_stay_exact_end_to_end() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                responses_tool_turn("resp_tool", "call_lifecycle"),
                responses_text_turn(
                    "resp_before_compact",
                    "continued after restart",
                    "response.completed",
                    "before-compact-opaque",
                ),
                responses_text_turn(
                    "resp_after_compact",
                    "continued after checkpoint",
                    "response.completed",
                    "after-compact-opaque",
                ),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let compact_output = serde_json::json!([
        {
            "type": "message",
            "id": "compact-leading",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "retained leading item"}],
            "future": {"preserved": true}
        },
        {
            "type": "compaction",
            "id": "compact-checkpoint",
            "encrypted_content": "encrypted-checkpoint",
            "future_compaction_field": [1, 2, 3]
        }
    ]);
    Mock::given(method("POST"))
        .and(path("responses/compact"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "output": compact_output.clone(),
            "usage": {"input_tokens": 30, "output_tokens": 2}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("lifecycle.txt"), "exact tool result\n").unwrap();
    let session_path = session_dir.path().join("responses-lifecycle.jsonl");
    let mut model = scripted_responses_model(&server.uri());
    let mut spec = (*model.spec).clone();
    spec.cache.session_affinity_format = Some(octet_ai::SessionAffinityFormat::Codex);
    model.spec = Arc::new(spec);
    let model = scripted_model_requiring_reasoning(model);

    let mut first = build_responses_agent_from_session(
        model.clone(),
        Session::create(&session_path).unwrap(),
        &workspace,
        Some(1),
        "ORIGINAL LIFECYCLE INSTRUCTIONS",
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
    );
    let mut run = first.prompt("start lifecycle").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::MaxTurns
    ));
    drop(first);

    let mut agent = build_responses_agent_from_session(
        model,
        Session::open(&session_path).unwrap(),
        &workspace,
        Some(4),
        "ORIGINAL LIFECYCLE INSTRUCTIONS",
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
    );
    let output = agent.complete("continue after restart").await.unwrap();
    assert_eq!(output.text, "continued after restart");

    let checkpoint = agent.compact_responses_native().await.unwrap();
    assert!(matches!(
        checkpoint.kind,
        octet_agent::CompactionKind::NativeResponses { .. }
    ));
    agent.set_system_prompt("CURRENT POST-CHECKPOINT INSTRUCTIONS");
    let output = agent.complete("continue after checkpoint").await.unwrap();
    assert_eq!(output.text, "continued after checkpoint");

    let requests = server.received_requests().await.unwrap();
    let responses = requests
        .iter()
        .filter(|request| request.url.path() == "/responses")
        .collect::<Vec<_>>();
    let compact = requests
        .iter()
        .find(|request| request.url.path() == "/responses/compact")
        .unwrap();
    assert_eq!(responses.len(), 3);

    let request_json = |request: &wiremock::Request| {
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap()
    };
    let restarted_body = request_json(responses[1]);
    let restarted_input = serde_json::to_string(&restarted_body["input"]).unwrap();
    assert!(
        restarted_input.contains("call_lifecycle"),
        "{restarted_input}"
    );
    assert!(
        restarted_input.contains("unknown_provider_field"),
        "{restarted_input}"
    );
    assert!(
        restarted_input.contains("encrypted-reasoning-state"),
        "{restarted_input}"
    );
    assert!(
        restarted_input.contains("exact tool result"),
        "{restarted_input}"
    );

    let compact_body = request_json(compact);
    assert_eq!(compact_body["reasoning"]["effort"], "high");
    assert_eq!(
        compact_body["instructions"],
        "ORIGINAL LIFECYCLE INSTRUCTIONS"
    );
    assert!(compact_body["tools"]
        .as_array()
        .is_some_and(|tools| !tools.is_empty()));
    assert_eq!(compact_body["prompt_cache_key"], "lifecycle-session");
    assert!(compact.headers.get("content-encoding").is_none());
    assert_eq!(compact.headers["x-client-request-id"], "lifecycle-session");

    let post_checkpoint_body = request_json(responses[2]);
    assert_eq!(
        post_checkpoint_body["instructions"],
        "CURRENT POST-CHECKPOINT INSTRUCTIONS"
    );
    let post_input = post_checkpoint_body["input"].as_array().unwrap();
    assert_eq!(post_input[0], compact_output[0]);
    assert_eq!(post_input[1], compact_output[1]);
    assert_eq!(post_input[1]["encrypted_content"], "encrypted-checkpoint");
    assert_eq!(
        post_input[1]["future_compaction_field"],
        serde_json::json!([1, 2, 3])
    );
}

#[tokio::test]
async fn accepted_post_dropped_before_headers_is_not_replayed() {
    for protocol in [
        Protocol::OpenAiResponses,
        Protocol::OpenAiChat,
        Protocol::AnthropicMessages,
    ] {
        let (uri, calls) = dropped_header_server(2).await;
        let workspace_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().canonicalize().unwrap();
        let session_path = session_dir.path().join("session.jsonl");
        let model = scripted_model_for_protocol(&uri, protocol);
        let mut agent = build_agent_with_reasoning(
            model,
            &session_path,
            &workspace,
            ReasoningConfig::Off,
            Some(4),
        );

        let mut run = agent
            .prompt("do not replay an accepted POST")
            .await
            .unwrap();
        let events = collect(&mut run).await;
        assert!(
            matches!(assert_single_run_finished(&events), FinishReason::Failed(_)),
            "{protocol:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })),
            "{protocol:?}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "{protocol:?}");
        let FinishReason::Failed(error) = assert_single_run_finished(&events) else {
            unreachable!("failure was asserted above")
        };
        assert!(
            error.to_string().contains("ResponseHeaders"),
            "{protocol:?}: {error}"
        );
    }
}

#[tokio::test]
async fn repeated_connect_failure_is_visible_and_bounded() {
    let (uri, calls) = failed_tls_connect_server(MAX_CONNECT_ATTEMPTS_FOR_TEST).await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&uri, &workspace, &session_path, Some(4));

    let mut run = agent.prompt("fail visibly").await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    let retries = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderRetry {
                attempt,
                max_attempts,
                error,
                ..
            } => Some((*attempt, *max_attempts, error)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(retries.len(), 5);
    assert_eq!((retries[0].0, retries[0].1), (1, 5));
    assert_eq!((retries[4].0, retries[4].1), (5, 5));
    assert!(retries
        .iter()
        .all(|(_, _, error)| error.contains("Are you connected to the internet?")));
    assert!(retries[0].2.contains("provider=test model=scripted"));
    assert!(retries[0].2.contains("phase=connection"));
    // Bounded transport detail is intentional. It may include the hyper/OS
    // connect label (for example "client error (Connect)"), but never the
    // endpoint URL, credentials, or internal enum path.
    assert!(retries[0].2.contains("detail="), "{}", retries[0].2);
    assert!(!retries[0].2.contains("TransportPhase"));
    assert!(!retries[0].2.contains(&uri));
    assert!(!retries[0].2.contains("test-key"));
    assert_eq!(calls.load(Ordering::SeqCst), MAX_CONNECT_ATTEMPTS_FOR_TEST);

    let FinishReason::Failed(error) = assert_single_run_finished(&events) else {
        unreachable!("failure was asserted above")
    };
    let failure = error.to_string();
    assert!(failure.contains("after 5 retries"), "{failure}");
    assert!(
        failure.contains("Are you connected to the internet?"),
        "{failure}"
    );
}

#[tokio::test]
async fn connect_retry_delay_is_cancellable() {
    let (uri, calls) = failed_tls_connect_server(2).await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&uri, &workspace, &session_path, Some(4));

    let mut run = agent.prompt("cancel retry").await.unwrap();
    let control = run.control();
    let started = std::time::Instant::now();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::ProviderRetry { .. }) {
            control.abort();
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ProviderRetry { .. }))
            .count(),
        1
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "aborting the visible backoff must prevent the retry request"
    );
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "abort must not wait for the retry backoff"
    );
}

#[tokio::test]
async fn body_disconnect_after_output_never_replays_ambiguous_generation() {
    let (uri, calls) = interrupted_body_server(
        partial_text_turn("observed partial generation"),
        text_turn("must never be requested"),
    )
    .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&uri, &workspace, &session_path, Some(4));

    let mut run = agent.prompt("do not replay generated work").await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text,
        } if text.contains("observed partial generation")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })),
        "observed generation makes the request ambiguous and non-replayable"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnStarted))
            .count(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn body_disconnect_before_output_is_ambiguous_and_not_replayed() {
    let (uri, calls) = interrupted_body_server(msg_start(), text_turn("must not replay")).await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&uri, &workspace, &session_path, Some(4));
    let mut run = agent.prompt("inspect before retrying").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::TurnStarted))
            .count(),
        1
    );
    drop(agent);
    assert!(Session::open_read_only(&session_path)
        .unwrap()
        .context()
        .unwrap()
        .iter()
        .any(|message| matches!(
            message, Message::User(user) if user.content.iter().any(|part| matches!(
                part, UserPart::Text(text) if text == "inspect before retrying"
            ))
        )));
}

#[tokio::test]
async fn missing_terminal_before_output_is_ambiguous_and_not_replayed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![msg_start(), text_turn("must not replay")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("session.jsonl"),
        Some(4),
    );
    let mut run = agent.prompt("keep this pending input").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    drop(agent);
    assert!(
        Session::open_read_only(sessions.path().join("session.jsonl"))
            .unwrap()
            .context()
            .unwrap()
            .iter()
            .any(|message| matches!(
                message, Message::User(user) if user.content.iter().any(|part| matches!(
                    part, UserPart::Text(text) if text == "keep this pending input"
                ))
            ))
    );
}

#[tokio::test]
async fn failed_provider_turn_does_not_replay_old_intent_on_next_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(FailThenSucceed {
            calls: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let mut agent = build_agent(
        &server.uri(),
        &workspace,
        &session_dir.path().join("failed-turn.jsonl"),
        Some(4),
    );

    assert!(agent.complete("old request").await.is_err());
    let output = agent.complete("hi").await.unwrap();
    assert_eq!(output.text, "hello from the new turn");

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["role"], "user");
    assert!(messages[0].to_string().contains("old request"));
    assert_eq!(messages[1]["role"], "assistant");
    assert!(messages[1].to_string().contains("failed before completion"));
    assert_eq!(messages[2]["role"], "user");
    assert!(messages[2].to_string().contains("hi"));
}

#[tokio::test]
async fn one_user_task_compacts_completed_tool_episodes_in_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(ContextAwareScript {
            main_calls: AtomicUsize::new(0),
            reject_at: vec![],
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(
        workspace.join("large.txt"),
        (0..600)
            .map(|index| format!("line {index}: a long repeated payload for sizing\n"))
            .collect::<String>(),
    )
    .unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let model = scripted_model_with_limits(&server.uri(), 12_000, 1_024);
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(&session_path).unwrap(),
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(10),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let output = agent
        .complete("inspect the large file repeatedly")
        .await
        .unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert!(agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

#[tokio::test]
async fn authoritative_usage_compacts_and_reports_phase_before_opening_slow_main_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(SummaryThenSlowMain)
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("authoritative-pressure.jsonl");
    let session = session_with_authoritative_pressure(&session_path, 180_000);
    let model = scripted_model_requiring_reasoning(scripted_model(&server.uri()));
    let mut agent = build_agent_from_session_with_model(
        model,
        workspace.path(),
        session,
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        Some(4),
    );
    // Even when the keep preference exceeds the number of available turns,
    // the configured threshold still has to trigger compaction.
    agent
        .set_compaction_token_policy(true, 0.85, 10_000)
        .unwrap();

    let mut run = agent.prompt("new work").await.unwrap();
    let control = run.control();
    let mut saw_start = false;
    let mut saw_finish = false;
    while !saw_finish {
        let event = tokio::time::timeout(Duration::from_secs(1), run.next())
            .await
            .expect("compaction phase must be visible before the slow main route")
            .expect("run event");
        match event {
            AgentEvent::CompactionStarted {
                reason: octet_agent::CompactionReason::Threshold,
            } => saw_start = true,
            AgentEvent::CompactionFinished {
                result: Ok(ref info),
                ..
            } => {
                assert!(saw_start, "finish preceded start");
                assert!(info.summary.contains("authoritative usage"));
                saw_finish = true;
                control.abort();
            }
            _ => {}
        }
    }
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    drop(run);

    let requests = wire_requests(&server).await;
    assert!(!requests.is_empty(), "compaction summary request missing");
    assert_eq!(requests[0]["output_config"]["effort"], "high");
    assert!(
        requests.iter().all(|request| request
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty)),
        "normal provider request opened before the abort"
    );
    assert!(agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

#[tokio::test]
async fn disabled_auto_compaction_allows_below_capacity_request_past_threshold() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(text_turn("auto compaction is off"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session = session_with_authoritative_pressure(
        &sessions.path().join("disabled-auto-compaction.jsonl"),
        180_000,
    );
    let mut agent = build_agent_from_session(&server.uri(), workspace.path(), session, Some(1));
    agent
        .set_compaction_token_policy(false, 0.85, 4_000)
        .unwrap();

    let mut run = agent.prompt("new work").await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::CompactionStarted { .. } | AgentEvent::CompactionFinished { .. }
    )));
    drop(run);
    assert!(agent
        .session()
        .entries()
        .iter()
        .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| !tools.is_empty()));
}

#[tokio::test]
async fn request_output_ceiling_clamps_only_to_remaining_context() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(text_turn("context-aware cap"))
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session = session_with_authoritative_pressure(
        &sessions.path().join("request-output-cap.jsonl"),
        70_000,
    );
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let model = scripted_model_with_limits(&server.uri(), 100_000, 65_536);
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "test".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(1),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent
        .set_compaction_token_policy(false, 1.0, 4_000)
        .unwrap();

    agent.complete("new work").await.unwrap();
    let requests = wire_requests(&server).await;
    let request_max = requests[0]["max_tokens"].as_u64().unwrap();
    assert!(request_max < 65_536, "{request_max}");
    assert!(request_max >= 16_384, "{request_max}");
}

#[tokio::test]
async fn hard_cost_reservation_blocks_network_before_a_request_can_overshoot() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut model = scripted_model(&server.uri());
    let mut spec = (*model.spec).clone();
    spec.pricing = Some(Pricing {
        input: TokenRate(1_000_000),
        output: TokenRate(1_000_000),
        cache_read: TokenRate(1_000_000),
        cache_write_5m: TokenRate(1_000_000),
        cache_write_1h: Some(TokenRate(1_000_000)),
        reasoning: Some(TokenRate(1_000_000)),
        tiers: vec![],
    });
    model.spec = Arc::new(spec);
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(sessions.path().join("cost-limit.jsonl")).unwrap(),
        system: "cost test".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(2),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    agent.set_max_session_cost_microdollars(Some(1));

    let error = agent
        .complete("do not spend beyond the ceiling")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("cost limit"), "{error}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn zero_input_budget_fails_locally_before_opening_a_provider_request() {
    let server = MockServer::start().await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let model = scripted_model_with_limits(&server.uri(), 64, 64);
    let mut agent = build_agent_with_reasoning(
        model,
        &sessions.path().join("zero-budget.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(1),
    );

    let error = agent.complete("this request cannot fit").await.unwrap_err();
    assert!(error.to_string().contains("context"), "{error}");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn provider_context_error_forces_one_compaction_before_retry() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(ContextAwareScript {
            main_calls: AtomicUsize::new(0),
            reject_at: vec![2],
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("large.txt"), "small\n").unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(&session_path).unwrap(),
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(10),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let output = agent.complete("force context recovery").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert!(agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })));
}

/// A strict self-hosted/local server answers HTTP 400 with a machine-readable
/// `"code":400` inside a provider-shaped error body. Before this regression the
/// numeric code selected the permanent branch, so the request failed instead of
/// compacting; a real vLLM deployment rejected a 131072-token window this way.
#[tokio::test]
async fn local_server_request_size_rejection_compacts_once_and_retries() {
    let server = MockServer::start().await;
    let main_calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(LocalServerRequestSizeScript {
            main_calls: Arc::clone(&main_calls),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    // The session must hold a reducible episode before the rejection, exactly
    // like a real run; otherwise there is legitimately nothing to compact.
    std::fs::write(workspace.join("large.txt"), "small\n").unwrap();
    let mut agent = build_agent(
        &server.uri(),
        &workspace,
        &sessions.path().join("local-request-size.jsonl"),
        Some(6),
    );
    let output = agent.complete("compact and retry").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed), "{output:?}");
    assert!(
        agent
            .session()
            .entries()
            .iter()
            .any(|entry| matches!(entry.value, EntryValue::Compaction { .. })),
        "the rejection must commit exactly the one compaction it used"
    );
    assert_eq!(
        main_calls.load(Ordering::SeqCst),
        4,
        "two tool turns, one rejected request, then one successful retry"
    );
}

#[tokio::test]
async fn abort_cancels_compaction_without_late_usage_or_summary_commits() {
    let server = MockServer::start().await;
    let summary_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(AbortableCompactionScript {
            summary_started: Arc::clone(&summary_started),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("abort-compaction.jsonl"),
        Some(4),
    );
    for index in 0..3 {
        agent
            .session_mut()
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text(format!("prior user {index}"))],
            })))
            .unwrap();
        agent
            .session_mut()
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text(format!("prior answer {index}"))],
                model: ModelId("scripted".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();
    }

    let mut run = agent.prompt("trigger context recovery").await.unwrap();
    let control = run.control();
    let abort_task = tokio::spawn(async move {
        while !summary_started.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        control.abort();
    });
    let started = std::time::Instant::now();
    let mut reason = None;
    while let Some(event) = run.next().await {
        if let AgentEvent::RunFinished { reason: ended, .. } = event {
            reason = Some(ended);
        }
    }
    abort_task.await.unwrap();
    drop(run);

    assert!(matches!(reason, Some(FinishReason::Aborted)));
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(agent
        .session()
        .entries()
        .iter()
        .all(|entry| !matches!(entry.value, EntryValue::Compaction { .. })));
    assert!(agent
        .session()
        .usage_records()
        .iter()
        .all(|record| !matches!(record.kind, UsageRecordKind::Compaction)));
}

#[tokio::test]
async fn repeated_context_rejection_advances_compaction_without_spending_turns() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(ContextAwareScript {
            main_calls: AtomicUsize::new(0),
            // Three completed tool episodes provide two successively newer
            // compaction boundaries. Reject both requests before generation.
            reject_at: vec![3, 4],
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("large.txt"), "small\n").unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut model = scripted_model(&server.uri());
    Arc::make_mut(&mut model.spec).pricing = Some(Pricing {
        input: TokenRate(1_000_000),
        output: TokenRate(1_000_000),
        cache_read: TokenRate(1_000_000),
        cache_write_5m: TokenRate(1_000_000),
        cache_write_1h: Some(TokenRate(1_000_000)),
        reasoning: None,
        tiers: vec![],
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session: Session::create(&session_path).unwrap(),
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        // Exactly three tool responses plus the final response. Failed context
        // opens and summary calls must not consume this logical-turn budget.
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let output = agent
        .complete("force repeated context recovery")
        .await
        .unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(output.text, "done after compaction");
    // Each compaction performs one grounded summary request, so two
    // recoveries add two tool-free subagent calls to the four main turns.
    assert_eq!(output.usage.input_tokens, 30);
    assert_eq!(output.usage.output_tokens, 18);
    // Four main turns plus two compaction summaries each cost 5 + 3
    // microdollars. Compaction must reach both run and durable session totals.
    assert_eq!(output.cost_microdollars, 48);
    assert_eq!(agent.session().total_cost_microdollars(), 48);
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::Compaction))
            .count(),
        2
    );

    let compactions = agent
        .session()
        .entries()
        .iter()
        .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
        .count();
    assert_eq!(compactions, 2);
    let visible_summaries = agent
        .session()
        .context()
        .unwrap()
        .iter()
        .filter(|message| {
            matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Text(text) if text.starts_with("[summary of earlier conversation]"))))
        })
        .count();
    assert_eq!(visible_summaries, 1, "older overlapping summary leaked");
}

#[tokio::test]
async fn text_only_completion() {
    let mut h = harness(vec![text_turn("Hello world")], Some(8)).await;
    let output = h.agent.complete("hi").await.unwrap();

    assert_eq!(output.text, "Hello world");
    assert!(matches!(output.reason, FinishReason::Completed));
    assert!(output.usage.output_tokens > 0, "usage must propagate");

    // User + assistant persisted, head on the assistant message.
    let session = h.agent.session();
    assert_eq!(session.entries().len(), 2);
    assert_eq!(session.head(), Some(output.head.clone()));
    assert_eq!(session.checkpoints().len(), 1);
    assert_eq!(session.checkpoints()[0].prompt, session.entries()[0].id);
    assert_eq!(session.checkpoints()[0].head, output.head);
}

#[tokio::test]
async fn consuming_an_unfinished_run_returns_its_dropped_context_snapshot() {
    let mut h = harness(vec![text_turn("unused")], Some(4)).await;
    let run = h.agent.prompt("stop before polling").await.unwrap();

    let snapshot = run.into_context_snapshot();

    assert_eq!(snapshot.phase, octet_agent::RunPhase::Finished);
    assert_eq!(
        snapshot.terminal_state,
        Some(octet_agent::RunTerminalState::Dropped)
    );
    assert!(snapshot.context.total_tokens > 0);
    assert!(snapshot.context.context_limit > 0);
}

#[tokio::test]
async fn context_snapshot_polling_does_not_mutate_durable_history() {
    let mut h = harness(vec![text_turn("unused")], Some(4)).await;
    let session_path = h.session_path.clone();
    let run = h.agent.prompt("observe without writing").await.unwrap();
    let durable_before = std::fs::read(&session_path).unwrap();

    let first = run.context_snapshot();
    let second = run.context_snapshot();

    assert_eq!(first, second);
    assert_eq!(std::fs::read(&session_path).unwrap(), durable_before);
    drop(run);
}

#[tokio::test]
async fn run_context_snapshot_updates_without_a_presentation_layer() {
    let mut h = harness(vec![text_turn("streamed context")], Some(4)).await;
    let mut run = h.agent.prompt("track it").await.unwrap();
    let mut revision = 0;
    while run.next().await.is_some() {
        let snapshot = run.context_snapshot();
        assert!(snapshot.revision >= revision);
        revision = snapshot.revision;
    }
    let snapshot = run.context_snapshot();
    assert_eq!(snapshot.responses_started, 1);
    assert_eq!(snapshot.responses_finished, 1);
    assert_eq!(snapshot.phase, octet_agent::RunPhase::Finished);
    assert_eq!(
        snapshot.terminal_state,
        Some(octet_agent::RunTerminalState::Completed)
    );
    assert!(snapshot.response_text_bytes >= "streamed context".len() as u64);
    assert!(snapshot.response_usage.total_tokens > 0);
    assert_eq!(snapshot.run_usage, snapshot.response_usage);
}

#[tokio::test]
async fn openai_lifecycle_feedback_is_forwarded_but_not_persisted() {
    let server = MockServer::start().await;
    let body = concat!(
        ": octet-lifecycle: loading; warming test-key\n\n",
        "data: {\"id\":\"lifecycle\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"Ready\"}}]}\n\n",
        "data: {\"id\":\"lifecycle\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("x-octet-lifecycle", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream")
                .insert_header("x-octet-lifecycle", "queued; accepted test-key"),
        )
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("lifecycle.jsonl");
    let mut model = openai_multimodal_model(&server.uri());
    Arc::make_mut(&mut model.endpoint)
        .runtime
        .lifecycle_feedback = true;
    let mut agent = build_agent_with_reasoning(
        model,
        &session_path,
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );

    let mut run = agent.prompt("wait for the local endpoint").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    let lifecycle = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ProviderLifecycle { lifecycle } => Some(lifecycle),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(lifecycle.len(), 2);
    assert_eq!(lifecycle[0].state, ProviderLifecycleState::Queued);
    assert_eq!(lifecycle[1].state, ProviderLifecycleState::Loading);
    assert!(lifecycle.iter().all(|lifecycle| {
        lifecycle
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("[REDACTED]"))
    }));

    let persisted = std::fs::read_to_string(&session_path).unwrap();
    assert!(persisted.contains("Ready"));
    assert!(!persisted.contains("provider_lifecycle"));
    assert!(!persisted.contains("warming test-key"));
    assert!(!persisted.contains("accepted test-key"));
}

#[tokio::test]
async fn openai_compatible_agent_sends_inline_image_end_to_end() {
    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"vision\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"content\":\"I see it\"}}]}\n\n",
        "data: {\"id\":\"vision\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":9,\"completion_tokens\":3,\"total_tokens\":12}}\n\n",
        "data: [DONE]\n\n"
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(body)
                .insert_header("content-type", "text/event-stream"),
        )
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_reasoning(
        openai_multimodal_model(&server.uri()),
        &sessions.path().join("vision.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );
    let input = UserInput::from(vec![
        InputPart::Text("describe this image".into()),
        InputPart::Media(Media::image_bytes(
            bytes::Bytes::from_static(b"\x89PNG\r\n\x1a\n"),
            "image/png".parse().unwrap(),
        )),
    ]);
    let output = agent.complete(input).await.unwrap();
    assert_eq!(output.text, "I see it");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let content = request["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    assert_eq!(
        content[0],
        serde_json::json!({"type":"text","text":"describe this image"})
    );
    assert_eq!(content[1]["type"], "image_url");
    assert_eq!(
        content[1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
    assert!(matches!(
        &agent.session().context().unwrap()[0],
        Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::Media(_)))
    ));
}

#[tokio::test]
async fn openai_audio_request_emits_output_media_and_commits_transcript() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(JsonScript {
            bodies: vec![openai_audio_turn(
                "audio-event",
                b"generated wav",
                "Spoken response.",
            )],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_reasoning(
        openai_audio_model(&server.uri()),
        &sessions.path().join("audio-events.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );
    configure_audio_output(&mut agent);

    let input = UserInput::from(vec![
        InputPart::Text("answer aloud".into()),
        InputPart::Media(Media::audio_bytes(
            bytes::Bytes::from_static(b"input wav"),
            AudioFormat::Wav,
        )),
    ]);
    let mut run = agent.prompt(input).await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let (index, media) = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::OutputMedia { index, media } => Some((*index, media)),
            _ => None,
        })
        .expect("completed output media event");
    assert_eq!(index, 0);
    let Media::Audio(audio) = media else {
        panic!("expected generated audio");
    };
    assert_eq!(audio.format, AudioFormat::Wav);
    assert_eq!(audio.transcript.as_deref(), Some("Spoken response."));
    let AudioPayload::InlineWithProviderRef { data, reference } = &audio.payload else {
        panic!("expected output bytes with a reusable provider reference");
    };
    assert_eq!(data.as_ref(), b"generated wav");
    assert_eq!(reference.id, "audio-event-audio");

    let turn = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::TurnFinished { message, .. } => Some(message),
            _ => None,
        })
        .expect("committed turn");
    assert!(turn.content.iter().any(|part| {
        matches!(part, AssistantPart::Media(Media::Audio(audio))
            if audio.transcript.as_deref() == Some("Spoken response."))
    }));
    drop(run);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let request: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(request["stream"], false);
    assert_eq!(request["modalities"], serde_json::json!(["text", "audio"]));
    assert_eq!(
        request["audio"],
        serde_json::json!({"voice":"alloy", "format":"wav"})
    );
    let user_content = request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|message| message["role"] == "user")
        .unwrap()["content"]
        .as_array()
        .unwrap();
    assert_eq!(
        user_content[0],
        serde_json::json!({"type":"text", "text":"answer aloud"})
    );
    assert_eq!(
        user_content[1],
        serde_json::json!({
            "type": "input_audio",
            "input_audio": {"data": "aW5wdXQgd2F2", "format": "wav"}
        })
    );
}

#[tokio::test]
async fn complete_discards_rejected_audio_and_returns_only_committed_media() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(JsonScript {
            bodies: vec![
                openai_audio_turn("rejected", b"reject me", "Unverified answer."),
                openai_text_turn("C"),
                openai_audio_turn("accepted", b"keep me", "Verified answer."),
                openai_text_turn("R"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_reasoning(
        openai_audio_model(&server.uri()),
        &sessions.path().join("audio-complete.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );
    configure_audio_output(&mut agent);
    agent.set_completion_policy(CompletionPolicy::TerminalGate);

    let output = agent.complete("answer with verified audio").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert!(output.text.is_empty());
    assert_eq!(output.media.len(), 1);
    let Media::Audio(audio) = &output.media[0] else {
        panic!("expected generated audio");
    };
    assert_eq!(audio.transcript.as_deref(), Some("Verified answer."));
    let AudioPayload::InlineWithProviderRef { data, reference } = &audio.payload else {
        panic!("expected output bytes with a reusable provider reference");
    };
    assert_eq!(data.as_ref(), b"keep me");
    assert_eq!(reference.id, "accepted-audio");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);
    for index in [0, 2] {
        let request: serde_json::Value = serde_json::from_slice(&requests[index].body).unwrap();
        assert_eq!(request["modalities"], serde_json::json!(["text", "audio"]));
        assert_eq!(request["stream"], false);
    }
    for index in [1, 3] {
        let request: serde_json::Value = serde_json::from_slice(&requests[index].body).unwrap();
        assert!(request.get("modalities").is_none());
        assert_eq!(request["stream"], true);
    }
    let decisions = agent
        .session()
        .usage_records()
        .iter()
        .filter_map(|record| match record.kind {
            UsageRecordKind::TerminalGate { returned } => Some(returned),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions, vec![Some(false), Some(true)]);
}

#[tokio::test]
async fn tool_output_locked_causes_a_corrective_openai_turn() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            bodies: vec![
                openai_text_turn("preparing [tool_output_locked]"),
                openai_text_turn("LOCK_RECOVERED"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_reasoning(
        openai_multimodal_model(&server.uri()),
        &sessions.path().join("locked.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );

    let output = agent.complete("recover the call").await.unwrap();
    assert!(output.text.contains("LOCK_RECOVERED"));
    let context = agent.session().context().unwrap();
    let serialized = serde_json::to_string(&context).unwrap();
    assert!(!serialized.contains("tool_output_locked"));
    assert!(serialized.contains("Re-issue that tool call now"));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn max_token_tool_calls_are_failed_without_execution_and_reissued() {
    let mut h = harness(
        vec![
            tool_turn_with_stop(
                &[(
                    "truncated_write",
                    "write",
                    serde_json::json!({
                        "path": "must-not-exist.txt",
                        "content": "unsafe partial output"
                    }),
                )],
                "max_tokens",
            ),
            text_turn("recovered without executing the truncated call"),
        ],
        Some(8),
    )
    .await;

    let output = h.agent.complete("write safely").await.unwrap();
    assert!(output.text.contains("recovered without executing"));
    assert!(!h.workspace.join("must-not-exist.txt").exists());

    let context = serde_json::to_string(&h.agent.session().context().unwrap()).unwrap();
    assert!(context.contains("truncated_write"), "{context}");
    assert!(context.contains("was not executed"), "{context}");
    assert!(context.contains("output token limit"), "{context}");

    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    let recovery = requests[1].to_string();
    assert!(recovery.contains("was not executed"), "{recovery}");
    assert!(
        recovery.contains("truncated at the token limit"),
        "{recovery}"
    );
}

#[tokio::test]
async fn non_normal_stop_reasons_never_become_completed() {
    let mut h = harness(
        vec![
            text_turn_with_stop("truncated", "max_tokens"),
            text_turn("continued"),
        ],
        Some(8),
    )
    .await;
    let output = h.agent.complete("finish the task").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(output.text, "truncatedcontinued");
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .to_string()
        .contains("truncated at the token limit"));

    let mut paused = harness(
        vec![
            text_turn_with_stop("paused", "pause_turn"),
            text_turn("resumed"),
        ],
        Some(8),
    )
    .await;
    assert!(paused.agent.complete("resume").await.is_ok());

    let mut refusal = harness(vec![text_turn_with_stop("no", "refusal")], Some(8)).await;
    assert!(refusal.agent.complete("try").await.is_err());
    let mut unknown = harness(
        vec![text_turn_with_stop("?", "provider_new_reason")],
        Some(8),
    )
    .await;
    assert!(unknown.agent.complete("try").await.is_err());
}

#[tokio::test]
async fn resumed_agent_reexecutes_only_missing_tool_results() {
    let mut h = harness(vec![text_turn("resumed")], Some(8)).await;
    std::fs::write(h.workspace.join("recover.txt"), "recovered content\n").unwrap();
    h.agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("prior request".into())],
        })))
        .unwrap();
    h.agent
        .session_mut()
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                id: octet_ai::ToolCallId("crashed_call".into()),
                name: "read".into(),
                arguments_json: serde_json::json!({"path": "recover.txt"}).to_string(),
                argument_error: None,
            })],
            model: ModelId("scripted".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    let output = h.agent.complete("continue after restart").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert!(h.agent.session().entries().iter().any(|entry| {
        matches!(
            &entry.value,
            EntryValue::Message(Message::User(user))
                if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(result) if result.tool_call_id.0 == "crashed_call"))
        )
    }));
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0].to_string().contains("recovered content"));
}

#[tokio::test]
async fn restart_never_replays_a_mutating_tool_without_an_idempotency_contract() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(text_turn("reconciled")))
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(1));
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("unsafe-recovery.jsonl"),
        Some(8),
        UnsafeRecoveryTool {
            calls: Arc::clone(&calls),
        },
    );
    agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("perform one irreversible action".into())],
        })))
        .unwrap();
    agent
        .session_mut()
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                id: octet_ai::ToolCallId("possibly_committed".into()),
                name: "unsafe_recovery".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })],
            model: ModelId("scripted".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    agent
        .session()
        .tool_invocation(0)
        .unwrap()
        .replace_partial_output("latest checkpoint: effect outcome not known")
        .unwrap();
    let output = agent.complete("continue after restart").await.unwrap();
    assert_eq!(output.text, "reconciled");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body.to_string().contains("indeterminate after restart"));
    assert!(body
        .to_string()
        .contains("latest checkpoint: effect outcome not known"));
    assert!(body.to_string().contains("external outcome is unknown"));
}

#[tokio::test]
async fn terminal_gate_uses_an_isolated_one_token_decision() {
    let mut h = harness(
        vec![
            text_turn("Completed and verified for the user."),
            text_turn("R"),
        ],
        Some(8),
    )
    .await;
    h.agent
        .set_completion_policy(CompletionPolicy::TerminalGate);

    let output = h.agent.complete("complete autonomously").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(output.text, "Completed and verified for the user.");
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tool| tool["name"] != "finish"));
    assert!(request_has_no_tools(&requests[1]));
    assert_eq!(requests[1]["max_tokens"], 1);
    assert!(!requests[1].to_string().contains("\"name\":\"bash\""));
    assert!(h.agent.session().usage_records().iter().any(|record| {
        matches!(
            record.kind,
            UsageRecordKind::TerminalGate {
                returned: Some(true)
            }
        )
    }));
    assert!(h.agent.session().entries().iter().all(|entry| {
        !matches!(
            &entry.value,
            EntryValue::Message(Message::Assistant(assistant))
                if assistant.content.iter().any(|part| matches!(part, AssistantPart::Text(text) if text == "R"))
        )
    }));
}

#[tokio::test]
async fn terminal_gate_uses_advertised_default_when_reasoning_cannot_be_disabled() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![text_turn("Completed and verified."), text_turn("R")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let model = scripted_model_requiring_reasoning(scripted_model(&server.uri()));
    let mut agent = build_agent_with_reasoning(
        model,
        &sessions.path().join("required-reasoning-gate.jsonl"),
        workspace.path(),
        ReasoningConfig::Effort(octet_ai::ReasoningEffort::High),
        Some(4),
    );
    agent.set_completion_policy(CompletionPolicy::TerminalGate);

    let output = agent.complete("complete autonomously").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["output_config"]["effort"], "high");
    assert_eq!(requests[1]["output_config"]["effort"], "high");
    assert_eq!(requests[1]["max_tokens"], 1);
}

#[tokio::test]
async fn rejected_terminal_candidate_is_retracted_and_work_continues() {
    let mut h = harness(
        vec![
            // This is the critical local-model failure mode: a natural stop
            // while narrating the next action must not end the user prompt.
            text_turn("Let me inspect that now."),
            text_turn("C"),
            tool_turn(&[(
                "read_after_reject",
                "read",
                serde_json::json!({"path": "verification.txt"}),
            )]),
            text_turn("Final answer after verification."),
            text_turn("R"),
        ],
        Some(8),
    )
    .await;
    std::fs::write(h.workspace.join("verification.txt"), "verified\n").unwrap();
    h.agent
        .set_completion_policy(CompletionPolicy::TerminalGate);

    let output = h.agent.complete("complete and verify").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(output.text, "Final answer after verification.");
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 5);
    assert!(request_has_no_tools(&requests[1]));
    assert!(requests[2]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool["name"] == "read"));
    assert!(requests[2]
        .to_string()
        .contains("candidate response was not returnable"));
    assert!(request_has_no_tools(&requests[4]));
    let decisions = h
        .agent
        .session()
        .usage_records()
        .iter()
        .filter_map(|record| match record.kind {
            UsageRecordKind::TerminalGate { returned } => Some(returned),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions, vec![Some(false), Some(true)]);
}

#[tokio::test]
async fn prompt_with_media_persists_media_user_part() {
    use octet_agent::{InputPart, UserInput};
    use octet_ai::{Media, Message, UserPart};

    let mut h = harness(vec![text_turn("seen")], Some(8)).await;
    let input = UserInput::from(vec![
        InputPart::Text("what is in this image?".into()),
        InputPart::Media(Media::image_bytes(
            bytes::Bytes::from_static(&[0x89, 0x50, 0x4e, 0x47]),
            "image/png".parse().unwrap(),
        )),
    ]);
    let mut run = h.agent.prompt(input).await.unwrap();
    let events = collect(&mut run).await;
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    drop(run);

    // The first session entry is the user message with both parts.
    let session = h.agent.session();
    let mut entries = Vec::new();
    let mut cursor = session.head();
    while let Some(id) = cursor {
        let entry = session.entry(&id).unwrap();
        entries.push(entry);
        cursor = entry.parent.clone();
    }
    entries.reverse();
    let EntryValue::Message(Message::User(user)) = &entries[0].value else {
        panic!("first entry is not a user message");
    };
    assert_eq!(user.content.len(), 2);
    assert!(matches!(&user.content[0], UserPart::Text(t) if t == "what is in this image?"));
    assert!(matches!(&user.content[1], UserPart::Media(Media::Image(_))));
}

#[tokio::test]
async fn reasoning_and_text_deltas_use_distinct_channels() {
    let body = msg_start()
        + &thinking_block(0, "pondering deeply")
        + &text_block(1, &["Hello", " world"])
        + &msg_end("end_turn");
    let mut h = harness(vec![body], Some(8)).await;

    let mut run = h.agent.prompt("hi").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    let reasoning: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "pondering deeply");
    assert_eq!(text, "Hello world");
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

#[tokio::test]
async fn one_tool_call_executes_and_persists() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "read", serde_json::json!({"path": "foo.txt"}))]),
            text_turn("done"),
        ],
        Some(8),
    )
    .await;
    std::fs::write(h.workspace.join("foo.txt"), "alpha\nbeta\n").unwrap();

    let mut run = h.agent.prompt("read foo").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    // ToolStarted carries the parsed args; ToolFinished the bounded output.
    let started = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolStarted { name, args, .. } if name == "read" => Some(args.clone()),
            _ => None,
        })
        .expect("ToolStarted for read");
    assert_eq!(started["path"], "foo.txt");
    let finished = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::ToolFinished { result, .. } => Some(result),
            _ => None,
        })
        .expect("ToolFinished");
    let output = finished.as_ref().expect("read must succeed");
    assert!(output.text.contains("1: alpha"), "{}", output.text);
    assert!(output.text.contains("hash="), "{}", output.text);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // Persistence: user, assistant(tool call), tool result, final assistant.
    let session = h.agent.session();
    assert_eq!(session.entries().len(), 4);
    match &events[events.len() - 1] {
        AgentEvent::RunFinished { head, .. } => assert_eq!(session.head(), Some(head.clone())),
        _ => unreachable!(),
    }

    // The second wire request must carry the tool result back to the model.
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    let last_message = requests[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last_message["role"], "user");
    assert_eq!(count_tool_results(last_message), 1);
    assert!(
        requests[1].to_string().contains("1: alpha"),
        "tool output must reach the model"
    );
}

#[tokio::test]
async fn multiple_parallel_safe_tool_calls_start_together_and_coalesce_in_order() {
    let mut h = harness(
        vec![
            tool_turn(&[
                ("call_a", "read", serde_json::json!({"path": "a.txt"})),
                ("call_b", "read", serde_json::json!({"path": "b.txt"})),
            ]),
            text_turn("ok"),
        ],
        Some(8),
    )
    .await;
    std::fs::write(h.workspace.join("a.txt"), "AAA\n").unwrap();
    std::fs::write(h.workspace.join("b.txt"), "BBB\n").unwrap();

    let mut run = h.agent.prompt("read both").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // Both reads start before either finishes; completion and persistence stay
    // deterministic in the model's emitted order.
    let tool_order: Vec<String> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolStarted { id, .. } => Some(format!("start:{}", id.0)),
            AgentEvent::ToolFinished { id, .. } => Some(format!("finish:{}", id.0)),
            _ => None,
        })
        .collect();
    assert_eq!(
        tool_order,
        vec![
            "start:call_a",
            "start:call_b",
            "finish:call_a",
            "finish:call_b"
        ]
    );

    // Each result is its own session entry (user, assistant, 2 results, assistant).
    assert_eq!(h.agent.session().entries().len(), 5);

    // On the wire the two persisted results coalesce into ONE user message.
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    let last_message = requests[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(last_message["role"], "user");
    assert_eq!(count_tool_results(last_message), 2);
}

struct ParallelOverlapProbe {
    active: Arc<AtomicUsize>,
    maximum: Arc<AtomicUsize>,
    effect: ToolEffect,
}

#[async_trait::async_trait]
impl Tool for ParallelOverlapProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "parallel_overlap_probe".into(),
            description: "Records whether independent calls overlap".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.maximum.fetch_max(active, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        self.active.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput::new("observed"))
    }
}

#[tokio::test]
async fn parallel_safe_tool_implementations_really_overlap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[
                    ("call_a", "parallel_overlap_probe", serde_json::json!({})),
                    ("call_b", "parallel_overlap_probe", serde_json::json!({})),
                ]),
                text_turn("done"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let probe = ParallelOverlapProbe {
        active: Arc::clone(&active),
        maximum: Arc::clone(&maximum),
        effect: ToolEffect::Pure,
    };
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("parallel-overlap.jsonl"),
        Some(4),
        probe,
    );

    let output = agent.complete("run both probes").await.unwrap();

    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(maximum.load(Ordering::SeqCst), 2);
    assert_eq!(agent.session().entries().len(), 5);
}

#[tokio::test]
async fn network_classification_overrides_a_parallel_tool_claim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[
                    ("call_a", "parallel_overlap_probe", serde_json::json!({})),
                    ("call_b", "parallel_overlap_probe", serde_json::json!({})),
                ]),
                text_turn("done"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let active = Arc::new(AtomicUsize::new(0));
    let maximum = Arc::new(AtomicUsize::new(0));
    let probe = ParallelOverlapProbe {
        active: Arc::clone(&active),
        maximum: Arc::clone(&maximum),
        effect: ToolEffect::Network,
    };
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("network-effect-sequential.jsonl"),
        Some(4),
        probe,
    );

    let output = agent.complete("run both probes").await.unwrap();

    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(maximum.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn parallel_media_tool_results_precede_the_adjacent_user_message_on_the_wire() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(Script {
            bodies: vec![
                openai_tool_turn(&[
                    ("call_a", "read", serde_json::json!({"path": "a.png"})),
                    ("call_b", "read", serde_json::json!({"path": "b.png"})),
                ]),
                openai_text_turn("both images received"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.png"), b"\x89PNG\r\n\x1a\nfirst").unwrap();
    std::fs::write(workspace.path().join("b.png"), b"\x89PNG\r\n\x1a\nsecond").unwrap();
    let mut agent = build_agent_with_reasoning(
        openai_multimodal_model(&server.uri()),
        &sessions.path().join("parallel-media.jsonl"),
        workspace.path(),
        ReasoningConfig::Off,
        Some(4),
    );

    let output = agent.complete("read both images").await.unwrap();
    assert_eq!(output.text, "both images received");

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    let messages = requests[1]["messages"].as_array().unwrap();
    let tool_call_turn = messages
        .iter()
        .rposition(|message| {
            message["role"] == "assistant"
                && message["tool_calls"]
                    .as_array()
                    .is_some_and(|calls| calls.len() == 2)
        })
        .expect("parallel tool-call turn");
    let follow_up = &messages[tool_call_turn + 1..];
    let roles: Vec<_> = follow_up
        .iter()
        .map(|message| message["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, vec!["tool", "tool", "user"]);
    assert_eq!(follow_up[0]["tool_call_id"], "call_a");
    assert_eq!(follow_up[1]["tool_call_id"], "call_b");
    assert_eq!(
        follow_up[2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|part| part["type"] == "image_url")
            .count(),
        2
    );
}

#[tokio::test]
async fn tool_errors_and_unknown_tools_return_to_the_model() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "no_such_tool", serde_json::json!({}))]),
            tool_turn(&[("call_2", "read", serde_json::json!({"path": "missing.txt"}))]),
            text_turn("recovered"),
        ],
        Some(8),
    )
    .await;

    let output = h.agent.complete("try tools").await.unwrap();
    assert_eq!(output.text, "recovered");
    assert!(matches!(output.reason, FinishReason::Completed));

    // Both failures went back as is_error tool results, not run failures.
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 3);
    let unknown = requests[1]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(unknown["content"][0]["type"], "tool_result");
    assert_eq!(unknown["content"][0]["is_error"], true);
    assert!(
        unknown.to_string().contains("unknown tool: no_such_tool"),
        "{unknown}"
    );
    let failed_read = requests[2]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(failed_read["content"][0]["is_error"], true);
}

#[tokio::test]
async fn steering_enters_at_the_next_turn_boundary() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "read", serde_json::json!({"path": "f.txt"}))]),
            text_turn("steered answer"),
        ],
        Some(8),
    )
    .await;
    std::fs::write(h.workspace.join("f.txt"), "data\n").unwrap();

    h.agent.set_prompt_display_text(Some("start".to_owned()));
    let mut run = h.agent.prompt("start").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        // Queue multiple steers mid-run, right after the tool finishes; the
        // texts must enter together before the *next* model turn.
        if matches!(&event, AgentEvent::ToolFinished { .. }) {
            control.steer("also check the docs").await.unwrap();
            control.steer("and run the tests").await.unwrap();
        }
        events.push(event);
    }
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // The steers are persisted together and included in the second request.
    let delivered = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SteeringDelivered { messages } => Some(messages),
            _ => None,
        })
        .expect("steering delivery event");
    assert_eq!(
        delivered,
        &vec![
            "also check the docs".to_owned(),
            "and run the tests".to_owned()
        ]
    );
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].to_string().contains("also check the docs"));
    assert!(requests[1].to_string().contains("and run the tests"));
    let session_texts = format!("{:?}", h.agent.session().entries());
    assert!(session_texts.contains("also check the docs"));
    assert!(session_texts.contains("and run the tests"));

    // Only the initial composed prompt owns the initial draft's display
    // override. Reopening the durable session must reveal each steer under its
    // own text instead of aliasing all three user entries to "start".
    let reopened = Session::open_read_only(&h.session_path).unwrap();
    let display_texts = reopened
        .entries()
        .iter()
        .filter_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(message))
                if message
                    .content
                    .iter()
                    .any(|part| matches!(part, UserPart::Text(_))) =>
            {
                Some(
                    entry
                        .metadata
                        .as_ref()
                        .and_then(|metadata| metadata.display_text.as_deref()),
                )
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(display_texts, vec![Some("start"), None, None]);
}

#[tokio::test]
async fn prompt_without_tools_exposes_no_tool_schema() {
    let mut h = harness(vec![text_turn("final answer")], Some(4)).await;

    let mut run = h
        .agent
        .prompt_without_tools("answer from existing evidence")
        .await
        .unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .get("tools")
        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
}

#[tokio::test]
async fn finish_now_rejects_calls_from_an_already_open_provider_turn() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "read", serde_json::json!({"path": "f.txt"}))]),
            text_turn("final answer"),
        ],
        Some(4),
    )
    .await;
    std::fs::write(h.workspace.join("f.txt"), "data\n").unwrap();

    let mut run = h.agent.prompt("investigate").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let mut requested = false;
    while let Some(event) = run.next().await {
        if !requested && matches!(&event, AgentEvent::TurnStarted) {
            control
                .finish_now("answer now without more tools")
                .await
                .unwrap();
            requested = true;
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(!requests[0]
        .get("tools")
        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
    assert!(requests[1]
        .get("tools")
        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
    assert!(requests[1]
        .to_string()
        .contains("was not executed: the user requested an immediate final answer"));
}

#[tokio::test]
async fn finish_now_delivers_all_pending_steering_at_the_next_boundary() {
    let mut h = harness(
        vec![text_turn("interim"), text_turn("final answer")],
        Some(4),
    )
    .await;

    let mut run = h.agent.prompt("investigate").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let mut requested = false;
    while let Some(event) = run.next().await {
        if !requested && matches!(&event, AgentEvent::TurnStarted) {
            control
                .set_steering_mode(QueueDeliveryMode::OneAtATime)
                .await
                .unwrap();
            control.steer("first correction").await.unwrap();
            control.steer("second correction").await.unwrap();
            control.finish_now("answer now").await.unwrap();
            requested = true;
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let delivered = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::SteeringDelivered { messages } => Some(messages),
            _ => None,
        })
        .expect("steering delivery event");
    assert_eq!(delivered.len(), 3);
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    let final_request = requests[1].to_string();
    assert!(final_request.contains("first correction"));
    assert!(final_request.contains("second correction"));
    assert!(final_request.contains("answer now"));
    assert!(requests[1]
        .get("tools")
        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
}

#[tokio::test]
async fn finish_now_disables_tools_after_the_current_safe_boundary() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "read", serde_json::json!({"path": "f.txt"}))]),
            text_turn("final answer"),
        ],
        Some(4),
    )
    .await;
    std::fs::write(h.workspace.join("f.txt"), "data\n").unwrap();

    let mut run = h.agent.prompt("investigate").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::ToolFinished { .. }) {
            control
                .finish_now("answer now without more tools")
                .await
                .unwrap();
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1]
        .to_string()
        .contains("answer now without more tools"));
    assert!(requests[1]
        .get("tools")
        .is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty)));
}

#[tokio::test]
async fn follow_up_begins_after_the_run_settles() {
    let mut h = harness(vec![text_turn("first"), text_turn("second")], Some(8)).await;

    let mut run = h.agent.prompt("question one").await.unwrap();
    let control = run.control();
    control.follow_up("question two").await.unwrap();

    let events = collect(&mut run).await;
    drop(run);

    // Two model turns on one run; exactly one RunFinished.
    let turns = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnFinished { .. }))
        .count();
    assert_eq!(turns, 2);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // The follow-up became a persisted user message and the second request
    // contains it (after the first answer).
    assert_eq!(h.agent.session().entries().len(), 4);
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 2);
    assert!(requests[1].to_string().contains("question two"));
    assert!(requests[1].to_string().contains("first"));
}

#[tokio::test]
async fn abort_during_model_streaming_finishes_once_and_preserves_entries() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // A raw server that streams one delta and then stalls forever.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    let server_task = tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = vec![0u8; 16384];
            let mut read = 0;
            loop {
                let n = socket.read(&mut buf[read..]).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                read += n;
                if String::from_utf8_lossy(&buf[..read]).contains("\r\n\r\n") {
                    break;
                }
            }
            let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n";
            socket.write_all(headers.as_bytes()).await.unwrap();
            let partial = msg_start()
                + &frame(
                    "content_block_start",
                    serde_json::json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""}
                    }),
                )
                + &frame(
                    "content_block_delta",
                    serde_json::json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": "partial"}
                    }),
                );
            let chunk = format!("{:x}\r\n{partial}\r\n", partial.len());
            socket.write_all(chunk.as_bytes()).await.unwrap();
            // Stall: never finish the stream.
            tokio::time::sleep(Duration::from_secs(120)).await;
        }
    });

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent(&uri, &workspace, &session_path, Some(8));

    let mut run = agent.prompt("stream forever").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::OutputDelta { .. }) {
            control.abort();
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "abort must not wait for the stalled stream"
    );
    // The incomplete assistant turn was never persisted; the user entry was.
    assert_eq!(agent.session().entries().len(), 1);
    server_task.abort();
}

#[tokio::test]
async fn abort_during_process_execution_kills_the_tool() {
    let mut h = harness(
        vec![
            tool_turn(&[("call_1", "bash", serde_json::json!({"command": "sleep 60"}))]),
            text_turn("never reached"),
        ],
        Some(8),
    )
    .await;

    let mut run = h.agent.prompt("run something slow").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::ToolStarted { .. }) {
            control.abort();
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "abort must cancel the child, not wait for it"
    );
    // Controlled aborts persist an explicit cancellation result so reopening
    // the session does not mistake the deliberate stop for a crash.
    assert_eq!(h.agent.session().entries().len(), 3);
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished {
            result: Err(error), ..
        } if error.message.contains("cancelled by user")
    )));
}

#[tokio::test]
async fn abort_then_new_prompt_does_not_replay_cancelled_tool() {
    let mut h = harness(
        vec![
            tool_turn(&[(
                "edit_1",
                "write",
                serde_json::json!({
                    "path": "must-not-exist.txt",
                    "content": "aborted side effect"
                }),
            )]),
            text_turn("new prompt handled"),
        ],
        Some(8),
    )
    .await;

    let mut run = h.agent.prompt("make the file").await.unwrap();
    let control = run.control();
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::ToolStarted { .. }) {
            control.abort();
        }
    }
    drop(run);
    assert!(!h.workspace.join("must-not-exist.txt").exists());

    let output = h.agent.complete("continue after the abort").await.unwrap();
    assert_eq!(output.text, "new prompt handled");
    assert!(!h.workspace.join("must-not-exist.txt").exists());
}

#[tokio::test]
async fn dropping_run_does_not_replay_an_interrupted_tool() {
    let mut h = harness(
        vec![
            tool_turn(&[(
                "edit_drop",
                "write",
                serde_json::json!({
                    "path": "drop-must-not-run.txt",
                    "content": "dropped side effect"
                }),
            )]),
            text_turn("drop continuation handled"),
        ],
        Some(8),
    )
    .await;
    let mut run = h.agent.prompt("start and then drop").await.unwrap();
    // Consume up to the committed assistant turn. Dropping earlier would
    // suspend the generator at the advisory TurnStarted event, before the
    // provider response (and therefore the durable tool call) exists.
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::TurnFinished { .. }) {
            break;
        }
    }
    drop(run);
    assert!(!h.workspace.join("drop-must-not-run.txt").exists());
    // Run::drop itself must close the durable tool-call boundary. Do not rely
    // on a later prompt or Agent::drop, because the process can die between
    // those operations.
    assert!(h.agent.session().context().unwrap().iter().any(|message| {
        matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(result) if result.tool_call_id.0 == "edit_drop" && result.is_error)))
    }));
    let concurrently_reopened = Session::open(&h.session_path).unwrap();
    assert!(concurrently_reopened.context().unwrap().iter().any(|message| {
        matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(result) if result.tool_call_id.0 == "edit_drop" && result.is_error)))
    }));

    let session_path = h.session_path.clone();
    let server_uri = h.server.as_ref().unwrap().uri();
    let workspace = h.workspace.clone();
    drop(h.agent);
    let reopened = Session::open(&session_path).unwrap();
    assert!(reopened.context().unwrap().iter().any(|message| {
        matches!(message, Message::User(user) if user.content.iter().any(|part| matches!(part, UserPart::ToolResult(result) if result.is_error)))
    }));
    h.agent = build_agent_from_session(
        &server_uri,
        &workspace,
        Session::open(&session_path).unwrap(),
        Some(8),
    );

    let output = h
        .agent
        .complete("continue after dropping the run")
        .await
        .unwrap();
    assert_eq!(output.text, "drop continuation handled");
    assert!(!h.workspace.join("drop-must-not-run.txt").exists());
}

#[tokio::test]
async fn max_turns_terminates_the_run() {
    // The script always answers with another tool call; the guard must stop it.
    let mut h = harness(
        vec![tool_turn(&[(
            "call_loop",
            "read",
            serde_json::json!({"path": "loop.txt"}),
        )])],
        Some(2),
    )
    .await;
    std::fs::write(h.workspace.join("loop.txt"), "again\n").unwrap();

    let mut run = h.agent.prompt("loop forever").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    let turns = events
        .iter()
        .filter(|e| matches!(e, AgentEvent::TurnFinished { .. }))
        .count();
    assert_eq!(turns, 2);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::MaxTurns
    ));
}

#[tokio::test]
async fn duplicate_tool_registration_is_rejected() {
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.load(&CoreTools); // registers every core tool twice

    let result = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model("http://127.0.0.1:1/"),
        session: Session::create(session_dir.path().join("s.jsonl")).unwrap(),
        system: String::new(),
        sandbox: SandboxConfig::new(workspace_dir.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(8),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    });
    match result {
        Err(octet_agent::AgentError::DuplicateTool(name)) => assert_eq!(name, "read"),
        Err(other) => panic!("expected DuplicateTool, got {other}"),
        Ok(_) => panic!("duplicate registration must be rejected"),
    }
}

/// The key end-to-end invariant: create session → prompt scripted model →
/// execute multiple tools → persist every semantic boundary → complete →
/// reopen session → reconstruct equivalent provider context → checkout an
/// ancestor → continue on a new branch.
#[tokio::test]
async fn end_to_end_session_invariant() {
    let mut h = harness(
        vec![
            tool_turn(&[(
                "call_1",
                "write",
                serde_json::json!({
                    "path": "hello.txt",
                    "content": "hello from the agent\n"
                }),
            )]),
            tool_turn(&[("call_2", "read", serde_json::json!({"path": "hello.txt"}))]),
            text_turn("all done"),
            text_turn("branched"),
        ],
        Some(8),
    )
    .await;

    // Run to completion through two tools.
    let output = h
        .agent
        .complete("create then verify hello.txt")
        .await
        .unwrap();
    assert_eq!(output.text, "all done");
    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(
        std::fs::read_to_string(h.workspace.join("hello.txt")).unwrap(),
        "hello from the agent\n"
    );
    // Every semantic boundary persisted: user, assistant, result, assistant,
    // result, assistant.
    assert_eq!(h.agent.session().entries().len(), 6);

    // Reopen the file independently: identical head and equivalent context.
    let reopened = Session::open(&h.session_path).unwrap();
    assert_eq!(reopened.head(), h.agent.session().head());
    let original_ctx = serde_json::to_value(h.agent.session().context().unwrap()).unwrap();
    let reopened_ctx = serde_json::to_value(reopened.context().unwrap()).unwrap();
    assert_eq!(original_ctx, reopened_ctx);

    // Checkout the first user entry and continue: a new branch forms while
    // the old one is preserved.
    let root = EntryId("001".to_string());
    let old_head = h.agent.session().head().unwrap();
    let entries_before = h.agent.session().entries().len();
    h.agent.session_mut().checkout(root.clone()).unwrap();

    let output = h
        .agent
        .complete("take a different direction")
        .await
        .unwrap();
    assert_eq!(output.text, "branched");

    let session = h.agent.session();
    assert_eq!(session.entries().len(), entries_before + 2);
    // The new user entry forks from the checked-out ancestor…
    let new_user = &session.entries()[entries_before];
    assert_eq!(new_user.parent, Some(root));
    // …the old branch is intact and the head moved to the new branch.
    assert!(session.entry(&old_head).is_some());
    assert_ne!(session.head(), Some(old_head));

    // The branched request must not contain the abandoned branch's messages.
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    let branched_request = requests.last().unwrap().to_string();
    assert!(branched_request.contains("take a different direction"));
    // Nothing from the abandoned branch: no assistant turns, no tool traffic.
    assert!(!branched_request.contains("all done"));
    assert!(!branched_request.contains("tool_result"));
    assert!(!branched_request.contains("tool_use"));

    // And the branch survives reopening: original user, branch user, branch
    // assistant — with the abandoned branch absent from the context.
    drop(h.agent);
    let reopened = Session::open(&h.session_path).unwrap();
    let ctx = serde_json::to_value(reopened.context().unwrap()).unwrap();
    assert_eq!(ctx.as_array().unwrap().len(), 3);
    assert_eq!(ctx[2]["Assistant"]["content"][0]["Text"], "branched");
}

// ── Tool-progress integration tests ──────────────────────────────────────

use bytes::Bytes;

/// A tool that sleeps briefly while emitting progress chunks.
struct ProgressTool {
    duration_ms: u64,
    abortable: bool,
}

#[async_trait::async_trait]
impl Tool for ProgressTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "progress_test".to_string(),
            description: "Emits progress and sleeps".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let chunk_count = 20u64;
        for i in 0..chunk_count {
            if self.abortable {
                tokio::time::sleep(Duration::from_millis(self.duration_ms / chunk_count)).await;
            }
            ctx.progress
                .output(OutputStream::Stdout, Bytes::from(format!("chunk{i}\n")));
        }
        Ok(ToolOutput::new("progress_test done"))
    }
}

struct QueuedActivationTool {
    abort_control: Arc<std::sync::Mutex<Option<RunControl>>>,
}

#[async_trait::async_trait]
impl Tool for QueuedActivationTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "queued_activation".into(),
            description: "Queues a semantic activation event".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostMutation)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let append = ctx.append_session_entry(EntryValue::SkillActivated {
            descriptor: octet_agent::SkillDescriptor {
                id: "queued-skill".into(),
                name: "Queued Skill".into(),
                description: "Cancellation regression fixture".into(),
                license: None,
                compatibility: None,
                metadata: Default::default(),
                allowed_tools: vec![],
                disable_model_invocation: false,
                version: None,
                source: octet_agent::SkillSource::BuiltIn,
                trust: octet_agent::SkillTrust::BuiltIn,
                required_tools: vec![],
                tags: vec![],
            },
            instructions_hash: "queued-hash".into(),
            instructions: "must not survive cancellation".into(),
        });
        tokio::pin!(append);
        assert!(futures_util::poll!(&mut append).is_pending());
        self.abort_control
            .lock()
            .unwrap()
            .as_ref()
            .expect("test installs abort control before polling the tool")
            .abort();
        append.await?;
        Ok(ToolOutput::new("activated"))
    }
}

struct LargeOutputTool;

#[async_trait::async_trait]
impl Tool for LargeOutputTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "large_output".into(),
            description: "Returns a large result".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("x".repeat(10_000)))
    }
}

struct RichErrorTool;

#[async_trait::async_trait]
impl Tool for RichErrorTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "rich_error".into(),
            description: "Returns a structured error with supported media".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("rich extension failure")
            .with_media(Media::image_bytes(
                Bytes::from_static(&[0x89, 0x50, 0x4e, 0x47]),
                "image/png".parse().unwrap(),
            ))
            .try_with_details(
                Some(serde_json::json!({"code": "machine-code-sentinel"})),
                Some(serde_json::json!({"trace": "private-trace-sentinel"})),
            )
            .unwrap()
            .with_is_error(true))
    }
}

struct RegisteredToolsProbe {
    observed: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Tool for RegisteredToolsProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "registered_tools_probe".into(),
            description: "Records the final registered tool set".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        *self.observed.lock().unwrap() = ctx.registered_tools.to_vec();
        Ok(ToolOutput::new("recorded"))
    }
}

#[tokio::test]
async fn tool_context_sees_the_exact_post_filter_core_and_extension_set() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[(
                    "call_registered",
                    "registered_tools_probe",
                    serde_json::json!({}),
                )]),
                text_turn("done"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(RegisteredToolsProbe {
        observed: Arc::clone(&observed),
    });
    extensions.retain_tools(|name| matches!(name, "read" | "registered_tools_probe"));
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "test".into(),
        sandbox: SandboxConfig::new(&workspace),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let output = agent.complete("inspect tools").await.unwrap();

    assert!(matches!(output.reason, FinishReason::Completed));
    assert_eq!(
        *observed.lock().unwrap(),
        vec!["read".to_string(), "registered_tools_probe".to_string()]
    );
    assert_eq!(
        agent.registered_tool_names(),
        vec!["read".to_string(), "registered_tools_probe".to_string()]
    );
}

struct CountingRecoveryTool {
    calls: Arc<AtomicUsize>,
    effect: ToolEffect,
}

struct UnsafeRecoveryTool {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for UnsafeRecoveryTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "unsafe_recovery".into(),
            description: "Represents an irreversible external mutation".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::HostMutation)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("mutated"))
    }
}

#[async_trait::async_trait]
impl Tool for CountingRecoveryTool {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "count_recovery".into(),
            description: "Counts crash-recovery executions".into(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("executed"))
    }
}

fn scripted_tool_turn(tool_name: &str) -> String {
    msg_start() + &tool_block(0, "call_p", tool_name, &serde_json::json!({})) + &msg_end("tool_use")
}

fn build_agent_with_extra_tool(
    uri: &str,
    workspace: &Path,
    session_path: &Path,
    max_turns: Option<u64>,
    tool: impl Tool + 'static,
) -> Agent {
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(tool);
    let mut sandbox = SandboxConfig::new(workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(uri),
        session: Session::create(session_path).unwrap(),
        system: "You are a scripted test agent.".to_string(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns,
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap()
}

#[tokio::test]
async fn marked_tool_output_remains_rich_across_lowering_events_and_session_reopen() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_rich_error", "rich_error", serde_json::json!({}))]),
                text_turn("recovered from the tool error"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        &workspace,
        &session_path,
        Some(4),
        RichErrorTool,
    );

    let mut run = agent.prompt("exercise the rich error tool").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let event_output = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolFinished { result, .. } => Some(
                result
                    .as_ref()
                    .expect("a marked output remains Ok at the event boundary"),
            ),
            _ => None,
        })
        .expect("ToolFinished");
    assert!(event_output.is_error());
    assert_eq!(
        event_output.media_kinds(),
        &[octet_agent::ToolOutputMediaKind::Image]
    );
    assert!(
        event_output.media().is_empty(),
        "observer copies must not expose binary payloads"
    );
    assert_eq!(
        event_output.structured_content(),
        Some(&serde_json::json!({"code": "machine-code-sentinel"}))
    );
    assert_eq!(
        event_output.metadata(),
        Some(&serde_json::json!({"trace": "private-trace-sentinel"}))
    );

    let persisted_entry = agent
        .session()
        .entries()
        .iter()
        .find(|entry| {
            matches!(
                &entry.value,
                EntryValue::Message(Message::User(message))
                    if message.content.iter().any(|part| matches!(
                        part,
                        UserPart::ToolResult(result)
                            if result.tool_call_id.0 == "call_rich_error"
                    ))
            )
        })
        .expect("durable rich tool result");
    let persisted_id = persisted_entry.id.clone();
    let EntryValue::Message(Message::User(message)) = &persisted_entry.value else {
        unreachable!();
    };
    let persisted_result = message
        .content
        .iter()
        .find_map(|part| match part {
            UserPart::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("canonical tool result");
    assert!(persisted_result.is_error);
    assert!(persisted_result
        .content
        .iter()
        .any(|part| matches!(part, octet_ai::ToolResultPart::Media(Media::Image(_)))));
    let details = persisted_entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.tool_output.as_ref())
        .expect("durable tool output details")
        .clone();
    assert_eq!(
        details.structured_content(),
        Some(&serde_json::json!({"code": "machine-code-sentinel"}))
    );
    assert_eq!(
        details.metadata(),
        Some(&serde_json::json!({"trace": "private-trace-sentinel"}))
    );

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    let replay = requests[1].to_string();
    assert!(replay.contains("rich extension failure"));
    assert!(replay.contains("image"));
    assert!(!replay.contains("machine-code-sentinel"));
    assert!(!replay.contains("private-trace-sentinel"));

    drop(agent);
    let reopened = Session::open(&session_path).unwrap();
    let reopened_entry = reopened
        .entry(&persisted_id)
        .expect("rich result survives session reopen");
    let reopened_details = reopened_entry
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.tool_output.as_ref())
        .expect("rich details survive session reopen");
    assert_eq!(reopened_details, &details);
    let EntryValue::Message(Message::User(reopened_message)) = &reopened_entry.value else {
        panic!("reopened rich result must remain a user tool-result message");
    };
    let reopened_result = reopened_message
        .content
        .iter()
        .find_map(|part| match part {
            UserPart::ToolResult(result) => Some(result),
            _ => None,
        })
        .expect("reopened canonical tool result");
    assert!(reopened_result.is_error);
    assert!(reopened_result
        .content
        .iter()
        .any(|part| matches!(part, octet_ai::ToolResultPart::Media(Media::Image(_)))));
}


fn assert_bounded_invocation_batch(path: &Path, expected_calls: usize, prefix: &str) {
    let records = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<octet_agent::SessionRecord>(line).unwrap())
        .collect::<Vec<_>>();
    let scopes = records
        .iter()
        .filter_map(|record| match record {
            octet_agent::SessionRecord::ToolInvocation { scope, .. } => Some(scope),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(!scopes.is_empty());
    assert!(
        scopes
            .iter()
            .all(|scope| scope.invocation_id().parse::<usize>().unwrap() < 32),
        "refused calls must not allocate live-effect slots"
    );
    let reopened = Session::open_read_only(path).unwrap();
    let results = reopened
        .entries()
        .iter()
        .flat_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(user)) => user
                .content
                .iter()
                .filter_map(|part| match part {
                    UserPart::ToolResult(result) => Some(result),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), expected_calls);
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result.tool_call_id.0, format!("{prefix}{index}"));
        assert_eq!(result.is_error, index >= 32);
    }
}

#[tokio::test]
async fn crash_recovery_preserves_the_live_tool_call_execution_cap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![text_turn("recovered")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("recovery-cap.jsonl");
    let mut session = Session::create(&session_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("persisted request".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: (0..65)
                .map(|index| {
                    AssistantPart::ToolCall(ToolCall {
                        id: octet_ai::ToolCallId(format!("recover-{index}")),
                        name: "count_recovery".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    })
                })
                .collect(),
            model: ModelId("scripted".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(CountingRecoveryTool {
        calls: Arc::clone(&calls),
        effect: ToolEffect::Pure,
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_edit = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session,
        system: "test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(2),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let output = agent.complete("continue").await.unwrap();
    assert_eq!(output.text, "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 32);
    let results = agent
        .session()
        .entries()
        .iter()
        .filter_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(user)) => user.content.iter().find_map(|part| {
                let UserPart::ToolResult(result) = part else {
                    return None;
                };
                Some(result)
            }),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 65);
    for index in 32..65 {
        let id = format!("recover-{index}");
        let result = results
            .iter()
            .find(|result| result.tool_call_id.0 == id)
            .unwrap();
        assert!(result.is_error);
        assert!(matches!(
            result.content.first(),
            Some(octet_ai::ToolResultPart::Text(text)) if text.contains("per-turn tool-call limit")
        ));
    }
    assert_bounded_invocation_batch(&session_path, 65, "recover-");
}

#[tokio::test]
async fn host_classification_overrides_a_safe_replay_claim() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(ResponseTemplate::new(200).set_body_string(text_turn("reconciled")))
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let mut session =
        Session::create(session_dir.path().join("classified-recovery.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("prior request".into())],
        })))
        .unwrap();
    session
        .append(EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::ToolCall(ToolCall {
                id: octet_ai::ToolCallId("classified-recovery".into()),
                name: "count_recovery".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            })],
            model: ModelId("scripted".into()),
            protocol: Protocol::AnthropicMessages,
        })))
        .unwrap();

    let calls = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool(CountingRecoveryTool {
        calls: Arc::clone(&calls),
        effect: ToolEffect::HostRead,
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session,
        system: "test".into(),
        sandbox: SandboxConfig::new(&workspace),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(2),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    agent
        .session()
        .tool_invocation(0)
        .unwrap()
        .replace_partial_output("checkpoint from host-classified observation")
        .unwrap();
    let output = agent.complete("continue").await.unwrap();

    assert_eq!(output.text, "reconciled");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let requests = server.received_requests().await.unwrap();
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(body
        .to_string()
        .contains("did not replay this host-classified effect"));
    assert!(body
        .to_string()
        .contains("checkpoint from host-classified observation"));
    assert!(body.to_string().contains("external outcome is unknown"));
}

#[tokio::test]
async fn batched_tool_results_keep_independent_bounded_outputs() {
    let h = harness(
        vec![
            tool_turn(&[
                ("large_a", "large_output", serde_json::json!({})),
                ("large_b", "large_output", serde_json::json!({})),
                ("large_c", "large_output", serde_json::json!({})),
            ]),
            text_turn("done"),
        ],
        Some(8),
    )
    .await;
    // Use a separate session file for the extension-enabled agent while
    // reusing the scripted server.
    let session_path = h.session_path.with_file_name("large-output.jsonl");
    let server_uri = h.server.as_ref().unwrap().uri();
    let mut agent = build_agent_with_extra_tool(
        &server_uri,
        &h.workspace,
        &session_path,
        Some(8),
        LargeOutputTool,
    );
    let output = agent.complete("produce bounded output").await.unwrap();
    assert!(matches!(output.reason, FinishReason::Completed));
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    let results = requests[1]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_array()
        .unwrap();
    let text_bytes: usize = results
        .iter()
        .map(|result| result["content"][0]["text"].as_str().unwrap().len())
        .sum();
    assert_eq!(text_bytes, 30_000);
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|result| {
        result["content"][0]["text"]
            .as_str()
            .is_some_and(|text| text.len() == 10_000)
    }));
}

#[tokio::test]
async fn repeated_tool_call_diagnostic_reaches_a_later_model_turn() {
    let mut h = harness(
        vec![
            tool_turn(&[("repeat-1", "read", serde_json::json!({"path": "missing"}))]),
            tool_turn(&[("repeat-2", "read", serde_json::json!({"path": "missing"}))]),
            tool_turn(&[("repeat-3", "read", serde_json::json!({"path": "missing"}))]),
            text_turn("done"),
        ],
        Some(8),
    )
    .await;

    let output = h.agent.complete("make progress").await.unwrap();
    assert_eq!(output.text, "done");
    let requests = wire_requests(h.server.as_ref().unwrap()).await;
    assert!(requests[3].to_string().contains("exact call repeated 3x"));
}

#[tokio::test]
async fn websocket_connection_limit_is_retried_by_agent() {
    let server = ResponsesConnectionLimitServer::start().await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let mut model = scripted_responses_model(&server.base_url);
    let mut endpoint = (*model.endpoint).clone();
    endpoint.transport = octet_ai::EndpointTransport::WebSocketPreferred;
    model.endpoint = Arc::new(endpoint);
    let mut agent = build_responses_agent_from_session(
        model,
        Session::create(&session_path).unwrap(),
        &workspace,
        Some(4),
        "You are a scripted Responses test agent.",
        ReasoningConfig::Off,
    );

    let output = tokio::time::timeout(
        Duration::from_secs(15),
        agent.complete("continue after the socket refresh"),
    )
    .await
    .expect("socket refresh recovery must remain bounded")
    .unwrap();
    assert_eq!(output.text, "recovered");
    assert_eq!(server.websocket_requests.load(Ordering::SeqCst), 1);
    assert_eq!(server.http_requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn tool_progress_events_arrive_between_start_and_finish() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![scripted_tool_turn("progress_test"), text_turn("done")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");

    let tool = ProgressTool {
        duration_ms: 200,
        abortable: true,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test progress").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // Verify ordering: ToolStarted first, then ToolProgress*, then ToolFinished.
    let mut saw_start = false;
    let mut saw_progress = false;
    let mut saw_finish = false;
    let mut last_was_finish = false;
    for event in &events {
        match event {
            AgentEvent::ToolStarted { name, .. } if name == "progress_test" => {
                assert!(!saw_start, "duplicate ToolStarted");
                assert!(!saw_finish);
                saw_start = true;
            }
            AgentEvent::ToolProgress { .. } => {
                assert!(saw_start, "ToolProgress before ToolStarted");
                assert!(!saw_finish, "ToolProgress after ToolFinished");
                assert!(!last_was_finish);
                saw_progress = true;
            }
            AgentEvent::ToolFinished { id, .. } if id.0 == "call_p" => {
                assert!(saw_start, "ToolFinished before ToolStarted");
                assert!(!saw_finish, "duplicate ToolFinished");
                saw_finish = true;
                last_was_finish = true;
            }
            _ => {}
        }
    }
    assert!(saw_start);
    assert!(saw_progress);
    assert!(saw_finish);
}

#[tokio::test]
async fn abort_before_completion_persists_cancellation_result() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![scripted_tool_turn("progress_test"), text_turn("never")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");

    let tool = ProgressTool {
        duration_ms: 5000, // long enough to abort
        abortable: true,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test abort").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let started = std::time::Instant::now();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::ToolStarted { name, .. } if name == "progress_test") {
            control.abort();
        }
        events.push(event);
    }
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "abort must cancel, not wait"
    );
    // The controlled abort is itself reported and persisted as a result.
    assert!(events.iter().any(|e| matches!(
        e,
        AgentEvent::ToolFinished {
            id,
            result: Err(error),
            ..
        } if id.0 == "call_p" && error.message.contains("cancelled by user")
    )));
    // One cancellation result persisted.
    let session_entries = agent.session().entries().to_vec();
    let result_count = session_entries
        .iter()
        .filter(|e| {
            matches!(&e.value, EntryValue::Message(
            octet_ai::Message::User(octet_ai::UserMessage { content, .. })
        ) if content.iter().any(|p| matches!(p, octet_ai::UserPart::ToolResult(_))))
        })
        .count();
    assert_eq!(
        result_count, 1,
        "controlled abort must persist cancellation"
    );
}

#[tokio::test]
async fn cancellation_discards_a_queued_semantic_session_event() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![scripted_tool_turn("queued_activation"), text_turn("never")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let abort_control = Arc::new(std::sync::Mutex::new(None));
    let tool = QueuedActivationTool {
        abort_control: abort_control.clone(),
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test semantic cancellation").await.unwrap();
    *abort_control.lock().unwrap() = Some(run.control());
    let events = collect(&mut run).await;
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert!(!agent
        .session()
        .entries()
        .iter()
        .any(|entry| matches!(entry.value, EntryValue::SkillActivated { .. })));
}

#[tokio::test]
async fn abort_after_completion_preserves_result_and_emits_finished() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_r", "read", serde_json::json!({"path": "f.txt"}))]),
                scripted_tool_turn("progress_test"),
                text_turn("never_reached"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    std::fs::write(workspace.join("f.txt"), "data\n").unwrap();

    let tool = ProgressTool {
        duration_ms: 100, // fast: completes before abort
        abortable: true,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test post-completion abort").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        // Abort AFTER the progress tool's ToolFinished.
        if matches!(&event, AgentEvent::ToolFinished { id, .. } if id.0 == "call_p") {
            control.abort();
        }
        events.push(event);
    }
    drop(run);

    // Run ends as Aborted (no subsequent model turn).
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    // But the progress_test tool DID finish before abort.
    let progress_finished = events.iter().any(|e| {
        matches!(
            e,
            AgentEvent::ToolFinished { id, .. } if id.0 == "call_p"
        )
    });
    assert!(
        progress_finished,
        "ToolFinished must be emitted before abort stops the run"
    );
    // And its result was persisted.
    let session_entries = agent.session().entries().to_vec();
    let has_progress_result = session_entries.iter().any(|e| {
        matches!(&e.value, EntryValue::Message(_))
            && format!("{:?}", e).contains("progress_test done")
    });
    assert!(has_progress_result, "committed result must survive abort");
    // No subsequent model request was made.
    let requests = wire_requests(&server).await;
    assert_eq!(
        requests.len(),
        2,
        "third request (never_reached) must not fire"
    );
}

#[tokio::test]
async fn steer_arrives_during_continuous_progress() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![scripted_tool_turn("progress_test"), text_turn("steered")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");

    let tool = ProgressTool {
        duration_ms: 500, // enough time for steer to arrive
        abortable: true,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test steer").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(&event, AgentEvent::ToolStarted { name, .. } if name == "progress_test") {
            // Send steer during tool execution.
            control.steer("redirect").await.unwrap();
        }
        events.push(event);
    }
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    // The steered text must have been persisted and sent in the second request.
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].to_string().contains("redirect"),
        "steer must reach the model"
    );
}

#[tokio::test]
async fn progress_never_persisted_in_session() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![scripted_tool_turn("progress_test"), text_turn("ok")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");

    let tool = ProgressTool {
        duration_ms: 50,
        abortable: false,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    agent.complete("test").await.unwrap();

    // Read the raw session file — no progress keywords.
    let raw = std::fs::read_to_string(&session_path).unwrap();
    assert!(!raw.contains("ToolProgress"));
    assert!(!raw.contains("chunk"));
    assert!(raw.contains("progress_test done"));
}

#[tokio::test]
async fn multiple_tools_have_isolated_progress() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[
                    ("call_a", "progress_test", serde_json::json!({})),
                    ("call_b", "progress_test", serde_json::json!({})),
                ]),
                text_turn("all_done"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");

    let tool = ProgressTool {
        duration_ms: 100,
        abortable: false,
    };
    let mut agent =
        build_agent_with_extra_tool(&server.uri(), &workspace, &session_path, Some(8), tool);

    let mut run = agent.prompt("test isolation").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // All ToolProgress events for call_a must appear before ToolStarted for call_b.
    let mut call_a_done = false;
    for event in &events {
        match event {
            AgentEvent::ToolStarted { id, .. } if id.0 == "call_b" => {
                call_a_done = true;
            }
            AgentEvent::ToolProgress { id, .. } if id.0 == "call_a" => {
                assert!(!call_a_done, "call_a progress after call_b started");
            }
            AgentEvent::ToolProgress { id, .. } if id.0 == "call_b" => {
                assert!(call_a_done, "call_b progress before call_b ToolStarted");
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn torn_session_and_crash_recovery_still_green() {
    // Sanity: the session invariants still hold with progress infrastructure.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");

    let mut s = Session::create(&path).unwrap();
    let e1 = s
        .append(EntryValue::Message(octet_ai::Message::User(
            octet_ai::UserMessage {
                content: vec![octet_ai::UserPart::Text("hello".to_string())],
            },
        )))
        .unwrap();
    drop(s);

    let reopened = Session::open(&path).unwrap();
    assert_eq!(reopened.head(), Some(e1));
}

// ── Reasoning configuration ───────────────────────────────────────────────
//
// `Agent::prompt` must thread the configured `ReasoningConfig` into every
// `octet_ai::Request` instead of hardcoding `ReasoningConfig::Off`. These tests
// pin that behavior end-to-end against the real request-build + SSE path.

#[tokio::test]
async fn reasoning_off_sends_explicit_disabled_thinking() {
    // Off must override a thinking-capable provider's default explicitly,
    // without an enabled budget or inferred Minimal selection.
    let (mut agent, server, _path, _dirs) =
        reasoning_harness(vec![text_turn("hi")], ReasoningConfig::Off, true).await;

    let output = agent.complete("hello").await.unwrap();
    assert_eq!(output.text, "hi");

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["thinking"],
        serde_json::json!({"type": "disabled"})
    );
}

#[tokio::test]
async fn non_off_reasoning_reaches_the_provider_request() {
    let (mut agent, server, _path, _dirs) =
        reasoning_harness(vec![text_turn("hi")], ReasoningConfig::Budget(2048), true).await;

    agent.complete("hello").await.unwrap();

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1);
    let thinking = requests[0]
        .get("thinking")
        .expect("reasoning budget must reach the wire");
    assert_eq!(thinking["type"], "enabled");
    assert_eq!(thinking["budget_tokens"], 2048);
}

#[tokio::test]
async fn provider_output_ceiling_is_not_replaced_by_reasoning_reserve() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![text_turn("hi")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("large-reasoning.jsonl");
    let base = scripted_model_with_reasoning(&server.uri());
    let mut spec = (*base.spec).clone();
    spec.limits.max_output_tokens = 65_536;
    let capability = spec.capabilities.reasoning.as_mut().unwrap();
    capability.effort_budgets = Some(ReasoningEffortBudgets {
        minimal: 1024,
        low: 2048,
        medium: 4096,
        high: 32_768,
        xhigh: 32_768,
        max: 32_768,
    });
    let model = Model {
        spec: Arc::new(spec),
        endpoint: base.endpoint,
    };
    let mut agent = build_agent_with_reasoning(
        model,
        &session_path,
        &workspace,
        ReasoningConfig::Budget(32_768),
        Some(2),
    );

    agent.complete("hello").await.unwrap();
    let requests = wire_requests(&server).await;
    assert_eq!(requests[0]["thinking"]["budget_tokens"], 32_768);
    assert_eq!(requests[0]["max_tokens"], 65_536);
}

#[tokio::test]
async fn reasoning_deltas_stream_on_the_reasoning_channel_when_enabled() {
    let body = msg_start()
        + &thinking_block(0, "weighing options")
        + &text_block(1, &["Answer", " here"])
        + &msg_end("end_turn");
    let (mut agent, _server, _path, _dirs) =
        reasoning_harness(vec![body], ReasoningConfig::Budget(2048), true).await;

    let mut run = agent.prompt("go").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    let reasoning: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, "weighing options");
    assert_eq!(text, "Answer here");

    // Ordering: reasoning deltas precede the text deltas.
    let first_reasoning = events.iter().position(|e| {
        matches!(
            e,
            AgentEvent::OutputDelta {
                channel: OutputChannel::Reasoning,
                ..
            }
        )
    });
    let first_text = events.iter().position(|e| {
        matches!(
            e,
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                ..
            }
        )
    });
    assert!(first_reasoning < first_text, "reasoning must precede text");
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

#[tokio::test]
async fn complete_with_reasoning_enabled_returns_only_visible_text() {
    let body = msg_start()
        + &thinking_block(0, "internal deliberation")
        + &text_block(1, &["final answer"])
        + &msg_end("end_turn");
    let (mut agent, _server, _path, _dirs) =
        reasoning_harness(vec![body], ReasoningConfig::Budget(4096), true).await;

    let output = agent.complete("question").await.unwrap();
    // Reasoning text must not leak into the aggregate visible text.
    assert_eq!(output.text, "final answer");
    assert!(matches!(output.reason, FinishReason::Completed));
}

#[tokio::test]
async fn unsupported_reasoning_fails_through_octet_ai_validation() {
    // A model WITHOUT a reasoning capability plus a non-off config must fail via
    // `octet-ai`'s strict validation, surfacing as a failed run — never silently
    // disabled.
    let (mut agent, server, _path, _dirs) = reasoning_harness(
        vec![text_turn("unused")],
        ReasoningConfig::Budget(2048),
        false,
    )
    .await;

    let mut run = agent.prompt("go").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Failed(_)),
        "unsupported reasoning must fail the run, not be silently dropped"
    );
    // No provider request should have been accepted (validation fails pre-send).
    let requests = wire_requests(&server).await;
    assert!(
        requests.is_empty(),
        "request must be rejected before send: {requests:?}"
    );
    // No provider-produced assistant turn was persisted, but the failed user
    // turn is closed by the durable synthetic boundary used on the next run.
    assert_eq!(agent.session().entries().len(), 2);
    assert!(matches!(
        &agent.session().entries()[1].value,
        EntryValue::Message(Message::Assistant(message))
            if message.content.iter().any(|part| matches!(
                part,
                AssistantPart::Text(text) if text.contains("failed before completion")
            ))
    ));
}

#[tokio::test]
async fn complete_surfaces_unsupported_reasoning_as_err() {
    let (mut agent, _server, _path, _dirs) = reasoning_harness(
        vec![text_turn("unused")],
        ReasoningConfig::Budget(2048),
        false,
    )
    .await;

    let result = agent.complete("go").await;
    assert!(
        matches!(result, Err(octet_agent::AgentError::Ai(_))),
        "complete must return the octet-ai error, got {result:?}"
    );
}

struct ClassifiedEffectProbe {
    name: &'static str,
    effect: ToolEffect,
    concurrency: ToolConcurrency,
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for ClassifiedEffectProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: self.name.to_owned(),
            description: "effect admission probe".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }

    fn concurrency(&self) -> ToolConcurrency {
        self.concurrency
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("probe executed"))
    }
}

struct SchemaMismatchBashProbe {
    effect_calls: Arc<AtomicUsize>,
    executions: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for SchemaMismatchBashProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "bash".into(),
            description: "Records schema-rejected Bash calls".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "enum": ["pwd"]}
                },
                "required": ["command"],
                "additionalProperties": false,
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        self.effect_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new("must not execute"))
    }
}

struct AdmissionHookProbe {
    before: Arc<AtomicUsize>,
    after: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl ToolCallHook for AdmissionHookProbe {
    async fn before_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        self.before.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    async fn after_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _output: &str,
        _is_error: bool,
        _context: &ToolContext<'_>,
    ) {
        self.after.fetch_add(1, Ordering::SeqCst);
    }
}

struct DenyingAdmissionHook {
    before: Arc<AtomicUsize>,
    after: Arc<AtomicUsize>,
}

const HOOK_DENIAL_SECRET: &str = "secondary-hook-secret-marker";

#[async_trait::async_trait]
impl ToolCallHook for DenyingAdmissionHook {
    async fn before_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _context: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        self.before.fetch_add(1, Ordering::SeqCst);
        Err(ToolError::new(HOOK_DENIAL_SECRET))
    }

    async fn after_tool_call(
        &self,
        _name: &str,
        _arguments: &serde_json::Value,
        _output: &str,
        _is_error: bool,
        _context: &ToolContext<'_>,
    ) {
        self.after.fetch_add(1, Ordering::SeqCst);
    }
}

async fn assert_secondary_hook_denials(parallel: bool) {
    let server = MockServer::start().await;
    let calls = if parallel {
        vec![
            ("call_parallel_one", "pure_probe", serde_json::json!({})),
            ("call_parallel_two", "pure_probe", serde_json::json!({})),
        ]
    } else {
        vec![("call_serial", "pure_probe", serde_json::json!({}))]
    };
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![tool_turn(&calls), text_turn("hook denial observed")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.tool(ClassifiedEffectProbe {
        name: "pure_probe",
        effect: ToolEffect::Pure,
        concurrency: if parallel {
            ToolConcurrency::Parallel
        } else {
            ToolConcurrency::Sequential
        },
        executions: Arc::clone(&executions),
    });
    extensions.tool_call_hook(DenyingAdmissionHook {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "secondary admission hook test".into(),
        sandbox: SandboxConfig::new(workspace_dir.path()),
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("attempt probes").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    for (id, _, _) in &calls {
        let decision = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolPolicyDecision {
                    id: event_id,
                    decision,
                    ..
                } if event_id.0 == *id => Some(decision),
                _ => None,
            })
            .expect("secondary hook denial must be policy-visible");
        assert_eq!(decision.effect, Some(ToolEffect::Pure));
        assert!(!decision.allowed);
        assert_eq!(decision.authorization, None);
        assert_eq!(
            decision.denial_code,
            Some(ToolPolicyDenialCode::SecondaryHookDenied)
        );
        assert!(!serde_json::to_string(decision)
            .unwrap()
            .contains(HOOK_DENIAL_SECRET));
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ToolFinished { id: event_id, result: Err(error), .. }
                if event_id.0 == *id
                    && error.message == "tool call denied by host policy"
                    && !error.message.contains(HOOK_DENIAL_SECRET)
        )));
        let started = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolStarted { id: event_id, .. } if event_id.0 == *id))
            .unwrap();
        let decided = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolPolicyDecision { id: event_id, .. } if event_id.0 == *id))
            .unwrap();
        let finished = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolFinished { id: event_id, .. } if event_id.0 == *id))
            .unwrap();
        assert!(started < decided && decided < finished);
    }
    assert_eq!(before.load(Ordering::SeqCst), calls.len());
    assert_eq!(after.load(Ordering::SeqCst), 0);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn secondary_hook_denials_are_policy_visible_in_serial_and_parallel_paths() {
    assert_secondary_hook_denials(false).await;
    assert_secondary_hook_denials(true).await;
}

const SCHEMA_MISMATCH_ERROR: &str =
    "tool call was not executed because its arguments do not satisfy the advertised schema; correct the arguments and try again";

#[tokio::test]
async fn schema_rejected_bash_is_never_classified_or_executed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            // Even a read-looking command must satisfy the request schema,
            // which accepts only `pwd`, before classification or dispatch.
            bodies: vec![
                tool_turn(&[(
                    "schema_rejected_bash",
                    "bash",
                    serde_json::json!({"command": "ls"}),
                )]),
                text_turn("schema error received"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let effect_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.tool(SchemaMismatchBashProbe {
        effect_calls: Arc::clone(&effect_calls),
        executions: Arc::clone(&executions),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "schema rejection test".into(),
        sandbox: SandboxConfig::new(workspace_dir.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("try the rejected bash call").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let error = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolFinished {
                id,
                result: Err(error),
                ..
            } if id.0 == "schema_rejected_bash" => Some(error.message.as_str()),
            _ => None,
        })
        .expect("schema rejection is surfaced as a tool error");
    assert_eq!(error, SCHEMA_MISMATCH_ERROR);
    assert!(!error.contains("ls"));
    let decision = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolPolicyDecision { id, decision, .. }
                if id.0 == "schema_rejected_bash" =>
            {
                Some(decision)
            }
            _ => None,
        })
        .expect("schema rejection must be policy-visible");
    assert_eq!(decision.effect, None);
    assert!(!decision.allowed);
    assert_eq!(decision.authorization, None);
    assert_eq!(
        decision.denial_code,
        Some(ToolPolicyDenialCode::InvalidToolArguments)
    );
    assert!(!serde_json::to_string(decision)
        .unwrap()
        .contains(r#""command":"ls""#));
    let started = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolStarted { id, .. } if id.0 == "schema_rejected_bash"))
        .expect("ToolStarted before schema rejection decision");
    let decided = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolPolicyDecision { id, .. } if id.0 == "schema_rejected_bash"))
        .expect("ToolPolicyDecision for schema rejection");
    let finished = events
        .iter()
        .position(|event| matches!(event, AgentEvent::ToolFinished { id, .. } if id.0 == "schema_rejected_bash"))
        .expect("ToolFinished after schema rejection decision");
    assert!(started < decided && decided < finished);
    assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
    assert_eq!(executions.load(Ordering::SeqCst), 0);

    let call = agent
        .session()
        .entries()
        .iter()
        .find_map(|entry| match &entry.value {
            EntryValue::Message(Message::Assistant(message)) => {
                message.content.iter().find_map(|part| match part {
                    AssistantPart::ToolCall(call) if call.id.0 == "schema_rejected_bash" => {
                        Some(call)
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("schema-rejected call is durable");
    assert_eq!(call.arguments_json, r#"{"command":"ls"}"#);
    assert_eq!(
        call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );
    let result = agent
        .session()
        .entries()
        .iter()
        .find_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(message)) => {
                message.content.iter().find_map(|part| match part {
                    UserPart::ToolResult(result)
                        if result.tool_call_id.0 == "schema_rejected_bash" =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("paired rejection result is durable");
    assert!(result.is_error);
    let text = result
        .content
        .iter()
        .find_map(|part| match part {
            octet_ai::ToolResultPart::Text(text) => Some(text),
            _ => None,
        })
        .expect("static error text");
    assert_eq!(text, SCHEMA_MISMATCH_ERROR);

    let requests = wire_requests(&server).await;
    assert_eq!(
        requests.len(),
        2,
        "one rejected-tool turn and one corrective turn"
    );
    assert!(requests[1].to_string().contains(SCHEMA_MISMATCH_ERROR));
}

#[tokio::test]
async fn resumed_schema_rejection_skips_hooks_effects_and_replay() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![text_turn("resumed after schema rejection")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    {
        let mut session = Session::create(&session_path).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("prior request".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::ToolCall(ToolCall {
                    id: octet_ai::ToolCallId("persisted_schema_rejection".into()),
                    name: "bash".into(),
                    arguments_json: r#"{"command":"provider-secret-value"}"#.into(),
                    argument_error: Some(ToolCallArgumentError::SchemaMismatch),
                })],
                model: ModelId("scripted".into()),
                protocol: Protocol::AnthropicMessages,
            })))
            .unwrap();
    }
    let session = Session::open(&session_path).unwrap();
    let persisted_call = session
        .entries()
        .iter()
        .find_map(|entry| match &entry.value {
            EntryValue::Message(Message::Assistant(message)) => {
                message.content.iter().find_map(|part| match part {
                    AssistantPart::ToolCall(call) => Some(call),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("reopened call");
    assert_eq!(
        persisted_call.argument_error,
        Some(ToolCallArgumentError::SchemaMismatch)
    );

    let effect_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.tool(SchemaMismatchBashProbe {
        effect_calls: Arc::clone(&effect_calls),
        executions: Arc::clone(&executions),
    });
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session,
        system: "schema rejection resume test".into(),
        sandbox: SandboxConfig::new(workspace_dir.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("continue after restart").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
    assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(before.load(Ordering::SeqCst), 0);
    assert_eq!(after.load(Ordering::SeqCst), 0);

    let result = agent
        .session()
        .entries()
        .iter()
        .find_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(message)) => {
                message.content.iter().find_map(|part| match part {
                    UserPart::ToolResult(result)
                        if result.tool_call_id.0 == "persisted_schema_rejection" =>
                    {
                        Some(result)
                    }
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("recovered schema error result");
    assert!(result.is_error);
    let text = result
        .content
        .iter()
        .find_map(|part| match part {
            octet_ai::ToolResultPart::Text(text) => Some(text),
            _ => None,
        })
        .expect("static error text");
    assert_eq!(text, SCHEMA_MISMATCH_ERROR);
    assert!(!text.contains("provider-secret-value"));

    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1, "recovery must not replay the old POST");
    assert!(requests[0].to_string().contains(SCHEMA_MISMATCH_ERROR));
}

#[tokio::test]
async fn controlled_effects_are_denied_before_hooks_or_execution() {
    let server = MockServer::start().await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let external_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let external_file = external_dir.path().join("host-secret.txt");
    std::fs::write(&external_file, "host secret").unwrap();
    let external_write = external_dir.path().join("must-not-exist.txt");
    let bash_marker = workspace.join("bash-must-not-run.txt");

    let calls = vec![
        (
            "call_host_read",
            "read",
            serde_json::json!({"path": external_file}),
        ),
        (
            "call_network",
            "read",
            serde_json::json!({"path": "https://example.com/image.png"}),
        ),
        (
            "call_search",
            "search",
            serde_json::json!({"query": "secret"}),
        ),
        (
            "call_process",
            "bash",
            serde_json::json!({"command": format!("printf ran > {}", bash_marker.display())}),
        ),
        (
            "call_host_mutation",
            "write",
            serde_json::json!({"path": external_write, "content": "forbidden"}),
        ),
        ("call_delegation", "delegation_probe", serde_json::json!({})),
        ("call_extension", "extension_probe", serde_json::json!({})),
        ("call_unknown", "unknown_probe", serde_json::json!({})),
    ];
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![tool_turn(&calls), text_turn("denials observed")],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let executions = Arc::new(AtomicUsize::new(0));
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    for (name, effect) in [
        ("delegation_probe", ToolEffect::Delegation),
        ("extension_probe", ToolEffect::Extension),
        ("unknown_probe", ToolEffect::Unknown),
    ] {
        extensions.tool(ClassifiedEffectProbe {
            name,
            effect,
            concurrency: ToolConcurrency::Sequential,
            executions: Arc::clone(&executions),
        });
    }
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_external_paths = true;
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    sandbox.allow_remote_read = true;
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "effect admission test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("try every denied effect").await.unwrap();
    let mut events = Vec::new();
    let mut approvals = 0usize;
    while let Some(event) = run.next().await {
        if let AgentEvent::ToolProgress {
            id,
            progress: octet_agent::ToolProgress::Confirmation(request),
            ..
        } = &event
        {
            if id.0 == "call_process" {
                approvals += 1;
                request.clone().respond(true);
            }
        }
        events.push(event);
    }
    drop(run);

    let denied = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished {
                id,
                result: Err(error),
                ..
            } => Some((id.0.as_str(), error.message.as_str())),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    let succeeded = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished {
                id, result: Ok(_), ..
            } => Some(id.0.as_str()),
            _ => None,
        })
        .collect::<std::collections::HashSet<_>>();
    let decisions = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolPolicyDecision { id, decision, .. } => Some((id.0.as_str(), decision)),
            _ => None,
        })
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(decisions.len(), calls.len());
    for (id, effect, denial_code) in [
        (
            "call_host_read",
            ToolEffect::HostRead,
            ToolPolicyDenialCode::EffectHostReadDenied,
        ),
        (
            "call_network",
            ToolEffect::Network,
            ToolPolicyDenialCode::EffectNetworkDenied,
        ),
        (
            "call_search",
            ToolEffect::HostProcess,
            ToolPolicyDenialCode::EffectNativeProcessDenied,
        ),
        (
            "call_host_mutation",
            ToolEffect::HostMutation,
            ToolPolicyDenialCode::EffectHostMutationDenied,
        ),
        (
            "call_delegation",
            ToolEffect::Delegation,
            ToolPolicyDenialCode::EffectDelegationDenied,
        ),
        (
            "call_extension",
            ToolEffect::Extension,
            ToolPolicyDenialCode::EffectExtensionDenied,
        ),
        (
            "call_unknown",
            ToolEffect::Unknown,
            ToolPolicyDenialCode::EffectUnknown,
        ),
    ] {
        let decision = decisions.get(id).expect("policy decision for denied call");
        assert_eq!(decision.effect, Some(effect), "{id}");
        assert!(!decision.allowed, "{id}");
        assert_eq!(decision.authorization, None, "{id}");
        assert_eq!(decision.denial_code, Some(denial_code), "{id}");
        assert_eq!(
            decision.policy.effect_policy.value,
            EffectPolicy::Controlled,
            "{id}"
        );
    }
    let approved = decisions
        .get("call_process")
        .expect("policy decision for approved command");
    assert_eq!(approved.effect, Some(ToolEffect::HostProcess));
    assert!(approved.allowed);
    assert!(approved.authorization.is_some());
    assert_eq!(approved.denial_code, None);
    assert!(approved.policy.allow_process.value);
    assert!(approved.policy.allow_shell.value);

    for &call_id in decisions.keys() {
        let started = events
            .iter()
            .position(
                |event| matches!(event, AgentEvent::ToolStarted { id, .. } if id.0 == call_id),
            )
            .expect("ToolStarted before policy decision");
        let decided = events
            .iter()
            .position(|event| matches!(event, AgentEvent::ToolPolicyDecision { id, .. } if id.0 == call_id))
            .expect("ToolPolicyDecision");
        let finished = events
            .iter()
            .position(
                |event| matches!(event, AgentEvent::ToolFinished { id, .. } if id.0 == call_id),
            )
            .expect("ToolFinished after policy decision");
        assert!(
            started < decided && decided < finished,
            "invalid lifecycle for {call_id}"
        );
    }
    let decisions_json = decisions
        .values()
        .map(|decision| serde_json::to_string(decision).unwrap())
        .collect::<String>();
    assert!(!decisions_json.contains(external_file.to_str().unwrap()));
    assert!(!decisions_json.contains(external_write.to_str().unwrap()));
    assert!(!decisions_json.contains(bash_marker.to_str().unwrap()));
    assert!(!decisions_json.contains("forbidden"));
    assert!(!decisions_json.contains("https://example.com/image.png"));

    for id in [
        "call_host_read",
        "call_network",
        "call_search",
        "call_host_mutation",
        "call_delegation",
        "call_extension",
        "call_unknown",
    ] {
        assert!(denied.contains_key(id), "missing broker denial for {id}");
    }
    assert!(!denied.contains_key("call_process"));
    assert!(succeeded.contains("call_process"));
    assert!(denied["call_host_read"].contains("reading outside the workspace"));
    assert!(denied["call_network"].contains("trusted egress broker"));
    assert!(denied["call_search"].contains("OS or VM isolation backend"));
    assert!(denied["call_host_mutation"].contains("mutating outside the workspace"));
    assert!(denied["call_delegation"].contains("attenuated authority"));
    assert!(denied["call_extension"].contains("executable extensions"));
    assert!(denied["call_unknown"].contains("no host-owned effect classification"));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(approvals, 1);
    assert_eq!(before.load(Ordering::SeqCst), 1);
    assert_eq!(after.load(Ordering::SeqCst), 1);
    assert!(!external_write.exists());
    assert!(bash_marker.exists());
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

#[tokio::test]
async fn controlled_safe_bash_runs_without_approval_prompt() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_process", "bash", serde_json::json!({"command": "ls"}))]),
                text_turn("safe process complete"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;

    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "safe bash test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("run a read-only command").await.unwrap();
    let mut approvals = 0usize;
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(
            &event,
            AgentEvent::ToolProgress {
                progress: octet_agent::ToolProgress::Confirmation(_),
                ..
            }
        ) {
            approvals += 1;
        }
        events.push(event);
    }
    drop(run);

    assert_eq!(approvals, 0);
    assert_eq!(before.load(Ordering::SeqCst), 1);
    assert_eq!(after.load(Ordering::SeqCst), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolFinished {
            id,
            result: Ok(_),
            ..
        } if id.0 == "call_process"
    )));
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

#[tokio::test]
async fn controlled_safe_bash_approval_profile_needs_confirmation() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_process", "bash", serde_json::json!({"command": "ls"}))]),
                text_turn("safe process complete"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;

    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "safe bash test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::ControlledBashApproval),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("run a read-only command").await.unwrap();
    let mut approvals = 0usize;
    while let Some(event) = run.next().await {
        if let AgentEvent::ToolProgress {
            progress: octet_agent::ToolProgress::Confirmation(request),
            ..
        } = &event
        {
            approvals += 1;
            request.clone().respond(true);
        }
    }
    drop(run);

    assert_eq!(approvals, 1);
    assert_eq!(before.load(Ordering::SeqCst), 1);
    assert_eq!(after.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unsafe_host_still_denies_unknown_tools_before_hooks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_unknown", "unknown_probe", serde_json::json!({}))]),
                text_turn("unknown denied"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let executions = Arc::new(AtomicUsize::new(0));
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.tool(ClassifiedEffectProbe {
        name: "unknown_probe",
        effect: ToolEffect::Unknown,
        concurrency: ToolConcurrency::Sequential,
        executions: Arc::clone(&executions),
    });
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "unknown effect test".into(),
        sandbox: SandboxConfig::new(workspace_dir.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("call the unknown tool").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);

    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolFinished { result: Err(error), .. }
            if error.message.contains("no host-owned effect classification")
    )));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(before.load(Ordering::SeqCst), 0);
    assert_eq!(after.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn controlled_workspace_mutation_requires_and_consumes_exact_approval() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[(
                    "call_write",
                    "write",
                    serde_json::json!({"path": "approved.txt", "content": "approved content"}),
                )]),
                text_turn("write complete"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let before = Arc::new(AtomicUsize::new(0));
    let after = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    extensions.load(&CoreTools);
    extensions.tool_call_hook(AdmissionHookProbe {
        before: Arc::clone(&before),
        after: Arc::clone(&after),
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_write = true;
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(session_dir.path().join("session.jsonl")).unwrap(),
        system: "approval test".into(),
        sandbox,
        effect_broker: EffectBroker::new(EffectPolicy::Controlled),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();

    let mut run = agent.prompt("write the approved file").await.unwrap();
    let mut events = Vec::new();
    let mut approvals = 0usize;
    while let Some(event) = run.next().await {
        if let AgentEvent::ToolProgress {
            progress: octet_agent::ToolProgress::Confirmation(request),
            ..
        } = &event
        {
            approvals += 1;
            assert!(request.destructive);
            assert!(!request.default);
            let detail = request.detail.as_deref().expect("canonical intent detail");
            assert!(detail.contains("workspace_mutation"));
            assert!(detail.contains("approved.txt"));
            assert!(detail.contains("approved content"));
            request.clone().respond(true);
        }
        events.push(event);
    }
    drop(run);

    assert_eq!(approvals, 1);
    assert_eq!(
        std::fs::read_to_string(workspace.join("approved.txt")).unwrap(),
        "approved content"
    );
    assert_eq!(before.load(Ordering::SeqCst), 1);
    assert_eq!(after.load(Ordering::SeqCst), 1);
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolFinished { result: Ok(_), .. })));
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

// ── Tool execution waits for the durable assistant turn ────────────────────

const TOOL_TURN_PENDING: &str = "tool call ended; response still pending";

/// The text marker follows ToolCallEnd on the wire. The response cannot finish
/// until the test releases the gate, regardless of scheduler or process speed.
struct GatedToolTurnServer {
    uri: String,
    finish: Option<tokio::sync::oneshot::Sender<()>>,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl GatedToolTurnServer {
    async fn start(head: String, tail: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("http://{}", listener.local_addr().unwrap());
        let (finish, mut finish_rx) = tokio::sync::oneshot::channel();
        let requests = Arc::new(AtomicUsize::new(0));
        let server_requests = Arc::clone(&requests);
        let task = tokio::spawn(async move {
            // The marker must occupy a fresh content-block index. Deriving it
            // from the supplied head keeps fixtures with different block counts
            // from colliding with an existing block.
            let marker_index = highest_block_index(&head) + 1;
            let head = head + &text_block(marker_index, &[TOOL_TURN_PENDING]);
            for (index, body) in [head, text_turn("done")].into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let read = socket.read(&mut buf).await.unwrap();
                    assert_ne!(read, 0);
                    request.extend_from_slice(&buf[..read]);
                    let Some(header_end) = request.windows(4).position(|b| b == b"\r\n\r\n") else {
                        continue;
                    };
                    let header_end = header_end + 4;
                    let headers = String::from_utf8_lossy(&request[..header_end]);
                    let content_length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or_default();
                    if request.len() >= header_end + content_length {
                        break;
                    }
                }
                server_requests.fetch_add(1, Ordering::SeqCst);
                socket.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                ).await.unwrap();
                socket.write_all(body.as_bytes()).await.unwrap();
                socket.flush().await.unwrap();
                if index == 0 {
                    if (&mut finish_rx).await.is_err() {
                        return;
                    }
                    socket.write_all(tail.as_bytes()).await.unwrap();
                }
                socket.shutdown().await.unwrap();
            }
        });
        Self {
            uri,
            finish: Some(finish),
            requests,
            task,
        }
    }

    fn finish(&mut self) {
        self.finish.take().unwrap().send(()).unwrap();
    }
}

impl Drop for GatedToolTurnServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Highest `content_block_start`/`content_block_delta` index present in a
/// fixture stream, so generated follow-up blocks never reuse an index.
fn highest_block_index(frames: &str) -> usize {
    let mut highest = None;
    let mut rest = frames;
    while let Some(position) = rest.find("\"index\":") {
        rest = &rest[position + "\"index\":".len()..];
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(index) = digits.parse::<usize>() {
            highest = Some(highest.map_or(index, |current: usize| current.max(index)));
        }
    }
    highest.unwrap_or(0)
}

fn recon_bash_head() -> String {
    msg_start()
        + &tool_block(
            0,
            "call_bash",
            "bash",
            &serde_json::json!({"command": "cat probe.txt"}),
        )
}

async fn observe_unfinished_tool_turn(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut events = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = run.next().await.expect("provider must reach the gate");
            let reached_gate = matches!(
                &event, AgentEvent::OutputDelta { text, .. } if text == TOOL_TURN_PENDING
            );
            assert!(!matches!(event, AgentEvent::ToolStarted { .. }));
            events.push(event);
            if reached_gate {
                break;
            }
        }
    })
    .await
    .expect("provider must deliver ToolCallEnd and the following marker");
    // Keep driving the agent and give any illegally spawned execution time to
    // run. Unlike the old sleep-based fixture, this cannot release the response.
    assert!(tokio::time::timeout(Duration::from_millis(25), run.next())
        .await
        .is_err());
    events
}

struct DurableBashProbe {
    effect: ToolEffect,
    effect_calls: Arc<AtomicUsize>,
    executions: Arc<AtomicUsize>,
    session_path: PathBuf,
}

#[async_trait::async_trait]
impl Tool for DurableBashProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_agent::BashTool.definition()
    }

    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Sequential
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        self.effect_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.effect)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        // Reopen the file, not the agent's in-memory view: successful dispatch
        // must already have a complete durable assistant envelope and usage.
        let session = Session::open_read_only(&self.session_path).unwrap();
        assert!(session.entries().iter().any(|entry| matches!(
            &entry.value,
            EntryValue::Message(Message::Assistant(message)) if message.content.iter().any(|part|
                matches!(part, AssistantPart::ToolCall(call)
                    if call.name == "bash" && call.arguments_value().unwrap() == args)
            )
        )));
        assert!(!session.usage_records().is_empty());
        if self.effect == ToolEffect::WorkspaceMutation {
            std::fs::write(ctx.workspace.join("mutation.txt"), "executed").unwrap();
            Ok(ToolOutput::new("mutation executed"))
        } else {
            octet_agent::BashTool.execute(args, ctx).await
        }
    }
}

fn bash_probe_harness(
    uri: &str,
    effect: ToolEffect,
    policy: EffectPolicy,
) -> (Harness, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("probe.txt"), "before").unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let effect_calls = Arc::new(AtomicUsize::new(0));
    let executions = Arc::new(AtomicUsize::new(0));
    let mut extensions = ExtensionHost::new();
    // Deliberately register an arbitrary sequential implementation named bash,
    // with no tool hooks that could suppress an unsafe streaming fast path.
    extensions.tool(DurableBashProbe {
        effect,
        effect_calls: Arc::clone(&effect_calls),
        executions: Arc::clone(&executions),
        session_path: session_path.clone(),
    });
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_write = true;
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    let agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(uri),
        session: Session::create(&session_path).unwrap(),
        system: "tool ordering test".into(),
        sandbox,
        effect_broker: EffectBroker::new(policy),
        extensions,
        max_turns: Some(2),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (
        Harness {
            agent,
            server: None,
            workspace,
            session_path,
            _dirs: (workspace_dir, session_dir),
        },
        effect_calls,
        executions,
    )
}

#[tokio::test]
async fn recon_bash_waits_for_complete_response_and_durable_assistant() {
    for effect in [ToolEffect::WorkspaceMutation, ToolEffect::HostProcess] {
        let mut server = GatedToolTurnServer::start(recon_bash_head(), msg_end("tool_use")).await;
        let (mut h, effect_calls, executions) =
            bash_probe_harness(&server.uri, effect, EffectPolicy::UnsafeHost);
        let mut run = h.agent.prompt("probe").await.unwrap();
        let mut events = observe_unfinished_tool_turn(&mut run).await;
        assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert!(!h.workspace.join("mutation.txt").exists());
        std::fs::write(h.workspace.join("probe.txt"), "after").unwrap();
        server.finish();
        events.extend(collect(&mut run).await);
        drop(run);
        assert_eq!(executions.load(Ordering::SeqCst), 1);
        assert_eq!(effect_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
                .count(),
            1
        );
        let output = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolFinished { result, .. } => Some(result.as_ref().unwrap()),
                _ => None,
            })
            .unwrap();
        if effect == ToolEffect::HostProcess {
            assert!(output.text.contains("after"), "{}", output.text);
            assert!(!output.text.contains("before"), "{}", output.text);
        } else {
            assert_eq!(
                std::fs::read_to_string(h.workspace.join("mutation.txt")).unwrap(),
                "executed"
            );
        }
        assert!(matches!(
            assert_single_run_finished(&events),
            FinishReason::Completed
        ));
    }
}

#[tokio::test]
async fn recon_bash_provider_error_or_eof_after_tool_end_never_executes() {
    for tail in [
        frame(
            "error",
            serde_json::json!({
                "type": "error", "error": {"type": "overloaded_error", "message": "failed after tool end"}
            }),
        ),
        String::new(),
    ] {
        let mut server = GatedToolTurnServer::start(recon_bash_head(), tail).await;
        let (mut h, effect_calls, executions) = bash_probe_harness(
            &server.uri,
            ToolEffect::HostProcess,
            EffectPolicy::UnsafeHost,
        );
        let mut run = h.agent.prompt("probe").await.unwrap();
        let mut events = observe_unfinished_tool_turn(&mut run).await;
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        server.finish();
        events.extend(collect(&mut run).await);
        drop(run);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            server.requests.load(Ordering::SeqCst),
            1,
            "generation must prevent retry"
        );
        assert!(matches!(
            assert_single_run_finished(&events),
            FinishReason::Failed(_)
        ));
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
        let session = Session::open_read_only(&h.session_path).unwrap();
        // Failure may append a local synthetic assistant boundary, but never
        // commit the provisional provider tool call.
        assert!(!session.entries().iter().any(|entry| matches!(
            &entry.value, EntryValue::Message(Message::Assistant(message))
                if message.content.iter().any(|part| matches!(part, AssistantPart::ToolCall(_)))
        )));
    }
}

#[tokio::test]
async fn recon_bash_assistant_append_failure_never_executes() {
    let mut server = GatedToolTurnServer::start(recon_bash_head(), msg_end("tool_use")).await;
    let (mut h, effect_calls, executions) = bash_probe_harness(
        &server.uri,
        ToolEffect::WorkspaceMutation,
        EffectPolicy::UnsafeHost,
    );
    let mut run = h.agent.prompt("probe").await.unwrap();
    let mut events = observe_unfinished_tool_turn(&mut run).await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    // A second valid append on this disposable session deterministically makes
    // the agent handle stale. Its assistant append must fail before dispatch.
    Session::open(&h.session_path)
        .unwrap()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("concurrent append".into())],
        })))
        .unwrap();
    server.finish();
    events.extend(collect(&mut run).await);
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(octet_agent::AgentError::Session(
            octet_agent::SessionError::ConcurrentModification
        ))
    ));
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
    assert!(!h.workspace.join("mutation.txt").exists());
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
}

#[tokio::test]
async fn recon_bash_max_tokens_and_abort_never_execute() {
    for abort in [false, true] {
        let mut server = GatedToolTurnServer::start(recon_bash_head(), msg_end("max_tokens")).await;
        let (mut h, effect_calls, executions) = bash_probe_harness(
            &server.uri,
            ToolEffect::WorkspaceMutation,
            EffectPolicy::UnsafeHost,
        );
        let mut run = h.agent.prompt("probe").await.unwrap();
        let mut events = observe_unfinished_tool_turn(&mut run).await;
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        if abort {
            run.control().abort();
        } else {
            server.finish();
        }
        events.extend(collect(&mut run).await);
        drop(run);
        assert_eq!(executions.load(Ordering::SeqCst), 0);
        assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
        assert!(!h.workspace.join("mutation.txt").exists());
        if abort {
            assert!(matches!(
                assert_single_run_finished(&events),
                FinishReason::Aborted
            ));
        } else {
            assert!(matches!(
                assert_single_run_finished(&events),
                FinishReason::Completed
            ));
            assert!(events.iter().any(|event| matches!(
                event, AgentEvent::ToolFinished { result: Err(error), .. }
                    if error.message.contains("output token limit")
            )));
        }
    }
}

#[tokio::test]
async fn recon_bash_obeys_per_turn_call_limit() {
    let mut head = msg_start();
    for index in 0..65 {
        head += &tool_block(
            index,
            &format!("call_{index}"),
            "bash",
            &serde_json::json!({"command": "ls"}),
        );
    }
    let mut server = GatedToolTurnServer::start(head, msg_end("tool_use")).await;
    let (mut h, effect_calls, executions) = bash_probe_harness(
        &server.uri,
        ToolEffect::WorkspaceMutation,
        EffectPolicy::UnsafeHost,
    );
    let mut run = h.agent.prompt("probe").await.unwrap();
    let mut events = observe_unfinished_tool_turn(&mut run).await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    server.finish();
    events.extend(collect(&mut run).await);
    drop(run);
    assert_eq!(executions.load(Ordering::SeqCst), 32);
    assert_eq!(effect_calls.load(Ordering::SeqCst), 32);
    let results: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolFinished { id, result, .. } => Some((id, result)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 65);
    for (index, (id, result)) in results.iter().enumerate() {
        assert_eq!(id.0, format!("call_{index}"));
        if index < 32 {
            assert!(result.is_ok());
        } else {
            assert!(result
                .as_ref()
                .unwrap_err()
                .message
                .contains("per-turn tool-call limit"));
        }
    }
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    assert_bounded_invocation_batch(&h.session_path, 65, "call_");
}

#[tokio::test]
async fn recon_bash_approval_is_requested_after_persistence_without_cached_denial() {
    let mut server = GatedToolTurnServer::start(recon_bash_head(), msg_end("tool_use")).await;
    let (mut h, effect_calls, executions) = bash_probe_harness(
        &server.uri,
        ToolEffect::HostProcess,
        EffectPolicy::ControlledBashApproval,
    );
    let mut run = h.agent.prompt("probe").await.unwrap();
    let mut events = observe_unfinished_tool_turn(&mut run).await;
    assert_eq!(effect_calls.load(Ordering::SeqCst), 0);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    server.finish();
    let mut approvals = 0;
    while let Some(event) = run.next().await {
        if let AgentEvent::ToolProgress {
            progress: octet_agent::ToolProgress::Confirmation(request),
            ..
        } = &event
        {
            approvals += 1;
            assert_eq!(executions.load(Ordering::SeqCst), 0);
            let session = Session::open_read_only(&h.session_path).unwrap();
            assert!(session
                .entries()
                .iter()
                .any(|entry| matches!(entry.value, EntryValue::Message(Message::Assistant(_)))));
            request.clone().respond(true);
        }
        events.push(event);
    }
    drop(run);
    assert_eq!(approvals, 1);
    assert_eq!(effect_calls.load(Ordering::SeqCst), 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolFinished { result: Ok(_), .. })));
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

#[tokio::test]
async fn write_then_recon_bash_observes_new_contents_and_preserves_result_order() {
    let head = msg_start()
        + &tool_block(
            0,
            "call_write",
            "write",
            &serde_json::json!({"path": "probe.txt", "content": "after"}),
        )
        + &tool_block(
            1,
            "call_bash",
            "bash",
            &serde_json::json!({"command": "cat probe.txt"}),
        );
    let mut server = GatedToolTurnServer::start(head, msg_end("tool_use")).await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("session.jsonl");
    std::fs::write(workspace.path().join("probe.txt"), "before").unwrap();
    let mut agent = build_agent(&server.uri, workspace.path(), &session_path, Some(2));
    let mut run = agent.prompt("write then read").await.unwrap();
    let mut events = observe_unfinished_tool_turn(&mut run).await;
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("probe.txt")).unwrap(),
        "before"
    );
    server.finish();
    events.extend(collect(&mut run).await);
    drop(run);
    let order: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStarted { id, .. } => Some(format!("start:{}", id.0)),
            AgentEvent::ToolFinished { id, result, .. } => {
                let output = result.as_ref().unwrap();
                if id.0 == "call_bash" {
                    assert!(output.text.contains("after"), "{}", output.text);
                    assert!(!output.text.contains("before"), "{}", output.text);
                }
                Some(format!("finish:{}", id.0))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        [
            "start:call_write",
            "finish:call_write",
            "start:call_bash",
            "finish:call_bash"
        ]
    );
    let session = Session::open_read_only(&session_path).unwrap();
    let results: Vec<_> = session
        .entries()
        .iter()
        .filter_map(|entry| match &entry.value {
            EntryValue::Message(Message::User(user)) => {
                user.content.iter().find_map(|part| match part {
                    UserPart::ToolResult(result) => Some(result.tool_call_id.0.as_str()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect();
    assert_eq!(results, ["call_write", "call_bash"]);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
}

// PATH belongs to this subprocess alone: never mutate the concurrent test
// runner's environment to simulate a read-looking command with side effects.
#[cfg(unix)]
#[test]
fn recon_bash_path_override_waits_for_complete_response() {
    const CHILD_ENV: &str = "OCTET_TEST_RECON_BASH_PATH_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("cat");
        std::fs::write(
            &program,
            concat!(
                "#!/bin/sh\n",
                "printf executed > mutation.txt\n",
                "printf shadowed\n",
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![dir.path().to_path_buf()];
        paths.extend(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        ));
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "recon_bash_path_override_waits_for_complete_response",
                "--nocapture",
            ])
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env(CHILD_ENV, "1")
            .env_remove("BASH_ENV")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut server =
                GatedToolTurnServer::start(recon_bash_head(), msg_end("tool_use")).await;
            let workspace = tempfile::tempdir().unwrap();
            let marker = workspace.path().join("mutation.txt");
            let sessions = tempfile::tempdir().unwrap();
            let mut agent = build_agent(
                &server.uri,
                workspace.path(),
                &sessions.path().join("session.jsonl"),
                Some(2),
            );
            let mut run = agent.prompt("probe").await.unwrap();
            let mut events = observe_unfinished_tool_turn(&mut run).await;
            assert!(
                !marker.exists(),
                "PATH override must not run during generation"
            );
            server.finish();
            events.extend(collect(&mut run).await);
            drop(run);
            assert_eq!(std::fs::read_to_string(marker).unwrap(), "executed");
            assert!(events.iter().any(|event| matches!(
                event, AgentEvent::ToolFinished { result: Ok(output), .. }
                    if output.text.contains("shadowed")
            )));
            assert!(matches!(
                assert_single_run_finished(&events),
                FinishReason::Completed
            ));
        });
}

// Valid synthetic one-pixel PNG, not an attachment or a provider fixture.
const OWNER_IMAGE_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0,
    0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1,
    39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

struct StrippedToolObserver(Arc<AtomicUsize>);
impl octet_agent::EventObserver for StrippedToolObserver {
    fn on_event(&self, event: &AgentEvent) {
        if let AgentEvent::ToolFinished {
            result: Ok(output), ..
        } = event
        {
            assert!(output.media().is_empty());
            assert!(!output
                .content_parts()
                .iter()
                .any(|part| matches!(part, octet_agent::ToolOutputContentPart::Media(_))));
            assert!(!output.presentation_images_omitted());
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn real_read_tool_images_are_owner_opt_in_and_observers_stay_stripped() {
    for enabled in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("messages"))
            .respond_with(Script {
                bodies: vec![
                    tool_turn(&[(
                        "read-image",
                        "read",
                        serde_json::json!({"path":"pixel.png"}),
                    )]),
                    text_turn("image accepted"),
                ],
                next: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        let workspace = tempfile::tempdir().unwrap();
        let sessions = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("pixel.png"), OWNER_IMAGE_PNG).unwrap();
        let observed = Arc::new(AtomicUsize::new(0));
        let mut extensions = ExtensionHost::new();
        extensions.load(&CoreTools);
        extensions.observe(StrippedToolObserver(observed.clone()));
        let mut agent = Agent::new(AgentConfig {
            client: AiClient::new(),
            model: scripted_model(&server.uri()),
            session: Session::create(sessions.path().join("session.jsonl")).unwrap(),
            system: "Test the actual read tool".into(),
            sandbox: SandboxConfig::new(workspace.path()),
            effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
            extensions,
            max_turns: Some(4),
            reasoning: ReasoningConfig::Off,
            reasoning_mode: octet_ai::ReasoningMode::Standard,
            cache_retention: octet_ai::CacheRetention::Short,
            session_id: None,
        })
        .unwrap();
        if enabled {
            agent.set_owner_tool_images_enabled(true);
        }
        let mut run = agent.prompt("read pixel.png").await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        assert!(matches!(
            assert_single_run_finished(&events),
            FinishReason::Completed
        ));
        let output = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ToolFinished {
                    result: Ok(output), ..
                } => Some(output),
                _ => None,
            })
            .unwrap();
        assert_eq!(output.media().len(), usize::from(enabled));
        assert_eq!(
            output
                .content_parts()
                .iter()
                .filter(|part| matches!(part, octet_agent::ToolOutputContentPart::Media(_)))
                .count(),
            usize::from(enabled)
        );
        assert_eq!(observed.load(Ordering::SeqCst), 1);
        let debug = format!("{events:?}");
        assert!(!debug.contains("iVBOR"));
        assert!(!debug.contains("\\x89PNG"));
        assert!(!debug.contains("137, 80, 78, 71"));
        assert!(agent
            .session()
            .context()
            .unwrap()
            .iter()
            .any(|message| matches!(message,
                Message::User(user) if user.content.iter().any(|part| matches!(part,
                    UserPart::ToolResult(result) if result.content.iter().any(|part| matches!(part,
                        octet_ai::ToolResultPart::Media(Media::Image(_))
                    ))
                ))
            )));
    }
}

// #350: explicitly host-qualified Codex runtime, with all effects still local.
fn recovery_codex_model(uri: &str) -> Model {
    let mut model = scripted_responses_model(uri);
    Arc::make_mut(&mut model.spec).pricing = Some(Pricing {
        input: TokenRate(1),
        output: TokenRate(1),
        cache_read: TokenRate(1),
        cache_write_5m: TokenRate(1),
        cache_write_1h: None,
        reasoning: None,
        tiers: vec![],
    });
    Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
        octet_ai::ResponsesRuntimeProfile::Codex;
    model
}

fn interrupted_responses_prefix(kind: &str) -> String {
    let body = match kind {
        "tool" => responses_tool_turn("failed", "failed-call"),
        "reasoning" => {
            include_str!("../../octet-ai/tests/fixtures/openai_responses/reasoning_summary.sse")
                .to_owned()
        }
        _ => responses_text_turn(
            "failed",
            "discarded provisional text",
            "response.completed",
            "failed-opaque",
        ),
    };
    body.lines()
        .take_while(|line| !line.contains("\"type\":\"response.completed\""))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn recovery_provider_error(code: &str) -> String {
    format!(
        "data: {}\n\n",
        serde_json::json!({
            "type": "error", "code": code, "message": "synthetic provider interruption",
            "request_id": "recovery-request"
        })
    )
}

async fn recovery_harness(bodies: Vec<String>) -> (Agent, MockServer, tempfile::TempDir, PathBuf) {
    recovery_harness_with_output_cap(bodies, false).await
}

async fn recovery_harness_with_output_cap(
    bodies: Vec<String>,
    capped: bool,
) -> (Agent, MockServer, tempfile::TempDir, PathBuf) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies,
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let session_path = workspace.path().join("session.jsonl");
    std::fs::write(workspace.path().join("lifecycle.txt"), "local result").unwrap();
    let mut model = recovery_codex_model(&server.uri());
    if capped {
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Default;
    }
    let agent = build_responses_agent_from_session(
        model,
        Session::create(&session_path).unwrap(),
        workspace.path(),
        Some(4),
        "You are a test agent.",
        ReasoningConfig::Off,
    );
    (agent, server, workspace, session_path)
}

#[tokio::test]
async fn qualified_codex_interrupted_text_reasoning_and_provisional_tool_replace_before_commit() {
    for kind in ["text", "reasoning", "tool"] {
        let (mut agent, server, _workspace, session_path) = recovery_harness(vec![
            interrupted_responses_prefix(kind) + &recovery_provider_error("server_error"),
            responses_tool_turn("accepted", "accepted-call"),
            responses_text_turn(
                "final",
                "successful answer",
                "response.completed",
                "accepted-opaque",
            ),
        ])
        .await;
        let mut run = agent.prompt("finish the work automatically").await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        assert!(
            matches!(assert_single_run_finished(&events), FinishReason::Completed),
            "{kind}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::ProviderRetry { .. }))
                .count(),
            1,
            "{kind}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
                .count(),
            1,
            "{kind}"
        );
        let requests = wire_requests(&server).await;
        assert_eq!(requests.len(), 3, "{kind}");
        let replacement = requests[1].to_string();
        assert!(!replacement.contains("failed-call"));
        assert!(!replacement.contains("discarded provisional text"));
        assert!(!replacement.contains("Planning briefly."));
        drop(agent);
        let durable = std::fs::read_to_string(session_path).unwrap();
        assert!(!durable.contains("failed-call"));
        assert!(!durable.contains("discarded provisional text"));
        assert!(!durable.contains("Planning briefly."));
        assert!(!durable.contains("previous provider turn failed"));
    }
}

#[tokio::test]
async fn qualified_codex_premature_eof_replaces_without_user_prompt() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("text"),
        responses_text_turn("ok", "recovered", "response.completed", "accepted"),
    ])
    .await;
    assert_eq!(agent.complete("finish").await.unwrap().text, "recovered");
    assert_eq!(wire_requests(&server).await.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn qualified_codex_exhaustion_is_finite_and_reports_unknown_usage_and_replacements() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("text") + &recovery_provider_error("server_error"),
    ])
    .await;
    let mut run = agent.prompt("finish").await.unwrap();
    let events = collect_virtual_recovery(&mut run).await;
    drop(run);
    let FinishReason::Failed(error) = assert_single_run_finished(&events) else {
        panic!("{events:?}")
    };
    assert!(matches!(
        error,
        octet_agent::AgentError::ProviderRecovery {
            retries: 11,
            usage_unknown: true,
            ..
        }
    ));
    let diagnostic = octet_agent::public_error_diagnostic(error, "codex", "model");
    assert!(diagnostic.contains("replacements=11"), "{diagnostic}");
    assert!(diagnostic.contains("failed_usage=unknown"), "{diagnostic}");
    assert!(!diagnostic.contains("did not replay"));
    assert_eq!(wire_requests(&server).await.len(), 12);
}

#[tokio::test]
async fn qualified_codex_permanent_failures_do_not_replace() {
    for error in [
        recovery_provider_error("invalid_api_key"),
        recovery_provider_error("invalid_request_error"),
    ] {
        let (mut agent, server, _workspace, _) = recovery_harness(vec![
            interrupted_responses_prefix("text") + &error,
            responses_text_turn("no", "must not replay", "response.completed", "no"),
        ])
        .await;
        let mut run = agent.prompt("finish").await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        assert!(matches!(
            assert_single_run_finished(&events),
            FinishReason::Failed(_)
        ));
        assert!(!events
            .iter()
            .any(|e| matches!(e, AgentEvent::ProviderRetry { .. })));
        assert_eq!(wire_requests(&server).await.len(), 1);
    }
}

#[tokio::test]
async fn cap_supported_hard_cost_budget_fails_closed_on_unknown_interrupted_usage() {
    let (mut agent, server, _workspace, _) = recovery_harness_with_output_cap(
        vec![
            interrupted_responses_prefix("text") + &recovery_provider_error("server_error"),
            responses_text_turn("no", "must not replay", "response.completed", "no"),
        ],
        true,
    )
    .await;
    agent.set_max_session_cost_microdollars(Some(u64::MAX));
    let mut run = agent.prompt("bounded spending").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(
        matches!(
            assert_single_run_finished(&events),
            FinishReason::Failed(octet_agent::AgentError::ProviderRecovery {
                retries: 0,
                usage_unknown: true,
                ..
            })
        ),
        "{events:?}"
    );
    assert_eq!(wire_requests(&server).await.len(), 1);
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ProviderRetry { .. })));
}

#[tokio::test]
async fn qualified_codex_replacement_wait_is_cancellable_without_tool_effects() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("tool") + &recovery_provider_error("server_error"),
        responses_tool_turn("no", "never-execute"),
    ])
    .await;
    let mut run = agent.prompt("cancel recovery").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::ProviderRetry { .. }) {
            control.abort();
        }
        events.push(event);
    }
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Aborted
    ));
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolStarted { .. })));
    assert_eq!(wire_requests(&server).await.len(), 1);
}

#[tokio::test]
async fn qualified_codex_replacement_prepares_steering_and_finish_now_exactly_once() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("tool") + &recovery_provider_error("server_error"),
        responses_text_turn(
            "ok",
            "finished without tools",
            "response.completed",
            "accepted",
        ),
    ])
    .await;
    let mut run = agent.prompt("finish my work").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::ProviderRetry { .. }) {
            control.steer("new steering sentinel").await.unwrap();
            control.finish_now("finish now sentinel").await.unwrap();
        }
        events.push(event);
    }
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ToolStarted { .. })));
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert!(!requests[0]["tools"].as_array().unwrap().is_empty());
    assert!(requests[1]["tools"].as_array().is_none_or(Vec::is_empty));
    assert_eq!(requests[1]["tool_choice"], "none");
    let replacement = requests[1].to_string();
    assert_eq!(replacement.matches("new steering sentinel").count(), 1);
    assert_eq!(replacement.matches("finish now sentinel").count(), 1);
    let delivered: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::SteeringDelivered { messages } => Some(messages.len()),
            _ => None,
        })
        .collect();
    assert_eq!(delivered, [2]);
}

#[tokio::test]
async fn qualified_codex_recovery_preserves_committed_mutations_without_replaying_effects() {
    let mutation = |body: String| {
        body.replace(r#""name":"read""#, r#""name":"bash""#)
            .replace(
                r#"{\"path\":\"lifecycle.txt\"}"#,
                r#"{\"command\":\"printf x >> effects.txt\"}"#,
            )
    };
    let (mut agent, server, workspace, session_path) = recovery_harness(vec![
        mutation(responses_tool_turn("prior", "committed-call")),
        mutation(interrupted_responses_prefix("tool")) + &recovery_provider_error("server_error"),
        mutation(responses_tool_turn("accepted", "accepted-call")),
        responses_text_turn("final", "effects settled", "response.completed", "accepted"),
    ])
    .await;
    let mut run = agent.prompt("do the two local mutations").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("effects.txt")).unwrap(),
        "xx"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, AgentEvent::ToolStarted { .. }))
            .count(),
        2
    );
    assert_eq!(wire_requests(&server).await.len(), 4);
    drop(agent);
    assert!(!std::fs::read_to_string(session_path)
        .unwrap()
        .contains("failed-call"));
}

#[tokio::test]
async fn qualified_codex_disabled_retries_remain_terminal() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("text") + &recovery_provider_error("server_error"),
    ])
    .await;
    agent.set_provider_retries_enabled(false);
    let mut run = agent.prompt("no retry").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(_)
    ));
    assert!(!events
        .iter()
        .any(|e| matches!(e, AgentEvent::ProviderRetry { .. })));
    assert_eq!(wire_requests(&server).await.len(), 1);
}

#[tokio::test]
async fn qualified_codex_body_disconnect_recovers_in_the_same_run() {
    let (uri, calls) = interrupted_body_server(
        interrupted_responses_prefix("text"),
        responses_text_turn("ok", "recovered", "response.completed", "accepted"),
    )
    .await;
    let workspace = tempfile::tempdir().unwrap();
    let mut agent = build_responses_agent_from_session(
        recovery_codex_model(&uri),
        Session::create(workspace.path().join("session.jsonl")).unwrap(),
        workspace.path(),
        Some(1),
        "system",
        ReasoningConfig::Off,
    );
    assert_eq!(agent.complete("finish").await.unwrap().text, "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn qualified_codex_terminal_gate_recovery_does_not_discard_main_answer() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        responses_text_turn("main", "accepted main answer", "response.completed", "main"),
        interrupted_responses_prefix("text") + &recovery_provider_error("server_error"),
        responses_text_turn("gate", "R", "response.completed", "gate"),
    ])
    .await;
    agent.set_completion_policy(CompletionPolicy::TerminalGate);
    let mut run = agent.prompt("complete and verify").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderOperationRetry {
            operation: octet_agent::ProviderOperation::TerminalGate,
            attempt: 1,
            max_attempts: Some(11),
            ..
        }
    )));
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
    assert_eq!(wire_requests(&server).await.len(), 3);
}

// Keep Tokio from auto-advancing provider I/O deadlines while the loopback
// server is scheduled by the OS. Only observed host retry delays advance the
// clock. A hidden transport timer is a regression, not permission to spin
// forever: use a real-time per-event watchdog even with Tokio time paused.
async fn collect_virtual_recovery(run: &mut octet_agent::Run<'_>) -> Vec<AgentEvent> {
    let mut delay = None;
    let mut events = Vec::new();
    loop {
        let next = run.next();
        tokio::pin!(next);
        if let Some(wait) = delay.take() {
            assert!(futures_util::poll!(&mut next).is_pending());
            tokio::time::advance(wait).await;
        }
        let started = std::time::Instant::now();
        let event = loop {
            if let std::task::Poll::Ready(event) = futures_util::poll!(&mut next) {
                break event;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "no event after 15s wall time with provider clock paused; hidden transport wait? last event: {:?}",
                events.last(),
            );
            // Stay runnable to prevent virtual auto-advance, but do not burn a
            // core while the OS services the loopback socket. No detached
            // spinner survives a panic or cancellation of this collector.
            tokio::task::yield_now().await;
            std::thread::sleep(Duration::from_millis(1));
        };
        let Some(event) = event else { break };
        delay = match &event {
            AgentEvent::ProviderRetry { delay, .. }
            | AgentEvent::ProviderWaitingForNetwork { delay, .. }
            | AgentEvent::ProviderOperationRetry { delay, .. } => Some(*delay),
            _ => None,
        };
        events.push(event);
    }
    events
}

#[tokio::test(start_paused = true)]
async fn qualified_codex_four_eofs_then_success_preserves_unknown_usage() {
    let mut bodies = vec![interrupted_responses_prefix("text"); 4];
    bodies.push(responses_text_turn(
        "ok",
        "recovered",
        "response.completed",
        "accepted",
    ));
    let (mut agent, server, workspace, session_path) = recovery_harness(bodies).await;
    let mut run = agent.prompt("finish").await.unwrap();
    let events = collect_virtual_recovery(&mut run).await;
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(wire_requests(&server).await.len(), 5);
    assert_eq!(agent.session().usage_uncertainty_records().len(), 4);
    let mut recovered_session = Session::open(&session_path).unwrap();
    assert!(recovered_session.take_partial_assistant().unwrap().is_none());
    drop(recovered_session);
    drop(agent);
    let mut agent = build_responses_agent_from_session(
        recovery_codex_model(&server.uri()),
        Session::open(&session_path).unwrap(),
        workspace.path(),
        Some(4),
        "system",
        ReasoningConfig::Off,
    );
    let mut run = agent.prompt("later turn").await.unwrap();
    assert!(matches!(
        run.next().await,
        Some(AgentEvent::ProviderUsageUncertain)
    ));
    drop(run);
    agent.set_max_session_cost_microdollars(Some(u64::MAX));
    assert!(matches!(
        agent.complete("bounded later turn").await,
        Err(octet_agent::AgentError::UsageUncertain)
    ));
    assert_eq!(wire_requests(&server).await.len(), 5);
}

struct OutageAfterFirstRequest {
    calls: AtomicUsize,
    offline_attempts: usize,
}
#[async_trait::async_trait]
impl octet_ai::CredentialResolver for OutageAfterFirstRequest {
    async fn resolve(&self) -> Result<octet_ai::ResolvedCredential, octet_ai::AuthError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if (1..=self.offline_attempts).contains(&call) {
            return Err(octet_ai::AuthError::Unavailable);
        }
        Ok(octet_ai::ResolvedCredential {
            scheme: octet_ai::CredentialScheme::Bearer,
            value: "synthetic".into(),
            extra_headers: http::HeaderMap::new(),
        })
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_codex_ws_http_cumulative_twelve_attempt_envelope() {
    for failures in [10, 11] {
        let server = ResponsesConnectionLimitServer::with_http_failures(failures).await;
        let workspace = tempfile::tempdir().unwrap();
        let mut model = recovery_codex_model(&server.base_url);
        let auth = Arc::new(OutageAfterFirstRequest {
            calls: AtomicUsize::new(0),
            offline_attempts: 20_200,
        });
        Arc::make_mut(&mut model.endpoint).auth = Auth::dynamic(auth.clone());
        Arc::make_mut(&mut model.endpoint).transport =
            octet_ai::EndpointTransport::WebSocketPreferred;
        let mut agent = build_responses_agent_from_session(
            model,
            Session::create(workspace.path().join("session.jsonl")).unwrap(),
            workspace.path(),
            Some(1),
            "system",
            ReasoningConfig::Off,
        );
        let start = tokio::time::Instant::now();
        let mut run = agent.prompt("recover").await.unwrap();
        let events = collect_virtual_recovery(&mut run).await;
        drop(run);
        let result = assert_single_run_finished(&events);
        if failures == 10 {
            assert!(matches!(result, FinishReason::Completed), "{events:?}");
        } else {
            assert!(
                matches!(
                    result,
                    FinishReason::Failed(octet_agent::AgentError::ProviderRecovery {
                        retries: 11,
                        ..
                    })
                ),
                "{events:?}"
            );
        }
        assert_eq!(server.websocket_requests.load(Ordering::SeqCst), 1);
        assert_eq!(server.http_requests.load(Ordering::SeqCst), 11);
        assert!(agent.session().has_uncertain_usage());
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ProviderWaitingForNetwork { .. }))
                .count(),
            20_200
        );
        assert!(start.elapsed() > Duration::from_secs(14 * 24 * 60 * 60));
        assert_eq!(auth.calls.load(Ordering::SeqCst), 20_212);
    }
}

// Operation-callsite tests use the same public transport adapter as embedders,
// with explicit virtual-time body delays rather than a live-provider claim.
enum RecoveryStep {
    GateControl(Arc<std::sync::Mutex<Option<RunControl>>>, u8),
    Opening(octet_ai::TransportPhase),
    HoldOpening,
    Http(u16),
    Offline,
    Interrupted,
    Reply(&'static str, Duration),
}
struct OperationRecoveryTransport {
    steps: std::sync::Mutex<std::collections::VecDeque<RecoveryStep>>,
    requests: std::sync::Mutex<Vec<octet_ai::Request>>,
}
#[async_trait::async_trait]
impl octet_ai::HostStreamTransport for OperationRecoveryTransport {
    async fn stream(
        &self,
        model: octet_ai::HostStreamModel,
        request: octet_ai::Request,
        _: Vec<octet_ai::Diagnostic>,
    ) -> Result<octet_ai::ResponseStream, octet_ai::AiError> {
        // These synthetic successful responses explicitly report zero tokens.
        // Preserve their declared zero price rather than fabricate unpriced
        // history that prevents the auxiliary HTTP-budget test from dispatching.
        let response_cost = model
            .pricing
            .as_ref()
            .map(|pricing| octet_ai::pricing::cost_of(pricing, &Usage::default()).unwrap());
        self.requests.lock().unwrap().push(request);
        let step = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected provider attempt");
        match step {
            RecoveryStep::HoldOpening => std::future::pending().await,
            RecoveryStep::GateControl(holder, kind) => Ok(Box::pin(async_stream::stream! {
                yield Ok(octet_ai::StreamEvent::Started { response_id: None });
                let control = { holder.lock().unwrap().take().unwrap() };
                submit_gate_boundary_control(&control, kind).await;
                yield Ok(octet_ai::StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage { content: vec![AssistantPart::Text("R".into())], model: model.id, protocol: model.protocol },
                    stop_reason: octet_ai::StopReason::EndTurn, usage: octet_ai::Usage::default(),
                    deferred: None,
                    cost: response_cost, response_id: None, responses_output: None, diagnostics: Vec::new(),
                }));
            })),
            RecoveryStep::Opening(phase) => {
                Err(octet_ai::AiError::Transport(octet_ai::TransportError {
                    phase,
                    timeout: false,
                    message: "synthetic opening failure".into(),
                }))
            }
            RecoveryStep::Http(status) => Err(octet_ai::AiError::Http(octet_ai::HttpError {
                status: http::StatusCode::from_u16(status).unwrap(),
                request_id: None,
                retry_after: None,
                provider_code: Some("server_error".into()),
                body_snippet: None,
                retryable: true,
            })),
            RecoveryStep::Offline => Err(octet_ai::AiError::Auth(octet_ai::AuthError::Unavailable)),
            RecoveryStep::Interrupted => Ok(Box::pin(futures_util::stream::iter([
                Ok(octet_ai::StreamEvent::Started { response_id: None }),
                Err(octet_ai::AiError::StreamProtocol(
                    octet_ai::StreamProtocolError::PrematureEof,
                )),
            ]))),
            RecoveryStep::Reply(text, delay) => Ok(Box::pin(async_stream::stream! {
                yield Ok(octet_ai::StreamEvent::Started { response_id: None });
                tokio::time::sleep(delay).await;
                yield Ok(octet_ai::StreamEvent::Finished(octet_ai::Response {
                    message: AssistantMessage { content: vec![AssistantPart::Text(text.into())], model: model.id, protocol: model.protocol },
                    stop_reason: octet_ai::StopReason::EndTurn,
                    usage: octet_ai::Usage::default(), cost: response_cost, response_id: None,
                    deferred: None,
                    responses_output: None, diagnostics: Vec::new(),
                }));
            })),
        }
    }
}

fn operation_recovery_agent(
    steps: Vec<RecoveryStep>,
    extensions: ExtensionHost,
) -> (Agent, Arc<OperationRecoveryTransport>, tempfile::TempDir) {
    operation_recovery_agent_with_output_cap(steps, extensions, false)
}

fn operation_recovery_agent_with_output_cap(
    steps: Vec<RecoveryStep>,
    extensions: ExtensionHost,
    capped: bool,
) -> (Agent, Arc<OperationRecoveryTransport>, tempfile::TempDir) {
    let transport = Arc::new(OperationRecoveryTransport {
        steps: std::sync::Mutex::new(steps.into()),
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let mut model = recovery_codex_model("http://127.0.0.1:1/");
    if capped {
        Arc::make_mut(&mut model.endpoint).runtime.responses_profile =
            octet_ai::ResponsesRuntimeProfile::Default;
    }
    let client = AiClient::new();
    client.register_host_stream_transport(model.endpoint.id.clone(), transport.clone());
    let workspace = tempfile::tempdir().unwrap();
    let agent = Agent::new(AgentConfig {
        client,
        model,
        session: Session::create(workspace.path().join("session.jsonl")).unwrap(),
        system: "system".into(),
        sandbox: SandboxConfig::new(workspace.path()),
        effect_broker: EffectBroker::default(),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    (agent, transport, workspace)
}

#[tokio::test(start_paused = true)]
async fn auxiliary_gate_healthy_reconnected_body_outlives_outage_deadline() {
    let (mut agent, transport, _workspace) = operation_recovery_agent(
        vec![
            RecoveryStep::Reply("candidate", Duration::ZERO),
            RecoveryStep::Offline,
            RecoveryStep::Reply("R", Duration::from_secs(30)),
        ],
        ExtensionHost::new(),
    );
    agent.set_completion_policy(CompletionPolicy::TerminalGate);
    agent.set_max_network_wait(Some(Duration::from_secs(10)));
    let start = tokio::time::Instant::now();
    let result = agent.complete("finish").await.unwrap();
    assert!(matches!(result.reason, FinishReason::Completed));
    assert!(start.elapsed() >= Duration::from_secs(34));
    assert_eq!(transport.requests.lock().unwrap().len(), 3);
    assert!(!agent.session().has_uncertain_usage());
}

#[tokio::test(start_paused = true)]
async fn auxiliary_gate_services_more_than_channel_capacity_and_delivers_before_return() {
    let (mut agent, transport, _workspace) = operation_recovery_agent(
        vec![
            RecoveryStep::Reply("candidate", Duration::ZERO),
            RecoveryStep::Offline,
            RecoveryStep::Reply("R", Duration::from_secs(30)),
            RecoveryStep::Reply("steered", Duration::ZERO),
            RecoveryStep::Reply("followed up", Duration::ZERO),
            RecoveryStep::Reply("R", Duration::ZERO),
        ],
        ExtensionHost::new(),
    );
    agent.set_completion_policy(CompletionPolicy::TerminalGate);
    let mut run = agent.prompt("finish").await.unwrap();
    let control = run.control();
    let mut sender = None;
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if matches!(event, AgentEvent::ProviderOperationRetry { .. }) {
            let control = control.clone();
            sender = Some(tokio::spawn(async move {
                for index in 0..12 {
                    control
                        .steer(format!("aux-steer-{index}-sentinel"))
                        .await
                        .unwrap();
                }
                control.follow_up("aux-followup-sentinel").await.unwrap();
                control.finish_now("aux-finish-sentinel").await.unwrap();
            }));
        }
        events.push(event);
    }
    sender.unwrap().await.unwrap();
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    let delivered: usize = events
        .iter()
        .map(|event| match event {
            AgentEvent::SteeringDelivered { messages }
            | AgentEvent::FollowUpDelivered { messages } => messages.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(delivered, 14);
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 6);
    for request in requests.iter().skip(3) {
        assert!(request.tools.is_empty());
        assert_eq!(request.tool_choice, octet_ai::ToolChoice::None);
    }
    let durable = std::fs::read_to_string(agent.session().path()).unwrap();
    for index in 0..12 {
        assert_eq!(
            durable
                .matches(&format!("aux-steer-{index}-sentinel"))
                .count(),
            1
        );
    }
}

struct AuxiliaryRetryAdvice(octet_agent::ProviderRetryAdvice);
#[async_trait::async_trait]
impl octet_agent::ProviderRetryHook for AuxiliaryRetryAdvice {
    async fn provider_retry(
        &self,
        context: &octet_agent::ProviderRetryContext,
    ) -> octet_agent::ProviderRetryAdvice {
        assert_eq!(
            context.operation,
            Some(octet_agent::ProviderOperation::TerminalGate)
        );
        assert_eq!(context.max_attempts, Some(11));
        assert!(context.run_id.starts_with("run:"));
        self.0
    }
}
#[tokio::test(start_paused = true)]
async fn auxiliary_gate_retry_hook_stop_and_bounded_delay_are_honored() {
    for stop in [true, false] {
        let mut extensions = ExtensionHost::new();
        extensions.provider_retry_hook(AuxiliaryRetryAdvice(if stop {
            octet_agent::ProviderRetryAdvice::Stop
        } else {
            octet_agent::ProviderRetryAdvice::Delay {
                additional: Duration::from_secs(500),
            }
        }));
        let (mut agent, transport, _workspace) = operation_recovery_agent(
            vec![
                RecoveryStep::Reply("candidate", Duration::ZERO),
                RecoveryStep::Interrupted,
                RecoveryStep::Reply("R", Duration::ZERO),
            ],
            extensions,
        );
        agent.set_completion_policy(CompletionPolicy::TerminalGate);
        let start = tokio::time::Instant::now();
        let result = agent.complete("finish").await;
        assert_eq!(result.is_err(), stop, "{result:?}");
        assert_eq!(
            transport.requests.lock().unwrap().len(),
            if stop { 2 } else { 3 }
        );
        if !stop {
            assert!(start.elapsed() >= Duration::from_secs(5));
            assert!(start.elapsed() < Duration::from_secs(6));
        }
        assert!(agent.session().has_uncertain_usage());
    }
}

#[tokio::test(start_paused = true)]
async fn main_and_auxiliary_outage_limit_waits_until_actual_deadline() {
    for auxiliary in [false, true] {
        let mut steps = Vec::new();
        if auxiliary {
            steps.push(RecoveryStep::Reply("candidate", Duration::ZERO));
        }
        steps.push(RecoveryStep::Offline);
        let (mut agent, transport, _workspace) =
            operation_recovery_agent(steps, ExtensionHost::new());
        if auxiliary {
            agent.set_completion_policy(CompletionPolicy::TerminalGate);
        }
        agent.set_max_network_wait(Some(Duration::from_secs(1)));
        let start = tokio::time::Instant::now();
        assert!(matches!(
            agent.complete("finish").await,
            Err(octet_agent::AgentError::NetworkWaitLimit { .. })
        ));
        assert_eq!(start.elapsed(), Duration::from_secs(1));
        assert_eq!(
            transport.requests.lock().unwrap().len(),
            if auxiliary { 2 } else { 1 }
        );
        assert!(!agent.session().has_uncertain_usage());
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_codex_postgeneration_rate_limit_honors_retry_hint() {
    let error = "data: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"Please try again in 11.054s.\"}\n\n";
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("text") + error,
        responses_text_turn("ok", "recovered", "response.completed", "accepted"),
    ])
    .await;
    let mut run = agent.prompt("finish").await.unwrap();
    let events = collect_virtual_recovery(&mut run).await;
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert!(events.iter().any(|event| matches!(event, AgentEvent::ProviderRetry { delay, .. } if *delay == Duration::from_millis(11_054))));
    assert_eq!(wire_requests(&server).await.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn auxiliary_local_compaction_recovery_delivers_held_controls_before_main_request() {
    let (mut agent, transport, workspace) = operation_recovery_agent(
        vec![
            RecoveryStep::Offline,
            RecoveryStep::Reply("compacted summary", Duration::from_secs(30)),
            RecoveryStep::Reply("answer", Duration::ZERO),
        ],
        ExtensionHost::new(),
    );
    agent
        .replace_session_at_idle(session_with_authoritative_pressure(
            &workspace.path().join("pressure.jsonl"),
            180_000,
        ))
        .unwrap();
    agent
        .set_compaction_token_mode(octet_agent::AgentCompactionMode::Local, 0.85, 1)
        .unwrap();
    agent.set_max_network_wait(Some(Duration::from_secs(10)));
    let mut run = agent.prompt("new work").await.unwrap();
    let control = run.control();
    let mut events = Vec::new();
    let mut sender = None;
    while let Some(event) = run.next().await {
        if matches!(
            event,
            AgentEvent::ProviderOperationRetry {
                operation: octet_agent::ProviderOperation::LocalCompaction,
                ..
            }
        ) {
            let control = control.clone();
            sender = Some(tokio::spawn(async move {
                for index in 0..12 {
                    control
                        .steer(format!("compact-steer-{index}-sentinel"))
                        .await
                        .unwrap();
                }
                control.finish_now("compact-finish-sentinel").await.unwrap();
            }));
        }
        events.push(event);
    }
    sender.expect("compaction was retried").await.unwrap();
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::Compaction))
            .count(),
        1
    );
    let requests = transport.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    let main = serde_json::to_string(&requests[2].messages).unwrap();
    for index in 0..12 {
        assert_eq!(
            main.matches(&format!("compact-steer-{index}-sentinel"))
                .count(),
            1
        );
    }
    assert!(requests[2].tools.is_empty());
    assert_eq!(requests[2].tool_choice, octet_ai::ToolChoice::None);
}

struct CompactRejectedOnce(AtomicUsize);
impl wiremock::Respond for CompactRejectedOnce {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            ResponseTemplate::new(503)
                .set_body_json(serde_json::json!({"error": {"code": "server_error"}}))
        } else {
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "output": [{"type": "compaction", "id": "compact-checkpoint", "encrypted_content": "opaque-checkpoint"}],
                "usage": {"input_tokens": 10, "output_tokens": 2}
            }))
        }
    }
}
#[tokio::test]
async fn manual_native_compaction_uses_safe_recovery_before_one_checkpoint() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![responses_text_turn(
        "main",
        "prior answer",
        "response.completed",
        "main-opaque",
    )])
    .await;
    Mock::given(method("POST"))
        .and(path("responses/compact"))
        .respond_with(CompactRejectedOnce(AtomicUsize::new(0)))
        .expect(2)
        .mount(&server)
        .await;
    agent.complete("initial task").await.unwrap();
    let result = agent.compact_responses_native().await.unwrap();
    assert!(matches!(
        result.kind,
        octet_agent::CompactionKind::NativeResponses { .. }
    ));
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::Compaction))
            .count(),
        1
    );
    assert!(agent.session().has_uncertain_usage());
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1], requests[2],
        "immutable native snapshot must survive recovery"
    );
}

struct HttpAdmissionScript {
    status: u16,
    http_failures: usize,
    eof_failures: usize,
    calls: AtomicUsize,
}
impl wiremock::Respond for HttpAdmissionScript {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call < self.http_failures {
            ResponseTemplate::new(self.status).set_body_json(serde_json::json!({"error": {"code": "server_error", "message": "gateway unavailable"}}))
        } else if call < self.http_failures + self.eof_failures {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(interrupted_responses_prefix("text"))
        } else {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(responses_text_turn(
                    "ok",
                    "recovered",
                    "response.completed",
                    "accepted",
                ))
        }
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_http_admission_and_stream_budgets_are_independent_and_cumulatively_finite() {
    for status in [503, 520] {
        for (http_failures, eof_failures, requests, succeeds) in [
            (6, 0, 7, true),
            (11, 0, 12, true),
            (28, 0, 29, true),
            (20, 1, 22, true),
            (29, 0, 30, true),
            (30, 0, 30, false),
            (24, 11, 36, true),
            (25, 11, 36, false),
            (20, 12, 32, false),
        ] {
            let (mut agent, server, _workspace, _) = recovery_harness(vec![]).await;
            server.reset().await;
            Mock::given(method("POST"))
                .and(path("responses"))
                .respond_with(HttpAdmissionScript {
                    status,
                    http_failures,
                    eof_failures,
                    calls: AtomicUsize::new(0),
                })
                .mount(&server)
                .await;
            let mut run = agent
                .prompt("recover without replaying committed work")
                .await
                .unwrap();
            let events = collect_virtual_recovery(&mut run).await;
            drop(run);
            assert_eq!(
                matches!(assert_single_run_finished(&events), FinishReason::Completed),
                succeeds,
                "http={http_failures} eof={eof_failures} {events:?}"
            );
            assert_eq!(
                wire_requests(&server).await.len(),
                requests,
                "http={http_failures} eof={eof_failures}"
            );
            assert_eq!(
                agent.session().usage_uncertainty_records().len(),
                requests - usize::from(succeeds)
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::ProviderUsageUncertain))
                    .count(),
                1
            );
            for event in &events {
                if let AgentEvent::ProviderRetry {
                    attempt,
                    max_attempts,
                    ..
                } = event
                {
                    assert!(*attempt <= *max_attempts);
                    assert!(*max_attempts <= 35);
                }
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_http_503_hard_budget_and_permanent_rejections_never_spend_admission_budget() {
    for (status, code, hard_budget) in [
        (500, "server_error", true),
        (502, "server_error", true),
        (503, "server_error", true),
        (504, "server_error", true),
        (520, "server_error", true),
        (520, "cyber_policy", false),
        (520, "slow_down", false),
        (408, "request_timeout", true),
        (503, "insufficient_quota", false),
        (429, "insufficient_quota", false),
        (401, "invalid_api_key", false),
    ] {
        let (mut agent, server, _workspace, _) =
            recovery_harness_with_output_cap(vec![], hard_budget).await;
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("responses"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(serde_json::json!({"error":{"code":code}})),
            )
            .mount(&server)
            .await;
        if hard_budget {
            agent.set_max_session_cost_microdollars(Some(u64::MAX));
        }
        let mut run = agent
            .prompt("do not exceed hard budget or retry permanent failures")
            .await
            .unwrap();
        let events = collect_virtual_recovery(&mut run).await;
        drop(run);
        assert!(
            matches!(assert_single_run_finished(&events), FinishReason::Failed(_)),
            "{events:?}"
        );
        let requests = wire_requests(&server).await;
        assert_eq!(requests.len(), 1);
        if hard_budget {
            assert!(requests[0]["max_output_tokens"].as_u64().is_some());
        }
        assert!(!events
            .iter()
            .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
        if status == 503 {
            assert!(agent.session().has_uncertain_usage());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn unpriced_history_blocks_auxiliary_cost_reservation_before_dispatch() {
    let (mut agent, transport, workspace) =
        operation_recovery_agent(Vec::new(), ExtensionHost::new());
    agent
        .replace_session_at_idle(session_with_authoritative_pressure(
            &workspace.path().join("unpriced-pressure.jsonl"),
            180_000,
        ))
        .unwrap();
    agent
        .set_compaction_token_mode(octet_agent::AgentCompactionMode::Local, 0.85, 1)
        .unwrap();
    agent.set_max_session_cost_microdollars(Some(u64::MAX));
    let error = agent
        .complete("cannot price historical exposure")
        .await
        .unwrap_err();
    assert!(
        matches!(error, octet_agent::AgentError::CostUnavailable { .. }),
        "{error:?}"
    );
    assert!(transport.requests.lock().unwrap().is_empty());
    assert!(agent.session().has_unpriced_usage());
    assert!(
        !agent.session().has_uncertain_usage(),
        "known tokens without pricing are not unknown token usage"
    );
}

#[tokio::test(start_paused = true)]
async fn auxiliary_gate_and_local_http_admission_preserve_stream_budget_and_uncertainty() {
    for status in [503, 520] {
        for gate in [false, true] {
            for hard_budget in [false, true] {
                let mut steps = Vec::new();
                if gate {
                    steps.push(RecoveryStep::Reply("candidate", Duration::ZERO));
                }
                steps.extend((0..20).map(|_| RecoveryStep::Http(status)));
                steps.push(RecoveryStep::Interrupted);
                steps.push(RecoveryStep::Reply(
                    if gate { "R" } else { "summary" },
                    Duration::ZERO,
                ));
                if !gate {
                    steps.push(RecoveryStep::Reply("answer", Duration::ZERO));
                }
                let (mut agent, transport, workspace) = operation_recovery_agent_with_output_cap(
                    steps,
                    ExtensionHost::new(),
                    hard_budget,
                );
                if gate {
                    agent.set_completion_policy(CompletionPolicy::TerminalGate);
                } else {
                    agent
                        .replace_session_at_idle(session_with_authoritative_pressure_and_pricing(
                            &workspace.path().join("pressure.jsonl"),
                            180_000,
                            agent.model().spec.pricing.as_ref(),
                        ))
                        .unwrap();
                    agent
                        .set_compaction_token_mode(octet_agent::AgentCompactionMode::Local, 0.85, 1)
                        .unwrap();
                }
                assert!(
                    !agent.session().has_unpriced_usage(),
                    "this fixture must reach HTTP admission, not stop at pricing preflight"
                );
                if hard_budget {
                    agent.set_max_session_cost_microdollars(Some(u64::MAX));
                }
                let mut run = agent.prompt("recover auxiliary").await.unwrap();
                let events = collect(&mut run).await;
                drop(run);
                assert_eq!(
                    matches!(assert_single_run_finished(&events), FinishReason::Completed),
                    !hard_budget,
                    "gate={gate} hard={hard_budget} {events:?}"
                );
                assert_eq!(
                    transport.requests.lock().unwrap().len(),
                    if hard_budget {
                        1 + usize::from(gate)
                    } else {
                        23
                    }
                );
                assert_eq!(
                    agent.session().usage_uncertainty_records().len(),
                    if hard_budget { 1 } else { 21 }
                );
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| matches!(event, AgentEvent::ProviderUsageUncertain))
                        .count(),
                    1
                );
            }
        }
    }
}

#[tokio::test]
async fn manual_native_with_hard_budget_refuses_uncapped_dispatch() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![responses_text_turn(
        "main",
        "prior answer",
        "response.completed",
        "main-opaque",
    )])
    .await;
    Mock::given(method("POST"))
        .and(path("responses/compact"))
        .respond_with(ResponseTemplate::new(503))
        .expect(0)
        .mount(&server)
        .await;
    agent.complete("initial task").await.unwrap();
    agent.set_max_session_cost_microdollars(Some(u64::MAX));
    assert!(matches!(
        agent.compact_responses_native().await,
        Err(octet_agent::AgentError::OutputLimitUnavailable)
    ));
    assert!(!agent.session().has_uncertain_usage());
    assert_eq!(
        wire_requests(&server).await.len(),
        1,
        "only the earlier unbounded main answer was dispatched"
    );
}

struct HeldOutageRetryHook(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl octet_agent::ProviderRetryHook for HeldOutageRetryHook {
    async fn provider_retry(
        &self,
        context: &octet_agent::ProviderRetryContext,
    ) -> octet_agent::ProviderRetryAdvice {
        assert_eq!(
            context.kind,
            octet_agent::ProviderRetryKind::WaitingForNetwork
        );
        self.0.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

#[tokio::test(start_paused = true)]
async fn main_and_auxiliary_outage_deadline_preempts_pending_retry_hooks() {
    for auxiliary in [false, true] {
        let calls = Arc::new(AtomicUsize::new(0));
        let mut extensions = ExtensionHost::new();
        extensions.provider_retry_hook(HeldOutageRetryHook(calls.clone()));
        let mut steps = Vec::new();
        if auxiliary {
            steps.push(RecoveryStep::Reply("candidate", Duration::ZERO));
        }
        steps.push(RecoveryStep::Offline);
        let (mut agent, transport, _workspace) = operation_recovery_agent(steps, extensions);
        if auxiliary {
            agent.set_completion_policy(CompletionPolicy::TerminalGate);
        }
        let limit = Duration::from_millis(10);
        agent.set_max_network_wait(Some(limit));
        let started = tokio::time::Instant::now();
        assert!(matches!(
            agent.complete("finish").await,
            Err(octet_agent::AgentError::NetworkWaitLimit { .. })
        ));
        assert_eq!(started.elapsed(), limit);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            transport.requests.lock().unwrap().len(),
            1 + usize::from(auxiliary)
        );
        assert!(!agent.session().has_uncertain_usage());
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_opening_transport_has_thirty_attempt_envelope_without_indefinite_waits() {
    for phase in [
        octet_ai::TransportPhase::Connect,
        octet_ai::TransportPhase::ResponseHeaders,
    ] {
        for (opening_failures, body_failures, expected, success) in
            [(29, 0, 30, true), (30, 0, 30, false), (20, 1, 22, true)]
        {
            for auxiliary in [false, true] {
                let mut steps = Vec::new();
                if auxiliary {
                    steps.push(RecoveryStep::Reply("candidate", Duration::ZERO));
                }
                steps.extend((0..opening_failures).map(|_| RecoveryStep::Opening(phase)));
                steps.extend((0..body_failures).map(|_| RecoveryStep::Interrupted));
                steps.push(RecoveryStep::Reply(
                    if auxiliary { "R" } else { "answer" },
                    Duration::ZERO,
                ));
                let (mut agent, transport, _workspace) =
                    operation_recovery_agent(steps, ExtensionHost::new());
                if auxiliary {
                    agent.set_completion_policy(CompletionPolicy::TerminalGate);
                }
                let mut run = agent.prompt("recover").await.unwrap();
                let events = collect(&mut run).await;
                drop(run);
                assert_eq!(
                    matches!(assert_single_run_finished(&events), FinishReason::Completed),
                    success,
                    "{phase:?} {events:?}"
                );
                assert_eq!(
                    transport.requests.lock().unwrap().len(),
                    expected + usize::from(auxiliary)
                );
                assert!(!events.iter().any(|event| matches!(
                    event,
                    AgentEvent::ProviderWaitingForNetwork { .. }
                        | AgentEvent::ProviderOperationRetry {
                            max_attempts: None,
                            ..
                        }
                )));
                assert_eq!(
                    agent.session().has_uncertain_usage(),
                    phase == octet_ai::TransportPhase::ResponseHeaders || body_failures > 0
                );
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_provider_stream_json_recovery_never_dispatches_provisional_tools() {
    for malformed in [
        "data: {invalid JSON}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"content_index\":0,\"delta\":7}\n\n",
    ] {
        for hard_budget in [false, true] {
            let (mut agent, server, _workspace, _) = recovery_harness_with_output_cap(vec![
                interrupted_responses_prefix("tool") + malformed,
                responses_text_turn("ok", "recovered", "response.completed", "accepted"),
            ], hard_budget).await;
            if hard_budget { agent.set_max_session_cost_microdollars(Some(u64::MAX)); }
            let mut run = agent.prompt("recover malformed provider frame").await.unwrap();
            let events = collect_virtual_recovery(&mut run).await;
            drop(run);
            assert_eq!(matches!(assert_single_run_finished(&events), FinishReason::Completed), !hard_budget, "{events:?}");
            assert_eq!(wire_requests(&server).await.len(), if hard_budget { 1 } else { 2 });
            assert_eq!(agent.session().usage_uncertainty_records().len(), 1, "{events:?}");
            assert!(!events.iter().any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn qualified_malformed_provider_json_exhausts_finite_stream_budget() {
    let (mut agent, server, _workspace, _) =
        recovery_harness(vec!["data: {invalid JSON}\n\n".into()]).await;
    let mut run = agent.prompt("bounded malformed recovery").await.unwrap();
    let events = collect_virtual_recovery(&mut run).await;
    drop(run);
    assert!(
        matches!(
            assert_single_run_finished(&events),
            FinishReason::Failed(octet_agent::AgentError::ProviderRecovery {
                retries: 11,
                usage_unknown: true,
                ..
            })
        ),
        "{events:?}"
    );
    assert_eq!(wire_requests(&server).await.len(), 12);
}

async fn submit_gate_boundary_control(control: &RunControl, kind: u8) {
    match kind {
        0 => control.steer("final-boundary-sentinel").await.unwrap(),
        1 => control.follow_up("final-boundary-sentinel").await.unwrap(),
        2 => control.finish_now("final-boundary-sentinel").await.unwrap(),
        _ => unreachable!(),
    }
}

#[tokio::test(start_paused = true)]
async fn terminal_gate_final_poll_and_turn_finished_submission_boundaries_preserve_controls() {
    for final_poll in [false, true] {
        for kind in 0..3 {
            let holder = Arc::new(std::sync::Mutex::new(None));
            let gate = if final_poll {
                RecoveryStep::GateControl(holder.clone(), kind)
            } else {
                RecoveryStep::Reply("R", Duration::ZERO)
            };
            let (mut agent, transport, _workspace) = operation_recovery_agent(
                vec![
                    RecoveryStep::Reply("first candidate", Duration::ZERO),
                    gate,
                    RecoveryStep::Reply("continued", Duration::ZERO),
                    RecoveryStep::Reply("R", Duration::ZERO),
                ],
                ExtensionHost::new(),
            );
            agent.set_completion_policy(CompletionPolicy::TerminalGate);
            let mut run = agent.prompt("finish").await.unwrap();
            let control = run.control();
            *holder.lock().unwrap() = Some(control.clone());
            let mut submitted = final_poll;
            let mut events = Vec::new();
            while let Some(event) = run.next().await {
                if !submitted && matches!(event, AgentEvent::TurnFinished { .. }) {
                    submit_gate_boundary_control(&control, kind).await;
                    submitted = true;
                }
                events.push(event);
            }
            assert!(matches!(
                control.steer("too late").await,
                Err(octet_agent::AgentError::RunEnded)
            ));
            drop(run);
            assert!(
                matches!(assert_single_run_finished(&events), FinishReason::Completed),
                "final_poll={final_poll} kind={kind} {events:?}"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, AgentEvent::TurnFinished { .. }))
                    .count(),
                2
            );
            let emitted_costs = events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::TurnFinished { turn_cost, .. } => Some(*turn_cost),
                    _ => None,
                })
                .collect::<Vec<_>>();
            let durable_costs = agent
                .session()
                .usage_records()
                .iter()
                .filter_map(|record| match record.kind {
                    UsageRecordKind::AssistantTurn { .. } => Some(record.cost),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(
                emitted_costs, durable_costs,
                "control admission must retain each immutable assistant cost"
            );
            let delivered: usize = events
                .iter()
                .map(|event| match event {
                    AgentEvent::SteeringDelivered { messages }
                    | AgentEvent::FollowUpDelivered { messages } => messages.len(),
                    _ => 0,
                })
                .sum();
            assert_eq!(delivered, 1);
            assert_eq!(transport.requests.lock().unwrap().len(), 4);
            let durable = std::fs::read_to_string(agent.session().path()).unwrap();
            assert_eq!(durable.matches("final-boundary-sentinel").count(), 1);
            assert!(!durable.contains("too late"));
        }
    }
}

struct InvalidUtf8ThenSuccess(AtomicUsize);
impl wiremock::Respond for InvalidUtf8ThenSuccess {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        let body = if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            let mut bytes = interrupted_responses_prefix("tool").into_bytes();
            bytes.extend_from_slice(b"data: \xff\n\n");
            bytes
        } else {
            responses_text_turn("ok", "recovered", "response.completed", "accepted").into_bytes()
        };
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_bytes(body)
    }
}
#[tokio::test(start_paused = true)]
async fn qualified_codec_wrapped_utf8_failure_recovers_without_provisional_effects() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![]).await;
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(InvalidUtf8ThenSuccess(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let mut run = agent.prompt("recover malformed SSE bytes").await.unwrap();
    let events = collect_virtual_recovery(&mut run).await;
    drop(run);
    assert!(
        matches!(assert_single_run_finished(&events), FinishReason::Completed),
        "{events:?}"
    );
    assert_eq!(wire_requests(&server).await.len(), 2);
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
}

#[tokio::test]
async fn uncertainty_persistence_failure_still_warns_before_terminal_failure() {
    use std::io::Write;
    let (mut agent, _server, _workspace, path) =
        recovery_harness(vec![interrupted_responses_prefix("text")]).await;
    let mut run = agent.prompt("observe interrupted work").await.unwrap();
    let mut injected = false;
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        if !injected && matches!(event, AgentEvent::OutputDelta { .. }) {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"\n")
                .unwrap();
            injected = true;
        }
        events.push(event);
    }
    drop(run);
    assert!(injected);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Failed(octet_agent::AgentError::Session(_))
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ProviderUsageUncertain))
            .count(),
        1
    );
    assert!(!events
        .iter()
        .any(|event| matches!(event, AgentEvent::ProviderRetry { .. })));
}

#[tokio::test(start_paused = true)]
async fn qualified_unknown_responses_terminals_replace_partial_without_tool_replay() {
    for incomplete in [false, true] {
        for (code, succeeds) in [
            ("future_unknown_reason", true),
            ("cyber_policy", false),
            ("bio_policy", false),
            ("invalid_prompt", false),
            ("misalignment_policy_violation", false),
            ("server_is_overloaded", false),
            ("slow_down", false),
            ("insufficient_quota", false),
            ("invalid_api_key", false),
        ] {
            let failed = if incomplete {
                responses_tool_turn("failed", "failed-call")
                    .replace("response.completed", "response.incomplete")
                    .replace(
                        "\"usage\":",
                        &format!("\"incomplete_details\":{{\"reason\":\"{code}\"}},\"usage\":"),
                    )
            } else {
                interrupted_responses_prefix("tool")
                    + &format!(
                        "data: {}\n\n",
                        serde_json::json!({
                            "type": "response.failed", "response": {"error": {"code": code, "message": "try again"}}
                        })
                    )
            };
            let (mut agent, server, _workspace, session_path) = recovery_harness(vec![
                failed,
                responses_text_turn("ok", "recovered", "response.completed", "accepted"),
            ])
            .await;
            let mut run = agent
                .prompt("recover only unfinished generation")
                .await
                .unwrap();
            let events = collect_virtual_recovery(&mut run).await;
            drop(run);
            assert_eq!(
                matches!(assert_single_run_finished(&events), FinishReason::Completed),
                succeeds,
                "incomplete={incomplete} code={code} {events:?}"
            );
            assert_eq!(
                wire_requests(&server).await.len(),
                if succeeds { 2 } else { 1 }
            );
            assert!(!events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolStarted { .. })));
            assert!(!std::fs::read_to_string(session_path)
                .unwrap()
                .contains("failed-call"));
            assert!(agent.session().has_uncertain_usage());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn natural_turn_finished_submission_boundary_preserves_controls() {
    for kind in 0..3 {
        let (mut agent, transport, _workspace) = operation_recovery_agent(
            vec![
                RecoveryStep::Reply("first candidate", Duration::ZERO),
                RecoveryStep::Reply("continued", Duration::ZERO),
            ],
            ExtensionHost::new(),
        );
        let mut run = agent.prompt("finish").await.unwrap();
        let control = run.control();
        let mut submitted = false;
        let mut events = Vec::new();
        while let Some(event) = run.next().await {
            if !submitted && matches!(event, AgentEvent::TurnFinished { .. }) {
                submit_gate_boundary_control(&control, kind).await;
                submitted = true;
            }
            events.push(event);
        }
        assert!(matches!(
            control.steer("too late").await,
            Err(octet_agent::AgentError::RunEnded)
        ));
        drop(run);
        assert!(matches!(
            assert_single_run_finished(&events),
            FinishReason::Completed
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
        if kind == 2 {
            let requests = transport.requests.lock().unwrap();
            assert!(requests[1].tools.is_empty());
            assert_eq!(requests[1].tool_choice, octet_ai::ToolChoice::None);
        }
        let durable = std::fs::read_to_string(agent.session().path()).unwrap();
        assert_eq!(durable.matches("final-boundary-sentinel").count(), 1);
        assert!(!durable.contains("too late"));
    }
}

struct NativeOutageCredential {
    calls: AtomicUsize,
    hold_opening: bool,
}
#[async_trait::async_trait]
impl octet_ai::CredentialResolver for NativeOutageCredential {
    async fn resolve(&self) -> Result<octet_ai::ResolvedCredential, octet_ai::AuthError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(octet_ai::AuthError::Unavailable);
        }
        if self.hold_opening {
            std::future::pending::<()>().await;
        }
        Ok(octet_ai::ResolvedCredential {
            scheme: octet_ai::CredentialScheme::Bearer,
            value: "synthetic".into(),
            extra_headers: http::HeaderMap::new(),
        })
    }
}

// Drive real loopback I/O without Tokio jumping straight to provider deadlines.
async fn drive_native_virtual<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::pin!(future);
    for _ in 0..10_000 {
        for _ in 0..100 {
            if let std::task::Poll::Ready(result) = futures_util::poll!(&mut future) {
                return result;
            }
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_millis(50)).await;
    }
    panic!("native recovery failed to settle within 500 virtual seconds");
}

#[tokio::test(start_paused = true)]
async fn native_compaction_calls_bound_reopening_but_not_healthy_reconnected_body() {
    for autonomous in [false, true] {
        for held_phase in ["credential", "headers", "body"] {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let uri = format!("http://{}", listener.local_addr().unwrap());
            let compact_requests = Arc::new(AtomicUsize::new(0));
            let server_requests = compact_requests.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    let (body_start, content_length) = loop {
                        let count = socket.read(&mut buffer).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..count]);
                        let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") else {
                            continue;
                        };
                        let start = end + 4;
                        let length = String::from_utf8_lossy(&request[..start])
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or_default();
                        break (start, length);
                    };
                    while request.len() - body_start < content_length {
                        let count = socket.read(&mut buffer).await.unwrap();
                        if count == 0 {
                            return;
                        }
                        request.extend_from_slice(&buffer[..count]);
                    }
                    let compact = request.starts_with(b"POST /responses/compact ");
                    let body = if compact {
                        server_requests.fetch_add(1, Ordering::SeqCst);
                        if held_phase == "headers" {
                            std::future::pending::<()>().await;
                        }
                        serde_json::json!({
                            "output": [{"type":"compaction", "id":"bounded-native", "encrypted_content":"opaque"}],
                            "usage": {"input_tokens": 10, "output_tokens": 2}
                        }).to_string()
                    } else {
                        responses_text_turn("final", "done", "response.completed", "accepted")
                    };
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", if compact {"application/json"} else {"text/event-stream"}, body.len()).as_bytes()).await.unwrap();
                    if compact {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    }
                    socket.write_all(body.as_bytes()).await.unwrap();
                    socket.shutdown().await.unwrap();
                }
            });
            let workspace = tempfile::tempdir().unwrap();
            let mut model = recovery_codex_model(&uri);
            let credentials = Arc::new(NativeOutageCredential {
                calls: AtomicUsize::new(0),
                hold_opening: held_phase == "credential",
            });
            Arc::make_mut(&mut model.endpoint).auth = Auth::dynamic(credentials.clone());
            Arc::make_mut(&mut model.endpoint).timeout = Duration::from_secs(60);
            let mut session = Session::create(workspace.path().join("session.jsonl")).unwrap();
            session
                .append(EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::Text("prior task".into())],
                })))
                .unwrap();
            session.append_assistant_turn_with_metadata(
                AssistantMessage { content: vec![AssistantPart::Text("prior answer".into())], model: model.spec.id.clone(), protocol: Protocol::OpenAiResponses },
                model.endpoint.id.clone(), model.spec.id.clone(),
                Usage { input_tokens: 180_000, total_tokens: 180_000, ..Usage::default() },
                None, octet_ai::StopReason::EndTurn,
                Some(octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":"prior answer"}]})).unwrap()])), None,
            ).unwrap();
            let mut agent = build_responses_agent_from_session(
                model,
                session,
                workspace.path(),
                Some(4),
                "system",
                ReasoningConfig::Off,
            );
            agent.set_max_network_wait(Some(Duration::from_secs(10)));
            let start = tokio::time::Instant::now();
            let result = if autonomous {
                agent
                    .set_compaction_token_mode(
                        octet_agent::AgentCompactionMode::NativeResponses,
                        0.85,
                        1,
                    )
                    .unwrap();
                let output = drive_native_virtual(agent.complete("continue")).await;
                output.and_then(|output| match output.reason {
                    FinishReason::Completed => Ok(()),
                    FinishReason::Failed(error) => Err(error),
                    other => panic!("unexpected native outcome {other:?}"),
                })
            } else {
                drive_native_virtual(agent.compact_responses_native())
                    .await
                    .map(|_| ())
            };
            if held_phase == "body" {
                assert!(result.is_ok(), "autonomous={autonomous} {result:?}");
                assert!(start.elapsed() >= Duration::from_secs(34));
            } else {
                assert!(
                    matches!(
                        result,
                        Err(octet_agent::AgentError::NetworkWaitLimit { .. })
                    ),
                    "autonomous={autonomous} phase={held_phase} {result:?}"
                );
                assert!(start.elapsed() >= Duration::from_secs(10));
                assert!(start.elapsed() < Duration::from_secs(11));
            }
            assert_eq!(
                compact_requests.load(Ordering::SeqCst),
                usize::from(held_phase != "credential")
            );
            assert_eq!(
                agent
                    .session()
                    .usage_records()
                    .iter()
                    .filter(|record| matches!(record.kind, UsageRecordKind::Compaction))
                    .count(),
                usize::from(held_phase == "body")
            );
            assert_eq!(
                agent.session().has_uncertain_usage(),
                held_phase == "headers"
            );
            if held_phase == "headers" {
                let path = agent.session().path().to_owned();
                drop(agent);
                let mut resumed = build_responses_agent_from_session(
                    recovery_codex_model(&uri),
                    Session::open(&path).unwrap(),
                    workspace.path(),
                    Some(4),
                    "system",
                    ReasoningConfig::Off,
                );
                resumed.set_max_session_cost_microdollars(Some(u64::MAX));
                assert!(matches!(
                    resumed.complete("bounded resume").await,
                    Err(octet_agent::AgentError::UsageUncertain)
                ));
                assert_eq!(compact_requests.load(Ordering::SeqCst), 1);
            }
            server.abort();
        }
    }
}

#[tokio::test(start_paused = true)]
async fn unknown_responses_terminals_have_eleven_replacements_in_main_and_auxiliary_calls() {
    for operation in ["main", "gate", "local"] {
        let gate = operation == "gate";
        let local = operation == "local";
        for incomplete in [false, true] {
            for failures in [11, 12] {
                let failed = if incomplete {
                    responses_text_turn("failed", "partial", "response.incomplete", "failed-opaque")
                        .replace("max_output_tokens", "future_unknown_reason")
                } else {
                    interrupted_responses_prefix("text")
                        + &format!(
                            "data: {}\n\n",
                            serde_json::json!({
                                "type":"response.failed", "response":{"error":{"code":"future_unknown_reason", "message":"unknown"}}
                            })
                        )
                };
                let mut bodies = Vec::new();
                if gate {
                    bodies.push(responses_text_turn(
                        "candidate",
                        "answer",
                        "response.completed",
                        "accepted",
                    ));
                }
                bodies.extend(std::iter::repeat_n(failed, failures));
                bodies.push(responses_text_turn(
                    "ok",
                    if gate { "R" } else { "recovered" },
                    "response.completed",
                    "accepted",
                ));
                if local {
                    bodies.push(responses_text_turn(
                        "main",
                        "done",
                        "response.completed",
                        "accepted",
                    ));
                }
                let (mut agent, server, workspace, session_path) = recovery_harness(bodies).await;
                if local {
                    agent
                        .replace_session_at_idle(session_with_authoritative_pressure(
                            &workspace.path().join("pressure.jsonl"),
                            180_000,
                        ))
                        .unwrap();
                    agent
                        .set_compaction_token_mode(octet_agent::AgentCompactionMode::Local, 0.85, 1)
                        .unwrap();
                }
                if gate {
                    agent.set_completion_policy(CompletionPolicy::TerminalGate);
                }
                let mut run = agent.prompt("bounded terminal recovery").await.unwrap();
                let events = collect_virtual_recovery(&mut run).await;
                drop(run);
                assert_eq!(
                    matches!(assert_single_run_finished(&events), FinishReason::Completed),
                    failures == 11,
                    "operation={operation} incomplete={incomplete} failures={failures} {events:?}"
                );
                assert_eq!(
                    wire_requests(&server).await.len(),
                    12 + usize::from(gate) + usize::from(local && failures == 11)
                );
                assert_eq!(agent.session().usage_uncertainty_records().len(), failures);
                assert_eq!(
                    events
                        .iter()
                        .filter(|event| if gate || local {
                            matches!(
                                event,
                                AgentEvent::ProviderOperationRetry {
                                    operation: octet_agent::ProviderOperation::TerminalGate
                                        | octet_agent::ProviderOperation::LocalCompaction,
                                    ..
                                }
                            )
                        } else {
                            matches!(event, AgentEvent::ProviderRetry { .. })
                        })
                        .count(),
                    11
                );
                assert!(!std::fs::read_to_string(session_path)
                    .unwrap()
                    .contains("failed-opaque"));
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn opening_outage_deadlines_preserve_unknown_usage_in_main_local_and_gate() {
    for operation in ["main", "local", "gate"] {
        let mut steps = Vec::new();
        if operation == "gate" {
            steps.push(RecoveryStep::Reply("candidate", Duration::ZERO));
        }
        steps.extend([RecoveryStep::Offline, RecoveryStep::HoldOpening]);
        let (mut agent, transport, workspace) =
            operation_recovery_agent(steps, ExtensionHost::new());
        if operation == "gate" {
            agent.set_completion_policy(CompletionPolicy::TerminalGate);
        } else if operation == "local" {
            agent
                .replace_session_at_idle(session_with_authoritative_pressure(
                    &workspace.path().join("pressure.jsonl"),
                    180_000,
                ))
                .unwrap();
            agent
                .set_compaction_token_mode(octet_agent::AgentCompactionMode::Local, 0.85, 1)
                .unwrap();
        }
        agent.set_max_network_wait(Some(Duration::from_secs(10)));
        let mut run = agent.prompt("recover opening").await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        assert!(
            matches!(
                assert_single_run_finished(&events),
                FinishReason::Failed(octet_agent::AgentError::NetworkWaitLimit {
                    usage_unknown: true,
                    ..
                })
            ),
            "{operation}: {events:?}"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, AgentEvent::ProviderUsageUncertain))
                .count(),
            1
        );
        assert!(agent.session().has_uncertain_usage());
        let request_count = transport.requests.lock().unwrap().len();
        assert_eq!(request_count, if operation == "gate" { 3 } else { 2 });
        let path = agent.session().path().to_owned();
        drop(agent);
        let mut resumed = build_responses_agent_from_session(
            recovery_codex_model("http://127.0.0.1:1/"),
            Session::open(&path).unwrap(),
            workspace.path(),
            Some(4),
            "system",
            ReasoningConfig::Off,
        );
        resumed.set_max_session_cost_microdollars(Some(u64::MAX));
        assert!(matches!(
            resumed.complete("bounded resume").await,
            Err(octet_agent::AgentError::UsageUncertain)
        ));
        assert_eq!(transport.requests.lock().unwrap().len(), request_count);
    }
}

// ── Row 3.5: typed provider/turn/tool/compaction/delegation spans ──────────

fn recorded_span_names(
    spans: &[octet_agent::telemetry::spans::RecordedTelemetrySpan],
) -> Vec<&str> {
    spans.iter().map(|span| span.name.as_str()).collect()
}

fn span_index(
    spans: &[octet_agent::telemetry::spans::RecordedTelemetrySpan],
    name: &str,
) -> usize {
    spans
        .iter()
        .position(|span| span.name == name)
        .unwrap_or_else(|| panic!("span {name} missing from {:?}", recorded_span_names(spans)))
}

#[tokio::test]
async fn typed_spans_nest_run_turn_provider_and_tool_boundaries() {
    use octet_agent::telemetry::spans::{AttributeValue, InMemoryTelemetryContext, SpanStatus};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[(
                    "call_span",
                    "read",
                    serde_json::json!({"path": "probe.txt"}),
                )]),
                text_turn("read it"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    std::fs::write(workspace.join("probe.txt"), b"probe contents").unwrap();
    let session_path = session_dir.path().join("span-boundaries.jsonl");
    let mut agent = build_agent(&server.uri(), &workspace, &session_path, Some(4));
    let fixture = InMemoryTelemetryContext::default();
    agent.set_telemetry_context(fixture.context());

    let mut run = agent.prompt("read the probe").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    let spans = fixture.get_spans();
    assert_eq!(
        recorded_span_names(&spans),
        vec![
            "octet.agent.run",
            "octet.agent.turn",
            "octet.ai.request",
            "octet.ai.stream",
            "octet.agent.tool",
            "octet.agent.turn",
            "octet.ai.request",
            "octet.ai.stream",
        ],
        "one run, one turn per provider turn, and a nested provider stream and tool"
    );
    assert_eq!(spans[0].parent_id, None, "the run span is the root");
    for index in [1usize, 5] {
        assert_eq!(spans[index].parent_id, Some(spans[0].id), "turns nest under the run");
    }
    for index in [2usize, 6] {
        let turn = if index == 2 { 1 } else { 5 };
        assert_eq!(spans[index].parent_id, Some(spans[turn].id));
    }
    assert_eq!(spans[3].parent_id, Some(spans[2].id), "the stream nests under its request");
    assert_eq!(spans[7].parent_id, Some(spans[6].id));
    assert_eq!(spans[4].parent_id, Some(spans[1].id), "the tool nests under its turn");

    assert!(
        spans.iter().all(|span| span.settled),
        "every boundary settles when the run finishes: {spans:#?}"
    );
    assert!(
        spans.iter().all(|span| span.status == SpanStatus::Ok),
        "a tool-continuation turn is a completed turn, not a dropped guard: {spans:#?}"
    );
    assert_eq!(
        spans[4].attributes.get("name"),
        Some(&AttributeValue::String("read".to_string())),
        "the tool span carries the registered name and never arguments"
    );
    let request = &spans[2].attributes;
    assert_eq!(request.get("input_tokens"), Some(&AttributeValue::Number(5.0)));
    assert_eq!(request.get("output_tokens"), Some(&AttributeValue::Number(3.0)));
    assert_eq!(
        request.get("has_uncertain_usage"),
        Some(&AttributeValue::Boolean(false)),
        "reported usage is recorded with its uncertainty flag"
    );

    // Settlement order follows the nested boundaries: the stream closes before
    // its request, the tool before its turn, and the run last of all.
    assert!(spans[3].end_sequence < spans[2].end_sequence);
    assert!(spans[4].end_sequence < spans[1].end_sequence);
    assert!(spans[1].end_sequence < spans[5].end_sequence);
    assert_eq!(
        spans[0].end_sequence,
        Some(spans.len() as u64),
        "the run span settles last"
    );
}

/// The same scripted failure twice: the inert context must not lose accounting,
/// and the recording context must label the failed boundaries as errors while
/// leaving the provider attempt that actually completed marked ok.
#[tokio::test]
async fn typed_spans_label_failed_runs_without_changing_accounting() {
    use octet_agent::telemetry::spans::{InMemoryTelemetryContext, SpanStatus};

    let mut observed: Vec<(String, usize, bool, usize)> = Vec::new();
    for record in [false, true] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("messages"))
            .respond_with(Script {
                bodies: vec![empty_turn()],
                next: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        let workspace_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().canonicalize().unwrap();
        let session_path = session_dir.path().join("span-failure.jsonl");
        let mut agent = build_agent(&server.uri(), &workspace, &session_path, Some(4));
        let fixture = InMemoryTelemetryContext::default();
        if record {
            agent.set_telemetry_context(fixture.context());
        }

        let mut run = agent.prompt("answer nothing").await.unwrap();
        let events = collect(&mut run).await;
        drop(run);
        let reason = format!("{:?}", assert_single_run_finished(&events));
        assert!(reason.contains("no user-visible content"), "got {reason}");
        observed.push((
            reason,
            agent.session().entries().len(),
            agent.session().has_uncertain_usage(),
            agent.session().usage_records().len(),
        ));

        if record {
            let spans = fixture.get_spans();
            assert_eq!(
                recorded_span_names(&spans),
                vec![
                    "octet.agent.run",
                    "octet.agent.turn",
                    "octet.ai.request",
                    "octet.ai.stream",
                ]
            );
            assert_eq!(spans[0].status, SpanStatus::Error, "a failed run is an error span");
            assert_eq!(spans[1].status, SpanStatus::Error, "a failed turn is an error span");
            assert_eq!(
                spans[2].status,
                SpanStatus::Ok,
                "the provider attempt itself completed"
            );
            assert_eq!(spans[3].status, SpanStatus::Ok);
            assert!(spans.iter().all(|span| span.settled));
        }
    }
    assert_eq!(
        observed[0], observed[1],
        "an installed observer must not change the durable outcome or accounting"
    );
}

/// The telemetry hard gate: the inert and recording adapters are business
/// neutral. The identical scripted two-turn run is replayed under
/// `NOOP_TELEMETRY_CONTEXT` and under the recording adapter, and their full
/// business projections — streamed deltas, finish reasons, durable entries with
/// timing stripped, usage numbers, and cost totals — must match exactly while
/// the recording adapter really recorded every boundary.
#[tokio::test]
async fn noop_and_in_memory_telemetry_keep_identical_business_outcomes() {
    use octet_agent::telemetry::spans::{InMemoryTelemetryContext, NOOP_TELEMETRY_CONTEXT};

    /// Timing-free business projection of one scripted two-turn run.
    #[derive(Debug, PartialEq)]
    struct Outcome {
        deltas: Vec<(OutputChannel, String)>,
        finishes: Vec<String>,
        entries: Vec<(String, Option<String>, String)>,
        record_types: Vec<String>,
        usage: Vec<(String, u64, u64, u64)>,
        assistant_texts: Vec<String>,
        cost_microdollars: u64,
        cost_picodollars_remainder: u32,
        uncertain: bool,
    }

    async fn run_under(context: octet_agent::telemetry::spans::TelemetryContext) -> Outcome {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("messages"))
            .respond_with(Script {
                bodies: vec![text_turn("first answer"), text_turn("second answer")],
                next: AtomicUsize::new(0),
            })
            .mount(&server)
            .await;
        let workspace_dir = tempfile::tempdir().unwrap();
        let session_dir = tempfile::tempdir().unwrap();
        let workspace = workspace_dir.path().canonicalize().unwrap();
        let session_path = session_dir.path().join("noop-vs-in-memory.jsonl");
        let mut agent = build_agent(&server.uri(), &workspace, &session_path, Some(4));
        agent.set_telemetry_context(context);

        let mut deltas = Vec::new();
        let mut finishes = Vec::new();
        for prompt in ["first prompt", "second prompt"] {
            let mut run = agent.prompt(prompt).await.unwrap();
            let events = collect(&mut run).await;
            drop(run);
            finishes.push(format!("{:?}", assert_single_run_finished(&events)));
            for event in &events {
                if let AgentEvent::OutputDelta { channel, text } = event {
                    deltas.push((*channel, text.clone()));
                }
            }
        }
        let entries = agent
            .session()
            .entries()
            .iter()
            .map(|entry| {
                let mut metadata = entry.metadata.clone();
                if let Some(metadata) = metadata.as_mut() {
                    metadata.tool_started_unix_ms = None;
                    metadata.tool_finished_unix_ms = None;
                    metadata.run_outcome = None;
                }
                (
                    entry.id.0.clone(),
                    entry.parent.as_ref().map(|parent| parent.0.clone()),
                    format!("{:?}|{:?}", entry.value, metadata),
                )
            })
            .collect::<Vec<_>>();
        let usage = agent
            .session()
            .usage_records()
            .iter()
            .map(|record| {
                (
                    format!("{:?}", record.kind),
                    record.usage.input_tokens,
                    record.usage.output_tokens,
                    record.usage.total_tokens,
                )
            })
            .collect();
        let assistant_texts = agent
            .session()
            .entries()
            .iter()
            .filter_map(|entry| match &entry.value {
                EntryValue::Message(Message::Assistant(assistant)) => Some(
                    assistant
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            AssistantPart::Text(text) => Some(text.clone()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                _ => None,
            })
            .collect();
        let outcome = Outcome {
            deltas,
            finishes,
            entries,
            record_types: Vec::new(),
            usage,
            assistant_texts,
            cost_microdollars: agent.session().total_cost_microdollars(),
            cost_picodollars_remainder: agent.session().total_cost_picodollars_remainder(),
            uncertain: agent.session().has_uncertain_usage(),
        };
        drop(agent);
        // Durable record shape, timing excluded: the two adapters must write the
        // same session log records, and a telemetry adapter must never add one.
        let record_types = std::fs::read_to_string(&session_path)
            .unwrap()
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        Outcome {
            record_types,
            ..outcome
        }
    }

    let fixture = InMemoryTelemetryContext::default();
    let recorded = run_under(fixture.context()).await;
    let inert = run_under(NOOP_TELEMETRY_CONTEXT).await;
    assert_eq!(
        inert, recorded,
        "NOOP and InMemory must keep identical business outcomes"
    );

    // The equality above is only meaningful because the run really happened and
    // the recording adapter really observed it.
    assert_eq!(inert.deltas.len(), 2, "both scripted turns streamed");
    assert_eq!(
        inert.entries.len(),
        4,
        "two prompts and two assistant turns were durably compared"
    );
    assert_eq!(inert.assistant_texts, ["first answer", "second answer"]);
    assert!(!inert.usage.is_empty(), "usage must be accounted");
    assert!(inert.record_types.iter().any(|kind| kind == "usage"));
    let spans = fixture.get_spans();
    assert!(
        !spans.is_empty() && spans.iter().all(|span| span.settled),
        "the recording adapter observes every settled boundary: {spans:#?}"
    );
}

struct CompactionThenAnswer;

impl Respond for CompactionThenAnswer {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let tools_empty = body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty);
        let body = if tools_empty {
            text_turn("compacted summary")
        } else {
            text_turn("answer after compaction")
        };
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header("content-type", "text/event-stream")
    }
}

#[tokio::test]
async fn typed_spans_cover_compaction_and_summary_boundaries() {
    use octet_agent::telemetry::spans::{InMemoryTelemetryContext, SpanStatus};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(CompactionThenAnswer)
        .mount(&server)
        .await;

    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session = session_with_authoritative_pressure(
        &sessions.path().join("span-compaction.jsonl"),
        180_000,
    );
    let mut agent = build_agent_from_session(&server.uri(), workspace.path(), session, Some(4));
    agent.set_compaction_token_policy(true, 0.85, 10_000).unwrap();
    let fixture = InMemoryTelemetryContext::default();
    agent.set_telemetry_context(fixture.context());

    let mut run = agent.prompt("new work").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    let spans = fixture.get_spans();
    let compaction = span_index(&spans, "octet.agent.compaction");
    let summary = span_index(&spans, "octet.agent.summary");
    assert!(
        compaction < summary,
        "the summary nests inside the compaction: {:?}",
        recorded_span_names(&spans)
    );
    assert_eq!(spans[summary].parent_id, Some(spans[compaction].id));
    let summary_request = (summary + 1..spans.len())
        .find(|index| spans[*index].name == "octet.ai.request")
        .expect("the summary issues one provider request");
    assert_eq!(
        spans[summary_request].parent_id,
        Some(spans[summary].id),
        "the summary request nests under the summary span"
    );
    assert_eq!(spans[compaction].parent_id, Some(spans[1].id), "compaction nests under the turn");
    assert!(
        spans.iter().all(|span| span.settled && span.status == SpanStatus::Ok),
        "a completed compaction settles every boundary: {spans:#?}"
    );
}
#[path = "support/extension_hooks.rs"]
mod extension_hooks;

// ── roadmap #175: the selected service tier reaches the wire, or is refused ──

/// One scripted one-turn Responses harness; `codex` selects the declared
/// Codex runtime profile, `false` keeps the plain OpenAI Responses profile.
async fn tier_harness(
    codex: bool,
    name: &str,
) -> (Agent, MockServer, tempfile::TempDir, tempfile::TempDir) {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![responses_text_turn(
                name,
                "tier answer",
                "response.completed",
                name,
            )],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let model = if codex {
        recovery_codex_model(&server.uri())
    } else {
        scripted_responses_model(&server.uri())
    };
    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("session.jsonl");
    let agent = build_responses_agent_from_session(
        model,
        Session::create(&session_path).unwrap(),
        &workspace,
        Some(4),
        "You are a test agent.",
        ReasoningConfig::Off,
    );
    (agent, server, workspace_dir, session_dir)
}

/// `/fast` is only real when the selected tier reaches the provider request.
#[tokio::test]
async fn service_tier_reaches_the_request_only_on_a_route_that_declares_it() {
    // ── Codex route: the tier is selected, then emitted on the wire ────────
    let (mut agent, server, _workspace, _session) = tier_harness(true, "codex").await;
    assert_eq!(agent.service_tier(), None, "no tier is sent by default");

    agent
        .set_service_tier(Some(octet_ai::ServiceTier::Priority))
        .expect("the Codex profile declares the Responses service_tier field");
    assert_eq!(
        agent.service_tier(),
        Some(octet_ai::ServiceTier::Priority),
        "the selection is readable by a frontend that renders `/fast`"
    );

    let output = agent.complete("answer quickly").await.unwrap();
    assert!(
        matches!(output.reason, FinishReason::Completed),
        "the tiered run must complete: {:?}",
        output.reason
    );
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1, "one model turn");
    assert_eq!(
        requests[0]["service_tier"], "priority",
        "the requested tier must be on the request: {}",
        requests[0]
    );

    // Clearing the selection stops sending the field on the next request.
    agent.set_service_tier(None).unwrap();
    agent.complete("answer normally").await.unwrap();
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].get("service_tier").is_none(),
        "a cleared tier must not linger: {}",
        requests[1]
    );

    // ── Non-Codex route: the selection is refused, never sent ─────────────
    let (mut agent, server, _workspace, _session) = tier_harness(false, "plain").await;
    let rejection = agent
        .set_service_tier(Some(octet_ai::ServiceTier::Priority))
        .expect_err("a route that does not declare the field must fail closed");
    assert_eq!(
        rejection.to_string(),
        "ai error: Unsupported error: Responses service tier is unsupported on this route",
        "the rejection is the codec's typed unsupported error"
    );
    assert_eq!(
        agent.service_tier(),
        None,
        "a refused selection never becomes agent state"
    );

    let output = agent.complete("answer without a tier").await.unwrap();
    assert!(
        matches!(output.reason, FinishReason::Completed),
        "the untiered run must complete: {:?}",
        output.reason
    );
    let requests = wire_requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].get("service_tier").is_none(),
        "an undeclared route must never carry the field: {}",
        requests[0]
    );
}

// ── parity 1e.2 durability half: a killed stream republishes its partial ──

/// A completing Responses turn that streams `text` as two deltas, so a kill
/// between them leaves a genuine prefix behind.
fn responses_two_delta_turn(response_id: &str, first: &str, second: &str) -> String {
    let terminal = serde_json::json!({
        "type": "response.completed",
        "response": {
            "output": [{
                "type": "message",
                "id": format!("msg_{response_id}"),
                "role": "assistant",
                "content": [{"type": "output_text", "text": format!("{first}{second}"), "annotations": []}],
                "unknown_provider_field": response_id,
            }],
            "usage": {"input_tokens": 5, "output_tokens": 2, "total_tokens": 7},
        },
    });
    [
        serde_json::json!({"type": "response.created", "response": {"id": response_id}}),
        serde_json::json!({
            "type": "response.output_item.added",
            "output_index": 0,
            "item": {"id": format!("msg_{response_id}"), "type": "message"},
        }),
        serde_json::json!({"type": "response.output_text.delta", "output_index": 0, "delta": first}),
        serde_json::json!({"type": "response.output_text.delta", "output_index": 0, "delta": second}),
        serde_json::json!({"type": "response.output_text.done", "output_index": 0}),
        terminal,
    ]
    .into_iter()
    .map(|event| format!("data: {event}\n\n"))
    .collect()
}

/// Kills a run mid-stream and proves the durable partial is republished exactly
/// once on the next start, matching the frame prefix rather than the whole turn.
#[tokio::test]
async fn a_killed_stream_republishes_its_partial_assistant_prefix_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("responses"))
        .respond_with(Script {
            bodies: vec![
                // 1: the attempt that is killed after its first text delta.
                responses_two_delta_turn("killed", "KILLED-PREFIX ", "KILLED-TAIL"),
                // 2: the restart's own turn.
                responses_text_turn("second", "SECOND-TURN", "response.completed", "second"),
                // 3: the third start, after the second one settled terminally.
                responses_text_turn("third", "THIRD-TURN", "response.completed", "third"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;

    let workspace_dir = tempfile::tempdir().unwrap();
    let session_dir = tempfile::tempdir().unwrap();
    let workspace = workspace_dir.path().canonicalize().unwrap();
    let session_path = session_dir.path().join("killed-stream.jsonl");
    let frames_path = session_dir
        .path()
        .join("killed-stream.jsonl.partial-assistant-frames");
    let model = scripted_responses_model(&server.uri());

    // ── 1. Kill the run after the first streamed delta ────────────────────
    let mut first = build_responses_agent_from_session(
        model.clone(),
        Session::create(&session_path).unwrap(),
        &workspace,
        Some(4),
        "You are a test agent.",
        ReasoningConfig::Off,
    );
    let mut run = first.prompt("first prompt").await.unwrap();
    let mut observed_prefix = String::new();
    while let Some(event) = run.next().await {
        if let AgentEvent::OutputDelta {
            channel: OutputChannel::Text,
            text,
        } = &event
        {
            observed_prefix.push_str(text);
            break;
        }
    }
    drop(run);
    drop(first);
    assert_eq!(
        observed_prefix, "KILLED-PREFIX ",
        "the killed attempt streamed only its first delta"
    );
    assert!(
        frames_path.exists(),
        "a killed attempt must leave its durable frame journal behind"
    );
    assert!(
        !std::fs::read_to_string(&session_path)
            .unwrap()
            .contains("KILLED-TAIL"),
        "an unsettled attempt is never committed to the session log"
    );

    // ── 2. Restart: the partial frame prefix is republished first ─────────
    let mut second = build_responses_agent_from_session(
        model.clone(),
        Session::open(&session_path).unwrap(),
        &workspace,
        Some(4),
        "You are a test agent.",
        ReasoningConfig::Off,
    );
    let mut run = second.prompt("second prompt").await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    drop(run);
    drop(second);
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.starts_with(&observed_prefix),
        "the restart must republish the frame prefix first: {text:?}"
    );
    assert!(
        !text.contains("KILLED-TAIL"),
        "a partial must never grow into the whole killed turn: {text:?}"
    );
    assert!(
        text.contains("SECOND-TURN"),
        "the restart still produced its own turn: {text:?}"
    );
    assert!(
        !frames_path.exists(),
        "a republished partial is consumed exactly once"
    );

    // ── 3. A settled turn is never republished as progress ────────────────
    let mut third = build_responses_agent_from_session(
        model,
        Session::open(&session_path).unwrap(),
        &workspace,
        Some(4),
        "You are a test agent.",
        ReasoningConfig::Off,
    );
    let mut run = third.prompt("third prompt").await.unwrap();
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        events.push(event);
    }
    drop(run);
    drop(third);
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::OutputDelta {
                channel: OutputChannel::Text,
                text,
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        text, "THIRD-TURN",
        "a terminally settled turn leaves no partial to republish"
    );
}

// ── row 4.10: the run loop consumes the batch termination request ─────────

/// A real tool that asks the run to stop once its work is complete.
struct TerminateProbe;

#[async_trait::async_trait]
impl Tool for TerminateProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "terminate_probe".into(),
            description: "Requests run termination when `stop` is true".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {"stop": {"type": "boolean"}},
                "required": ["stop"],
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let output = ToolOutput::new(if args["stop"].as_bool().unwrap_or(false) {
            "batch complete"
        } else {
            "batch continues"
        });
        Ok(if args["stop"].as_bool().unwrap_or(false) {
            output.requesting_termination()
        } else {
            output
        })
    }
}

/// A unanimous batch ends the run with the results already durable; one
/// sibling that did not ask to stop keeps the batch going.
#[tokio::test]
async fn unanimous_tool_termination_ends_the_run_and_a_lone_request_does_not() {
    // ── Unanimous single-call batch: no second model turn ─────────────────
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[(
                    "call_done",
                    "terminate_probe",
                    serde_json::json!({"stop": true}),
                )]),
                text_turn("SHOULD-NOT-BE-REQUESTED"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("unanimous-termination.jsonl"),
        Some(4),
        TerminateProbe,
    );

    let output = agent.complete("finish the work").await.unwrap();

    assert!(
        matches!(output.reason, FinishReason::Completed),
        "a unanimous termination completes the run: {:?}",
        output.reason
    );
    assert!(
        !output.text.contains("SHOULD-NOT-BE-REQUESTED"),
        "the loop must not open another model turn: {}",
        output.text
    );
    let requests = wire_requests(&server).await;
    assert_eq!(
        requests.len(),
        1,
        "exactly one model turn for a unanimous batch"
    );
    let durable = serde_json::to_string(&agent.session().context().unwrap()).unwrap();
    assert!(
        durable.contains("batch complete"),
        "the finalized result is durable before the run ends: {durable}"
    );
    let replayed = serde_json::to_string(&requests[0]).unwrap();
    assert!(
        replayed.contains("terminate_probe"),
        "the probe was really called: {replayed}"
    );

    // ── Non-unanimous batch: one stop request cannot discard a sibling ────
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[
                    (
                        "call_stop",
                        "terminate_probe",
                        serde_json::json!({"stop": true}),
                    ),
                    (
                        "call_keep",
                        "terminate_probe",
                        serde_json::json!({"stop": false}),
                    ),
                ]),
                text_turn("CONTINUED-WITH-BOTH-RESULTS"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("mixed-termination.jsonl"),
        Some(4),
        TerminateProbe,
    );

    let output = agent.complete("finish the work").await.unwrap();

    assert!(
        matches!(output.reason, FinishReason::Completed),
        "the batch continues to a normal completion: {:?}",
        output.reason
    );
    assert!(
        output.text.contains("CONTINUED-WITH-BOTH-RESULTS"),
        "a lone stop request must not end the run early: {}",
        output.text
    );
    let requests = wire_requests(&server).await;
    assert_eq!(
        requests.len(),
        2,
        "a non-unanimous batch keeps going to the next model turn"
    );
    let follow_up = serde_json::to_string(&requests[1]).unwrap();
    assert!(
        follow_up.contains("batch complete") && follow_up.contains("batch continues"),
        "both finalized sibling results must reach the next request: {follow_up}"
    );
}

// ── row 4.8: the live panel's replaceable state is really paced ────────────

/// A real tool that publishes a burst of replaceable decorations plus
/// append-only output, then finishes.
///
/// The burst is far larger than the panel's pace budget and is published
/// without an await point, so the run path—not the tool—decides what the panel
/// sees: one immediate state, collapsed intermediates, and the latest state at
/// the terminal boundary.
struct PreviewProbe;

#[async_trait::async_trait]
impl Tool for PreviewProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "preview_probe".into(),
            description: "Publishes a burst of replaceable panel state".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        ctx.progress
            .output(OutputStream::Stdout, "probe-output-chunk\n");
        for step in 0..12 {
            assert!(
                ctx.progress
                    .decoration(format!("step {step}"), Some(format!("detail {step}"))),
                "decoration {step} is accepted"
            );
        }
        Ok(ToolOutput::new("preview probe finished"))
    }
}

/// The run path coalesces replaceable panel state, never collapses append-only
/// chunks, and settles the latest state before the call is reported finished.
#[tokio::test]
async fn live_panel_decorations_are_coalesced_and_settle_the_latest_state() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_preview", "preview_probe", serde_json::json!({}))]),
                text_turn("preview probe finished"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("live-preview.jsonl");
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &session_path,
        Some(4),
        PreviewProbe,
    );

    let mut run = agent.prompt("watch the panel").await.unwrap();
    let mut decorations = Vec::new();
    let mut chunks = 0usize;
    let mut finished_after = None;
    let mut events = Vec::new();
    while let Some(event) = run.next().await {
        match &event {
            AgentEvent::ToolProgress {
                id,
                progress: octet_agent::ToolProgress::Decoration(decoration),
            } if id.0 == "call_preview" => decorations.push(decoration.clone()),
            AgentEvent::ToolProgress {
                id,
                progress: octet_agent::ToolProgress::Output { stream, bytes },
            } if id.0 == "call_preview" && *stream == OutputStream::Stdout => {
                chunks += 1;
                assert_eq!(bytes.as_ref(), b"probe-output-chunk\n");
            }
            AgentEvent::ToolFinished { id, .. } if id.0 == "call_preview" => {
                finished_after = Some(decorations.len());
            }
            _ => {}
        }
        events.push(event);
    }
    drop(run);

    assert_eq!(chunks, 1, "the append-only chunk is forwarded exactly once");
    assert!(
        !decorations.is_empty(),
        "the coalescer never drops the whole burst"
    );
    assert!(
        decorations.len() < 12,
        "the run path actually paced the burst: {} of 12 published",
        decorations.len()
    );
    assert_eq!(
        decorations[0].label(),
        "step 0",
        "the first state after idle is immediate"
    );
    assert_eq!(
        decorations
            .last()
            .expect("at least one decoration")
            .label(),
        "step 11",
        "the terminal boundary publishes the latest state, never a stale one"
    );
    assert_eq!(
        finished_after,
        Some(decorations.len()),
        "every decoration is published before the call is reported finished"
    );
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));

    // Panel state is presentation only: it never becomes durable state or the
    // model-visible tool result.
    let durable = std::fs::read_to_string(&session_path).unwrap();
    assert!(durable.contains("preview probe finished"), "{durable}");
    assert!(
        !durable.contains("step 11") && !durable.contains("detail 11"),
        "a decoration must never be persisted: {durable}"
    );
}

// ── row 4.7: the run path publishes bounded partial-output checkpoints ─────

#[cfg(any(unix, windows))]
#[derive(Default)]
struct RecordingRunCheckpointSink {
    snapshots: std::sync::Mutex<Vec<String>>,
}

#[cfg(any(unix, windows))]
impl RecordingRunCheckpointSink {
    fn snapshots(&self) -> Vec<String> {
        self.snapshots.lock().unwrap().clone()
    }
}

#[cfg(any(unix, windows))]
impl octet_agent::tool::PartialOutputCheckpointSink for RecordingRunCheckpointSink {
    fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
        assert!(
            snapshot.len() <= octet_agent::tools::BASH_CHECKPOINT_MAX_BYTES,
            "a checkpoint snapshot is bounded: {} bytes",
            snapshot.len()
        );
        self.snapshots.lock().unwrap().push(snapshot.to_owned());
        Ok(())
    }
}

/// A real tool that streams output across the checkpoint interval before it
/// returns, so the run path must publish while the call is still live.
#[cfg(any(unix, windows))]
struct StreamingProbe;

#[cfg(any(unix, windows))]
#[async_trait::async_trait]
impl Tool for StreamingProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "stream_probe".into(),
            description: "Streams bounded partial output".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            constrained_sampling: None,
        }
    }

    fn effect(
        &self,
        _args: &serde_json::Value,
        _ctx: &ToolContext<'_>,
    ) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        ctx: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        ctx.progress.output(OutputStream::Stdout, "ckpt-alpha\n");
        tokio::time::sleep(Duration::from_millis(30)).await;
        ctx.progress.output(OutputStream::Stderr, "ckpt-beta\n");
        tokio::time::sleep(Duration::from_millis(30)).await;
        Ok(ToolOutput::new("stream probe finished"))
    }
}

/// Checkpoints land on the live run path, stay bounded and non-terminal, and
/// are never durable state or a tool result; an unopted agent publishes none.
#[cfg(any(unix, windows))]
#[tokio::test]
async fn live_run_path_publishes_bounded_partial_output_checkpoints() {
    let checkpoint_interval = Duration::from_millis(10);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_stream", "stream_probe", serde_json::json!({}))]),
                text_turn("stream probe finished"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let session_path = sessions.path().join("checkpointed.jsonl");
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &session_path,
        Some(4),
        StreamingProbe,
    );
    let sink = Arc::new(RecordingRunCheckpointSink::default());
    agent.enable_partial_output_checkpoints(
        "stream_probe",
        Arc::clone(&sink) as Arc<dyn octet_agent::tool::PartialOutputCheckpointSink>,
        checkpoint_interval,
    );

    agent.complete("stream the output").await.unwrap();

    let snapshots = sink.snapshots();
    assert!(
        snapshots.len() >= 2,
        "the live call publishes across the interval: {snapshots:?}"
    );
    assert!(
        snapshots[0].contains("ckpt-alpha"),
        "the first observation publishes immediately: {snapshots:?}"
    );
    let last = snapshots.last().unwrap();
    assert!(
        last.contains("ckpt-alpha") && last.contains("ckpt-beta"),
        "a later checkpoint is a complete replacement snapshot: {last}"
    );
    for snapshot in &snapshots {
        assert!(
            !snapshot.contains("complete_stdout=true") && !snapshot.contains("complete_stderr=true"),
            "a checkpoint never claims the command finished: {snapshot}"
        );
    }
    let stats = agent
        .partial_output_checkpoint_stats()
        .expect("the consumer is enabled");
    assert_eq!(stats.published as usize, snapshots.len());
    assert!(stats.failures == 0);

    // Checkpoints are auxiliary observation data: the durable log carries the
    // settled result and none of the partial snapshots.
    let durable = std::fs::read_to_string(&session_path).unwrap();
    assert!(durable.contains("stream probe finished"), "{durable}");
    assert!(
        !durable.contains("ckpt-alpha") && !durable.contains("ckpt-beta"),
        "a checkpoint must never be persisted: {durable}"
    );

    // Unopted: the same tool publishes nothing and business output is identical.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                tool_turn(&[("call_stream", "stream_probe", serde_json::json!({}))]),
                text_turn("stream probe finished"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    let mut unopted = build_agent_with_extra_tool(
        &server.uri(),
        workspace.path(),
        &sessions.path().join("unopted.jsonl"),
        Some(4),
        StreamingProbe,
    );
    let unopted_sink = Arc::new(RecordingRunCheckpointSink::default());
    let unopted_output = unopted.complete("stream the output").await.unwrap();
    assert_eq!(unopted_output.text, "stream probe finished");
    assert!(
        matches!(unopted_output.reason, FinishReason::Completed),
        "checkpointing changes no business outcome: {:?}",
        unopted_output.reason
    );
    assert!(unopted_sink.snapshots().is_empty());
    assert!(unopted.partial_output_checkpoint_stats().is_none());
}

#[tokio::test]
async fn tool_prompt_section_is_opt_in_visible_and_never_names_withdrawn_tools() {
    // Row 4.13's prompt consumer: the registered tools' `promptSnippet` /
    // `promptGuidelines` reach the model request's system prompt through
    // `collect_tool_prompt_contributions`, and only when a host opts in.
    let mut default_off = harness(vec![text_turn("answer")], Some(1)).await;
    default_off.agent.complete("hello").await.unwrap();
    let requests = wire_requests(default_off.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 1);
    let default_system = wire_system_text(&requests[0]);
    assert_eq!(
        default_system, "You are a scripted test agent.",
        "the tool section is opt-in: an unopted host keeps a byte-identical prompt"
    );

    let mut enabled = harness(vec![text_turn("answer")], Some(1)).await;
    enabled.agent.set_tool_prompt_section_enabled(true);
    assert!(enabled.agent.tool_prompt_section_enabled());
    let contributions = enabled.agent.tool_prompt_contributions();
    let declared = contributions
        .iter()
        .map(|contribution| contribution.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        declared,
        vec!["read", "edit", "write", "bash", "search"],
        "contributions follow wire order for exactly the tools that declare a snippet"
    );
    // Search contributes its own snippet and stays callable. Rendering does
    // not reintroduce any of the withdrawn ls/find/grep aliases.
    assert!(
        enabled
            .agent
            .registered_tool_names()
            .iter()
            .any(|name| name == "search"),
        "search stays registered"
    );
    assert!(
        declared.contains(&"search"),
        "the real search contribution reaches the section"
    );
    enabled.agent.complete("hello").await.unwrap();
    let requests = wire_requests(enabled.server.as_ref().unwrap()).await;
    assert_eq!(requests.len(), 1);
    let system = wire_system_text(&requests[0]);
    assert!(
        system.starts_with("You are a scripted test agent.\n\nAvailable tools:"),
        "the section is appended once after the host prompt: {system}"
    );
    for contribution in &contributions {
        let line = format!("- {}: {}", contribution.name, contribution.snippet);
        assert_eq!(
            system.matches(&line).count(),
            1,
            "each registered tool appears exactly once with its own snippet: {line}"
        );
    }
    assert!(
        system.contains("- bash: ") && system.contains("ripgrep"),
        "the bash snippet names rg, not the withdrawn search tools: {system}"
    );
    for withdrawn in ["\n- ls:", "\n- find:", "\n- grep:"] {
        assert!(
            !system.contains(withdrawn),
            "a withdrawn tool must never be advertised: {system}"
        );
    }
    // The same composed prompt is what the idle context APIs report, so an
    // estimate cannot disagree with the request the run actually sends.
    let disabled_instruction_tokens = default_off
        .agent
        .request_context_breakdown()
        .unwrap()
        .instruction_tokens;
    let enabled_breakdown = enabled.agent.request_context_breakdown().unwrap();
    assert!(
        enabled_breakdown.instruction_tokens > disabled_instruction_tokens,
        "the enabled section must be accounted for in the same prompt the request uses: \
         {} vs {disabled_instruction_tokens}",
        enabled_breakdown.instruction_tokens
    );

    // A tool-free run never advertises tools, even with the section enabled.
    let mut answer_only = harness(vec![text_turn("answer")], Some(1)).await;
    answer_only.agent.set_tool_prompt_section_enabled(true);
    let mut run = answer_only
        .agent
        .prompt_without_tools("hello")
        .await
        .unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let requests = wire_requests(answer_only.server.as_ref().unwrap()).await;
    assert_eq!(
        wire_system_text(&requests[0]),
        "You are a scripted test agent.",
        "a run that exposes no tools must not advertise them"
    );
    assert!(request_has_no_tools(&requests[0]));
}

struct DurableMemoProbe {
    handles: Arc<std::sync::Mutex<Vec<octet_agent::tools::durability::InvocationHandle>>>,
}

#[async_trait::async_trait]
impl Tool for DurableMemoProbe {
    fn definition(&self) -> octet_ai::ToolDef {
        octet_ai::ToolDef {
            name: "durable_memo_probe".into(),
            description: "Records a session-backed memo".into(),
            parameters: serde_json::json!({"type":"object","properties":{}}),
            constrained_sampling: None,
        }
    }
    fn concurrency(&self) -> ToolConcurrency {
        ToolConcurrency::Parallel
    }
    fn replay_safety(&self) -> ReplaySafety {
        ReplaySafety::Safe
    }
    fn effect(&self, _: &serde_json::Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        context: &ToolContext<'_>,
    ) -> Result<ToolOutput, ToolError> {
        let handle = context
            .invocation()
            .expect("agent must inject the durable capability");
        let value: String =
            handle.replay_step("step/read", || "recorded observation".to_owned())?;
        self.handles.lock().unwrap().push(handle.clone());
        Ok(ToolOutput::new(value))
    }
}

#[tokio::test]
async fn durable_memos_are_injected_into_sequential_and_parallel_calls() {
    for width in [1, 2, 65] {
        let server = MockServer::start().await;
        let ids = (0..width)
            .map(|index| format!("memo-{index}"))
            .collect::<Vec<_>>();
        let script = vec![
            tool_turn(
                &(0..width)
                    .map(|index| {
                        (
                            ids[index].as_str(),
                            "durable_memo_probe",
                            serde_json::json!({}),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            text_turn("done"),
        ];
        let cursor = Arc::new(AtomicUsize::new(0));
        Mock::given(method("POST"))
            .and(path("messages"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200)
                    .set_body_string(script[cursor.fetch_add(1, Ordering::SeqCst)].clone())
                    .insert_header("content-type", "text/event-stream")
            })
            .mount(&server)
            .await;
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("memo.jsonl");
        let handles = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut agent = build_agent_with_extra_tool(
            &server.uri(),
            workspace.path(),
            &path,
            Some(4),
            DurableMemoProbe {
                handles: handles.clone(),
            },
        );
        assert_eq!(agent.complete("observe").await.unwrap().text, "done");
        let handles = handles.lock().unwrap();
        assert_eq!(handles.len(), width.min(32));
        assert_bounded_invocation_batch(&path, width, "memo-");
        assert!(handles
            .iter()
            .all(|handle| handle.get_memo("step/read").is_err()));
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert!(bytes.contains("tool_invocation"));
        assert!(bytes.contains("step/read"));
        assert!(bytes.contains("recorded observation"));
        drop(agent);
        assert!(Session::open(path).unwrap().tool_invocation(0).is_err());
    }
}

struct RetrySummaryOnce(AtomicUsize);
impl Respond for RetrySummaryOnce {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let summary = body
            .get("tools")
            .and_then(serde_json::Value::as_array)
            .is_none_or(Vec::is_empty);
        let body = if summary && self.0.fetch_add(1, Ordering::SeqCst) == 0 {
            msg_start() + &text_block(0, &["unfinished summary"])
        } else if summary {
            text_turn("recovered summary")
        } else {
            text_turn("answer")
        };
        ResponseTemplate::new(200)
            .set_body_string(body)
            .insert_header("content-type", "text/event-stream")
    }
}

#[tokio::test]
async fn ordinary_route_summary_retry_keeps_boundary_live_and_commits_once() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(RetrySummaryOnce(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("summary.jsonl");
    let session = session_with_authoritative_pressure(&path, 180_000);
    let mut agent = build_agent_from_session(&server.uri(), directory.path(), session, Some(4));
    agent
        .set_compaction_token_policy(true, 0.85, 10_000)
        .unwrap();
    let mut run = agent.prompt("continue").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    assert!(events.iter().any(|event| matches!(event,
        AgentEvent::ProviderOperationRetry { operation: octet_agent::ProviderOperation::LocalCompaction, error, .. }
            if error.contains("summarization retry scheduled"))));
    assert_eq!(
        agent
            .session()
            .entries()
            .iter()
            .filter(|entry| matches!(entry.value, EntryValue::Compaction { .. }))
            .count(),
        1
    );
    assert_eq!(
        agent
            .session()
            .usage_records()
            .iter()
            .filter(|record| matches!(record.kind, UsageRecordKind::Compaction))
            .count(),
        2,
        "history and split-turn prefix each have one completed usage record"
    );
    let requests = wire_requests(&server).await;
    let summaries = requests
        .iter()
        .filter(|request| {
            request
                .get("tools")
                .and_then(serde_json::Value::as_array)
                .is_none_or(Vec::is_empty)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        requests.len(),
        4,
        "failed history + history retry + prefix + answer"
    );
    assert_eq!(summaries.len(), 3);
    assert_eq!(
        summaries[0], summaries[1],
        "retry reuses the same history request"
    );
    assert_ne!(
        summaries[1]["messages"], summaries[2]["messages"],
        "prefix is a separate grounded summary, not duplicate history"
    );
    assert!(summaries[2].to_string().contains("PREFIX of a turn"));
    assert!(agent.session().entries().iter().any(|entry| matches!(&entry.value,
        EntryValue::Compaction { summary, .. } if summary.contains("**Turn Context (split turn):**"))));
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
    assert!(agent.session().has_uncertain_usage());
    drop(agent);
    assert!(Session::open(path).unwrap().has_uncertain_usage());
}

#[tokio::test]
async fn branch_summary_uses_real_retry_consumer_and_keeps_one_caller_commit() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(RetrySummaryOnce(AtomicUsize::new(0)))
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let mut agent = build_agent(
        &server.uri(),
        directory.path(),
        &directory.path().join("branch.jsonl"),
        Some(4),
    );
    let preparation = octet_agent::prepare_branch_handoff(
        vec![Message::User(UserMessage {
            content: vec![UserPart::Text("abandoned branch evidence".into())],
        })],
        &octet_agent::CompactionDetails::default(),
    );
    let mut events = Vec::new();
    let summary = agent
        .summarize_branch_with_retry(
            &preparation,
            octet_agent::CancellationToken::default(),
            |event| events.push(event),
        )
        .await
        .unwrap();
    assert!(summary.contains("recovered summary"));
    assert!(
        agent.session().entries().is_empty(),
        "retry consumer must not commit the branch on its own"
    );
    assert_eq!(agent.session().usage_records().len(), 1);
    assert!(agent.session().has_uncertain_usage());
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ProviderOperationRetry {
            operation: octet_agent::ProviderOperation::BranchSummary,
            ..
        }
    )));
    agent
        .session_mut()
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(summary)],
        })))
        .unwrap();
    assert_eq!(agent.session().entries().len(), 1);
}

#[tokio::test]
async fn session_checkpoint_consumer_writes_auxiliary_records_and_expires_them() {
    let server = MockServer::start().await;
    let script = [scripted_tool_turn("progress_test"), text_turn("done")];
    let cursor = AtomicUsize::new(0);
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(move |_: &wiremock::Request| {
            ResponseTemplate::new(200)
                .set_body_string(script[cursor.fetch_add(1, Ordering::SeqCst)].clone())
                .insert_header("content-type", "text/event-stream")
        })
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("checkpoint.jsonl");
    let mut agent = build_agent_with_extra_tool(
        &server.uri(),
        directory.path(),
        &path,
        Some(4),
        ProgressTool {
            duration_ms: 100,
            abortable: true,
        },
    );
    agent.enable_session_partial_output_checkpoints("progress_test", Duration::from_millis(10));
    assert_eq!(agent.complete("checkpoint").await.unwrap().text, "done");
    assert!(agent.partial_output_checkpoint_stats().unwrap().published > 0);
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert!(bytes.contains("pi.pending.tool_output"));
    for line in bytes.lines() {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        if value["type"] == "tool_invocation" {
            assert!(!line.contains("complete_stdout=true"));
        }
    }
    drop(agent);
    let reopened = Session::open(path).unwrap();
    assert!(!reopened
        .context()
        .unwrap()
        .iter()
        .any(|message| serde_json::to_string(message)
            .unwrap()
            .contains("pi.pending.tool_output")));
}

#[tokio::test]
async fn dropping_manual_summary_after_dispatch_persists_unknown_usage_without_fictional_totals() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_secs(60))
                .insert_header("content-type", "text/event-stream")
                .set_body_string(text_turn("late summary")),
        )
        .mount(&server)
        .await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("dropped-summary.jsonl");
    let mut agent = build_agent(&server.uri(), directory.path(), &path, Some(4));
    let preparation =
        octet_agent::prepare_branch_handoff(Vec::new(), &octet_agent::CompactionDetails::default());
    let mut pending = Box::pin(agent.summarize_branch_with_retry(
        &preparation,
        octet_agent::CancellationToken::default(),
        std::mem::drop,
    ));
    tokio::select! {
        result = &mut pending => panic!("request unexpectedly settled: {result:?}"),
        _ = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if !server.received_requests().await.unwrap().is_empty() { break; }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }) => {},
    }
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
    drop(pending);
    assert!(agent.session().has_uncertain_usage());
    assert!(agent.session().usage_records().is_empty());
    assert_eq!(agent.session().total_cost_microdollars(), 0);
    assert!(agent.session().entries().is_empty());
    drop(agent);
    assert!(Session::open_read_only(path).unwrap().has_uncertain_usage());
}

#[tokio::test]
async fn cancelled_summary_before_dispatch_does_not_invent_exposure() {
    let server = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let mut agent = build_agent(
        &server.uri(),
        directory.path(),
        &directory.path().join("presend.jsonl"),
        Some(4),
    );
    let preparation =
        octet_agent::prepare_branch_handoff(Vec::new(), &octet_agent::CompactionDetails::default());
    let cancel = octet_agent::CancellationToken::default();
    cancel.cancel();
    assert!(matches!(
        agent
            .summarize_branch_with_retry(&preparation, cancel, std::mem::drop)
            .await,
        Err(octet_agent::AgentError::Cancelled)
    ));
    assert!(!agent.session().has_uncertain_usage());
    assert!(agent.session().usage_records().is_empty());
    assert!(server.received_requests().await.unwrap().is_empty());
}

fn response_with_tier_echo(body: String, tier: &str) -> String {
    body.lines()
        .map(|line| {
            if let Some(json) = line.strip_prefix("data: ") {
                let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
                if value["type"] == "response.completed" {
                    value["response"]["service_tier"] = serde_json::json!(tier);
                }
                format!("data: {value}\n")
            } else {
                format!("{line}\n")
            }
        })
        .collect()
}

#[tokio::test]
async fn turn_finished_cost_is_the_exact_settled_tier_cost_not_catalog_or_gate_cost() {
    use octet_ai::{ResponsesRuntimeProfile as Profile, ServiceTier};
    for (api, requested, echoed, profile, expected) in [
        (
            "gpt-5.5",
            Some(ServiceTier::Priority),
            "default",
            Profile::Codex,
            Some((62, 500_045)),
        ),
        (
            "gpt-5.4",
            Some(ServiceTier::Priority),
            "default",
            Profile::Codex,
            Some((50, 36)),
        ),
        (
            "gpt-5.5",
            Some(ServiceTier::Flex),
            "default",
            Profile::Codex,
            Some((12, 500_009)),
        ),
        (
            "gpt-5.5",
            Some(ServiceTier::Priority),
            "flex",
            Profile::Codex,
            Some((12, 500_009)),
        ),
        (
            "gpt-5.5",
            Some(ServiceTier::Priority),
            "future-tier",
            Profile::Codex,
            None,
        ),
        ("gpt-5.5", None, "priority", Profile::Default, None),
    ] {
        for gated in [false, true] {
            let server = MockServer::start().await;
            let mut bodies = vec![response_with_tier_echo(
                responses_text_turn("answer", "answer", "response.completed", "answer"),
                echoed,
            )];
            if gated {
                bodies.push(response_with_tier_echo(
                    responses_text_turn("gate", "R", "response.completed", "gate"),
                    "default",
                ));
            }
            Mock::given(method("POST"))
                .and(path("responses"))
                .respond_with(Script {
                    bodies,
                    next: AtomicUsize::new(0),
                })
                .mount(&server)
                .await;
            let workspace = tempfile::tempdir().unwrap();
            let session_path = workspace.path().join("tier-cost.jsonl");
            let mut model = recovery_codex_model(&server.uri());
            Arc::make_mut(&mut model.spec).api_name = api.into();
            let pricing = Arc::make_mut(&mut model.spec).pricing.as_mut().unwrap();
            pricing.input = TokenRate(3_000_002);
            pricing.output = TokenRate(5_000_004);
            Arc::make_mut(&mut model.endpoint).runtime.responses_profile = profile;
            let base_pricing = model.spec.pricing.clone().unwrap();
            let mut agent = build_responses_agent_from_session(
                model,
                Session::create(&session_path).unwrap(),
                workspace.path(),
                Some(3),
                "test",
                ReasoningConfig::Off,
            );
            agent.set_service_tier(requested).unwrap();
            if gated {
                agent.set_completion_policy(CompletionPolicy::TerminalGate);
            }
            let mut run = agent.prompt("account this exact response").await.unwrap();
            let events = collect(&mut run).await;
            drop(run);
            assert!(
                matches!(assert_single_run_finished(&events), FinishReason::Completed),
                "{events:?}"
            );
            let turns = events
                .iter()
                .filter_map(|event| match event {
                    AgentEvent::TurnFinished {
                        turn_usage,
                        turn_cost,
                        run_cost_microdollars,
                        ..
                    } => Some((*turn_usage, *turn_cost, *run_cost_microdollars)),
                    _ => None,
                })
                .collect::<Vec<_>>();
            assert_eq!(turns.len(), 1);
            let (usage, cost, run_subtotal) = turns[0];
            assert_eq!(
                cost.map(|cost| (cost.total, cost.total_picodollars_remainder)),
                expected,
                "{api}/{requested:?}/{echoed}/gate={gated}"
            );
            let durable = agent
                .session()
                .usage_records()
                .iter()
                .find(|record| matches!(record.kind, UsageRecordKind::AssistantTurn { .. }))
                .unwrap();
            assert_eq!(cost, durable.cost);
            assert_eq!(usage, durable.usage);
            assert_ne!(
                cost,
                Some(octet_ai::pricing::cost_of(&base_pricing, &usage).unwrap())
            );
            let gate_cost = agent
                .session()
                .usage_records()
                .iter()
                .find_map(|record| match record.kind {
                    UsageRecordKind::TerminalGate { .. } => Some(record.cost.unwrap()),
                    _ => None,
                });
            assert_eq!(gate_cost.is_some(), gated);
            let exact_subtotal = cost
                .into_iter()
                .chain(gate_cost)
                .map(|cost| {
                    u128::from(cost.total) * 1_000_000
                        + u128::from(cost.total_picodollars_remainder)
                })
                .sum::<u128>();
            assert_eq!(u128::from(run_subtotal), exact_subtotal / 1_000_000);
            let requests = wire_requests(&server).await;
            if gated {
                assert!(requests[1].get("service_tier").is_none());
            }
            if cost.is_none() {
                assert!(agent.session().has_unpriced_usage());
                agent.set_max_session_cost_microdollars(Some(u64::MAX));
                assert!(matches!(
                    agent.ensure_request_cost_capacity(agent.model(), 1, 1),
                    Err(octet_agent::AgentError::CostUnavailable { .. })
                ));
            }
            drop(agent);
            let reopened = Session::open_read_only(&session_path).unwrap();
            let durable = reopened
                .usage_records()
                .iter()
                .find(|record| matches!(record.kind, UsageRecordKind::AssistantTurn { .. }))
                .unwrap();
            assert_eq!(cost, durable.cost);
        }
    }
}

#[tokio::test]
async fn turn_cost_after_retry_excludes_failed_attempt_uncertainty() {
    let (mut agent, server, _workspace, _) = recovery_harness(vec![
        interrupted_responses_prefix("text") + &recovery_provider_error("server_error"),
        responses_text_turn("settled", "settled", "response.completed", "settled"),
    ])
    .await;
    let mut run = agent
        .prompt("retry without fictional pricing")
        .await
        .unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let costs = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::TurnFinished { turn_cost, .. } => Some(*turn_cost),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(costs.len(), 1);
    assert_eq!(agent.session().usage_records().len(), 1);
    assert_eq!(costs[0], agent.session().usage_records()[0].cost);
    assert!(costs[0].is_some());
    assert_eq!(agent.session().usage_uncertainty_records().len(), 1);
    assert_eq!(wire_requests(&server).await.len(), 2);
}
