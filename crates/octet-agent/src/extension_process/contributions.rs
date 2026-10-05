//! Wire types for what an extension contributes: hook outputs, UI, editor, composer, terminal and session operations.

use super::*;

/// Output from an extension slash command.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CommandOutput {
    /// Text to display to the user.
    #[serde(default)]
    pub text: String,
    /// Notifications emitted by the command.
    #[serde(default)]
    pub notifications: Vec<ExtensionNotification>,
    /// Optional context that should be considered by prompt composition.
    #[serde(default)]
    pub context: Vec<ContextContribution>,
}

/// Where a context contribution is inserted by prompt composition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextPlacement {
    /// Before the host system prompt.
    SystemPrefix,
    /// After the host system prompt.
    SystemSuffix,
    /// Before the immediate user prompt.
    PromptPrefix,
    /// After the immediate user prompt.
    #[default]
    PromptSuffix,
}

/// Text contributed to prompt composition by an extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextContribution {
    /// Stable label shown in context inspection.
    pub label: String,
    /// Plain text sent to the model after host-side size enforcement.
    pub content: String,
    /// Semantic insertion point.
    #[serde(default)]
    pub placement: ContextPlacement,
}

/// A lifecycle hook's disposition.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExtensionHookDisposition {
    /// Continue the normal operation.
    #[default]
    Continue,
    /// Deny an interceptable operation such as `before_tool_call`.
    Deny {
        /// Inspectable reason presented to the user and model.
        reason: String,
    },
}

/// Provider-retry advice accepted only from the typed `provider_retry` hook.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProviderRetryAdvice {
    /// Support the host-proposed retry without expanding its budget.
    Retry,
    /// Add bounded delay to the host-selected retry time. The host clamps this
    /// value and never lets an extension shorten provider backoff.
    Delay {
        /// Requested additional delay in milliseconds.
        additional_delay_ms: u64,
    },
    /// Decline the host-proposed retry.
    Stop,
}

/// One process-owned metadata value proposed by `before_persistence`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionPersistenceMetadata {
    /// Whether ordinary frontend/export projections may expose this value.
    #[serde(default)]
    pub public: bool,
    /// Inert JSON owned by the manifest's extension namespace.
    pub value: serde_json::Value,
}

/// Post-mutation disposition accepted only from a typed `post_mutation` hook.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ExtensionPostMutationDisposition {
    /// The extension does not need a host resource rescan.
    #[default]
    NoRescan,
    /// Request a bounded subset of the host-disclosed affected resources.
    RequestRescan {
        /// Opaque resource identities selected for rescan.
        resource_ids: Vec<String>,
    },
}

/// Typed hook response.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHookOutput {
    /// Custom messages returned by before_agent_start, appended after the prompt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_messages: Vec<crate::session::CustomMessage>,
    /// Raw-input decision applied before prompt expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_event: Option<serde_json::Value>,
    /// Per-run composed system replacement, accepted only by a negotiated
    /// before_prompt_state_v1 frontend. Empty text is an explicit replacement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    /// Canonical messages/system proposal accepted only by the API 0.4
    /// provider_context preparation driver, never a provider payload override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_context: Option<serde_json::Value>,
    /// Whether the intercepted operation should proceed.
    #[serde(default)]
    pub disposition: ExtensionHookDisposition,
    /// Additional prompt context produced at this boundary.
    #[serde(default)]
    pub context: Vec<ContextContribution>,
    /// User-visible notifications produced at this boundary.
    #[serde(default)]
    pub notifications: Vec<ExtensionNotification>,
    /// Advice accepted only for a typed `provider_retry` hook response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_retry: Option<ExtensionProviderRetryAdvice>,
    /// Advisory action returned only from the API 0.4 cache-refresh hook.
    /// Absence or null is no opinion; neither action expands host authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_warming_decision: Option<CacheWarmingAction>,
    /// Base64 PNG frames returned only from the API 0.4 compaction hook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction_frames: Option<Vec<String>>,
    /// Metadata accepted only for a typed `before_persistence` hook response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistence_metadata: Option<ExtensionPersistenceMetadata>,
    /// Rescan disposition accepted only for a typed `post_mutation` hook
    /// response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_mutation: Option<ExtensionPostMutationDisposition>,
    /// Replacement tool arguments from a `before_tool_call` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<serde_json::Value>,
    /// Replacement tool result from an `after_tool_call` response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_result: Option<ExtensionToolResultReplacement>,
    /// Blocked-call hint; honored only by unanimous native batch termination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Replacement for a resolved tool result. Absent fields keep the current
