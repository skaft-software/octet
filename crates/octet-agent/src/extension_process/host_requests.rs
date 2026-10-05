//! Wire types for requests an extension makes of the host, and their results.

use super::*;

/// A tool schema supplied during the initialize handshake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    /// Manifest-declared tool name.
    pub name: String,
    /// Model-facing description.
    pub description: String,
    /// Optional concise model-facing usage summary, subject to host negotiation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_snippet: Option<String>,
    /// Optional bounded model-facing usage guidelines, subject to host negotiation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_guidelines: Vec<String>,
    /// JSON Schema for tool arguments.
    pub parameters: serde_json::Value,
    /// Optional API `0.2` JSON Schema for `structured_content`.
    #[serde(default)]
    pub output_schema: Option<serde_json::Value>,
    /// Optional negotiated API 0.4 operation metadata; ordinary Pi tools omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationDescriptor>,
    /// Optional API `0.4` request-scoped composition policy. Requires
    /// negotiated `tool_composition_v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composition: Option<ToolCompositionConfig>,
    /// Optional API `0.4` provider-side constrained-sampling request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub constrained_sampling: Option<octet_ai::ConstrainedSampling>,
    /// Whether a newly registered tool joins the model-visible active set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_active: Option<bool>,
    /// Request-scoped nested dispatch without composition presentation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub nested_execution: bool,
    /// Prepare raw arguments before the exact advertised-schema validation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prepare_arguments: bool,
}

/// API `0.2` request to add or replace extension-owned tools.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRegistrationRequest {
    /// Complete definitions to merge into the current extension catalog.
    pub tools: Vec<ToolDefinition>,
}

/// API `0.2` request to remove extension-owned tools by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolUnregistrationRequest {
    /// Tool names to remove. Missing names are ignored, making retries safe.
    pub names: Vec<String>,
}

/// Host acknowledgement for a live tool catalog mutation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCatalogUpdateResponse {
    /// Monotonic catalog epoch within this subprocess generation. It starts at
    /// zero after initialize/reload and increments for each accepted mutation.
    pub revision: u64,
    /// Complete active tool-name set for this extension.
    pub tools: Vec<String>,
}

/// Host-enforced policy for one bounded extension-owned child session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionPolicy {
    /// Optional configured provider, model and reasoning selection.
    #[serde(default)]
    pub model_selection: Option<crate::delegation::AgentModelSelection>,
    /// Requested upper-bound tool allowlist. Accepted standard tools are
    /// `read`, `search`, `edit`, `write`, and `bash`.
    pub tools: Vec<String>,
    /// Maximum absolute delegation depth. V1 requires one.
    pub max_depth: usize,
    /// Maximum active children for this principal/owner. The host caps this
    /// at eight.
    pub max_concurrent_children: usize,
    /// Maximum model turns in the child run. `None` inherits the parent
    /// session limit exactly (unlimited parents stay unlimited).
    #[serde(default)]
    pub max_turns: Option<u64>,
    /// Optional cumulative provider-token ceiling. `None` inherits the parent
    /// session setting, including an unlimited parent.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Optional hard cumulative priced-session cost ceiling in whole
    /// microdollars. `None` removes the child-specific ceiling; the parent
    /// session ceiling still applies.
    #[serde(default)]
    pub max_cost_microdollars: Option<u64>,
    /// Maximum UTF-8 bytes returned as the child summary.
    pub max_output_bytes: usize,
    /// Optional hard wall-clock duration from successful spawn admission, in
    /// milliseconds. `None` runs without a wall-clock kill; explicit values
    /// are capped at 24 hours.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

impl From<AgentSessionPolicy> for ExtensionAgentSessionPolicy {
    fn from(policy: AgentSessionPolicy) -> Self {
        Self {
            model_selection: policy.model_selection,
            resolved_model: None,
            resolved_reasoning: None,
            tools: policy.tools,
            max_depth: policy.max_depth,
            max_concurrent_children: policy.max_concurrent_children,
            max_turns: policy.max_turns,
            max_tokens: policy.max_tokens,
            max_cost_microdollars: policy.max_cost_microdollars,
            max_output_bytes: policy.max_output_bytes,
            timeout_ms: policy.timeout_ms,
        }
    }
}

