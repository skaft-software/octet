#![allow(missing_docs)]

//! Pi-compatible JSONL RPC frontend.
//!
//! A single task owns `App` and its borrowed `Run`. A blocking stdin reader
//! performs strict LF framing and forwards complete JSON values to that task;
//! only the owner writes stdout, so responses and streaming events can never
//! interleave at the byte level.
//!
//! This module owns the command loop itself: reading one framed command,
//! dispatching it, running the resulting turn, and reporting the result. The
//! three concerns it sits on top of are siblings rather than inline, because
//! each has a different reason to change:
//!
//! * [`wire`] owns the pipe: the 4 MiB record bound, the strict-LF framing, the
//!   single-writer response envelope and the command field accessors.
//! * [`projection`] owns the wire *shapes*: one pure function per domain value.
//! * [`events`] owns the streaming state machine that turns `AgentEvent`s into
//!   the mutually exclusive assistant-message, turn and tool events.
//!
//! The loop is the only place that knows the order those three are used in.

mod events;
mod projection;
mod wire;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use octet_agent::{
    AgentCompactionMode, AgentError, AgentEvent, BashTool, CancellationToken, EntryValue,
    InputPart, QueueDeliveryMode, Run, RunControl, SandboxConfig, SkillRegistry, Tool, ToolContext,
    ToolProgressSink, UserInput,
};
// Names the relocated modules no longer need but `rpc::tests` still names
// directly. They stay reachable through the suite's `use super::*` rather than
// editing the test file.
#[cfg(test)]
use octet_agent::OutputChannel;
#[cfg(test)]
use octet_ai::{AssistantMessage, Cost, Protocol, StopReason};
use octet_ai::{AssistantPart, Media, Message, Model, ModelId, Usage, UserMessage, UserPart};
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use crate::app::bootstrap::{
    build_app_with_resource_consumer as build_app, effective_compaction_threshold_fraction,
    rebuild_app, resolve_launch_print, Bootstrap,
};
use crate::app::{
    apply_reconfig, reasoning_label, supported_levels_with_subagents,
    thinking_to_reasoning_with_subagents, App, Reconfig,
};
use crate::compaction::{attempt_compaction, estimate_text_tokens, CompactionOutcome};
use crate::config::{CompactionMode, ThinkingLevel};
use crate::modes::HostRunOutcome;
use crate::prompts::{PromptRegistry, PromptRenderContext};
use crate::resources::{compose_instructions, expand_skill_command};

// The three sibling modules own the pipe, the wire shapes and the streaming
// state machine. These imports are how the loop reaches them; they are private
// re-exports rather than call-site edits so `rpc::tests` keeps reaching the
// same names through its own `use super::*`. `JsonEventStream` is the one
// exception: the JSON-event frontend reaches it as
// `crate::modes::rpc::JsonEventStream`, so it is re-exported `pub(crate)` at
// exactly the visibility and path it had before the move.
use events::{active_state_value, EventTranslator};
#[cfg(test)]
use events::{RpcToolProgress, MAX_RPC_TOOL_PROGRESS_BYTES, PARTIAL_SNAPSHOTS};
use projection::{
    assistant_value, dollars, inferred_stop_reason, iso_timestamp, media_content, model_value,
    rpc_commands, rpc_messages, tool_result_content, tool_result_value_at, user_content_at,
    user_input_value,
};
#[cfg(test)]
use projection::{protocol_name, user_value};
pub(crate) use wire::JsonEventStream;
use wire::{
    command_id, command_type, optional_bool, required_bool, required_string, spawn_input_reader,
    RpcInput, RpcOutput,
};
#[cfg(test)]
use wire::{compact_json_event, dispatch_line};

#[derive(Debug)]
struct RpcBashResult {
    output: String,
    exit_code: Option<i32>,
    cancelled: bool,
    truncated: bool,
}

impl RpcBashResult {
    fn value(&self) -> Value {
        let mut value = Map::new();
        value.insert("output".into(), Value::String(self.output.clone()));
        if let Some(exit_code) = self.exit_code {
            value.insert("exitCode".into(), json!(exit_code));
        }
        value.insert("cancelled".into(), Value::Bool(self.cancelled));
        value.insert("truncated".into(), Value::Bool(self.truncated));
        Value::Object(value)
    }
}

struct ActiveRpcBash {
    id: Option<String>,
    command: String,
    exclude_from_context: bool,
    cancellation: CancellationToken,
    task: tokio::task::JoinHandle<anyhow::Result<RpcBashResult>>,
}

fn parse_bash_exit_status(line: &str) -> Option<i32> {
    line.strip_prefix("exit=")?
        .split_ascii_whitespace()
        .next()?
        .parse()
        .ok()
}

fn rpc_bash_result(text: String) -> RpcBashResult {
    let truncated = text.contains("truncated_stdout=") || text.contains("truncated_stderr=");
    let payload = text.strip_prefix("error nonzero_exit\n").unwrap_or(&text);
    let (status, body) = payload.split_once('\n').unwrap_or((payload, ""));
    let exit_code = parse_bash_exit_status(status);
    let output = if exit_code.is_some() {
        if body == "(no output)" {
            String::new()
        } else {
            body.to_owned()
        }
    } else {
        text
    };
    RpcBashResult {
        output,
        exit_code,
        cancelled: false,
        truncated,
    }
}

async fn run_sandboxed_bash(
    tool: Arc<BashTool>,
    command: String,
    workspace: PathBuf,
    sandbox: SandboxConfig,
    cancellation: CancellationToken,
) -> anyhow::Result<RpcBashResult> {
    let active_skills = Vec::new();
    let registered_tools = vec!["bash".to_owned()];
    let context = ToolContext {
        workspace: &workspace,
        sandbox: &sandbox,
        execution_scope: "rpc-bash",
        resource_owner: "rpc-bash",
        active_skills: &active_skills,
        registered_tools: &registered_tools,
        progress: ToolProgressSink::null(),
        cancellation: cancellation.clone(),
    };
    let execution = tool.execute(json!({"command": command}), &context);
    tokio::pin!(execution);
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Ok(RpcBashResult {
            output: String::new(),
            exit_code: None,
            cancelled: true,
            truncated: false,
        }),
        result = &mut execution => match result {
            Ok(output) => Ok(rpc_bash_result(output.text)),
            Err(error) if error.message.starts_with("error nonzero_exit\n")
                || error.message.starts_with("error timeout\n") => {
                    Ok(rpc_bash_result(error.message))
                }
            Err(error) => Err(anyhow::anyhow!(error.message)),
        }
    }
}

async fn drive_rpc_bash(
    active: &mut ActiveRpcBash,
    input: &mut mpsc::Receiver<RpcInput>,
    output: &mut RpcOutput,
) -> anyhow::Result<(anyhow::Result<RpcBashResult>, VecDeque<Value>, bool)> {
    let mut deferred = VecDeque::new();
    let mut eof = false;
    let result = loop {
        tokio::select! {
            biased;
            result = &mut active.task => {
                break match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!("bash task failed: {error}")),
                };
            }
            inbound = input.recv(), if !eof => match inbound {
                Some(RpcInput::Value(command))
                    if command_type(&command) == Some("abort_bash") => {
                        active.cancellation.cancel();
                        output.success(command_id(&command), "abort_bash", None)?;
                    }
                Some(RpcInput::Value(command)) => deferred.push_back(command),
                Some(RpcInput::ParseError(error)) => output.error(None, "parse", error)?,
                Some(RpcInput::Eof) | None => {
                    eof = true;
                    active.cancellation.cancel();
                }
            }
        }
    };
    Ok((result, deferred, eof))
}