/// value. Replacing `content` without `structured_content` drops the old
/// structured content, which may no longer match the new content.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionToolResultReplacement {
    /// Text strings or negotiated typed text/image content parts. Images are
    /// admitted by the native result decoder with artifact owner/generation checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<serde_json::Value>>,
    /// Machine-readable result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Non-model metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    /// Whether the result is a tool failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Billed per-result usage, never model-context usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<octet_ai::Usage>,
}

/// A semantic status/header/footer contribution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionStatusContribution {
    /// Target semantic surface.
    pub surface: ExtensionUiSurface,
    /// Plain display text; terminal escape sequences are not interpreted.
    pub text: String,
    /// Optional semantic theme role, for example `extension.git.clean`.
    #[serde(default)]
    pub style_role: Option<String>,
    /// Higher values are retained first when space is constrained.
    #[serde(default)]
    pub priority: i32,
}

/// Placement for a bounded semantic widget relative to the host-owned editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionWidgetPlacement {
    /// Reserve rows above the host-owned composer.
    #[default]
    AboveEditor,
    /// Reserve rows below the host-owned composer.
    BelowEditor,
}

/// One extension-owned semantic UI snapshot.
///
/// These values are deliberately data-only. They cannot contain escape
/// sequences, terminal handles, callbacks, or arbitrary component factories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionUiContribution {
    /// A keyed compact status item. `text: null` clears the key.
    Status {
        /// Stable key scoped to the extension instance and generation.
        key: String,
        /// Plain text, or `null` to remove this status item.
        #[serde(default)]
        text: Option<String>,
        /// Finite host-resolved semantic role.
        #[serde(default)]
        style_role: Option<String>,
        /// Higher values are retained first when vertical space is constrained.
        #[serde(default)]
        priority: i32,
    },
    /// A keyed plain-text widget. `lines: null` clears the key.
    Widget {
        /// Stable key scoped to the extension instance and generation.
        key: String,
        /// Complete plain-text lines, or `null` to remove the widget.
        #[serde(default)]
        lines: Option<Vec<String>>,
        /// Host-owned placement around the composer.
        #[serde(default)]
        placement: ExtensionWidgetPlacement,
        /// Finite host-resolved semantic role for every line.
        #[serde(default)]
        style_role: Option<String>,
        /// Higher values are retained first when vertical space is constrained.
        #[serde(default)]
        priority: i32,
    },
    /// Bounded metadata for the host-owned working indicator.
    Working {
        /// Optional plain status text. `null` restores the host default.
        #[serde(default)]
        message: Option<String>,
        /// Optional visibility override. The host still controls run ownership.
        #[serde(default)]
        visible: Option<bool>,
        /// Optional finite indicator frame list. Empty means hidden.
        #[serde(default)]
        frames: Option<Vec<String>>,
        /// Optional bounded animation interval in milliseconds.
        #[serde(default)]
        interval_ms: Option<u64>,
    },
    /// Bounded label for a host-owned collapsed-thinking affordance.
    HiddenThinking {
        /// Plain replacement label, or `null` to restore the host default.
        #[serde(default)]
        label: Option<String>,
    },
}

