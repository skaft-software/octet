//! The octet side of the differential harness: replay a scenario through the
//! real `Session` / `SessionStore` API and snapshot the result.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use octet_agent::session::{CustomMessage, CustomMessageContent, CustomMessagePart};
use octet_agent::{EntryId, EntryValue, Session};
use octet_ai::{
    AssistantMessage, AssistantPart, ImageMedia, ImageSource, Media, Message, ModelId, Protocol,
    ReasoningPart, ReasoningState, ReasoningStateKind, ToolCall, ToolCallId, ToolResult,
    ToolResultPart, UserMessage, UserPart,
};
use serde_json::Value;

use super::canonical::{self, Snapshot};
use crate::session_store::SessionStore;

/// Extension namespace used for `custom` entries in native scenarios.
pub(super) const NAMESPACE: &str = "parity-fixture";

/// What replaying one operation did.
pub(super) enum Outcome {
    Applied,
    /// octet cannot represent the operation; the reason is stable test evidence.
    Refused(String),
}

pub(super) struct Native {
    store: SessionStore,
    session: Option<Session>,
    ids: HashMap<String, EntryId>,
    slots: HashMap<String, PathBuf>,
}

impl Native {
    pub(super) fn new(root: &Path) -> anyhow::Result<Self> {
        let workspace = root.join("work");
        let sessions = root.join("octet-sessions");
        std::fs::create_dir_all(&workspace)?;
        std::fs::create_dir_all(&sessions)?;
        let store = SessionStore::new(&sessions, &workspace);
        std::fs::create_dir_all(store.dir())?;
        let session = Session::create(store.new_path("session"))?;
        Ok(Self {
            store,
            session: Some(session),
            ids: HashMap::new(),
            slots: HashMap::new(),
        })
    }

    fn session(&self) -> &Session {
        self.session.as_ref().expect("native session is open")
    }

    fn session_mut(&mut self) -> &mut Session {
        self.session.as_mut().expect("native session is open")
    }