fn bash_context_message(command: &str, result: &RpcBashResult) -> Message {
    Message::User(UserMessage {
        content: vec![UserPart::Text(format!(
            "Ran `{command}`\n```\n{}\n```",
            result.output
        ))],
    })
}
#[derive(Clone)]
struct RpcSettings {
    steering_mode: String,
    follow_up_mode: String,
    auto_retry_enabled: bool,
    registered_tools: Vec<String>,
}

impl Default for RpcSettings {
    fn default() -> Self {
        Self {
            steering_mode: "one-at-a-time".into(),
            follow_up_mode: "one-at-a-time".into(),
            auto_retry_enabled: true,
            registered_tools: Vec::new(),
        }
    }
}

impl RpcSettings {
    fn steering_mode(&self) -> QueueDeliveryMode {
        queue_mode(&self.steering_mode)
    }

    fn follow_up_mode(&self) -> QueueDeliveryMode {
        queue_mode(&self.follow_up_mode)
    }
}

fn queue_mode(mode: &str) -> QueueDeliveryMode {
    if mode == "all" {
        QueueDeliveryMode::All
    } else {
        QueueDeliveryMode::OneAtATime
    }
}

#[derive(Clone)]
struct QueuedInput {
    text: String,
    input: UserInput,
    message: Value,
}

#[derive(Default)]
struct QueueState {
    steering: VecDeque<QueuedInput>,
    follow_up: VecDeque<QueuedInput>,
}

impl QueueState {
    fn len(&self) -> usize {
        self.steering.len().saturating_add(self.follow_up.len())
    }

    fn event(&self) -> Value {
        json!({
            "type": "queue_update",
            "steering": self.steering.iter().map(|queued| queued.text.as_str()).collect::<Vec<_>>(),
            "followUp": self.follow_up.iter().map(|queued| queued.text.as_str()).collect::<Vec<_>>()
        })
    }

    fn take_steering(&mut self, count: usize) -> Vec<QueuedInput> {
        (0..count)
            .filter_map(|_| self.steering.pop_front())
            .collect()
    }

    fn take_follow_up(&mut self, count: usize) -> Vec<QueuedInput> {
        (0..count)
            .filter_map(|_| self.follow_up.pop_front())
            .collect()
    }
}

fn session_id(app: &App) -> String {
    app.agent
        .session()
        .path()
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_owned()
}

struct RpcSessionProjection<'a> {
    app: &'a App,
    usage_by_assistant: HashMap<String, &'a octet_agent::UsageRecord>,
    tool_names: HashMap<String, String>,
}

impl<'a> RpcSessionProjection<'a> {
    fn new(app: &'a App) -> Self {
        let usage_by_assistant = app
            .agent
            .session()
            .usage_records()
            .iter()
            .filter_map(|record| match &record.kind {
                octet_agent::UsageRecordKind::AssistantTurn { assistant } => {
                    Some((assistant.0.clone(), record))
                }
                _ => None,
            })
            .collect();
        let mut tool_names = HashMap::new();
        for entry in app.agent.session().entries() {
            let EntryValue::Message(Message::Assistant(message)) = &entry.value else {
                continue;
            };
            for part in &message.content {
                if let AssistantPart::ToolCall(call) = part {
                    tool_names.insert(call.id.0.clone(), call.name.clone());
                }
            }
        }
        Self {
            app,
            usage_by_assistant,
            tool_names,
        }
    }