impl ExtensionUiContribution {
    pub(super) fn validate(&self) -> Result<(), String> {
        match self {
            Self::Status {
                key,
                text,
                style_role,
                ..
            } => {
                validate_extension_ui_key(key)?;
                if let Some(text) = text {
                    validate_extension_ui_text("status text", text, false)?;
                }
                validate_extension_style_role(style_role.as_deref())
            }
            Self::Widget {
                key,
                lines,
                style_role,
                ..
            } => {
                validate_extension_ui_key(key)?;
                if let Some(lines) = lines {
                    if lines.len() > MAX_EXTENSION_UI_LINES {
                        return Err(format!(
                            "widget has {} lines; limit is {MAX_EXTENSION_UI_LINES}",
                            lines.len()
                        ));
                    }
                    for line in lines {
                        validate_extension_ui_text("widget line", line, false)?;
                    }
                }
                validate_extension_style_role(style_role.as_deref())
            }
            Self::Working {
                message,
                frames,
                interval_ms,
                ..
            } => {
                if let Some(message) = message {
                    validate_extension_ui_text("working message", message, false)?;
                }
                if let Some(frames) = frames {
                    if frames.len() > MAX_EXTENSION_UI_INDICATOR_FRAMES {
                        return Err(format!(
                            "working indicator has {} frames; limit is {MAX_EXTENSION_UI_INDICATOR_FRAMES}",
                            frames.len()
                        ));
                    }
                    for frame in frames {
                        validate_extension_ui_text("working indicator frame", frame, false)?;
                    }
                }
                if interval_ms.is_some_and(|interval| !(16..=10_000).contains(&interval)) {
                    return Err("working indicator interval must be 16..=10000ms".into());
                }
                Ok(())
            }
            Self::HiddenThinking { label } => {
                if let Some(label) = label {
                    validate_extension_ui_text("hidden thinking label", label, false)?;
                }
                Ok(())
            }
        }
    }
}

/// A host-owned editor operation requested by an extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionEditorRequest {
    /// Return the latest host editor snapshot.
    Get,
    /// Replace the host editor text and discard its pending attachments.
    Set {
        /// Complete replacement text.
        text: String,
    },
    /// Insert text through the host's ordinary paste policy.
    Paste {
        /// Text to paste at the host editor cursor.
        text: String,
    },
    /// Ask the host to return focus to its normal editor at a safe boundary.
    Focus,
}

impl ExtensionEditorRequest {
    pub(super) fn validate(&self) -> Result<(), String> {
        let text = match self {
            Self::Set { text } | Self::Paste { text } => Some(text),
            Self::Get | Self::Focus => None,
        };
        if let Some(text) = text {
            validate_extension_editor_text(text)?;
        }
        Ok(())
    }
}

/// One host-owned composer operation requested by an API `0.2` extension.
///
/// The composer stays host-owned: an extension reads or mutates the ordinary
/// composer through the frontend that holds the resource owner's lease.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionComposerOperation {
    /// Return the current composer snapshot.
    Get,
    /// Replace the complete composer text.
    Set {
        /// Complete replacement text.
        text: String,
    },
    /// Insert text at the composer cursor.
    Insert {
        /// Text to insert at the cursor.
        text: String,
    },
    /// Commit a recovery checkpoint for a currently owned API 0.4 editor mount.
    Checkpoint {
        /// Complete replacement text.
        text: String,
        /// Authoritative owner resolved by the host at request admission.
        owner: ExtensionResourceOwner,
        /// Exact mount and input/checkpoint revisions to arbitrate at commit.
        checkpoint: ExtensionEditorCheckpoint,
    },
}

impl ExtensionComposerOperation {
    /// Validates every bounded field, mirroring
    /// [`ExtensionEditorRequest::validate`].
    pub fn validate(&self) -> Result<(), String> {
        let text = match self {
            Self::Set { text } | Self::Insert { text } => Some(text),
            Self::Checkpoint {
                text, checkpoint, ..
            } => {
                checkpoint.validate().map_err(|(_, detail)| detail)?;
                Some(text)
            }
            Self::Get => None,
        };
        if let Some(text) = text {
            validate_bounded_bytes("composer text", text, MAX_EXTENSION_COMPOSER_TEXT_BYTES)?;
            validate_plain_text("composer text", text)?;
        }
        Ok(())
    }
}

/// One foreground terminal handoff operation requested by an API `0.2`
/// extension.
///
/// The terminal stays host-owned: the frontend decides whether to cede the raw
/// tty, mints the grant identifier, and re-enters its own input loop on release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionTerminalOperation {
    /// Acquire the exclusive foreground terminal grant.
    Acquire,
    /// Return the grant to the host.
    Release,
}