/// API `0.2` request to create an isolated child model session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionSpawnRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Unique task label under the calling owner.
    pub task_name: String,
    /// Optional bounded presentation profile retained by the host for restart
    /// recovery. It never changes child authority or policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Optional extension-calculated canonical request fingerprint retained for
    /// idempotent recovery. The host treats it only as bounded opaque metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// Initial task delivered to the child model session.
    pub message: String,
    /// Retry key scoped to this extension and resource owner.
    pub idempotency_key: String,
    /// Complete host-enforced child execution policy.
    pub policy: AgentSessionPolicy,
}

/// API `0.2` request carrying a child-session target and message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionMessageRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Agent ID or path returned by `agent/spawn`.
    pub target: String,
    /// Message or follow-up task to deliver.
    pub message: String,
}

/// API `0.2` request carrying only a child-session target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionTargetRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Agent ID or path returned by `agent/spawn`.
    pub target: String,
}

/// API `0.2` request to list sessions owned by the calling extension owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionListRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// Owner-bound, bounded configured-model discovery.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionModelsRequest {
    /// Active host request defining the resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    #[serde(default)]
    /// Optional case-insensitive search, at most 128 bytes.
    pub query: Option<String>,
    #[serde(default)]
    /// Maximum rows, default 50, range 1 through 100.
    pub limit: Option<usize>,
}

/// API `0.2` request to wait for owned child-session state changes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionWaitRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires API 0.4 agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Bounded wait duration. Defaults to 30 seconds and is capped at 60.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// API 0.4 bounded, loss-detecting observation of one owned child session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSessionEventsRequest {
    /// Active or originating host request for the issued owner.
    pub parent_request_id: u64,
    /// Previously issued owner; requires agent_session_lifetime_v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Agent ID or path returned by agent/spawn.
    pub target: String,
    /// Last delivered sequence; zero requests the retained beginning.
    pub after_sequence: u64,
    /// Maximum wait in milliseconds, 0 through 25000; default zero.
    #[serde(default)]
    pub timeout_ms: u64,
}

/// API `0.2` request for the current host composer snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposerGetRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request carrying complete replacement composer text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposerTextRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Bounded UTF-8 text. `composer/set` replaces and `composer/insert`
    /// inserts it at the host composer cursor.
    pub text: String,
    /// API 0.4 remote-editor checkpoint; permitted only on `composer/set`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_checkpoint: Option<ExtensionEditorCheckpoint>,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// A bounded checkpoint for one host-issued editor mount, carried by composer/set.
/// The frontend separately validates the admitted owner, live mount and clocks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionEditorCheckpoint {
    /// Extension-local surface identity.
    pub surface_id: String,
    /// Host-issued mount identity, changed on every replacement.
    pub mount_id: String,
    /// Highest completely handled host input revision; zero denotes the seed.
    pub input_revision: u64,
    /// Strictly increasing checkpoint revision, starting at one.
    pub checkpoint_revision: u64,
}

impl ExtensionEditorCheckpoint {
    /// Validates the wire bounds without granting authority to commit a draft.
    pub fn validate(&self) -> Result<(), (ExtensionRequestFailure, String)> {
        use crate::extension_remote_ui::{validate_surface_id, MAX_EXTENSION_REMOTE_UI_REVISION};
        validate_surface_id(&self.surface_id)?;
        validate_surface_id(&self.mount_id)?;
        if self.input_revision > MAX_EXTENSION_REMOTE_UI_REVISION
            || self.checkpoint_revision == 0
            || self.checkpoint_revision > MAX_EXTENSION_REMOTE_UI_REVISION
        {
            return Err((
                ExtensionRequestFailure::BoundsExceeded,
                "editor checkpoint revisions exceed their portable bounds".into(),
            ));
        }
        Ok(())
    }
}

/// Result of one admitted composer snapshot request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComposerTextResult {
    /// Current bounded composer text.
    pub text: String,
}

