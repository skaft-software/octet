//! The byte-level JSONL transport under the RPC frontend: bounded LF framing
//! on stdin, the single-writer response envelope, and the command accessors.
//!
//! Why this is separate: the RPC command/response vocabulary is a property of
//! octet, but the framing is a property of the pipe. Keeping the 4 MiB record
//! bound, the UTF-8 check and the "only the owning task writes stdout" rule in
//! one place makes that invariant auditable without reading the whole event
//! translation, and lets the translation be exercised against an in-memory
//! `RpcOutput`, which is what the test suite does.

use std::io::{Read as _, Write as _};

use octet_agent::{AgentEvent, UserInput};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use super::events::EventTranslator;
use super::projection::{now_millis, user_input_value};
use super::{session_id, QueueState};
use crate::app::App;
use crate::modes::HostRunOutcome;

const MAX_RPC_LINE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(super) enum RpcInput {
    Value(Value),
    ParseError(String),
    Eof,
}

pub(super) struct RpcOutput {
    pub(super) stdout: Box<dyn std::io::Write>,
    pub(super) delta_only: bool,
}

impl RpcOutput {
    pub(super) fn new() -> Self {
        Self {
            stdout: Box::new(std::io::BufWriter::new(std::io::stdout())),
            delta_only: false,
        }
    }

    pub(super) fn send(&mut self, mut value: Value) -> anyhow::Result<()> {
        if self.delta_only {
            compact_json_event(&mut value);
        }
        serde_json::to_writer(&mut self.stdout, &value)?;
        self.stdout.write_all(b"\n")?;
        self.stdout.flush()?;
        Ok(())
    }

    pub(super) fn success(
        &mut self,
        id: Option<&str>,
        command: &str,
        data: Option<Value>,
    ) -> anyhow::Result<()> {
        let mut response = Map::new();
        if let Some(id) = id {
            response.insert("id".into(), Value::String(id.to_owned()));
        }
        response.insert("type".into(), Value::String("response".into()));
        response.insert("command".into(), Value::String(command.to_owned()));
        response.insert("success".into(), Value::Bool(true));
        if let Some(data) = data {
            response.insert("data".into(), data);
        }
        self.send(Value::Object(response))
    }

    pub(super) fn error(
        &mut self,
        id: Option<&str>,
        command: &str,
        error: impl Into<String>,
    ) -> anyhow::Result<()> {
        let mut response = Map::new();
        if let Some(id) = id {
            response.insert("id".into(), Value::String(id.to_owned()));
        }
        response.insert("type".into(), Value::String("response".into()));
        response.insert("command".into(), Value::String(command.to_owned()));
        response.insert("success".into(), Value::Bool(false));
        response.insert("error".into(), Value::String(error.into()));
        self.send(Value::Object(response))
    }
}

/// Pi's JSON mode removes cumulative snapshots from delta records. Native
/// session persistence and authoritative message_end records remain unchanged.
pub(super) fn compact_json_event(value: &mut Value) {
    if value["type"] != "message_update" || value.get("message").is_none() {
        return;
    }
    let message = value
        .as_object_mut()
        .expect("event object")
        .remove("message")
        .unwrap_or(Value::Null);
    value["usage"] = message["usage"].clone();
    let event = &mut value["assistantMessageEvent"];
    if event["type"] == "toolcall_start" {
        if let Some(index) = event["contentIndex"].as_u64() {
            let content = &message["content"][index as usize];
            event["id"] = content["id"].clone();
            event["toolName"] = content["name"].clone();
        }
    }
    if let Some(object) = event.as_object_mut() {
        object.remove("partial");
    }
}

/// Reuse the RPC semantic projection, without its command/response protocol.
/// JSONL writes are flushed per event and backpressure remains synchronous.
pub(crate) struct JsonEventStream {
    output: RpcOutput,
    translator: EventTranslator,
    queue: QueueState,
}

impl JsonEventStream {
    pub(crate) fn header(app: &App) -> anyhow::Result<()> {
        RpcOutput::new().send(json!({
            "type": "session", "version": 1, "format": "octet-json-events",
            "id": session_id(app), "cwd": app.config.workspace,
            "timestamp": now_millis(),
            "usageUncertain": (app.agent.session().has_uncertain_usage() || app.agent.session().has_unpriced_usage())
        }))
    }

    pub(crate) fn new(app: &App, input: &UserInput) -> Self {
        let mut output = RpcOutput::new();
        output.delta_only = true;
        Self {
            output,
            translator: EventTranslator::new(app, user_input_value(input)),
            queue: QueueState::default(),
        }
    }