    fn id(&self) -> anyhow::Result<String> {
        Ok(self
            .session()
            .path()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| anyhow::anyhow!("native session path has no id"))?
            .to_owned())
    }

    fn entry(&self, op: &Value, key: &str) -> Result<EntryId, String> {
        let name = op
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing {key}"))?;
        self.ids
            .get(name)
            .cloned()
            .ok_or_else(|| format!("unknown logical entry {name}"))
    }

    /// Replay one scenario operation. `Err(String)` is a capability refusal.
    pub(super) fn apply(&mut self, op: &Value) -> Outcome {
        let kind = op
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let result = self.apply_inner(&kind, op);
        if let Some(name) = op.get("as").and_then(Value::as_str) {
            if let Some(id) = self.session().head() {
                self.ids.insert(name.to_owned(), id);
            }
        }
        match result {
            Ok(()) => Outcome::Applied,
            Err(reason) => Outcome::Refused(reason),
        }
    }

    fn apply_inner(&mut self, kind: &str, op: &Value) -> Result<(), String> {
        match kind {
            "message" => self.append_message(op),
            "custom" => {
                let custom_type = op
                    .get("customType")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "custom:type".to_owned())?;
                let data = op.get("data").cloned().unwrap_or(Value::Null);
                self.session_mut()
                    .append_extension_entry(NAMESPACE, Some(1), custom_type, data)
                    .map(|_| ())
                    .map_err(|error| format!("custom:{error}"))
            }
            "custom_message" => self.append_custom_message(op),
            "model" => {
                let model = format!("{}/{}", text(op, "provider"), text(op, "modelId"));
                self.append_config(Some(model), None)
            }
            "thinking" => self.append_config(
                None,
                op.get("level").and_then(Value::as_str).map(str::to_owned),
            ),
            // Pi exposes appendUsage to extensions for arbitrary categories;
            // octet has no appendable model-attributed usage entry, only a
            // per-operation ledger written by the agent loop itself.
            "usage" => Err("usage".into()),
            "label" => {
                let target = self.entry(op, "target")?;
                let label = op.get("label").and_then(Value::as_str).unwrap_or("");
                self.session_mut()
                    .set_entry_label(&target, label)
                    .map(|_| ())
                    .map_err(|error| format!("label:{error}"))
            }
            "name" => self.rename(op),
            "branch" => {
                let target = self.entry(op, "target")?;
                self.session_mut()
                    .checkout(target)
                    .map_err(|error| format!("branch:{error}"))
            }
            "reset" => self
                .session_mut()
                .checkout_root()
                .map_err(|error| format!("reset:{error}")),
            "summary" => self.branch_summary(op),
            "compaction" => self.compact(op),
            // Pi's append-only context edits have no octet entry type, and the
            // importer refuses the record.
            "edit" => Err("edit".into()),
            "reopen" => self.reopen(),
            "fork" => {
                let target = self.entry(op, "target")?;
                self.fork(Some(target))
            }
            "fork_all" => {
                let head = self.session().head();
                self.fork(head)
            }
            "new" => {
                let slot = text(op, "slot").to_owned();
                let current = self.session().path().to_owned();
                self.slots.insert(slot, current);
                match Session::create(self.store.new_path("slot")) {
                    Ok(session) => {
                        self.session = Some(session);
                        Ok(())
                    }
                    Err(error) => Err(format!("new:{error}")),
                }
            }
            "switch" => {
                let slot = text(op, "slot").to_owned();
                match self.slots.get(&slot) {
                    Some(path) => {
                        let path = path.clone();
                        self.close();
                        match Session::open(&path) {
                            Ok(session) => {
                                self.session = Some(session);
                                Ok(())
                            }
                            Err(error) => Err(format!("switch:{error}")),
                        }
                    }
                    None => Err("switch:unknown-slot".into()),
                }
            }
            "recent" => {
                self.close();
                match self.store.latest() {
                    Ok(meta) => match Session::open(&meta.path) {
                        Ok(session) => {
                            self.session = Some(session);
                            Ok(())
                        }
                        Err(error) => Err(format!("recent:{error}")),
                    },
                    Err(error) => Err(format!("recent:{error}")),
                }
            }
            "probe" => self.probe(),
            "context" => Ok(()),
            other => Err(format!("unknown-operation:{other}")),
        }
    }

    fn close(&mut self) {
        // Dropping the writable handle closes its descriptor and lock.
        self.session = None;
    }

    fn append_message(&mut self, op: &Value) -> Result<(), String> {
        let message = op.get("message").ok_or("message:missing")?;
        let role = text(message, "role");
        let value = match role {
            "user" => EntryValue::Message(Message::User(UserMessage {
                content: user_content(&message["content"])?,
            })),
            "assistant" => {
                let protocol = protocol(text(message, "api"))?;
                let model = ModelId(text(message, "model").to_owned());
                EntryValue::Message(Message::Assistant(AssistantMessage {
                    content: assistant_content(&message["content"], protocol, &model)?,
                    model,
                    protocol,
                }))
            }
            "toolResult" => {
                let call = text(message, "toolCallId");
                EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        tool_call_id: ToolCallId(call.to_owned()),
                        content: tool_content(&message["content"])?,
                        is_error: message
                            .get("isError")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        added_tool_names: None,
                    })],
                }))
            }
            other => return Err(format!("message:role:{other}")),
        };
        self.session_mut()
            .append(value)
            .map(|_| ())
            .map_err(|error| format!("message:{error}"))
    }

    fn append_custom_message(&mut self, op: &Value) -> Result<(), String> {
        let content = match &op["content"] {
            Value::String(text) => CustomMessageContent::Text(text.clone()),
            Value::Array(blocks) => CustomMessageContent::Parts(
                blocks
                    .iter()
                    .map(|block| match text(block, "type") {
                        "image" => CustomMessagePart::Image {
                            data: text(block, "data").to_owned(),
                            mime_type: text(block, "mimeType").to_owned(),
                        },
                        _ => CustomMessagePart::Text {
                            text: text(block, "text").to_owned(),
                        },
                    })
                    .collect(),
            ),
            _ => return Err("custom_message:content".into()),
        };
        let custom = CustomMessage {
            custom_type: text(op, "customType").to_owned(),
            content,
            display: op.get("display").and_then(Value::as_bool).unwrap_or(false),
            details: op.get("details").cloned(),
        };
        self.session_mut()
            .append_custom_message(custom, None)
            .map(|_| ())
            .map_err(|error| format!("custom_message:{error}"))
    }

    fn append_config(
        &mut self,
        model: Option<String>,
        reasoning: Option<String>,
    ) -> Result<(), String> {
        self.session_mut()
            .append(EntryValue::Config {
                model,
                reasoning,
                reasoning_mode: None,
            })
            .map(|_| ())
            .map_err(|error| format!("config:{error}"))
    }

    fn rename(&mut self, op: &Value) -> Result<(), String> {
        let name = text(op, "name");
        // Pi replaces line breaks with spaces; octet refuses control characters
        // outright. That refusal is the observed divergence.
        if name.chars().any(char::is_control) {
            return Err("name:control-characters".into());
        }
        let id = self.id().map_err(|error| format!("name:{error}"))?;
        self.store
            .rename(&id, name)
            .map(|_| ())
            .map_err(|error| format!("name:{error}"))
    }

    fn branch_summary(&mut self, op: &Value) -> Result<(), String> {
        let details = compaction_details(op.get("details"))?;
        let summary = text(op, "summary").to_owned();
        let parent = if op.get("target").is_none() || op["target"].is_null() {
            None
        } else {
            Some(self.entry(op, "target")?)
        };
        self.session_mut()
            .branch_with_summary(parent, summary, details)
            .map(|_| ())
            .map_err(|error| format!("summary:{error}"))
    }

    fn compact(&mut self, op: &Value) -> Result<(), String> {
        if op.get("firstKept").is_none() || op["firstKept"].is_null() {
            return Err("compaction:empty-retention".into());
        }
        let first_kept = self.entry(op, "firstKept")?;
        let details = compaction_details(op.get("details"))?;
        let summary = text(op, "summary").to_owned();
        self.session_mut()
            .compact_with_details(summary, first_kept, details)
            .map(|_| ())
            .map_err(|error| format!("compaction:{error}"))
    }

    fn reopen(&mut self) -> Result<(), String> {
        let path = self.session().path().to_owned();
        self.close();
        match Session::open(&path) {
            Ok(session) => {
                self.session = Some(session);
                Ok(())
            }
            Err(error) => Err(format!("reopen:{error}")),
        }
    }

    fn fork(&mut self, checkpoint: Option<EntryId>) -> Result<(), String> {
        let destination = self.store.new_path("fork");
        let forked = self
            .session()
            .fork_to(destination, checkpoint.as_ref())
            .map_err(|error| format!("fork:{error}"))?;
        self.session = Some(forked);
        Ok(())
    }

    fn probe(&mut self) -> Result<(), String> {
        let id = self.id().map_err(|error| format!("probe:{error}"))?;
        let path = self.session().path().to_owned();
        match self.store.path_by_id(&id) {
            Ok(resolved) if resolved == path => {}
            Ok(_) => return Err("probe:path-by-id-mismatch".into()),
            Err(error) => return Err(format!("probe:path-by-id:{error}")),
        }
        if !self.store.list().iter().any(|meta| meta.id == id) {
            return Err("probe:list-missing".into());
        }
        if !self.store.list_all().iter().any(|meta| meta.id == id) {
            return Err("probe:list-all-missing".into());
        }
        Ok(())
    }

    /// Observable state of the current octet session.
    pub(super) fn snapshot(&self) -> anyhow::Result<Snapshot> {
        let session = self.session();
        let mut snapshot = self.project(session, session.path().exists())?;
        snapshot.settings_error = self.settings_error(session);
        Ok(snapshot)
    }

    /// Read-only reopen of the live file, used to prove replay is stable.
    pub(super) fn reopened(&self) -> anyhow::Result<Snapshot> {
        let session = self.session();
        let reopened = Session::open_read_only(session.path())?;
        let mut snapshot = self.project(&reopened, true)?;
        snapshot.settings_error = self.settings_error(&reopened);
        Ok(snapshot)
    }

    fn project(&self, session: &Session, has_file: bool) -> anyhow::Result<Snapshot> {
        let id = self.id()?;
        let (model, thinking) = if session.entries().is_empty() {
            (None, None)
        } else {
            match self.store.inspect_by_id(&id) {
                Ok(inspected) => (
                    inspected.catalog.configured_model,
                    inspected.catalog.configured_reasoning,
                ),
                Err(_) => (None, None),
            }
        };
        let name = self.store.load_metadata(&id)?.name;
        canonical::from_octet(session, NAMESPACE, name, (model, thinking), has_file)
    }

    /// octet's resume projection can refuse a file the session reader accepts.
    fn settings_error(&self, session: &Session) -> Option<String> {
        if session.entries().is_empty() {
            return None;
        }
        let id = self.id().ok()?;
        // The same projection resume uses to restore model and effort.
        self.store
            .inspect_by_id(&id)
            .err()
            .map(|error| format!("{error:#}"))
    }
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn protocol(api: &str) -> Result<Protocol, String> {
    Ok(match api {
        "openai-completions" => Protocol::OpenAiChat,
        "openai-responses" => Protocol::OpenAiResponses,
        "anthropic-messages" => Protocol::AnthropicMessages,
        "bedrock-converse-stream" => Protocol::BedrockConverse,
        "google-generative-ai" | "google-vertex" => Protocol::GoogleGenerativeAi,
        "mistral-conversations" => Protocol::MistralConversations,
        "pi-messages" => Protocol::PiMessages,
        other => return Err(format!("message:api:{other}")),
    })
}