/// API `0.2` request to register one runtime terminal shortcut.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutRegisterRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Extension-owned action identifier reported back by `shortcut/trigger`.
    pub id: String,
    /// Portable terminal key spelling, for example `ctrl+shift+c`.
    pub key: String,
    /// User-facing summary of the action.
    pub description: String,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to append one extension-owned durable session entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAppendEntryRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Extension-owned entry type name.
    pub entry_type: String,
    /// Bounded opaque entry payload.
    pub data: serde_json::Value,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// Result of one admitted session entry append.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionAppendEntryResult {
    /// Host-assigned durable entry identifier.
    pub entry_id: String,
}

/// API `0.2` request to set the active session name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSetNameRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Bounded session name. An empty name clears it.
    pub name: String,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to label one durable session entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSetLabelRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Durable entry returned by `session/append_entry`.
    pub entry_id: String,
    /// Bounded entry label. An empty label clears it.
    pub label: String,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// Pi `sendMessage`: one custom message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSendMessageRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Extension-defined message type.
    pub custom_type: String,
    /// Original string or ordered text blocks.
    pub content: crate::session::CustomMessageContent,
    /// Whether the transcript shows the message.
    #[serde(default)]
    pub display: bool,
    /// Extension-defined data that the model never sees.
    #[serde(default, deserialize_with = "crate::session::deserialize_custom_message_details", skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
    /// Pi `deliverAs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<ExtensionMessageDelivery>,
    /// Pi `triggerTurn`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_turn: Option<bool>,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to inject one bounded user message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionSendUserMessageRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Bounded injected user message text.
    pub text: String,
    /// Pi `deliverAs` while a run is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<ExtensionMessageDelivery>,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to replace the active model tool set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSetActiveRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Complete replacement active tool set, validated like `tools/register`.
    pub names: Vec<String>,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to cede the foreground raw terminal to this session owner.
///
/// The request carries no parameter beyond the owner envelope: the granting
/// frontend decides whether it can cede at all. The answer is
/// `{grant_id, columns, rows}` and is delivered later through the ordinary
/// child-request response path, so the child request stays registered until the
/// frontend answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalAcquireRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// API `0.2` request to return a ceded foreground terminal to the host.
///
/// Release is idempotent for the frontend that owns the grant: the frontend
/// decides whether the caller still holds the current grant, and a stale or
/// foreign caller is refused there. Like acquire, this request is answered on
/// the ordinary child-request response path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalReleaseRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// Result of one admitted `terminal/acquire` request.
///
/// The frontend mints `grant_id`, so every field here is produced by the host
/// process itself and never by the extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalAcquireResult {
    /// Opaque frontend-minted grant identifier, at most
    /// [`MAX_EXTENSION_TERMINAL_GRANT_ID_BYTES`] UTF-8 bytes.
    pub grant_id: String,
    /// Terminal width in columns at the moment the grant was handed out.
    pub columns: u16,
    /// Terminal height in rows at the moment the grant was handed out.
    pub rows: u16,
}

/// Result of one admitted `terminal/release` request.
///
/// Release has no result body: the host answers `{}` once the frontend reports
/// that it re-entered its own terminal and input loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalReleaseResult {}

/// API `0.2` owner-scoped envelope for one read-only context snapshot request.
///
/// Every context snapshot request carries only the owner envelope. The host
/// derives the snapshot from the foreground session, so the extension supplies
/// no parameters and can never write through these requests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSnapshotRequest {
    /// Active host request that supplies the authoritative resource owner.
    pub parent_request_id: u64,
    /// Explicit owner for a caller that outlived its host request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
}

/// Maximum UTF-8 bytes in one bounded model-view field.
pub const MAX_EXTENSION_MODEL_FIELD_BYTES: usize = 256;

/// Maximum UTF-8 bytes in one Pi wire API name.
pub const MAX_EXTENSION_MODEL_API_BYTES: usize = 64;

/// Maximum input modalities in one model view (Pi declares `text` and `image`).
pub const MAX_EXTENSION_MODEL_INPUTS: usize = 2;