/// One read-only host context snapshot requested by an API `0.2` extension.
///
/// The snapshot is derived from the foreground session and never mutates it.
/// `session_context` gates `SessionManager` and `PendingMessages`;
/// `system_prompt_read` gates `SystemPrompt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionContextOperation {
    /// Return the active session-manager snapshot.
    SessionManager,
    /// Return the bounded count of pending messages.
    PendingMessages,
    /// Return the host-owned composed system prompt text.
    SystemPrompt,
    /// Return authoritative active names and the registered tool catalog.
    Tools,
}

/// One read-only model operation requested by an API `0.2` extension.
///
/// `model_catalog` gates both operations. The view is metadata only; a caller
/// that cannot be answered is refused rather than handed a synthesized model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionModelOperation {
    /// Return the selected model view.
    Current,
    /// Return the secret-free model catalog.
    Catalog,
}

/// One extension-owned session entry operation requested by an API `0.2`
/// extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionSessionEntryOperation {
    /// Append one typed durable entry.
    Append {
        /// Extension-owned entry type name.
        entry_type: String,
        /// Bounded opaque entry payload.
        data: serde_json::Value,
    },
    /// Replace the active session name.
    SetName {
        /// Bounded session name.
        name: String,
    },
    /// Label one durable session entry.
    SetLabel {
        /// Durable entry returned by `session/append_entry`.
        entry_id: String,
        /// Bounded entry label.
        label: String,
    },
}

impl ExtensionSessionEntryOperation {
    /// Validates every bounded field of one session entry operation.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Append { entry_type, data } => {
                validate_bounded_bytes(
                    "session entry type",
                    entry_type,
                    MAX_EXTENSION_SESSION_ENTRY_TYPE_BYTES,
                )?;
                if entry_type.is_empty() {
                    return Err("session entry type must not be empty".into());
                }
                let bytes = serde_json::to_vec(data)
                    .map_err(|error| format!("session entry data is not serializable: {error}"))?;
                if bytes.len() > MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES {
                    return Err(format!(
                        "session entry data is {} JSON bytes; limit is {MAX_EXTENSION_SESSION_ENTRY_DATA_BYTES}",
                        bytes.len()
                    ));
                }
                Ok(())
            }
            Self::SetName { name } => {
                validate_bounded_bytes("session name", name, MAX_EXTENSION_SESSION_NAME_BYTES)
            }
            Self::SetLabel { entry_id, label } => {
                validate_bounded_bytes(
                    "session entry id",
                    entry_id,
                    MAX_CONFIRMATION_REQUEST_ID_BYTES,
                )?;
                validate_bounded_bytes(
                    "session entry label",
                    label,
                    MAX_EXTENSION_SESSION_LABEL_BYTES,
                )
            }
        }
    }
}

/// One bounded message injection requested by an API `0.2` extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionMessageInjection {
    /// Pi `sendUserMessage`: a user prompt. An idle session runs it; an
    /// active run steers it in or queues it as a follow-up.
    User {
        /// Bounded message text.
        text: String,
        /// Optional ordered text/image input, not custom-message metadata.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<crate::session::CustomMessageContent>,
        /// Delivery while a run is active.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deliver_as: Option<ExtensionMessageDelivery>,
    },
    /// Pi `sendMessage`: a custom message the model sees as user content.
    Custom {
        /// Extension-defined message type.
        custom_type: String,
        /// Original string or ordered text blocks.
        content: crate::session::CustomMessageContent,
        /// Whether the transcript shows the message.
        #[serde(default)]
        display: bool,
        /// Extension-defined data that the model never sees.
        #[serde(
            default,
            deserialize_with = "crate::session::deserialize_custom_message_details",
            skip_serializing_if = "Option::is_none"
        )]
        details: Option<serde_json::Value>,
        /// Delivery while a run is active, or `next_turn`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deliver_as: Option<ExtensionMessageDelivery>,
        /// Pi `triggerTurn`: `true` runs an idle session, `false` never runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_turn: Option<bool>,
    },
}