    fn user_message(&self, message: &UserMessage, timestamp: u64) -> Value {
        let mut tool_results = message.content.iter().filter_map(|part| match part {
            UserPart::ToolResult(result) => Some(result),
            _ => None,
        });
        if let Some(result) = tool_results.next() {
            if tool_results.next().is_none() {
                let tool_name = self
                    .tool_names
                    .get(&result.tool_call_id.0)
                    .map_or("", String::as_str);
                let mut value = tool_result_value_at(result, tool_name, timestamp);
                if let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) {
                    content.extend(message.content.iter().filter_map(|part| match part {
                        UserPart::Text(text) => Some(json!({"type": "text", "text": text})),
                        UserPart::Media(media) => Some(media_content(media)),
                        UserPart::ToolResult(_) => None,
                    }));
                }
                return value;
            }
        }
        user_content_at(
            message.content.iter().map(|part| match part {
                UserPart::Text(text) => json!({"type": "text", "text": text}),
                UserPart::Media(media) => media_content(media),
                UserPart::ToolResult(result) => json!({
                    "type": "text",
                    "text": tool_result_content(result)
                        .iter()
                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            }),
            timestamp,
        )
    }

    fn entry(&self, entry: &octet_agent::Entry) -> Value {
        let timestamp_ms = entry.timestamp_unix_ms.unwrap_or_default();
        let mut value = Map::new();
        value.insert("id".into(), Value::String(entry.id.0.clone()));
        value.insert(
            "parentId".into(),
            entry
                .parent
                .as_ref()
                .map_or(Value::Null, |parent| Value::String(parent.0.clone())),
        );
        value.insert(
            "timestamp".into(),
            Value::String(iso_timestamp(timestamp_ms)),
        );

        match &entry.value {
            EntryValue::Message(Message::User(message)) => {
                value.insert("type".into(), Value::String("message".into()));
                value.insert("message".into(), self.user_message(message, timestamp_ms));
            }
            EntryValue::Message(Message::Assistant(message)) => {
                let record = self.usage_by_assistant.get(&entry.id.0).copied();
                let reason = record
                    .and_then(|record| record.stop_reason.clone())
                    .unwrap_or_else(|| inferred_stop_reason(message));
                let usage = record.map_or_else(Usage::default, |record| record.usage);
                let endpoint = record
                    .and_then(|record| record.endpoint.as_ref())
                    .map_or(self.app.model.endpoint.id.0.as_str(), |endpoint| {
                        endpoint.0.as_str()
                    });
                value.insert("type".into(), Value::String("message".into()));
                value.insert(
                    "message".into(),
                    assistant_value(
                        message,
                        endpoint,
                        &usage,
                        record.and_then(|record| record.cost),
                        &reason,
                        Some(timestamp_ms),
                    ),
                );
            }
            EntryValue::Config {
                model: Some(model), ..
            } => {
                let provider = self
                    .app
                    .catalog
                    .resolve(&ModelId(model.clone()))
                    .map_or_else(
                        |_| self.app.model.endpoint.id.0.clone(),
                        |resolved| resolved.endpoint.id.0.clone(),
                    );
                value.insert("type".into(), Value::String("model_change".into()));
                value.insert("provider".into(), Value::String(provider));
                value.insert("modelId".into(), Value::String(model.clone()));
            }
            EntryValue::Config {
                reasoning: Some(reasoning),
                ..
            } => {
                value.insert("type".into(), Value::String("thinking_level_change".into()));
                value.insert("thinkingLevel".into(), Value::String(reasoning.clone()));
            }
            EntryValue::Compaction {
                summary,
                first_kept,
                details,
                ..
            } => {
                value.insert("type".into(), Value::String("compaction".into()));
                value.insert("summary".into(), Value::String(summary.clone()));
                value.insert(
                    "firstKeptEntryId".into(),
                    Value::String(first_kept.0.clone()),
                );
                value.insert("tokensBefore".into(), json!(0));
                if let Ok(details) = serde_json::to_value(details) {
                    value.insert("details".into(), details);
                }
            }
            EntryValue::BranchSummary {
                summary,
                from_entry,
                details,
            } => {
                value.insert("type".into(), Value::String("branch_summary".into()));
                value.insert("summary".into(), Value::String(summary.clone()));
                value.insert("fromId".into(), Value::String(from_entry.0.clone()));
                value.insert("details".into(), json!(details));
            }
            other => {
                let custom_type = match other {
                    EntryValue::Config { .. } => "octet:config",
                    EntryValue::PromptTemplateSelected { .. } => "octet:prompt-template-selected",
                    EntryValue::SkillActivated { .. } => "octet:legacy-skill-activated",
                    EntryValue::SkillResourceRead { .. } => "octet:legacy-skill-resource-read",
                    EntryValue::SkillDeactivated { .. } => "octet:legacy-skill-deactivated",
                    EntryValue::ResponsesTurn { .. } => "octet:responses-turn",
                    EntryValue::ResponsesCompaction { .. } => "octet:responses-compaction",
                    EntryValue::ResponsesReasoning { .. } => "octet:responses-reasoning",
                    EntryValue::ResponsesSteering { .. } => "octet:responses-steering",
                    EntryValue::Message(_)
                    | EntryValue::Compaction { .. }
                    | EntryValue::BranchSummary { .. } => {
                        unreachable!("message and summary entries are handled above")
                    }
                };
                value.insert("type".into(), Value::String("custom".into()));
                value.insert("customType".into(), Value::String(custom_type.into()));
                if let Ok(data) = serde_json::to_value(other) {
                    value.insert("data".into(), data);
                }
            }
        }
        Value::Object(value)
    }
}

fn rpc_tree(app: &App, projection: &RpcSessionProjection<'_>) -> Vec<Value> {
    let mut children = HashMap::<String, Vec<Value>>::new();
    let mut roots = Vec::new();
    for entry in app.agent.session().entries().iter().rev() {
        let mut entry_children = children.remove(&entry.id.0).unwrap_or_default();
        entry_children.reverse();
        let node = json!({
            "entry": projection.entry(entry),
            "children": entry_children
        });
        if let Some(parent) = &entry.parent {
            children.entry(parent.0.clone()).or_default().push(node);
        } else {
            roots.push(node);
        }
    }
    // Defensive replay rejects orphans, so only true roots remain here.
    roots.reverse();
    roots
}

fn state_value(
    app: &App,
    settings: &RpcSettings,
    streaming: bool,
    pending_messages: usize,
    message_count: Option<usize>,
) -> Value {
    let id = session_id(app);
    let session_name = app
        .sessions
        .load_metadata(&id)
        .ok()
        .and_then(|metadata| metadata.name);
    let mut state = json!({
        "model": model_value(&app.model),
        "thinkingLevel": reasoning_label(&app.reasoning),
        "isStreaming": streaming,
        "isCompacting": false,
        "usageUncertain": (app.agent.session().has_uncertain_usage() || app.agent.session().has_unpriced_usage()),
        "cacheWarmingMode": app.agent.cache_warming_mode(),
        "showCacheMissNotices": app.config.show_cache_miss_notices,
        "cacheWarmingStatus": app.agent.cache_warming_status(),
        "steeringMode": settings.steering_mode,
        "followUpMode": settings.follow_up_mode,
        "sessionFile": app.agent.session().path(),
        "sessionId": id,
        "autoCompactionEnabled": app.config.compaction.mode != CompactionMode::Disabled,
        "messageCount": message_count.unwrap_or_else(|| rpc_messages(app).len()),
        "pendingMessageCount": pending_messages
    });
    if let Some(name) = session_name {
        state
            .as_object_mut()
            .expect("state is an object")
            .insert("sessionName".into(), Value::String(name));
    }
    state
}

fn input_from_command(command: &Value, text: String) -> anyhow::Result<UserInput> {
    let mut parts = vec![InputPart::Text(text)];
    if let Some(images) = command.as_object().and_then(|object| object.get("images")) {
        let images = images
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("images must be an array"))?;
        for image in images {
            let data = image
                .get("data")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("image data must be a base64 string"))?;
            let mime_type = image
                .get("mimeType")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("image mimeType must be a string"))?
                .parse::<mime::Mime>()?;
            if mime_type.type_() != mime::IMAGE {
                anyhow::bail!("RPC prompt images must use an image MIME type");
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|error| anyhow::anyhow!("invalid base64 image: {error}"))?;
            parts.push(InputPart::Media(Media::image_bytes(
                bytes.into(),
                mime_type,
            )));
        }
    }
    Ok(UserInput::from(parts))
}

async fn prepare_prompt(
    app: &mut App,
    command: &Value,
) -> anyhow::Result<(UserInput, usize, Value)> {
    let original = required_string(command, "message")?.to_owned();
    crate::commands::reject_tui_changelog(&original)?;
    let mut expanded = expand_skill_command(
        app.skills.as_ref(),
        &original,
        &app.agent.registered_tool_names(),
    )?
    .unwrap_or(original.clone());
    if let Some(invocation) = expanded.trim().strip_prefix('/') {
        let split = invocation
            .find(char::is_whitespace)
            .unwrap_or(invocation.len());
        let (name, arguments) = invocation.split_at(split);
        if !name.is_empty() && app.prompts.contains(name) {
            let rendered = app.prompts.render(
                name,
                arguments.trim_start(),
                &PromptRenderContext {
                    workspace: &app.config.workspace,
                    selection: None,
                    active_skills: &[],
                },
            )?;
            app.agent
                .session_mut()
                .append(octet_agent::EntryValue::PromptTemplateSelected {
                    name: rendered.name,
                    content_hash: rendered.content_hash,
                })?;
            expanded = rendered.text;
        }
    }
    let rendered = match crate::prompts::render_configured(app, &expanded)? {
        Some(rendered) => rendered.text,
        None => expanded,
    };
    app.executable_extensions.refresh_host_state(
        app.agent.session(),
        &app.model,
        &app.reasoning,
        &app.sessions,
    );
    // Non-interactive modes serve the same extension requests: publish the
    // catalog an approved provider-credential request resolves against.
    app.executable_extensions
        .refresh_native_credential_catalog(&app.catalog);
    let composition = app
        .executable_extensions
        .compose_prompt(&app.system, rendered)
        .await?;
    for notification in composition.notifications {
        crate::output::stderr_line(format!("extension: {notification}"));
    }
    app.agent.set_system_prompt(composition.system);
    app.agent.set_prompt_display_text(Some(original.clone()));
    let mut input = input_from_command(command, composition.prompt)?;
    input.custom_messages = composition.custom_messages;
    let mut display_input = input.clone();
    if let Some(InputPart::Text(text)) = display_input.parts.first_mut() {
        *text = original;
    }
    let display_message = user_input_value(&display_input);
    Ok((input, composition.pending_context_count, display_message))
}

fn rpc_error_diagnostic(model: &Model, error: &AgentError) -> String {
    octet_agent::public_error_diagnostic(error, &model.endpoint.id.0, &model.spec.id.0)
}
fn parse_queue_mode(mode: &str) -> anyhow::Result<QueueDeliveryMode> {
    match mode {
        "all" => Ok(QueueDeliveryMode::All),
        "one-at-a-time" => Ok(QueueDeliveryMode::OneAtATime),
        _ => anyhow::bail!("mode must be all or one-at-a-time"),
    }
}