fn image_media(block: &Value) -> Result<Media, String> {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD
        .decode(text(block, "data"))
        .map_err(|_| "message:image-base64".to_owned())?;
    let media_type = text(block, "mimeType")
        .parse::<mime::Mime>()
        .map_err(|_| "message:image-mime".to_owned())?;
    Ok(Media::Image(ImageMedia {
        source: ImageSource::Inline(data.into()),
        media_type: Some(media_type),
        detail: None,
    }))
}

fn user_content(value: &Value) -> Result<Vec<UserPart>, String> {
    match value {
        Value::String(text) => Ok(vec![UserPart::Text(text.clone())]),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match text(block, "type") {
                "image" => Ok(UserPart::Media(image_media(block)?)),
                _ => Ok(UserPart::Text(text(block, "text").to_owned())),
            })
            .collect(),
        _ => Err("message:content".into()),
    }
}

fn tool_content(value: &Value) -> Result<Vec<ToolResultPart>, String> {
    match value {
        Value::String(text) => Ok(vec![ToolResultPart::Text(text.clone())]),
        Value::Array(blocks) => blocks
            .iter()
            .map(|block| match text(block, "type") {
                "image" => Ok(ToolResultPart::Media(image_media(block)?)),
                _ => Ok(ToolResultPart::Text(text(block, "text").to_owned())),
            })
            .collect(),
        _ => Err("message:tool-content".into()),
    }
}