/// Pi `deliverAs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionMessageDelivery {
    /// Inject into the active run at its next model-turn boundary.
    Steer,
    /// Run after the active run finishes.
    FollowUp,
    /// Send with the next user prompt.
    NextTurn,
}

impl ExtensionMessageInjection {
    /// Validates the bounded injected text, mirroring
    /// [`validate_extension_editor_text`]'s plain-text posture.
    pub fn validate(&self) -> Result<(), String> {
        let text = match self {
            Self::User { text, content, .. } => {
                if let Some(content) = content {
                    if !text.is_empty() {
                        return Err("ambiguous user message content".into());
                    }
                    content.validate().map_err(|error| error.to_string())?;
                }
                text
            }
            Self::Custom {
                custom_type,
                content,
                display,
                details,
                ..
            } => {
                return crate::session::CustomMessage {
                    custom_type: custom_type.clone(),
                    content: content.clone(),
                    display: *display,
                    details: details.clone(),
                }
                .validate()
                .map_err(|error| error.to_string());
            }
        };
        validate_bounded_bytes(
            "injected message",
            text,
            MAX_EXTENSION_INJECTED_MESSAGE_BYTES,
        )?;
        validate_plain_text("injected message", text)
    }
}

/// Contract failure kinds shared by every Wave-1 owner-scoped request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionRequestFailure {
    /// The extension did not negotiate the feature this request belongs to.
    UnsupportedFeature,
    /// The authoritative owner is not the foreground session.
    NotForegroundOwner,
    /// The request is malformed or violates a semantic rule.
    InvalidRequest,
    /// A bounded field exceeded its named cap.
    BoundsExceeded,
}

impl ExtensionRequestFailure {
    /// JSON-RPC error code carrying this contract failure on API 0.2.
    pub fn code(self) -> i64 {
        match self {
            Self::UnsupportedFeature => -32601,
            Self::NotForegroundOwner => -32002,
            Self::InvalidRequest | Self::BoundsExceeded => -32602,
        }
    }

    /// Stable contract name used by the bridge and the SDKs.
    pub fn name(self) -> &'static str {
        match self {
            Self::UnsupportedFeature => "unsupported_feature",
            Self::NotForegroundOwner => "not_foreground_owner",
            Self::InvalidRequest => "invalid_request",
            Self::BoundsExceeded => "bounds_exceeded",
        }
    }

    /// Bounded `error.message` text with the contract name as its first token.
    pub fn message(self, detail: &str) -> String {
        let detail = detail.trim();
        let mut bounded = String::new();
        for character in detail.chars() {
            if bounded.len() + character.len_utf8() > MAX_EXTENSION_REQUEST_ERROR_DETAIL_BYTES {
                break;
            }
            bounded.push(character);
        }
        if bounded.is_empty() {
            return self.name().to_owned();
        }
        format!("{}: {bounded}", self.name())
    }

    /// One JSON-RPC error object for this contract failure.
    pub fn error_object(self, detail: &str) -> serde_json::Value {
        serde_json::json!({
            "code": self.code(),
            "message": self.message(detail),
        })
    }
}

/// Terminal answer for one admitted Wave-1 owner-scoped request.
#[derive(Clone, Debug, PartialEq)]
pub enum ExtensionRequestOutcome {
    /// Successful bounded result.
    Ok(serde_json::Value),
    /// Typed refusal with a bounded detail.
    Failed(ExtensionRequestFailure, String),
}

impl ExtensionRequestOutcome {
    /// Successful outcome carrying one serializable result.
    pub fn result<T: Serialize>(result: T) -> Self {
        match serde_json::to_value(result) {
            Ok(value) => Self::Ok(value),
            Err(error) => Self::Failed(
                ExtensionRequestFailure::InvalidRequest,
                format!("unencodable result: {error}"),
            ),
        }
    }

    /// Typed refusal outcome with a bounded detail.
    pub fn failed(failure: ExtensionRequestFailure, detail: impl Into<String>) -> Self {
        Self::Failed(failure, detail.into())
    }