fn expand_queued_prompt(
    skills: &dyn SkillRegistry,
    prompts: &PromptRegistry,
    workspace: &Path,
    registered_tools: &[String],
    raw: &str,
) -> anyhow::Result<String> {
    let expanded =
        expand_skill_command(skills, raw, registered_tools)?.unwrap_or_else(|| raw.to_owned());
    let invocation = expanded.trim().strip_prefix('/');
    let Some(invocation) = invocation else {
        return Ok(expanded);
    };
    let split = invocation
        .find(char::is_whitespace)
        .unwrap_or(invocation.len());
    let (name, arguments) = invocation.split_at(split);
    if name.is_empty() || !prompts.contains(name) {
        return Ok(expanded);
    }
    let rendered = prompts.render(
        name,
        arguments.trim_start(),
        &PromptRenderContext {
            workspace,
            selection: None,
            // Legacy active skills are intentionally context-inert. Agent
            // Skills have already been expanded above when applicable.
            active_skills: &[],
        },
    )?;
    Ok(rendered.text)
}

fn queued_input(
    command: &Value,
    raw: &str,
    skills: &dyn SkillRegistry,
    prompts: &PromptRegistry,
    workspace: &Path,
    registered_tools: &[String],
) -> anyhow::Result<QueuedInput> {
    crate::commands::reject_tui_changelog(raw)?;
    let expanded = expand_queued_prompt(skills, prompts, workspace, registered_tools, raw)?;
    let input = input_from_command(command, expanded)?;
    let mut display_input = input.clone();
    if let Some(InputPart::Text(text)) = display_input.parts.first_mut() {
        *text = raw.to_owned();
    }
    Ok(QueuedInput {
        text: raw.to_owned(),
        message: user_input_value(&display_input),
        input,
    })
}

// A pending admission owns its input until the bounded RunControl send succeeds.
// Queue/settings projections and command acknowledgments commit only afterward.
enum RpcControlRequest {
    Steer(QueuedInput),
    FollowUp(QueuedInput),
    SteeringMode(String),
    FollowUpMode(String),
}

struct RpcAdmission {
    id: Option<String>,
    command: String,
    request: RpcControlRequest,
    replay: bool,
}

impl RpcAdmission {
    // Forwards `RunControl`'s own public error type unchanged.
    #[allow(clippy::result_large_err)]
    async fn send(&self, control: &RunControl) -> Result<(), AgentError> {
        match &self.request {
            RpcControlRequest::Steer(queued) => control.steer(queued.input.clone()).await,
            RpcControlRequest::FollowUp(queued) => control.follow_up(queued.input.clone()).await,
            RpcControlRequest::SteeringMode(mode) => {
                control.set_steering_mode(queue_mode(mode)).await
            }
            RpcControlRequest::FollowUpMode(mode) => {
                control.set_follow_up_mode(queue_mode(mode)).await
            }
        }
    }

    fn complete(
        self,
        result: Result<(), AgentError>,
        queue: &mut QueueState,
        settings: &mut RpcSettings,
        output: &mut RpcOutput,
    ) -> anyhow::Result<()> {
        // Replayed inputs were already acknowledged and remain in QueueState
        // until delivery; failed admission must not discard or duplicate them.
        if self.replay {
            return Ok(());
        }
        if let Err(error) = result {
            return output.error(self.id.as_deref(), &self.command, error.to_string());
        }
        let queue_changed = match self.request {
            RpcControlRequest::Steer(queued) => {
                queue.steering.push_back(queued);
                true
            }
            RpcControlRequest::FollowUp(queued) => {
                queue.follow_up.push_back(queued);
                true
            }
            RpcControlRequest::SteeringMode(mode) => {
                settings.steering_mode = mode;
                false
            }
            RpcControlRequest::FollowUpMode(mode) => {
                settings.follow_up_mode = mode;
                false
            }
        };
        output.success(self.id.as_deref(), &self.command, None)?;
        if queue_changed {
            output.send(queue.event())?;
        }
        Ok(())
    }
}

// RPC active-run routing intentionally exposes the independently borrowed
// protocol registries, queues, settings, and output sink at this dispatch boundary.
#[allow(clippy::too_many_arguments)]
fn active_input(
    command: Value,
    control: &RunControl,
    skills: &Arc<dyn SkillRegistry>,
    prompts: &PromptRegistry,
    workspace: &Path,
    state: &Value,
    commands: &Value,
    translator: &EventTranslator,
    queue: &mut QueueState,
    settings: &mut RpcSettings,
    output: &mut RpcOutput,
    deferred: &mut VecDeque<Value>,
) -> anyhow::Result<Option<RpcAdmission>> {
    let id = command_id(&command).map(str::to_owned);
    let kind = command_type(&command).unwrap_or("parse").to_owned();
    let result: anyhow::Result<Option<RpcControlRequest>> = (|| {
        match kind.as_str() {
            "abort" => {
                control.abort();
                output.success(id.as_deref(), &kind, None)?;
            }
            "abort_retry" => {
                if translator.retry_attempt.is_some() {
                    control.abort();
                }
                output.success(id.as_deref(), &kind, None)?;
            }
            "steer" | "follow_up" => {
                let raw = required_string(&command, "message")?;
                let queued = queued_input(
                    &command,
                    raw,
                    skills.as_ref(),
                    prompts,
                    workspace,
                    &settings.registered_tools,
                )?;
                return Ok(Some(if kind == "steer" {
                    RpcControlRequest::Steer(queued)
                } else {
                    RpcControlRequest::FollowUp(queued)
                }));
            }
            "prompt" => {
                let behavior = command
                    .get("streamingBehavior")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "Agent is already streaming; specify streamingBehavior as steer or followUp"
                        )
                    })?;
                let raw = required_string(&command, "message")?;
                let queued = queued_input(
                    &command,
                    raw,
                    skills.as_ref(),
                    prompts,
                    workspace,
                    &settings.registered_tools,
                )?;
                return Ok(Some(match behavior {
                    "steer" => RpcControlRequest::Steer(queued),
                    "followUp" => RpcControlRequest::FollowUp(queued),
                    _ => anyhow::bail!("streamingBehavior must be steer or followUp"),
                }));
            }
            "set_steering_mode" => {
                let mode = required_string(&command, "mode")?;
                parse_queue_mode(mode)?;
                return Ok(Some(RpcControlRequest::SteeringMode(mode.to_owned())));
            }
            "set_follow_up_mode" => {
                let mode = required_string(&command, "mode")?;
                parse_queue_mode(mode)?;
                return Ok(Some(RpcControlRequest::FollowUpMode(mode.to_owned())));
            }
            "get_state" => output.success(
                id.as_deref(),
                "get_state",
                Some(active_state_value(state, translator, queue)),
            )?,
            "get_messages" => output.success(
                id.as_deref(),
                "get_messages",
                Some(json!({"messages": translator.messages})),
            )?,
            "get_commands" => {
                output.success(id.as_deref(), "get_commands", Some(commands.clone()))?
            }
            // Mutating lifecycle commands are serialized at the first idle
            // boundary. Their response is deliberately delayed until applied.
            _ => deferred.push_back(command),
        }
        Ok(None)
    })();
    match result {
        Ok(request) => Ok(request.map(|request| RpcAdmission {
            id,
            command: kind,
            request,
            replay: false,
        })),
        Err(error) => {
            output.error(id.as_deref(), &kind, error.to_string())?;
            Ok(None)
        }
    }
}