    pub(crate) fn start(&mut self, input: &UserInput) -> anyhow::Result<()> {
        self.output.send(json!({"type": "agent_start"}))?;
        self.output.send(json!({"type": "turn_start"}))?;
        let message = user_input_value(input);
        self.output
            .send(json!({"type": "message_start", "message": message}))?;
        self.output
            .send(json!({"type": "message_end", "message": message}))
    }

    pub(crate) fn observe(&mut self, event: AgentEvent) -> anyhow::Result<Option<HostRunOutcome>> {
        self.translator
            .observe(event, &mut self.output, &mut self.queue)
    }

    pub(crate) fn finish(&mut self, outcome: &HostRunOutcome) -> anyhow::Result<()> {
        // observe(RunFinished) already settles the translator. Abnormal host
        // termination must close an unfinished message explicitly as well.
        if matches!(
            outcome,
            HostRunOutcome::StreamLost | HostRunOutcome::Shutdown
        ) {
            self.translator.settle(outcome.clone(), &mut self.output)?;
        }
        self.output.send(json!({"type": "agent_end", "messages": self.translator.run_messages, "willRetry": false,
            "usageUncertain": self.translator.usage_uncertain}))?;
        if let Some(event) = self.translator.pending_retry_end.take() {
            self.output.send(event)?;
        }
        Ok(())
    }
}

pub(super) fn spawn_input_reader() -> mpsc::Receiver<RpcInput> {
    let (tx, rx) = mpsc::channel(64);
    std::thread::Builder::new()
        .name("octet-rpc-stdin".into())
        .spawn(move || {
            let mut input = std::io::stdin().lock();
            let mut chunk = [0u8; 8192];
            let mut pending = Vec::new();
            let mut discarding_oversized = false;
            loop {
                let read = match input.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(read) => read,
                    Err(error) => {
                        let _ = tx.blocking_send(RpcInput::ParseError(format!(
                            "Failed to read command: {error}"
                        )));
                        break;
                    }
                };
                let mut start = 0usize;
                for (index, byte) in chunk[..read].iter().enumerate() {
                    if *byte != b'\n' {
                        continue;
                    }
                    if discarding_oversized {
                        discarding_oversized = false;
                    } else {
                        pending.extend_from_slice(&chunk[start..index]);
                        dispatch_line(&tx, &mut pending);
                    }
                    start = index + 1;
                }
                if start < read && !discarding_oversized {
                    pending.extend_from_slice(&chunk[start..read]);
                    if pending.len() > MAX_RPC_LINE_BYTES {
                        pending.clear();
                        discarding_oversized = true;
                        let _ = tx.blocking_send(RpcInput::ParseError(format!(
                            "Failed to parse command: JSONL record exceeds {MAX_RPC_LINE_BYTES} bytes"
                        )));
                    }
                }
            }
            if !discarding_oversized && !pending.is_empty() {
                dispatch_line(&tx, &mut pending);
            }
            let _ = tx.blocking_send(RpcInput::Eof);
        })
        .expect("RPC stdin reader thread must start");
    rx
}

pub(super) fn dispatch_line(tx: &mpsc::Sender<RpcInput>, pending: &mut Vec<u8>) {
    if pending.last() == Some(&b'\r') {
        pending.pop();
    }
    let parsed = std::str::from_utf8(pending)
        .map_err(|_| "Failed to parse command: record is not valid UTF-8".to_owned())
        .and_then(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("Failed to parse command: {error}"))
        });
    pending.clear();
    let input = match parsed {
        Ok(value) => RpcInput::Value(value),
        Err(error) => RpcInput::ParseError(error),
    };
    let _ = tx.blocking_send(input);
}

pub(super) fn command_type(command: &Value) -> Option<&str> {
    command.as_object()?.get("type")?.as_str()
}

pub(super) fn command_id(command: &Value) -> Option<&str> {
    command.as_object()?.get("id")?.as_str()
}

pub(super) fn required_string<'a>(command: &'a Value, field: &str) -> anyhow::Result<&'a str> {
    command
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("{field} must be a string"))
}

pub(super) fn required_bool(command: &Value, field: &str) -> anyhow::Result<bool> {
    command
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(Value::as_bool)
        .ok_or_else(|| anyhow::anyhow!("{field} must be a boolean"))
}

pub(super) fn optional_bool(command: &Value, field: &str) -> anyhow::Result<Option<bool>> {
    let Some(value) = command.as_object().and_then(|object| object.get(field)) else {
        return Ok(None);
    };
    value
        .as_bool()
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("{field} must be a boolean"))
}