fn assistant_content(
    value: &Value,
    protocol: Protocol,
    model: &ModelId,
) -> Result<Vec<AssistantPart>, String> {
    let blocks = value
        .as_array()
        .ok_or_else(|| "message:assistant-content".to_owned())?;
    blocks
        .iter()
        .map(|block| match text(block, "type") {
            "thinking" => {
                let signature = block.get("thinkingSignature").and_then(Value::as_str);
                let state = match signature {
                    None => None,
                    Some(signature) => Some(ReasoningState {
                        protocol,
                        model: model.clone(),
                        kind: match protocol {
                            Protocol::AnthropicMessages => ReasoningStateKind::AnthropicSignature {
                                signature: signature.to_owned(),
                            },
                            Protocol::OpenAiResponses => ReasoningStateKind::OpenAiReasoning {
                                item_id: None,
                                encrypted_content: Some(signature.to_owned()),
                            },
                            _ => return Err("message:thinking-signature".into()),
                        },
                    }),
                };
                Ok(AssistantPart::Reasoning(ReasoningPart {
                    text: Some(text(block, "thinking").to_owned()),
                    state,
                }))
            }
            "toolCall" => Ok(AssistantPart::ToolCall(ToolCall {
                async_execution: false,
                id: ToolCallId(text(block, "id").to_owned()),
                name: text(block, "name").to_owned(),
                arguments_json: serde_json::to_string(
                    block.get("arguments").unwrap_or(&Value::Null),
                )
                .map_err(|_| "message:tool-arguments".to_owned())?,
                argument_error: None,
            })),
            "image" => Ok(AssistantPart::Media(image_media(block)?)),
            _ => Ok(AssistantPart::Text(text(block, "text").to_owned())),
        })
        .collect()
}

fn compaction_details(
    value: Option<&Value>,
) -> Result<octet_agent::compaction::CompactionDetails, String> {
    match value {
        None | Some(Value::Null) => Ok(Default::default()),
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| "details:unsupported-shape".to_owned()),
    }
}
