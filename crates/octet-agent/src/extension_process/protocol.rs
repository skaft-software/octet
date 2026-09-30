//! Protocol envelopes, negotiation results, notifications, progress and lifecycle events.

use super::*;

pub(super) fn validate_extension_ui_text(
    kind: &str,
    text: &str,
    allow_newlines: bool,
) -> Result<(), String> {
    if text.len() > MAX_EXTENSION_UI_TEXT_BYTES {
        return Err(format!(
            "{kind} exceeded {MAX_EXTENSION_UI_TEXT_BYTES} UTF-8 bytes"
        ));
    }
    if text.chars().any(|character| {
        character == '\u{1b}'
            || (character.is_control()
                && character != '\t'
                && (!allow_newlines || character != '\n'))
    }) {
        return Err(format!("{kind} contains a terminal control character"));
    }
    Ok(())
}

pub(super) fn validate_extension_style_role(role: Option<&str>) -> Result<(), String> {
    let Some(role) = role else {
        return Ok(());
    };
    const ALLOWED: &[&str] = &[
        "extension.pi.status",
        "extension.pi.muted",
        "extension.pi.accent",
        "extension.pi.warning",
        "extension.pi.error",
    ];
    if ALLOWED.contains(&role) {
        Ok(())
    } else {
        Err("UI style_role is not one of the finite host semantic roles".into())
    }
}

pub(super) fn validate_extension_renderer_role(role: Option<&str>) -> Result<(), String> {
    let Some(role) = role else {
        return Ok(());
    };
    if role.is_empty()
        || role.len() > MAX_EXTENSION_UI_KEY_BYTES
        || role.chars().any(|character| character.is_control())
    {
        return Err("tool renderer style_role is not bounded plain text".into());
    }
    Ok(())
}

pub(super) fn validate_extension_editor_text(text: &str) -> Result<(), String> {
    if text.len() > MAX_EXTENSION_EDITOR_TEXT_BYTES {
        return Err(format!(
            "editor text exceeded {MAX_EXTENSION_EDITOR_TEXT_BYTES} UTF-8 bytes"
        ));
    }
    if text.chars().any(|character| {
        character == '\u{1b}'
            || (character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
    }) {
        return Err("editor text contains a terminal control character".into());
    }
    Ok(())
}

pub(super) fn validate_extension_autocomplete_text(kind: &str, text: &str) -> Result<(), String> {
    if text.len() > MAX_EXTENSION_AUTOCOMPLETE_TEXT_BYTES {
        return Err(format!(
            "{kind} exceeded {MAX_EXTENSION_AUTOCOMPLETE_TEXT_BYTES} UTF-8 bytes"
        ));
    }
    if text.chars().any(|character| character.is_control()) {
        return Err(format!("{kind} contains a terminal control character"));
    }
    Ok(())
}

pub(super) fn validate_bounded_bytes(kind: &str, value: &str, limit: usize) -> Result<(), String> {
    if value.len() > limit {
        return Err(format!("{kind} exceeded {limit} UTF-8 bytes"));
    }
    Ok(())
}

/// Bounds one host-originated notification field, refusing rather than
/// truncating a value whose exact content is load-bearing.
pub(super) fn bounded_notification_text(
    kind: &str,
    value: &str,
    limit: usize,
) -> Result<String, ExtensionRuntimeError> {
    if value.len() > limit {
        return Err(ExtensionRuntimeError::Protocol(format!(
            "{kind} exceeded {limit} UTF-8 bytes"
        )));
    }
    validate_plain_text(kind, value).map_err(ExtensionRuntimeError::Protocol)?;
    Ok(value.to_owned())
}

/// Truncates one bounded host-originated diagnostic/reason field on a UTF-8
/// character boundary. Used only where dropping the notification would be
/// worse than shortening a human-readable reason.
pub(super) fn truncated_notification_text(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut bounded = String::new();
    for character in value.chars() {
        if bounded.len() + character.len_utf8() > limit {
            break;
        }
        bounded.push(character);
    }
    bounded
}

pub(super) fn validate_plain_text(kind: &str, value: &str) -> Result<(), String> {
    if value.chars().any(|character| {
        character == '\u{1b}'
            || (character.is_control() && !matches!(character, '\n' | '\t' | '\r'))
    }) {
        return Err(format!("{kind} contains a terminal control character"));
    }
    Ok(())
}

/// Classifies one bounded plain-text field into the contract's typed refusal.
pub(super) fn bounded_plain_text_failure(
    kind: &str,
    value: &str,
    limit: usize,
) -> Result<(), (ExtensionRequestFailure, String)> {
    if value.len() > limit {
        return Err((
            ExtensionRequestFailure::BoundsExceeded,
            format!("{kind} exceeded {limit} UTF-8 bytes"),
        ));
    }
    if let Err(detail) = validate_plain_text(kind, value) {
        return Err((ExtensionRequestFailure::InvalidRequest, detail));
    }
    Ok(())
}

/// Notification severity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionNotificationLevel {
    /// Informational message.
    #[default]
    Info,
    /// Successful operation.
    Success,
    /// Recoverable warning.
    Warning,
    /// Failure requiring attention.
    Error,
}