// The run loop coordinates independently owned protocol state and channels;
// retaining explicit borrows makes mutation and queue ownership auditable.
#[allow(clippy::too_many_arguments)]
async fn drive_run(
    run: &mut Run<'_>,
    input: &mut mpsc::Receiver<RpcInput>,
    output: &mut RpcOutput,
    app_snapshot: &Value,
    commands: &Value,
    skills: Arc<dyn SkillRegistry>,
    prompts: Arc<PromptRegistry>,
    workspace: PathBuf,
    translator: &mut EventTranslator,
    queue: &mut QueueState,
    settings: &mut RpcSettings,
) -> anyhow::Result<(VecDeque<Value>, bool, HostRunOutcome)> {
    let control = run.control();
    translator.cache_warming_control = Some(control.clone());
    control.set_steering_mode(settings.steering_mode()).await?;
    control
        .set_follow_up_mode(settings.follow_up_mode())
        .await?;
    // Replay is ordered, but cannot await a full control channel without
    // polling the caller-driven Run. Only the two initial mode sends above
    // happen before polling (a fresh Run has room for both).
    let mut admissions: VecDeque<_> = queue
        .steering
        .iter()
        .cloned()
        .map(RpcControlRequest::Steer)
        .chain(
            queue
                .follow_up
                .iter()
                .cloned()
                .map(RpcControlRequest::FollowUp),
        )
        .map(|request| RpcAdmission {
            id: None,
            command: String::new(),
            request,
            replay: true,
        })
        .collect();
    let mut deferred = VecDeque::new();
    // Bounded lookahead lets abort/EOF bypass a blocked control admission.
    // Other commands retain FIFO order and backpressure the stdin reader.
    let mut waiting = VecDeque::new();
    let mut eof = false;
    enum Ready {
        Input(Option<RpcInput>),
        Event(Option<Box<AgentEvent>>),
        Waiting,
    }
    let finish = loop {
        // Admit ready controls before advancing the next boundary. This can
        // send at most the bounded channel capacity before Run must be polled.
        // Within that backpressure, stdin and Run selection is fair, so a
        // continuously ready input stream cannot starve run/abort settlement.
        let ready = tokio::select! {
            biased;
            _ = crate::tui::terminal::wait_for_shutdown_signal() => {
                control.abort();
                octet_agent::extension_process::terminate_bash_process_groups(
                    std::time::Duration::from_millis(400),
                )
                .await;
                // Coordinated process shutdown truncates the active RPC turn.
                // Avoid emitting a partial settlement tail that cannot be
                // followed by `agent_end` and `agent_settled`.
                return Ok((deferred, eof, HostRunOutcome::shutdown()));
            }
            admitted = async { admissions.front().expect("pending admission").send(&control).await }, if !admissions.is_empty() => {
                admissions.pop_front().expect("pending admission")
                    .complete(admitted, queue, settings, output)?;
                continue;
            }
            ready = async {
                tokio::select! {
                    _ = std::future::ready(()), if admissions.is_empty() && !waiting.is_empty() => Ready::Waiting,
                    inbound = input.recv(), if !eof && waiting.len() < 64 => Ready::Input(inbound),
                    event = run.next() => Ready::Event(event.map(Box::new)),
                }
            } => ready,
        };
        match ready {
            Ready::Waiting => {
                let command = waiting.pop_front().expect("waiting command");
                if let Some(admission) = active_input(
                    command,
                    &control,
                    &skills,
                    prompts.as_ref(),
                    &workspace,
                    app_snapshot,
                    commands,
                    translator,
                    queue,
                    settings,
                    output,
                    &mut deferred,
                )? {
                    admissions.push_back(admission);
                }
            }
            Ready::Input(Some(RpcInput::Value(command))) => {
                let cancellation = matches!(command_type(&command), Some("abort" | "abort_retry"));
                if cancellation || (admissions.is_empty() && waiting.is_empty()) {
                    if let Some(admission) = active_input(
                        command,
                        &control,
                        &skills,
                        prompts.as_ref(),
                        &workspace,
                        app_snapshot,
                        commands,
                        translator,
                        queue,
                        settings,
                        output,
                        &mut deferred,
                    )? {
                        admissions.push_back(admission);
                    }
                } else {
                    waiting.push_back(command);
                }
            }
            Ready::Input(Some(RpcInput::ParseError(error))) => {
                output.error(None, "parse", error)?
            }
            Ready::Input(Some(RpcInput::Eof) | None) => {
                eof = true;
                control.abort();
            }
            Ready::Event(event) => {
                let Some(event) = event else {
                    let outcome = HostRunOutcome::stream_lost();
                    translator.settle(outcome.clone(), output)?;
                    break outcome;
                };
                if let Some(reason) = translator.observe(*event, output, queue)? {
                    break reason;
                }
            }
        }
    };
    for admission in admissions {
        admission.complete(Err(AgentError::RunEnded), queue, settings, output)?;
    }
    // Commands read while active retain active routing even if settlement wins
    // admission: a prompt must not silently become a new idle run. Only the
    // existing lifecycle-command branch may defer work to the idle boundary.
    for command in waiting {
        if let Some(admission) = active_input(
            command,
            &control,
            &skills,
            prompts.as_ref(),
            &workspace,
            app_snapshot,
            commands,
            translator,
            queue,
            settings,
            output,
            &mut deferred,
        )? {
            admission.complete(Err(AgentError::RunEnded), queue, settings, output)?;
        }
    }
    Ok((deferred, eof, finish))
}

fn command_error(
    output: &mut RpcOutput,
    command: &Value,
    error: anyhow::Error,
) -> anyhow::Result<()> {
    output.error(
        command_id(command),
        command_type(command).unwrap_or("parse"),
        error.to_string(),
    )
}

async fn reload_resources(mut app: App) -> anyhow::Result<App> {
    app.system = compose_instructions(&app.config)?;
    app.system_tokens = estimate_text_tokens(&app.system);
    let mut app = rebuild_app(app, None, None, None, None)?;
    app.mark_resource_paths_reload();
    app.refresh_resource_paths_headless().await?;
    Ok(app)
}

fn available_models(app: &App) -> Vec<Value> {
    let mut ids = app
        .catalog
        .models()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.0.cmp(&right.0));
    ids.into_iter()
        .filter_map(|id| app.catalog.resolve(&id).ok())
        .map(|model| model_value(&model))
        .collect()
}