/// Maximum rows in one secret-free model catalog snapshot.
pub const MAX_EXTENSION_MODEL_CATALOG_ROWS: usize = 64;

impl ExtensionModelView {
    /// Validates one bounded, secret-free model view.
    ///
    /// A view that cannot be stated within bounds is refused rather than
    /// truncated: a shortened identifier would name a different model. Callers
    /// answer `bounds_exceeded` instead.
    pub fn validate(&self) -> Result<(), String> {
        validate_bounded_bytes("model id", &self.id, MAX_EXTENSION_MODEL_FIELD_BYTES)?;
        if self.id.is_empty() {
            return Err("model id must not be empty".into());
        }
        if let Some(name) = &self.name {
            validate_bounded_bytes("model name", name, MAX_EXTENSION_MODEL_FIELD_BYTES)?;
        }
        validate_bounded_bytes("model api", &self.api, MAX_EXTENSION_MODEL_API_BYTES)?;
        validate_bounded_bytes(
            "model provider",
            &self.provider,
            MAX_EXTENSION_MODEL_FIELD_BYTES,
        )?;
        if self.provider.is_empty() {
            return Err("model provider must not be empty".into());
        }
        if self.input.len() > MAX_EXTENSION_MODEL_INPUTS {
            return Err(format!(
                "model input carries {} modalities; limit is {MAX_EXTENSION_MODEL_INPUTS}",
                self.input.len()
            ));
        }
        for modality in &self.input {
            if !matches!(modality.as_str(), "text" | "image") {
                return Err("model input modality must be `text` or `image`".into());
            }
        }
        Ok(())
    }
}

/// Result of one admitted `context/model_catalog` request.
///
/// Rows reuse [`ExtensionModelView`] so a catalog entry and the selected view
/// are the same bounded, secret-free shape: there is exactly one model-view
/// definition on the wire.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextModelCatalogResult {
    /// Catalog rows, at most [`MAX_EXTENSION_MODEL_CATALOG_ROWS`].
    pub models: Vec<ExtensionModelView>,
    /// Whether the host truncated the catalog to the bounded row count.
    #[serde(default)]
    pub truncated: bool,
}

impl ContextModelCatalogResult {
    /// Validates the bounded catalog snapshot.
    pub fn validate(&self) -> Result<(), String> {
        if self.models.len() > MAX_EXTENSION_MODEL_CATALOG_ROWS {
            return Err(format!(
                "model catalog contains {} rows; limit is {MAX_EXTENSION_MODEL_CATALOG_ROWS}",
                self.models.len()
            ));
        }
        for model in &self.models {
            model.validate()?;
        }
        Ok(())
    }
}

/// Result of one admitted `context/session_manager` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSessionManagerResult {
    /// Active host session identifier.
    pub session_id: String,
    /// Optional host session name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Optional selected model identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional selected reasoning level identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// Active skill summaries, at most [`MAX_EXTENSION_CONTEXT_ACTIVE_SKILLS`].
    #[serde(default)]
    pub active_skills: Vec<ContextSkillSummary>,
    /// Host working directory for the active session.
    pub cwd: String,
}

/// One active skill summary in a session-manager snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSkillSummary {
    /// Stable skill identifier.
    pub id: String,
    /// User-facing skill name.
    pub name: String,
}

/// Result of one admitted `context/pending_messages` request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPendingMessagesResult {
    /// Bounded count of pending messages for the active session.
    pub pending: u32,
}

/// Result of one admitted `context/system_prompt` request.
///
/// The disclosed `text` is bounded to
/// [`MAX_EXTENSION_SYSTEM_PROMPT_BYTES`]; the frontend that composes the prompt
/// must refuse rather than truncate an over-bound disclosure. Run
/// [`ContextSystemPromptResult::validate`] before answering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextSystemPromptResult {
    /// Host-owned composed system prompt text.
    pub text: String,
}