/// User-visible notification emitted by an extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionNotification {
    /// Semantic severity.
    #[serde(default)]
    pub level: ExtensionNotificationLevel,
    /// Optional concise title.
    #[serde(default)]
    pub title: Option<String>,
    /// Plain notification body.
    pub message: String,
}

/// A confirmation prompt requested by an extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfirmationRequest {
    /// Originating host request. Required for API `0.2` operation-scoped
    /// confirmations and absent from frozen API `0.1` frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_request_id: Option<u64>,
    /// Short action-oriented question.
    pub prompt: String,
    /// Optional additional consequence or scope detail.
    #[serde(default)]
    pub detail: Option<String>,
    /// Marks a potentially destructive action for stronger UI treatment.
    #[serde(default)]
    pub destructive: bool,
    /// Suggested default when a frontend supports one.
    #[serde(default)]
    pub default: bool,
}

/// Host answer to a confirmation request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfirmationResponse {
    /// Whether the user approved the operation.
    pub confirmed: bool,
}

/// Ephemeral input requested by an API `0.2` extension operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInputRequest {
    /// Originating host request whose cancellation owns this prompt.
    pub parent_request_id: u64,
    /// Short frontend-visible prompt. Secret answers never appear here.
    pub prompt: String,
    /// Whether the frontend should suppress echo and ordinary editor handling.
    #[serde(default)]
    pub secret: bool,
}

/// Host answer to an API `0.2` input request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionInputResponse {
    /// UTF-8 answer, or `null` when cancelled/unavailable.
    pub value: Option<String>,
}

/// One semantic segment returned by a tool renderer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRenderSegment {
    /// Plain text content.
    pub text: String,
    /// Optional semantic role resolved through the active theme.
    #[serde(default)]
    pub style_role: Option<String>,
}

/// Semantic renderer output for one tool call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedToolCall {
    /// Ordered semantic segments. Newlines remain explicit in segment text.
    #[serde(default)]
    pub segments: Vec<ToolRenderSegment>,
}

impl RenderedToolCall {
    pub(super) fn validate(&self) -> Result<(), String> {
        if self.segments.len() > MAX_EXTENSION_UI_LINES.saturating_mul(4) {
            return Err(format!(
                "tool renderer returned {} segments; limit is {}",
                self.segments.len(),
                MAX_EXTENSION_UI_LINES.saturating_mul(4)
            ));
        }
        self.segments.iter().try_for_each(|segment| {
            validate_extension_ui_text("tool renderer segment", &segment.text, true)?;
            validate_extension_renderer_role(segment.style_role.as_deref())
        })
    }
}

/// JSON-RPC identifier used by a process-originated request.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ExtensionRequestId {
    /// Numeric identifier.
    Number(u64),
    /// String identifier.
    String(String),
}

/// Host-advertised limits for the API `0.2` initialization handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProtocolLimits {
    /// Maximum number of concurrently admitted host requests.
    pub max_concurrent_requests: usize,
}

/// Additive feature negotiation sent by an API `0.2` host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProtocolRequest {
    /// Exact framing version selected by the manifest.
    pub version: String,
    /// Features without which the extension cannot be registered.
    pub required_features: Vec<String>,
    /// Supported features the extension may elect to use.
    pub optional_features: Vec<String>,
    /// Host-capped transport limits.
    pub limits: ExtensionProtocolLimits,
}

/// Feature subset and accepted limits returned by an API `0.2` extension.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionProtocolResponse {
    /// Exact negotiated framing version.
    pub version: String,
    /// Advertised subset of the host's required and optional features.
    #[serde(default)]
    pub features: Vec<String>,
    /// Extension-accepted limits, still capped by the host.
    pub limits: ExtensionProtocolLimits,
    /// Lifecycle methods the extension wants to observe. Omitting the field
    /// while negotiating `lifecycle_events` subscribes to every lifecycle
    /// method for compatibility with minimal SDKs.
    #[serde(default)]
    pub lifecycle_events: Vec<String>,
}