fn context_usage_value(app: &App) -> Value {
    let context_window = app.model.spec.limits.context_window;
    let mut cursor = app.agent.session().head_ref();
    let mut has_fresh_assistant_usage = false;
    let mut unknown_after_compaction = false;
    while let Some(id) = cursor {
        let Some(entry) = app.agent.session().entry(id) else {
            break;
        };
        match &entry.value {
            EntryValue::Message(Message::Assistant(_))
                if app.agent.session().usage_records().iter().any(|record| {
                    matches!(
                        &record.kind,
                        octet_agent::UsageRecordKind::AssistantTurn { assistant }
                            if assistant == id
                    )
                }) =>
            {
                has_fresh_assistant_usage = true;
            }
            EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. } => {
                unknown_after_compaction = !has_fresh_assistant_usage;
                break;
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    if unknown_after_compaction {
        return json!({
            "tokens": Value::Null,
            "contextWindow": context_window,
            "percent": Value::Null
        });
    }

    let messages = rpc_messages(app);
    let serialized = serde_json::to_string(&messages).unwrap_or_default();
    let tokens = app
        .system_tokens
        .saturating_add(estimate_text_tokens(&serialized));
    let percent = if context_window == 0 {
        0.0
    } else {
        tokens as f64 / context_window as f64 * 100.0
    };
    json!({
        "tokens": tokens,
        "contextWindow": context_window,
        "percent": percent
    })
}

fn session_stats_value(app: &App) -> Value {
    let mut stats = session_stats_for_session(app.agent.session());
    stats["sessionId"] = json!(session_id(app));
    stats["contextUsage"] = context_usage_value(app);
    stats["cacheWarmingMode"] = json!(app.agent.cache_warming_mode());
    stats["cacheWarmingStatus"] = json!(app.agent.cache_warming_status());
    stats
}

fn session_stats_for_session(session: &octet_agent::Session) -> Value {
    let mut user_messages = 0usize;
    let mut assistant_messages = 0usize;
    let mut tool_calls = 0usize;
    let mut tool_results = 0usize;
    let mut total_messages = 0usize;
    for entry in session.entries() {
        match &entry.value {
            EntryValue::Message(Message::Assistant(message)) => {
                assistant_messages = assistant_messages.saturating_add(1);
                total_messages = total_messages.saturating_add(1);
                tool_calls = tool_calls.saturating_add(
                    message
                        .content
                        .iter()
                        .filter(|part| matches!(part, AssistantPart::ToolCall(_)))
                        .count(),
                );
            }
            EntryValue::Message(Message::User(message)) => {
                let results = message
                    .content
                    .iter()
                    .filter(|part| matches!(part, UserPart::ToolResult(_)))
                    .count();
                if results == 0 {
                    user_messages = user_messages.saturating_add(1);
                    total_messages = total_messages.saturating_add(1);
                } else {
                    tool_results = tool_results.saturating_add(results);
                    total_messages = total_messages.saturating_add(results);
                }
            }
            _ => {}
        }
    }

    let mut usage = Usage::default();
    for record in session.usage_records() {
        usage.input_tokens = usage.input_tokens.saturating_add(record.usage.input_tokens);
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(record.usage.output_tokens);
        usage.cache_read_tokens = usage
            .cache_read_tokens
            .saturating_add(record.usage.cache_read_tokens);
        usage.cache_write_tokens = usage
            .cache_write_tokens
            .saturating_add(record.usage.cache_write_tokens);
    }
    let total_tokens = usage
        .input_tokens
        .saturating_add(usage.output_tokens)
        .saturating_add(usage.cache_read_tokens)
        .saturating_add(usage.cache_write_tokens);
    json!({
        "sessionFile": session.path(),
        "userMessages": user_messages,
        "assistantMessages": assistant_messages,
        "toolCalls": tool_calls,
        "toolResults": tool_results,
        "totalMessages": total_messages,
        "tokens": {
            "input": usage.input_tokens,
            "output": usage.output_tokens,
            "cacheRead": usage.cache_read_tokens,
            "cacheWrite": usage.cache_write_tokens,
            "total": total_tokens
        },
        "cost": dollars(session.total_cost_microdollars()),
        "usageUncertain": (session.has_uncertain_usage() || session.has_unpriced_usage())
    })
}

async fn handle_idle_command(
    mut app: App,
    command: Value,
    output: &mut RpcOutput,
    settings: &mut RpcSettings,
) -> anyhow::Result<App> {
    let id = command_id(&command).map(str::to_owned);
    let kind = command_type(&command).unwrap_or("parse").to_owned();
    macro_rules! respond_error {
        ($error:expr) => {{
            output.error(id.as_deref(), &kind, $error.to_string())?;
            return Ok(app);
        }};
    }

    match kind.as_str() {
        "get_state" => output.success(
            id.as_deref(),
            &kind,
            Some(state_value(&app, settings, false, 0, None)),
        )?,
        "get_messages" => output.success(
            id.as_deref(),
            &kind,
            Some(json!({"messages": rpc_messages(&app)})),
        )?,
        "get_commands" => output.success(id.as_deref(), &kind, Some(rpc_commands(&app)))?,
        "get_available_models" => output.success(
            id.as_deref(),
            &kind,
            Some(json!({"models": available_models(&app)})),
        )?,
        "get_available_thinking_levels" => output.success(
            id.as_deref(),
            &kind,
            Some(json!({
                "levels": supported_levels_with_subagents(&app.model, app.subagents_available())
                    .into_iter()
                    .map(|level| level.label())
                    .collect::<Vec<_>>()
            })),
        )?,
        "set_model" => {
            let model_id = match required_string(&command, "modelId") {
                Ok(model_id) => ModelId(model_id.to_owned()),
                Err(error) => respond_error!(error),
            };
            let model = match app.catalog.resolve(&model_id) {
                Ok(model) => model,
                Err(error) => respond_error!(error),
            };
            let value = model_value(&model);
            app = apply_reconfig(app, Reconfig::Model(model.spec.id.clone()))?;
            output.success(id.as_deref(), &kind, Some(value))?;
        }
        "cycle_model" => {
            let mut ids = app
                .catalog
                .models()
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            ids.sort_by(|left, right| left.0.cmp(&right.0));
            if ids.len() <= 1 {
                output.success(id.as_deref(), &kind, Some(Value::Null))?;
            } else {
                let index = ids
                    .iter()
                    .position(|candidate| candidate == &app.model.spec.id)
                    .unwrap_or_default();
                let model = app.catalog.resolve(&ids[(index + 1) % ids.len()])?;
                app = apply_reconfig(app, Reconfig::Model(model.spec.id.clone()))?;
                output.success(
                    id.as_deref(),
                    &kind,
                    Some(json!({
                        "model": model_value(&app.model),
                        "thinkingLevel": reasoning_label(&app.reasoning),
                        "isScoped": false
                    })),
                )?;
            }
        }
        "set_thinking_level" => {
            let level = match required_string(&command, "level").and_then(ThinkingLevel::parse) {
                Ok(level) => level,
                Err(error) => respond_error!(error),
            };
            let reasoning = match thinking_to_reasoning_with_subagents(
                level,
                &app.model,
                app.subagents_available(),
            ) {
                Ok(reasoning) => reasoning,
                Err(error) => respond_error!(error),
            };
            app = apply_reconfig(app, Reconfig::Thinking(reasoning))?;
            output.success(id.as_deref(), &kind, None)?;
        }
        "cycle_thinking_level" => {
            let levels = supported_levels_with_subagents(&app.model, app.subagents_available());
            if levels.len() <= 1 {
                output.success(id.as_deref(), &kind, Some(Value::Null))?;
            } else {
                let current = reasoning_label(&app.reasoning);
                let index = levels
                    .iter()
                    .position(|level| level.label() == current)
                    .unwrap_or_default();
                let level = levels[(index + 1) % levels.len()];
                let reasoning = thinking_to_reasoning_with_subagents(
                    level,
                    &app.model,
                    app.subagents_available(),
                )?;
                app = apply_reconfig(app, Reconfig::Thinking(reasoning))?;
                output.success(id.as_deref(), &kind, Some(json!({"level": level.label()})))?;
            }
        }
        "new_session" => {
            app = apply_reconfig(app, Reconfig::NewSession)?;
            output.success(id.as_deref(), &kind, Some(json!({"cancelled": false})))?;
        }
        "switch_session" => {
            let path = match required_string(&command, "sessionPath") {
                Ok(path) => PathBuf::from(path),
                Err(error) => respond_error!(error),
            };
            app = apply_reconfig(app, Reconfig::Resume(path))?;
            output.success(id.as_deref(), &kind, Some(json!({"cancelled": false})))?;
        }
        "reload" => {
            app = reload_resources(app).await?;
            output.success(id.as_deref(), &kind, None)?;
        }
        "compact" => {
            output.send(json!({"type": "compaction_start", "reason": "manual"}))?;
            let outcome = attempt_compaction(&mut app).await?;
            let data = match outcome {
                CompactionOutcome::Compacted { elided } => json!({"elided": elided}),
                CompactionOutcome::NativeCompacted => json!({"native": true}),
                CompactionOutcome::Skipped { reason } => json!({"skipped": true, "reason": reason}),
            };
            output.send(json!({"type": "compaction_end", "result": data}))?;
            output.success(id.as_deref(), &kind, Some(data))?;
        }
        "set_auto_compaction" => {
            let enabled = match required_bool(&command, "enabled") {
                Ok(enabled) => enabled,
                Err(error) => respond_error!(error),
            };
            app.config.compaction.mode = if enabled {
                CompactionMode::Local
            } else {
                CompactionMode::Disabled
            };
            let effective_threshold =
                effective_compaction_threshold_fraction(&app.config, &app.model);
            app.agent.set_compaction_token_mode(
                if enabled {
                    AgentCompactionMode::Local
                } else {
                    AgentCompactionMode::Disabled
                },
                effective_threshold,
                app.config.compaction.keep_recent_tokens,
            )?;
            output.success(id.as_deref(), &kind, None)?;
        }
        "set_steering_mode" | "set_follow_up_mode" => {
            let mode = match required_string(&command, "mode") {
                Ok("all" | "one-at-a-time") => required_string(&command, "mode")?.to_owned(),
                Ok(_) => respond_error!(anyhow::anyhow!("mode must be all or one-at-a-time")),
                Err(error) => respond_error!(error),
            };
            if kind == "set_steering_mode" {
                settings.steering_mode = mode;
            } else {
                settings.follow_up_mode = mode;
            }
            output.success(id.as_deref(), &kind, None)?;
        }
        "set_auto_retry" => {
            let enabled = match required_bool(&command, "enabled") {
                Ok(enabled) => enabled,
                Err(error) => respond_error!(error),
            };
            settings.auto_retry_enabled = enabled;
            app.agent.set_provider_retries_enabled(enabled);
            output.success(id.as_deref(), &kind, None)?;
        }
        "abort_retry" | "abort" | "abort_bash" => {
            output.success(id.as_deref(), &kind, None)?;
        }
        "get_session_stats" => {
            output.success(id.as_deref(), &kind, Some(session_stats_value(&app)))?
        }
        "get_entries" => {
            let entries = app.agent.session().entries();
            let start = if let Some(since) = command.get("since") {
                let Some(since) = since.as_str() else {
                    respond_error!(anyhow::anyhow!("since must be a string"));
                };
                let Some(index) = entries.iter().position(|entry| entry.id.0 == since) else {
                    respond_error!(anyhow::anyhow!("Entry not found: {since}"));
                };
                index.saturating_add(1)
            } else {
                0
            };
            let projection = RpcSessionProjection::new(&app);
            let projected = entries[start..]
                .iter()
                .map(|entry| projection.entry(entry))
                .collect::<Vec<_>>();
            output.success(
                id.as_deref(),
                &kind,
                Some(json!({
                    "entries": projected,
                    "leafId": app.agent.session().head_ref().map(|head| &head.0)
                })),
            )?;
        }
        "get_tree" => {
            let projection = RpcSessionProjection::new(&app);
            output.success(
                id.as_deref(),
                &kind,
                Some(json!({
                    "tree": rpc_tree(&app, &projection),
                    "leafId": app.agent.session().head_ref().map(|head| &head.0)
                })),
            )?;
        }
        "get_last_assistant_text" => {
            let text = app.agent.session().context().ok().and_then(|messages| {
                messages
                    .into_iter()
                    .rev()
                    .find_map(|message| match message {
                        Message::Assistant(message) => Some(
                            message
                                .content
                                .iter()
                                .filter_map(|part| match part {
                                    AssistantPart::Text(text) => Some(text.as_str()),
                                    _ => None,
                                })
                                .collect::<String>(),
                        ),
                        Message::User(_) => None,
                    })
            });
            output.success(id.as_deref(), &kind, Some(json!({"text": text})))?;
        }
        "set_session_name" => {
            let name = match required_string(&command, "name") {
                Ok(name) => name,
                Err(error) => respond_error!(error),
            };
            if let Err(error) = app.sessions.rename(&session_id(&app), name) {
                respond_error!(error);
            }
            output.success(id.as_deref(), &kind, None)?;
        }
        "export_html" | "fork" | "clone" | "get_fork_messages" => {
            output.error(
                id.as_deref(),
                &kind,
                format!("Command {kind:?} is not supported by octet RPC mode"),
            )?;
        }
        _ => output.error(id.as_deref(), &kind, format!("Unknown command: {kind}"))?,
    }
    Ok(app)
}

/// Run the strict LF-delimited JSONL RPC frontend until stdin closes.
pub async fn run_rpc(boot: Bootstrap) -> anyhow::Result<()> {
    let app = (|| {
        let launch = resolve_launch_print(&boot, &crate::modes::timestamp())?;
        let system = compose_instructions(&boot.config)?;
        build_app(boot, launch, system)
    })();
    match app {
        Ok(app) => {
            // Stdout is the JSONL protocol; startup notices go to stderr.
            for notice in app.executable_extensions.startup_failure_notices() {
                crate::output::stderr!("warning: {notice}");
            }
            let result = run_rpc_loop(app, spawn_input_reader(), RpcOutput::new()).await;
            finish_rpc_accounting(result)
        }
        Err(error) => finish_rpc_accounting(Err(error)),
    }
}

fn finish_rpc_accounting(result: anyhow::Result<()>) -> anyhow::Result<()> {
    // This boundary is also reached for startup, RPC framing, command and
    // stdout errors. The loop owns App; on an early exit its extension runtime
    // shuts down through ExecutableExtensions::drop before accounting is read.
    let accounting = crate::modes::print::finish_ephemeral_accounting();
    match result {
        Ok(()) => accounting,
        Err(error) => {
            if let Err(accounting_error) = accounting {
                crate::output::stderr_line(format!(
                    "warning: ephemeral accounting failed: {accounting_error:#}"
                ));
            }
            Err(error)
        }
    }
}

async fn run_rpc_loop(
    mut app: App,
    mut input: mpsc::Receiver<RpcInput>,
    mut output: RpcOutput,
) -> anyhow::Result<()> {
    let mut deferred = VecDeque::new();
    let mut settings = RpcSettings {
        registered_tools: app.agent.registered_tool_names(),
        ..RpcSettings::default()
    };
    let mut queue = QueueState::default();
    let mut eof = false;
    // Keep bounded spill files available between RPC commands and clean them
    // up when this frontend owner exits.
    let bash_tool = Arc::new(BashTool);

    while !eof {
        app.refresh_resource_paths_headless().await?;
        let inbound = if let Some(command) = deferred.pop_front() {
            RpcInput::Value(command)
        } else {
            let mut cache_warming_failed = false;
            loop {
                tokio::select! {
                    biased;
                    _ = crate::tui::terminal::wait_for_shutdown_signal() => break RpcInput::Eof,
                    inbound = input.recv() => break inbound.unwrap_or(RpcInput::Eof),
                    warm = app.agent.drive_cache_warming(), if !cache_warming_failed => {
                        match warm {
                            Ok(event) => events::emit_cache_warming_event(&event, &mut output)?,
                            Err(_) => {
                                cache_warming_failed = true;
                                crate::output::stderr!("warning: cache warming stopped; usage may be uncertain. See /cache-warming.");
                            }
                        }
                    }
                }
            }
        };
        let command = match inbound {
            RpcInput::ParseError(error) => {
                output.error(None, "parse", error)?;
                continue;
            }
            RpcInput::Eof => break,
            RpcInput::Value(command) => command,
        };
        app.refresh_resource_paths_headless().await?;
        let Some(kind) = command_type(&command).map(str::to_owned) else {
            output.error(
                command_id(&command),
                "parse",
                "Command must be an object with a string type",
            )?;
            continue;
        };

        if kind == "bash" {
            let id = command_id(&command).map(str::to_owned);
            let shell_command = match required_string(&command, "command") {
                Ok(command) => command.to_owned(),
                Err(error) => {
                    output.error(id.as_deref(), "bash", error.to_string())?;
                    continue;
                }
            };
            let exclude_from_context = match optional_bool(&command, "excludeFromContext") {
                Ok(value) => value.unwrap_or(false),
                Err(error) => {
                    output.error(id.as_deref(), "bash", error.to_string())?;
                    continue;
                }
            };
            let workspace = app.config.workspace.clone();
            let sandbox = app.config.sandbox.to_sandbox_config(&workspace);
            let cancellation = CancellationToken::default();
            let task = tokio::spawn(run_sandboxed_bash(
                Arc::clone(&bash_tool),
                shell_command.clone(),
                workspace,
                sandbox,
                cancellation.clone(),
            ));
            let mut active = ActiveRpcBash {
                id,
                command: shell_command,
                exclude_from_context,
                cancellation,
                task,
            };

            // Commands deferred by an agent run retain their order, but an
            // already-buffered abort must be able to stop this command now.
            let mut held = VecDeque::new();
            while let Some(pending) = deferred.pop_front() {
                if command_type(&pending) == Some("abort_bash") {
                    active.cancellation.cancel();
                    output.success(command_id(&pending), "abort_bash", None)?;
                } else {
                    held.push_back(pending);
                }
            }
            let (result, newly_deferred, input_eof) =
                drive_rpc_bash(&mut active, &mut input, &mut output).await?;
            held.extend(newly_deferred);
            deferred = held;
            eof |= input_eof;

            match result {
                Ok(result) => {
                    if !active.exclude_from_context {
                        if let Err(error) = app.agent.session_mut().append(EntryValue::Message(
                            bash_context_message(&active.command, &result),
                        )) {
                            output.error(active.id.as_deref(), "bash", error.to_string())?;
                            continue;
                        }
                    }
                    output.success(active.id.as_deref(), "bash", Some(result.value()))?;
                }
                Err(error) => {
                    output.error(active.id.as_deref(), "bash", error.to_string())?;
                }
            }
            continue;
        }

        // steer/follow_up queue at idle; they're applied when the next
        // prompt starts (drive_run re-submits QueueState).
        if kind == "steer" || kind == "follow_up" {
            let id = command_id(&command).map(str::to_owned);
            let raw = match required_string(&command, "message") {
                Ok(message) => message.to_owned(),
                Err(error) => {
                    output.error(id.as_deref(), &kind, error.to_string())?;
                    continue;
                }
            };
            let queued = match queued_input(
                &command,
                &raw,
                app.skills.as_ref(),
                app.prompts.as_ref(),
                &app.config.workspace,
                &app.agent.registered_tool_names(),
            ) {
                Ok(queued) => queued,
                Err(error) => {
                    output.error(id.as_deref(), &kind, error.to_string())?;
                    continue;
                }
            };
            if kind == "steer" {
                queue.steering.push_back(queued);
            } else {
                queue.follow_up.push_back(queued);
            }
            output.success(id.as_deref(), &kind, None)?;
            output.send(queue.event())?;
            continue;
        }

        if kind != "prompt" {
            app = handle_idle_command(app, command, &mut output, &mut settings).await?;
            continue;
        }

        // `/reload` is a resource command in every octet frontend. RPC applies it
        // synchronously at this idle boundary without creating a user turn.
        if command.get("message").and_then(Value::as_str) == Some("/reload") {
            let id = command_id(&command).map(str::to_owned);
            app = reload_resources(app).await?;
            output.success(id.as_deref(), "prompt", None)?;
            continue;
        }

        if app.config.prompt_template.is_none() {
            if let Some(message) = command.get("message").and_then(Value::as_str) {
                match crate::commands::handle_cache_warming_input(&mut app, message) {
                    Ok(true) => {
                        output.success(
                            command_id(&command),
                            "prompt",
                            Some(json!({
                                "cacheWarmingMode": app.agent.cache_warming_mode(),
                                "cacheWarmingStatus": app.agent.cache_warming_status(),
                            })),
                        )?;
                        continue;
                    }
                    Ok(false) => {}
                    Err(error) => {
                        command_error(&mut output, &command, error)?;
                        continue;
                    }
                }
            }
        }

        let id = command_id(&command).map(str::to_owned);
        let (prompt, pending_context_count, user_message) =
            match prepare_prompt(&mut app, &command).await {
                Ok(prepared) => prepared,
                Err(error) => {
                    command_error(&mut output, &command, error)?;
                    continue;
                }
            };
        let skills = app.skills.clone();
        let prompts = app.prompts.clone();
        let workspace = app.config.workspace.clone();
        let commands = rpc_commands(&app);
        let state = state_value(&app, &settings, true, queue.len(), None);
        let mut translator = EventTranslator::new(&app, user_message.clone());
        settings.registered_tools = app.agent.registered_tool_names();
        app.agent
            .set_provider_retries_enabled(settings.auto_retry_enabled);
        let prior_cache_misses = crate::commands::cache_miss_count(&app);
        let mut run = match app.agent.prompt_with_responses_prewarm(prompt).await {
            Ok(run) => run,
            Err(error) => {
                output.error(
                    id.as_deref(),
                    "prompt",
                    rpc_error_diagnostic(&translator.model, &error),
                )?;
                continue;
            }
        };
        let extension_turn = app.executable_extensions.begin_turn().await;
        app.executable_extensions
            .commit_prompt_context(pending_context_count);
        output.success(id.as_deref(), "prompt", None)?;
        output.send(json!({"type": "agent_start"}))?;
        output.send(json!({"type": "turn_start"}))?;
        output.send(json!({"type": "message_start", "message": user_message}))?;
        output.send(json!({"type": "message_end", "message": user_message}))?;

        let (queued, input_eof, finish) = drive_run(
            &mut run,
            &mut input,
            &mut output,
            &state,
            &commands,
            skills,
            prompts,
            workspace,
            &mut translator,
            &mut queue,
            &mut settings,
        )
        .await?;
        drop(run);
        if let Some(notice) = crate::commands::cache_miss_notice(&app, prior_cache_misses) {
            crate::output::stderr_line(notice);
        }
        app.executable_extensions
            .settle_turn(extension_turn, &finish)
            .await;
        app.agent.set_system_prompt(app.system.clone());
        if finish.shutdown_requested() {
            let _ = app.executable_extensions.drain_events();
            app.synchronize_extension_provider_catalog();
            let extension_presentations = app.executable_extensions.presentation_views();
            output.send(json!({
                "type": "extension_presentations",
                "presentations": extension_presentations,
            }))?;
            app.executable_extensions.shutdown().await;
            return Ok(());
        }
        deferred.extend(queued);
        eof = input_eof;
        output.send(json!({
            "type": "agent_end",
            "messages": translator.run_messages,
            "willRetry": false
        }))?;
        if let Some(event) = translator.pending_retry_end.take() {
            output.send(event)?;
        }
        if finish.allows_after_response() {
            for notification in app
                .executable_extensions
                .after_response(&translator.last_assistant_text)
                .await
            {
                crate::output::stderr_line(format!("extension: {notification}"));
            }
        }
        let _ = app.executable_extensions.drain_events();
        app.synchronize_extension_provider_catalog();
        let extension_presentations = app.executable_extensions.presentation_views();
        output.send(json!({
            "type": "extension_presentations",
            "presentations": extension_presentations,
        }))?;
        output.send(json!({"type": "agent_settled"}))?;
    }

    app.executable_extensions.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests;