impl ContextSystemPromptResult {
    /// Validates the disclosed prompt text against its disclosure bound.
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > MAX_EXTENSION_SYSTEM_PROMPT_BYTES {
            return Err(format!(
                "system prompt text is {} bytes; limit is {MAX_EXTENSION_SYSTEM_PROMPT_BYTES}",
                self.text.len()
            ));
        }
        if self.text.chars().any(|character| character == '\u{0}') {
            return Err("system prompt text must not contain NUL".to_owned());
        }
        Ok(())
    }
}

/// A global terminal shortcut declared in the manifest and echoed by initialize.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShortcutDefinition {
    /// Portable terminal key spelling, for example `ctrl+shift+p`.
    pub key: String,
    /// Stable action identifier chosen by the extension.
    pub name: String,
    /// User-facing summary of the action.
    pub description: String,
}

/// A slash-command definition supplied during initialization.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDefinition {
    /// Manifest-declared command name, without a leading slash.
    pub name: String,
    /// User-facing summary.
    pub description: String,
    /// Optional compact usage string.
    #[serde(default)]
    pub usage: Option<String>,
}

/// Fully negotiated contributions for a running process.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtensionContributions {
    /// Model-callable tools and their schemas.
    pub tools: Vec<ToolDefinition>,
    /// Interactive commands and their help metadata.
    pub commands: Vec<CommandDefinition>,
    /// Validated terminal shortcut metadata.
    pub shortcuts: Vec<ShortcutDefinition>,
    /// Lifecycle hooks declared in the manifest.
    pub hooks: Vec<ExtensionHook>,
    /// Whether context requests are supported.
    pub context: bool,
    /// Semantic TUI surfaces declared in the manifest.
    pub ui: Vec<ExtensionUiSurface>,
    /// Tool names with semantic renderers.
    pub tool_renderers: Vec<String>,
    /// Whether notifications may arrive from the process.
    pub notifications: bool,
    /// Whether confirmation requests may arrive from the process.
    pub confirmations: bool,
    /// Whether semantic presentation snapshots may arrive from the process.
    pub presentation: bool,
    /// Whether the process answers `menu/collect` with an options menu.
    #[serde(default)]
    pub menu: bool,
    /// Whether the current API 0.3 contract permits provider catalog mutation.
    pub providers: bool,
}

/// Session and model facts exposed to an extension through typed requests.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExtensionHostState {
    /// Stable session identifier, when a frontend has one.
    #[serde(default)]
    pub session_id: Option<String>,
    /// User-assigned session name, when present.
    #[serde(default)]
    pub session_name: Option<String>,
    /// Canonical current model identifier.
    #[serde(default)]
    pub model: Option<String>,
    /// Pi-shaped view of the current model, when the host resolved one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_view: Option<ExtensionModelView>,
    /// Bounded, secret-free Pi available/scoped model facts supplied by the frontend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pi_models: Option<serde_json::Value>,
    /// Portable Pi thinking level, absent for unrepresentable native controls.
    #[serde(default)]
    pub reasoning: Option<serde_json::Value>,
    /// Skills explicitly active at this boundary.
    #[serde(default)]
    pub active_skills: Vec<ExtensionActiveSkill>,
}

/// Pi-shaped model view exposed to an extension as `ctx.model`.
///
/// Pi's `Model` describes the provider's own model record. octet projects only
/// the bounded, secret-free subset it can state truthfully, so the endpoint base
/// URL and credentials are never part of an extension-visible model. `baseUrl`
/// is omitted rather than fabricated: an extension reading it observes
/// `undefined`, never a URL octet did not disclose.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionModelView {
    /// Provider model identifier (the route's API model name).
    pub id: String,
    /// Human-facing model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Pi's wire API name for the model's protocol.
    pub api: String,
    /// Pi provider identity that owns the model's route.
    pub provider: String,
    /// Whether the model accepts reasoning controls.
    pub reasoning: bool,
    /// Accepted input modalities from Pi's `("text" | "image")` set.
    pub input: Vec<String>,
    /// Per-million-token rates, absent when the route is unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ExtensionModelCost>,
    /// Context window in tokens.
    pub context_window: u64,
    /// Maximum output tokens.
    pub max_tokens: u64,
}