    /// Encodes one complete JSON-RPC response envelope for `id`.
    pub fn into_response(self, id: ExtensionRequestId) -> serde_json::Value {
        match self {
            Self::Ok(result) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }),
            Self::Failed(failure, detail) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": failure.error_object(&detail),
            }),
        }
    }
}

/// Bounded `message/started` and `message/settled` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMessageLifecycle {
    /// Full committed custom message; omitted on the existing assistant delta profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<serde_json::Value>,
    /// Host message identity within the active turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// Coalesced `message/updated` payload. One notification per batch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMessageUpdated {
    /// Host message identity within the active turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Coalesced delta text of this batch, bounded by
    /// [`MAX_EXTENSION_MESSAGE_UPDATED_TEXT_BYTES`].
    pub delta: String,
    /// Number of extension-visible deltas coalesced into this batch.
    pub deltas: u64,
}

/// Bounded compaction boundary payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionCompactionReport {
    /// Bounded host-provided reason, present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Bounded session name or label change payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionSessionInfoChanged {
    /// Host session identity the change belongs to.
    pub session_id: String,
    /// Current host session name, when one is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Durable entry that was labelled, when the change was a label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry_id: Option<String>,
}

/// Bounded dialog start or terminal boundary payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionDialogLifecycle {
    /// Host-owned dialog surface, for example `select`, `confirm`, or `input`.
    pub dialog: String,
}

/// Bounded model selection payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionModelSelected {
    /// Canonical selected model identifier, when the host has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Pi-shaped view of the selected model, when the host resolved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_view: Option<ExtensionModelView>,
}

/// Bounded reasoning selection payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionReasoningSelected {
    /// Selected reasoning level, when the host has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
}

/// Bounded user `!`/`!!` bash payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionUserBash {
    /// Bounded command text the user ran.
    pub command: String,
}

/// Bounded `shortcut/trigger` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionShortcutTrigger {
    /// Extension-owned action identifier returned by `shortcut/register`.
    pub id: String,
}

/// Bounded `terminal/grant-lost` payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalGrantLost {
    /// Bounded host-provided revocation reason.
    pub reason: String,
}

/// Snapshot returned after one host-owned editor operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionEditorResponse {
    /// Current plain editor text after the operation.
    pub text: String,
    /// Host-monotonic editor revision.
    pub revision: u64,
    /// Whether the normal host editor is the active input surface.
    pub focused: bool,
}

/// One observer-only normalized terminal input event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionTerminalInput {
    /// Normalized key/text payload. This cannot consume or transform input.
    pub data: String,
}

/// A host terminal resize observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionTerminalResize {
    /// Current terminal columns.
    pub columns: u16,
    /// Current terminal rows.
    pub rows: u16,
}

/// Registration marker for one extension-side autocomplete chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionAutocompleteRegistration {
    /// Extension-local registration revision. It is informational only.
    pub revision: u64,
}

/// A host-to-extension autocomplete query over one editor snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionAutocompleteRequest {
    /// Complete bounded editor text snapshot.
    pub text: String,
    /// UTF-8 byte offset at a host-validated character boundary.
    pub cursor: usize,
    /// Host-monotonic editor revision used to reject stale replies.
    pub revision: u64,
}

impl ExtensionAutocompleteRequest {
    pub(super) fn validate(&self) -> Result<(), String> {
        validate_extension_editor_text(&self.text)?;
        if self.cursor > self.text.len() || !self.text.is_char_boundary(self.cursor) {
            return Err("autocomplete cursor is outside a UTF-8 character boundary".into());
        }
        Ok(())
    }
}

/// One bounded semantic autocomplete choice.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionAutocompleteItem {
    /// Text inserted by the host when the choice is accepted.
    pub value: String,
    /// Plain primary label.
    pub label: String,
    /// Optional plain secondary label.
    #[serde(default)]
    pub description: Option<String>,
    /// Additional original bytes after the cursor to replace. Absence means zero.
    /// Presence requires negotiated API `0.4` `autocomplete_edit_v1`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "autocomplete_edit_offset"
    )]
    pub replace_after_bytes: Option<u32>,
    /// UTF-8 byte cursor within `value`. Absence means its byte length.
    /// Presence requires negotiated API `0.4` `autocomplete_edit_v1`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "autocomplete_edit_offset"
    )]
    pub cursor_offset_bytes: Option<u32>,
}