/// Immutable protocol facts negotiated for one process generation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionNegotiatedProtocol {
    /// Exact manifest-selected API version.
    pub version: String,
    /// Additive features supported by both peers.
    pub features: BTreeSet<String>,
    /// Effective host-capped concurrency limit.
    pub max_concurrent_requests: usize,
    /// Subscribed lifecycle wire methods.
    pub lifecycle_events: BTreeSet<String>,
}

impl ExtensionNegotiatedProtocol {
    pub(super) fn api_0_1(limit: usize) -> Self {
        Self {
            version: EXTENSION_API_VERSION_0_1.to_owned(),
            features: BTreeSet::new(),
            max_concurrent_requests: limit,
            lifecycle_events: BTreeSet::new(),
        }
    }

    pub(super) fn supports(&self, feature: &str) -> bool {
        self.features.contains(feature)
    }
}

/// Request-scoped progress payload emitted by an API `0.2` extension.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtensionProgressEvent {
    /// Human-readable progress with optional determinate units.
    Status {
        /// Bounded status text.
        message: String,
        /// Completed units, when known.
        #[serde(default)]
        current: Option<u64>,
        /// Total units, when known.
        #[serde(default)]
        total: Option<u64>,
        /// Unit label, for example `results`.
        #[serde(default)]
        unit: Option<String>,
    },
    /// Ephemeral stdout or stderr bytes.
    Output {
        /// Source stream.
        stream: ExtensionProgressStream,
        /// Text or binary encoding.
        encoding: ExtensionProgressEncoding,
        /// UTF-8 text or base64 data.
        data: String,
    },
    /// Replace the one bounded decoration associated with the active tool or
    /// command request. It is frontend-only and never enters session state.
    Decoration {
        /// Short current-state label.
        label: String,
        /// Optional one-line detail.
        #[serde(default)]
        detail: Option<String>,
    },
}

/// Stream named by an extension progress event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProgressStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// Encoding of an extension progress output event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionProgressEncoding {
    /// Direct Unicode text.
    Utf8,
    /// RFC 4648 base64 bytes.
    Base64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtensionProgressNotification {
    pub(super) request_id: u64,
    pub(super) sequence: u64,
    pub(super) event: ExtensionProgressEvent,
}

/// Outcome attached to settled API `0.2` lifecycle notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionLifecycleOutcome {
    /// Normal successful completion.
    Completed,
    /// Execution failed.
    Failed,
    /// User or parent cancellation won.
    Cancelled,
    /// Execution was interrupted without a cancellation acknowledgement.
    Interrupted,
    /// The owning frontend disconnected.
    FrontendDisconnected,
    /// Host shutdown settled the operation.
    Shutdown,
    /// A configured turn or resource limit was reached.
    LimitReached,
}

/// Observational lifecycle event sent best-effort to negotiated API `0.2`
/// subscribers. These notifications never replace host-owned finalizers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExtensionLifecycleEvent {
    /// A host session became active.
    SessionStarted {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID, when one already exists.
        #[serde(default)]
        run_id: Option<String>,
    },
    /// A host session reached its terminal boundary.
    SessionSettled {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID, when applicable.
        #[serde(default)]
        run_id: Option<String>,
        /// Terminal outcome.
        outcome: ExtensionLifecycleOutcome,
        /// Elapsed duration in milliseconds.
        duration_ms: u64,
        /// Bounded inspectable reason.
        #[serde(default)]
        reason: Option<String>,
    },
    /// A model turn was admitted.
    TurnStarted {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID.
        run_id: String,
        /// Stable turn ID.
        turn_id: String,
    },
    /// An admitted model turn settled exactly once.
    TurnSettled {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID.
        run_id: String,
        /// Stable turn ID.
        turn_id: String,
        /// Terminal outcome.
        outcome: ExtensionLifecycleOutcome,
        /// Elapsed duration in milliseconds.
        duration_ms: u64,
        /// Bounded inspectable reason.
        #[serde(default)]
        reason: Option<String>,
    },
    /// A globally observed tool call started.
    ToolStarted {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID.
        run_id: String,
        /// Stable turn ID.
        turn_id: String,
        /// Stable tool-call ID.
        tool_call_id: String,
        /// Registered tool name.
        tool_name: String,
    },
    /// A globally observed tool call settled.
    ToolSettled {
        /// Stable session ID.
        session_id: String,
        /// Stable run ID.
        run_id: String,
        /// Stable turn ID.
        turn_id: String,
        /// Stable tool-call ID.
        tool_call_id: String,
        /// Registered tool name.
        tool_name: String,
        /// Terminal outcome.
        outcome: ExtensionLifecycleOutcome,
        /// Elapsed duration in milliseconds.
        duration_ms: u64,
        /// Bounded inspectable reason.
        #[serde(default)]
        reason: Option<String>,
    },
}