/// Pi-shaped per-million-token model rates.
///
/// Rates stay in octet's own exact integer unit — microdollars per million
/// tokens — so the projection never rounds or re-derives a rate. The bridge
/// converts to Pi's `$/million` floating-point rates when it assembles
/// `ctx.model.cost`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionModelCost {
    /// Prompt input rate, in microdollars per million tokens.
    pub input: u64,
    /// Generated output rate, in microdollars per million tokens.
    pub output: u64,
    /// Cached input read rate, in microdollars per million tokens.
    pub cache_read: u64,
    /// Cache write rate, in microdollars per million tokens.
    pub cache_write: u64,
}

/// Compact skill metadata sent to executable extensions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionActiveSkill {
    /// Stable skill identifier.
    pub id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Optional skill version.
    #[serde(default)]
    pub version: Option<String>,
}

/// Ambient metadata supplied with commands, hooks, tools, and contributions.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionResourceOwner {
    /// Durable host-derived session identity. Extensions must treat this as
    /// the namespace for browser tabs, MCP/LSP state, and other handles.
    pub session_id: String,
    /// Host-created extension-process instance fence. It changes across a
    /// complete process-host rebuild even when generation numbering restarts.
    pub extension_instance_id: String,
    /// Process generation fence for rejecting stale resource operations after
    /// a restart or reload.
    pub process_generation: u64,
}

/// Ambient metadata supplied with commands, hooks, tools, and contributions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtensionExecutionContext {
    /// Active workspace root.
    pub workspace: PathBuf,
    /// Unique process-local tool execution scope, when invoked as a model tool.
    #[serde(default)]
    pub execution_scope: Option<String>,
    /// Durable extension-resource owner. Frozen API `0.1` omits this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_owner: Option<ExtensionResourceOwner>,
    /// Current host state.
    pub host: ExtensionHostState,
}

/// Result returned by an executable tool.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCallOutput {
    /// Compact model-visible result text.
    pub content: String,
    /// Whether the result represents a tool failure.
    #[serde(default)]
    pub is_error: bool,
    /// Optional non-model metadata. Frozen API `0.1` returns it to direct API
    /// callers but discards it at the native tool/session bridge; negotiated
    /// API `0.2` validates and retains it.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Optional machine-readable API `0.2` result, validated against the
    /// tool's declared output schema before this value is returned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<serde_json::Value>,
    /// Canonical native result preserving ordered text/media/details.
    #[serde(skip)]
    pub(super) native_output: Option<ToolOutput>,
}

impl PartialEq for ToolCallOutput {
    fn eq(&self, other: &Self) -> bool {
        self.content == other.content
            && self.is_error == other.is_error
            && self.metadata == other.metadata
            && self.structured_content == other.structured_content
    }
}

impl ToolCallOutput {
    pub(super) fn into_native(self) -> Result<ToolOutput, ExtensionRuntimeError> {
        if let Some(output) = self.native_output {
            return Ok(output);
        }
        ToolOutput::new(self.content)
            .try_with_details(self.structured_content, Some(self.metadata))
            .map_err(|error| {
                ExtensionRuntimeError::Protocol(format!("invalid tool output details: {error}"))
            })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ToolCallOutputWire {
    pub(super) content: serde_json::Value,
    #[serde(default)]
    pub(super) is_error: bool,
    #[serde(default)]
    pub(super) structured_content: PresentJsonValue,
    #[serde(default)]
    pub(super) metadata: serde_json::Value,
}

#[derive(Default)]
pub(super) enum PresentJsonValue {
    #[default]
    Missing,
    Present(serde_json::Value),
}

impl PresentJsonValue {
    pub(super) fn into_option(self) -> Option<serde_json::Value> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value),
        }
    }
}

impl<'de> Deserialize<'de> for PresentJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        serde_json::Value::deserialize(deserializer).map(Self::Present)
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ExtensionToolContentPart {
    Text {
        text: String,
    },
    Image {
        artifact_id: String,
        mime_type: String,
        #[serde(default, rename = "alt")]
        _alt: Option<String>,
    },
    Audio {
        artifact_id: String,
        mime_type: String,
        #[serde(default)]
        transcript: Option<String>,
    },
}