// Missing fields use serde(default); explicit null is present and invalid.
fn autocomplete_edit_offset<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<u32>, D::Error> {
    u32::deserialize(deserializer).map(Some)
}

impl ExtensionAutocompleteItem {
    fn validate(&self, edit_v1: bool) -> Result<(), String> {
        if !edit_v1 && (self.replace_after_bytes.is_some() || self.cursor_offset_bytes.is_some()) {
            return Err("autocomplete edit fields require negotiated autocomplete_edit_v1".into());
        }
        validate_extension_autocomplete_edit_text("autocomplete value", &self.value, edit_v1)?;
        validate_extension_autocomplete_text("autocomplete label", &self.label)?;
        if let Some(description) = &self.description {
            validate_extension_autocomplete_text("autocomplete description", description)?;
        }
        Ok(())
    }
}

/// Extension result for one host-mediated autocomplete query.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionAutocompleteResponse {
    /// Exact plain suffix before the cursor that the host may replace.
    pub prefix: String,
    /// Ordered bounded candidate list.
    #[serde(default)]
    pub items: Vec<ExtensionAutocompleteItem>,
}

impl ExtensionAutocompleteResponse {
    #[cfg(test)]
    pub(super) fn validate(&self) -> Result<(), String> {
        self.validate_fields(false)
    }

    fn validate_fields(&self, edit_v1: bool) -> Result<(), String> {
        validate_extension_autocomplete_edit_text("autocomplete prefix", &self.prefix, edit_v1)?;
        if self.items.len() > MAX_EXTENSION_AUTOCOMPLETE_ITEMS {
            return Err(format!(
                "autocomplete response has {} items; limit is {MAX_EXTENSION_AUTOCOMPLETE_ITEMS}",
                self.items.len()
            ));
        }
        self.items
            .iter()
            .try_for_each(|item| item.validate(edit_v1))
    }

    /// Validate the entire response against the exact original editor snapshot.
    /// `edit_v1` must reflect host-negotiated capability, never extension data.
    /// Frontends repeat this check before displaying or accepting a choice.
    pub fn validate_for_request(
        &self,
        request: &ExtensionAutocompleteRequest,
        edit_v1: bool,
    ) -> Result<(), String> {
        request.validate()?;
        self.validate_fields(edit_v1)?;
        if !request.text[..request.cursor].ends_with(&self.prefix) {
            return Err("autocomplete prefix is not the exact suffix before the cursor".into());
        }
        for item in &self.items {
            let after = item.replace_after_bytes.unwrap_or(0) as usize;
            if after > MAX_EXTENSION_EDITOR_TEXT_BYTES {
                return Err("autocomplete suffix exceeds editor byte budget".into());
            }
            let end = request
                .cursor
                .checked_add(after)
                .filter(|end| *end <= request.text.len() && request.text.is_char_boundary(*end))
                .ok_or("autocomplete replacement end is outside a UTF-8 character boundary")?;
            let offset = item
                .cursor_offset_bytes
                .map_or(item.value.len(), |value| value as usize);
            if offset > item.value.len() || !item.value.is_char_boundary(offset) {
                return Err(
                    "autocomplete inserted cursor is outside a UTF-8 character boundary".into(),
                );
            }
            let start = request.cursor - self.prefix.len();
            let bytes = request.text.len() - (end - start) + item.value.len();
            if bytes > MAX_EXTENSION_EDITOR_TEXT_BYTES {
                return Err("autocomplete result exceeds editor byte budget".into());
            }
        }
        Ok(())
    }
}

pub(super) fn validate_extension_ui_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > MAX_EXTENSION_UI_KEY_BYTES {
        return Err(format!(
            "UI key must contain 1..={MAX_EXTENSION_UI_KEY_BYTES} UTF-8 bytes"
        ));
    }
    if !key
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("UI key must use ASCII letters, digits, '.', '_', or '-'".into());
    }
    Ok(())
}