impl ExtensionLifecycleEvent {
    pub(super) fn method(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => methods::SESSION_STARTED,
            Self::SessionSettled { .. } => methods::SESSION_SETTLED,
            Self::TurnStarted { .. } => methods::TURN_STARTED,
            Self::TurnSettled { .. } => methods::TURN_SETTLED,
            Self::ToolStarted { .. } => methods::TOOL_STARTED,
            Self::ToolSettled { .. } => methods::TOOL_SETTLED,
        }
    }

    pub(super) fn params(&self) -> Result<serde_json::Value, ExtensionRuntimeError> {
        let mut value = serde_json::to_value(self)
            .map_err(|error| ExtensionRuntimeError::Protocol(error.to_string()))?;
        let object = value.as_object_mut().ok_or_else(|| {
            ExtensionRuntimeError::Protocol("lifecycle event must serialize as an object".into())
        })?;
        object.remove("type");
        if object
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|reason| reason.len() > MAX_LIFECYCLE_REASON_BYTES)
        {
            return Err(ExtensionRuntimeError::Protocol(format!(
                "lifecycle reason exceeded {MAX_LIFECYCLE_REASON_BYTES} bytes"
            )));
        }
        Ok(value)
    }
}

/// Extension-originated request for host policy classification.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionPolicyEvaluationRequest {
    /// Originating host request whose lifetime owns this decision.
    pub parent_request_id: u64,
    /// Structured, non-authoritative proposed action.
    pub intent: ExtensionActionIntent,
    /// Single-use capability returned by a prior approved evaluation. Tokens
    /// are consumed against this exact intent, process generation, and parent.
    #[serde(default)]
    pub approval_token: Option<ExtensionApprovalToken>,
}

/// Host answer to a policy evaluation request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionPolicyEvaluationResponse {
    /// Host-authoritative decision.
    pub decision: ExtensionPolicyDecision,
    /// Optional single-use, generation- and intent-bound approval capability.
    #[serde(default)]
    pub approval_token: Option<ExtensionApprovalToken>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ExtensionSecretGetRequest {
    pub(super) parent_request_id: u64,
    pub(super) name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PresentationUpdateRequest {
    pub(super) snapshot: ExtensionPresentationSnapshot,
    #[serde(default)]
    pub(super) parent_request_id: Option<u64>,
    #[serde(default)]
    pub(super) resource_owner: Option<ExtensionResourceOwner>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactPublishRequest {
    pub(super) parent_request_id: u64,
    pub(super) mime_type: String,
    pub(super) size: u64,
    pub(super) sha256: String,
    #[serde(default)]
    pub(super) data: Option<ArtifactInlineData>,
    #[serde(default)]
    pub(super) path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ArtifactInlineData {
    pub(super) encoding: ExtensionProgressEncoding,
    pub(super) data: String,
}

/// Observable health state for one executable-extension process generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionHealthState {
    /// Process spawn is underway.
    Starting,
    /// The initialize handshake is underway.
    Initializing,
    /// New operations are accepted.
    Ready,
    /// New operations are rejected while admitted work settles.
    Draining,
    /// Graceful or forced shutdown completed.
    Stopped,
    /// A recoverable service failure occurred.
    Degraded,
    /// The child or transport failed unexpectedly.
    Crashed,
    /// A manager-imposed restart delay is active.
    Backoff,
    /// Permanent configuration, authorization, or protocol failure.
    Parked,
}

/// Bounded machine-readable health snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionHealthSnapshot {
    /// Current state.
    pub state: ExtensionHealthState,
    /// Owning process generation.
    pub generation: u64,
    /// Host requests which have not yet settled.
    pub pending_requests: usize,
    /// Last bounded transport or protocol error.
    #[serde(default)]
    pub last_error: Option<String>,
}

impl ExtensionRequestId {
    pub(super) fn validate_confirmation_id(&self) -> Result<(), String> {
        let Self::String(id) = self else {
            return Ok(());
        };
        if id.len() > MAX_CONFIRMATION_REQUEST_ID_BYTES {
            return Err(format!(
                "confirmation request id is {} bytes; limit is {MAX_CONFIRMATION_REQUEST_ID_BYTES}",
                id.len()
            ));
        }
        Ok(())
    }
}
