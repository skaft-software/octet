//! Independently maintained API 0.4 data used by the feature-negotiated host.
//!
//! This is deliberately not a second wire implementation. Initialization,
//! feature negotiation and dispatch live in `extension_process`; only its flag,
//! provider, session-hook and response data belong here. Service data retains
//! its pre-existing portable-integer/depth checks, not canonical 0.3 transport
//! framing or negotiation semantics.

use std::collections::BTreeMap;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Largest exactly representable portable JSON integer.
pub const MAX_PORTABLE_JSON_INTEGER: i64 = 9007199254740991;

/// Maximum CLI flags declared or projected by one extension.
pub const MAX_EXTENSION_FLAGS: usize = 64;

/// Maximum nesting depth of provider JSON and error data.
pub const MAX_JSON_DEPTH: usize = 32;

/// Maximum UTF-8 bytes in a reverse request or session identifier.
pub const MAX_JSON_RPC_ID_BYTES: usize = 256;

/// Maximum providers in the host registry.
pub const MAX_PROVIDERS: usize = 32;

/// Maximum provider models in a batch and in the host registry.
pub const MAX_PROVIDER_MODELS: usize = 256;

/// Maximum bytes in provider and model identifiers.
pub const MAX_PROVIDER_ID_BYTES: usize = 64;

/// Maximum UTF-8 bytes in a provider or model display label.
pub const MAX_PROVIDER_LABEL_BYTES: usize = 128;

/// Maximum authorization scopes.
pub const MAX_PROVIDER_AUTH_SCOPES: usize = 32;

/// Maximum bytes in an opaque authorization lease.
pub const MAX_PROVIDER_AUTH_LEASE_BYTES: usize = 256;

/// Maximum encoded provider stream event payload bytes.
pub const MAX_PROVIDER_STREAM_EVENT_BYTES: usize = 65536;

/// Maximum UTF-8 bytes in a provider cancellation or acceptance reason.
pub const MAX_REASON_BYTES: usize = 4096;
/// Maximum bytes in a session-hook owner or instance identifier.
pub const MAX_SESSION_HOOK_ID_BYTES: usize = 256;
/// Maximum events in one provider stream.
pub const MAX_PROVIDER_STREAM_EVENTS: usize = 100000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// One fixed host error semantic, code and message.
pub struct ErrorSpec {
    /// Semantic identifier.
    pub name: &'static str,
    /// JSON-RPC code.
    pub code: i64,
    /// Exact wire message.
    pub message: &'static str,
}
/// Fixed host errors used by provider responses. Other 0.4 services have their
/// own error vocabularies; this is not a global 0.4 envelope restriction.
pub const ERRORS: &[ErrorSpec] = &[
    ErrorSpec {
        name: "parse_error",
        code: -32700,
        message: "parse error",
    },
    ErrorSpec {
        name: "invalid_request",
        code: -32600,
        message: "invalid request",
    },
    ErrorSpec {
        name: "unknown_method",
        code: -32601,
        message: "unknown or unnegotiated method",
    },
    ErrorSpec {
        name: "invalid_params",
        code: -32602,
        message: "invalid params",
    },
    ErrorSpec {
        name: "internal_error",
        code: -32603,
        message: "internal error",
    },
    ErrorSpec {
        name: "version_mismatch",
        code: -32010,
        message: "extension API version mismatch",
    },
    ErrorSpec {
        name: "capability_mismatch",
        code: -32011,
        message: "extension capability mismatch",
    },
    ErrorSpec {
        name: "resource_exhausted",
        code: -32012,
        message: "extension resource exhausted",
    },
    ErrorSpec {
        name: "request_cancelled",
        code: -32800,
        message: "request cancelled",
    },
];
/// Looks up a fixed host error by semantic name.
pub fn error_spec(name: &str) -> Option<&'static ErrorSpec> {
    ERRORS.iter().find(|entry| entry.name == name)
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
/// Distinguishes omitted fields from explicit JSON null.
pub enum Presence<T> {
    /// Field is absent; container serialization skips it.
    #[default]
    Absent,
    /// Explicit null.
    Null,
    /// Present value.
    Value(T),
}
impl<T> Presence<T> {
    /// Whether container serialization should omit the field.
    pub fn is_absent(value: &Self) -> bool {
        matches!(value, Self::Absent)
    }
    /// Collapses absence and null to None.
    pub fn into_option(self) -> Option<T> {
        match self {
            Self::Value(value) => Some(value),
            Self::Absent | Self::Null => None,
        }
    }
}
impl<T: Serialize> Serialize for Presence<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Absent => serializer.serialize_unit(),
            Self::Null => serializer.serialize_none(),
            Self::Value(value) => value.serialize(serializer),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Presence<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => Self::Value(value),
            None => Self::Null,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A host-resolved CLI flag value; projection validates the declared type.
pub struct InitializeFlagValue {
    /// Wire field `name`.
    pub name: String,
    /// Wire field `value`.
    pub value: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// A fixed host error with presence-preserving optional data.
pub struct ErrorObject {
    /// Wire field `code`.
    pub code: i64,
    /// Wire field `message`.
    pub message: String,
    #[serde(default, skip_serializing_if = "Presence::is_absent")]
    /// Wire field `data`.
    pub data: Presence<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderAuthRequirement`.
pub struct ProviderAuthRequirement {
    /// Wire field `kind`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `subject`.
    pub subject: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `scopes`.
    pub scopes: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderDefinition`.
pub struct ProviderDefinition {
    /// Wire field `id`.
    pub id: String,
    /// Wire field `label`.
    pub label: String,
    /// Wire field `auth`.
    pub auth: ProviderAuthRequirement,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderModelCapabilities`.
pub struct ProviderModelCapabilities {
    /// Wire field `tools`.
    pub tools: bool,
    /// Wire field `parallel_tool_calls`.
    pub parallel_tool_calls: bool,
    /// Wire field `structured_output`.
    pub structured_output: bool,
    /// Wire field `reasoning`.
    pub reasoning: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderModelDefinition`.
pub struct ProviderModelDefinition {
    /// Wire field `id`.
    pub id: String,
    /// Wire field `api_name`.
    pub api_name: String,
    /// Wire field `protocol`.
    pub protocol: String,
    /// Wire field `context_window`.
    pub context_window: usize,
    /// Wire field `max_output_tokens`.
    pub max_output_tokens: usize,
    /// Wire field `capabilities`.
    pub capabilities: ProviderModelCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `display_name`.
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderRegisterParams`.
pub struct ProviderRegisterParams {
    /// Wire field `provider`.
    pub provider: ProviderDefinition,
    /// Wire field `models`.
    pub models: Vec<ProviderModelDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderUpdateParams`.
pub struct ProviderUpdateParams {
    /// Wire field `provider`.
    pub provider: ProviderDefinition,
    /// Wire field `models`.
    pub models: Vec<ProviderModelDefinition>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderUnregisterParams`.
pub struct ProviderUnregisterParams {
    /// Wire field `provider_id`.
    pub provider_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderCatalogCompleteParams`.
pub struct ProviderCatalogCompleteParams {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderCatalogResult`.
pub struct ProviderCatalogResult {
    /// Wire field `revision`.
    pub revision: usize,
    /// Wire field `provider_ids`.
    pub provider_ids: Vec<String>,
    /// Wire field `model_ids`.
    pub model_ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderStreamRequest`.
pub struct ProviderStreamRequest {
    /// Wire field `stream_id`.
    pub stream_id: String,
    /// Wire field `provider_id`.
    pub provider_id: String,
    /// Wire field `model_id`.
    pub model_id: String,
    /// Wire field `request`.
    pub request: serde_json::Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `authorization_lease`.
    pub authorization_lease: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderStreamAccepted`.
pub struct ProviderStreamAccepted {
    /// Wire field `stream_id`.
    pub stream_id: String,
    /// Wire field `accepted`.
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `reason`.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderStreamEvent`.
pub struct ProviderStreamEvent {
    /// Wire field `stream_id`.
    pub stream_id: String,
    /// Wire field `sequence`.
    pub sequence: usize,
    /// Wire field `kind`.
    pub kind: String,
    /// Wire field `payload`.
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderStreamCancelParams`.
pub struct ProviderStreamCancelParams {
    /// Wire field `stream_id`.
    pub stream_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `reason`.
    pub reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderAuthorizationRequest`.
pub struct ProviderAuthorizationRequest {
    /// Wire field `provider_id`.
    pub provider_id: String,
    /// Wire field `action`.
    pub action: String,
    /// Wire field `interactive`.
    pub interactive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `scopes`.
    pub scopes: Option<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Secret-free provider wire record: `ProviderAuthorizationResult`.
pub struct ProviderAuthorizationResult {
    /// Wire field `status`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Wire field `lease`.
    pub lease: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A provider/data contract validation failure, not a transport error.
pub struct ContractError {
    /// JSON-RPC code.
    pub code: i64,
    /// Stable host diagnostic, including validation detail.
    pub message: String,
}
impl ContractError {
    fn named(name: &str, detail: impl Into<String>) -> Self {
        let spec = error_spec(name).expect("host error semantic exists");
        let detail = detail.into();
        Self {
            code: spec.code,
            message: if detail.is_empty() {
                spec.message.to_owned()
            } else {
                format!("{}: {detail}", spec.message)
            },
        }
    }
}
impl fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.message)
    }
}
impl std::error::Error for ContractError {}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Host-owned session-hook data: `SessionBinding`.
pub struct SessionBinding {
    /// Wire field `session_id`.
    pub session_id: String,
    /// Wire field `extension_instance_id`.
    pub extension_instance_id: String,
    /// Wire field `process_generation`.
    pub process_generation: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Host-owned session-hook data: `SessionStart`.
pub struct SessionStart {
    /// Wire field `binding`.
    pub binding: SessionBinding,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Host-owned session-hook data: `SessionEnd`.
pub struct SessionEnd {
    /// Wire field `binding`.
    pub binding: SessionBinding,
    /// Wire field `outcome`.
    pub outcome: String,
    /// Wire field `reason`.
    pub reason: String,
    /// Wire field `duration_ms`.
    pub duration_ms: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "hook", deny_unknown_fields)]
/// Common typed payload before 0.4 adds its execution context and session mirror.
pub enum SessionHookParams {
    /// Session lifecycle observation.
    #[serde(rename = "session_start")]
    SessionStart {
        /// Host-issued session facts.
        payload: SessionStart,
    },
    /// Session lifecycle observation.
    #[serde(rename = "session_end")]
    SessionEnd {
        /// Host-issued session facts.
        payload: SessionEnd,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Session-control completion identifying the active session.
pub struct SessionLifecycleResult {
    /// Active session identifier.
    pub session_id: String,
}

#[derive(Clone, Copy)]
struct WireFieldSpec {
    name: &'static str,
    type_name: &'static str,
    required: bool,
    nullable: bool,
    max_items: Option<usize>,
    max_bytes: Option<usize>,
    values: &'static [&'static str],
}

const FIELDS_ERROR_OBJECT: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "code",
        type_name: "signed_integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "message",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "data",
        type_name: "json",
        required: false,
        nullable: true,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_AUTH_REQUIREMENT: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "kind",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["none", "oauth", "host_credential"],
    },
    WireFieldSpec {
        name: "subject",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "scopes",
        type_name: "string[]",
        required: false,
        nullable: false,
        max_items: Some(MAX_PROVIDER_AUTH_SCOPES),
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_DEFINITION: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "label",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_LABEL_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "auth",
        type_name: "ProviderAuthRequirement",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_MODEL_CAPABILITIES: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "tools",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "parallel_tool_calls",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "structured_output",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "reasoning",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_MODEL_DEFINITION: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "api_name",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "protocol",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["openai_chat", "openai_responses", "anthropic_messages"],
    },
    WireFieldSpec {
        name: "context_window",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "max_output_tokens",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "capabilities",
        type_name: "ProviderModelCapabilities",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "display_name",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_LABEL_BYTES),
        values: &[],
    },
];

const FIELDS_PROVIDER_REGISTER_PARAMS: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "provider",
        type_name: "ProviderDefinition",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "models",
        type_name: "ProviderModelDefinition[]",
        required: true,
        nullable: false,
        max_items: Some(MAX_PROVIDER_MODELS),
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_UPDATE_PARAMS: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "provider",
        type_name: "ProviderDefinition",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "models",
        type_name: "ProviderModelDefinition[]",
        required: true,
        nullable: false,
        max_items: Some(MAX_PROVIDER_MODELS),
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_UNREGISTER_PARAMS: &[WireFieldSpec] = &[WireFieldSpec {
    name: "provider_id",
    type_name: "string",
    required: true,
    nullable: false,
    max_items: None,
    max_bytes: Some(MAX_PROVIDER_ID_BYTES),
    values: &[],
}];

const FIELDS_PROVIDER_CATALOG_COMPLETE_PARAMS: &[WireFieldSpec] = &[];

const FIELDS_PROVIDER_CATALOG_RESULT: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "revision",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "provider_ids",
        type_name: "string[]",
        required: true,
        nullable: false,
        max_items: Some(MAX_PROVIDERS),
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "model_ids",
        type_name: "string[]",
        required: true,
        nullable: false,
        max_items: Some(MAX_PROVIDER_MODELS),
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_STREAM_REQUEST: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "stream_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_JSON_RPC_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "provider_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "model_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "request",
        type_name: "json",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "authorization_lease",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_AUTH_LEASE_BYTES),
        values: &[],
    },
];

const FIELDS_PROVIDER_STREAM_ACCEPTED: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "stream_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_JSON_RPC_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "accepted",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "reason",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_REASON_BYTES),
        values: &[],
    },
];

const FIELDS_PROVIDER_STREAM_EVENT: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "stream_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_JSON_RPC_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "sequence",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "kind",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[
            "started",
            "text_start",
            "text_delta",
            "text_end",
            "reasoning_start",
            "reasoning_delta",
            "reasoning_end",
            "tool_call_start",
            "tool_call_args_delta",
            "tool_call_end",
            "usage",
            "finished",
            "error",
            "heartbeat",
        ],
    },
    WireFieldSpec {
        name: "payload",
        type_name: "json",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_STREAM_CANCEL_PARAMS: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "stream_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_JSON_RPC_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "reason",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_REASON_BYTES),
        values: &[],
    },
];

const FIELDS_PROVIDER_AUTHORIZATION_REQUEST: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "provider_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "action",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["authorize", "refresh", "revoke"],
    },
    WireFieldSpec {
        name: "interactive",
        type_name: "boolean",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "scopes",
        type_name: "string[]",
        required: false,
        nullable: false,
        max_items: Some(MAX_PROVIDER_AUTH_SCOPES),
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_PROVIDER_AUTHORIZATION_RESULT: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "status",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["ready", "pending", "denied", "unavailable", "revoked"],
    },
    WireFieldSpec {
        name: "lease",
        type_name: "string",
        required: false,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_PROVIDER_AUTH_LEASE_BYTES),
        values: &[],
    },
];

const FIELDS_SESSION_BINDING: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "session_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_SESSION_HOOK_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "extension_instance_id",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: Some(MAX_SESSION_HOOK_ID_BYTES),
        values: &[],
    },
    WireFieldSpec {
        name: "process_generation",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_SESSION_START: &[WireFieldSpec] = &[WireFieldSpec {
    name: "binding",
    type_name: "SessionBinding",
    required: true,
    nullable: false,
    max_items: None,
    max_bytes: None,
    values: &[],
}];

const FIELDS_SESSION_END: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "binding",
        type_name: "SessionBinding",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "outcome",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[
            "completed",
            "failed",
            "cancelled",
            "interrupted",
            "frontend_disconnected",
            "shutdown",
            "limit_reached",
        ],
    },
    WireFieldSpec {
        name: "reason",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["shutdown", "reload", "crash", "cancelled"],
    },
    WireFieldSpec {
        name: "duration_ms",
        type_name: "integer",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_SESSION_HOOK_PARAMS_SESSION_START: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "hook",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["session_start"],
    },
    WireFieldSpec {
        name: "payload",
        type_name: "SessionStart",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_SESSION_HOOK_PARAMS_SESSION_END: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "hook",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["session_end"],
    },
    WireFieldSpec {
        name: "payload",
        type_name: "SessionEnd",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_SESSION_LIFECYCLE_RESULT: &[WireFieldSpec] = &[WireFieldSpec {
    name: "session_id",
    type_name: "string",
    required: true,
    nullable: false,
    max_items: None,
    max_bytes: Some(MAX_JSON_RPC_ID_BYTES),
    values: &[],
}];

const FIELDS_JSON_RPC_SUCCESS_RESPONSE: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "jsonrpc",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["2.0"],
    },
    WireFieldSpec {
        name: "id",
        type_name: "rpc_id",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "result",
        type_name: "json",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

const FIELDS_JSON_RPC_ERROR_RESPONSE: &[WireFieldSpec] = &[
    WireFieldSpec {
        name: "jsonrpc",
        type_name: "string",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &["2.0"],
    },
    WireFieldSpec {
        name: "id",
        type_name: "rpc_id",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
    WireFieldSpec {
        name: "error",
        type_name: "ErrorObject",
        required: true,
        nullable: false,
        max_items: None,
        max_bytes: None,
        values: &[],
    },
];

fn canonical_value(
    value: &serde_json::Value,
    depth: usize,
) -> Result<serde_json::Value, ContractError> {
    if depth > MAX_JSON_DEPTH {
        return Err(ContractError::named(
            "invalid_params",
            "canonical JSON nesting exceeds max_json_depth",
        ));
    }
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::String(_) => {
            Ok(value.clone())
        }
        serde_json::Value::Number(number) => {
            let valid = number.as_i64().is_some_and(|number| {
                (-MAX_PORTABLE_JSON_INTEGER..=MAX_PORTABLE_JSON_INTEGER).contains(&number)
            }) || number
                .as_u64()
                .is_some_and(|number| number <= MAX_PORTABLE_JSON_INTEGER as u64);
            if valid {
                Ok(value.clone())
            } else {
                Err(ContractError::named(
                    "invalid_params",
                    "canonical JSON permits only portable integers",
                ))
            }
        }
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| canonical_value(value, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        serde_json::Value::Object(values) => {
            let mut sorted = BTreeMap::new();
            for (key, value) in values {
                sorted.insert(key.clone(), canonical_value(value, depth + 1)?);
            }
            Ok(serde_json::Value::Object(sorted.into_iter().collect()))
        }
    }
}

/// Encodes bounded portable service JSON (not a transport framing rule).
pub fn portable_json(value: &serde_json::Value) -> Result<String, ContractError> {
    serde_json::to_string(&canonical_value(value, 0)?)
        .map_err(|error| ContractError::named("internal_error", error.to_string()))
}

fn validate_wire_type(type_name: &str, value: &serde_json::Value) -> Result<(), ContractError> {
    if let Some(element) = type_name.strip_suffix("[]") {
        let values = value.as_array().ok_or_else(|| {
            ContractError::named("invalid_params", format!("expected {type_name}"))
        })?;
        for entry in values {
            validate_wire_type(element, entry)?;
        }
        return Ok(());
    }
    match type_name {
        "string" => {
            if value.is_string() {
                Ok(())
            } else {
                Err(ContractError::named("invalid_params", "expected string"))
            }
        }
        "boolean" => {
            if value.is_boolean() {
                Ok(())
            } else {
                Err(ContractError::named("invalid_params", "expected boolean"))
            }
        }
        "integer" => {
            if value.as_u64().is_some() {
                Ok(())
            } else {
                Err(ContractError::named(
                    "invalid_params",
                    "expected unsigned integer",
                ))
            }
        }
        "signed_integer" => {
            if value.as_i64().is_some() {
                Ok(())
            } else {
                Err(ContractError::named(
                    "invalid_params",
                    "expected signed integer",
                ))
            }
        }
        "json" => canonical_value(value, 0).map(|_| ()),
        "rpc_id" => match value {
            serde_json::Value::String(value)
                if !value.is_empty() && value.len() <= MAX_JSON_RPC_ID_BYTES =>
            {
                Ok(())
            }
            serde_json::Value::Number(number) if number.as_u64().is_some() => Ok(()),
            _ => Err(ContractError::named(
                "invalid_request",
                "JSON-RPC id must be a bounded string or unsigned integer",
            )),
        },
        model => validate_model_value(model, value),
    }
}
fn validate_record(
    value: &serde_json::Value,
    fields: &[WireFieldSpec],
) -> Result<(), ContractError> {
    let object = value
        .as_object()
        .ok_or_else(|| ContractError::named("invalid_params", "model must be an object"))?;
    for key in object.keys() {
        if !fields.iter().any(|field| field.name == key) {
            return Err(ContractError::named(
                "invalid_params",
                format!("unknown model field {key:?}"),
            ));
        }
    }
    for field in fields {
        let Some(value) = object.get(field.name) else {
            if field.required {
                return Err(ContractError::named(
                    "invalid_params",
                    format!("{} is required", field.name),
                ));
            }
            continue;
        };
        if value.is_null() {
            if field.nullable {
                continue;
            }
            return Err(ContractError::named(
                "invalid_params",
                format!("{} must not be null", field.name),
            ));
        }
        validate_wire_type(field.type_name, value)?;
        if let Some(max_items) = field.max_items {
            if value
                .as_array()
                .is_some_and(|items| items.len() > max_items)
            {
                return Err(ContractError::named(
                    "resource_exhausted",
                    format!("{} exceeds item bound", field.name),
                ));
            }
        }
        if let Some(max_bytes) = field.max_bytes {
            let exceeds = value.as_str().is_some_and(|text| text.len() > max_bytes)
                || (field.type_name.strip_suffix("[]") == Some("string")
                    && value.as_array().is_some_and(|items| {
                        items
                            .iter()
                            .any(|item| item.as_str().is_some_and(|text| text.len() > max_bytes))
                    }));
            if exceeds {
                return Err(ContractError::named(
                    "resource_exhausted",
                    format!("{} exceeds byte bound", field.name),
                ));
            }
        }
        if !field.values.is_empty()
            && !value
                .as_str()
                .is_some_and(|text| field.values.contains(&text))
        {
            return Err(ContractError::named(
                "invalid_params",
                format!("{} has unsupported value", field.name),
            ));
        }
    }
    Ok(())
}

fn validate_model_value(name: &str, value: &serde_json::Value) -> Result<(), ContractError> {
    match name {
        "ErrorObject" => validate_record(value, FIELDS_ERROR_OBJECT),

        "ProviderAuthRequirement" => validate_record(value, FIELDS_PROVIDER_AUTH_REQUIREMENT),

        "ProviderDefinition" => validate_record(value, FIELDS_PROVIDER_DEFINITION),

        "ProviderModelCapabilities" => validate_record(value, FIELDS_PROVIDER_MODEL_CAPABILITIES),

        "ProviderModelDefinition" => validate_record(value, FIELDS_PROVIDER_MODEL_DEFINITION),

        "ProviderRegisterParams" => validate_record(value, FIELDS_PROVIDER_REGISTER_PARAMS),

        "ProviderUpdateParams" => validate_record(value, FIELDS_PROVIDER_UPDATE_PARAMS),

        "ProviderUnregisterParams" => validate_record(value, FIELDS_PROVIDER_UNREGISTER_PARAMS),

        "ProviderCatalogCompleteParams" => {
            validate_record(value, FIELDS_PROVIDER_CATALOG_COMPLETE_PARAMS)
        }

        "ProviderCatalogResult" => validate_record(value, FIELDS_PROVIDER_CATALOG_RESULT),

        "ProviderStreamRequest" => validate_record(value, FIELDS_PROVIDER_STREAM_REQUEST),

        "ProviderStreamAccepted" => validate_record(value, FIELDS_PROVIDER_STREAM_ACCEPTED),

        "ProviderStreamEvent" => validate_record(value, FIELDS_PROVIDER_STREAM_EVENT),

        "ProviderStreamCancelParams" => {
            validate_record(value, FIELDS_PROVIDER_STREAM_CANCEL_PARAMS)
        }

        "ProviderAuthorizationRequest" => {
            validate_record(value, FIELDS_PROVIDER_AUTHORIZATION_REQUEST)
        }

        "ProviderAuthorizationResult" => {
            validate_record(value, FIELDS_PROVIDER_AUTHORIZATION_RESULT)
        }

        "SessionLifecycleResult" => validate_record(value, FIELDS_SESSION_LIFECYCLE_RESULT),
        "SessionBinding" => validate_record(value, FIELDS_SESSION_BINDING),
        "SessionStart" => validate_record(value, FIELDS_SESSION_START),
        "SessionEnd" => validate_record(value, FIELDS_SESSION_END),
        "SessionHookParams" => {
            let object = value.as_object().ok_or_else(|| {
                ContractError::named("invalid_params", "SessionHookParams must be an object")
            })?;
            match object.get("hook").and_then(serde_json::Value::as_str) {
                Some("session_start") => {
                    validate_record(value, FIELDS_SESSION_HOOK_PARAMS_SESSION_START)
                }
                Some("session_end") => {
                    validate_record(value, FIELDS_SESSION_HOOK_PARAMS_SESSION_END)
                }
                _ => Err(ContractError::named(
                    "invalid_params",
                    "unknown SessionHookParams variant",
                )),
            }
        }
        _ => unreachable!("internal provider/data model name"),
    }
}

fn parse_model<T: DeserializeOwned>(
    name: &str,
    value: serde_json::Value,
) -> Result<T, ContractError> {
    canonical_value(&value, 0)?;
    validate_model_value(name, &value)?;
    serde_json::from_value(value)
        .map_err(|error| ContractError::named("invalid_params", format!("invalid {name}: {error}")))
}

/// Parses and validates the closed `ErrorObject` data record.
pub fn parse_error_object(value: serde_json::Value) -> Result<ErrorObject, ContractError> {
    parse_model("ErrorObject", value)
}

/// Parses and validates the closed `ProviderAuthRequirement` data record.
pub fn parse_provider_auth_requirement(
    value: serde_json::Value,
) -> Result<ProviderAuthRequirement, ContractError> {
    parse_model("ProviderAuthRequirement", value)
}

/// Parses and validates the closed `ProviderDefinition` data record.
pub fn parse_provider_definition(
    value: serde_json::Value,
) -> Result<ProviderDefinition, ContractError> {
    parse_model("ProviderDefinition", value)
}

/// Parses and validates the closed `ProviderModelCapabilities` data record.
pub fn parse_provider_model_capabilities(
    value: serde_json::Value,
) -> Result<ProviderModelCapabilities, ContractError> {
    parse_model("ProviderModelCapabilities", value)
}

/// Parses and validates the closed `ProviderModelDefinition` data record.
pub fn parse_provider_model_definition(
    value: serde_json::Value,
) -> Result<ProviderModelDefinition, ContractError> {
    parse_model("ProviderModelDefinition", value)
}

/// Parses and validates the closed `ProviderRegisterParams` data record.
pub fn parse_provider_register_params(
    value: serde_json::Value,
) -> Result<ProviderRegisterParams, ContractError> {
    parse_model("ProviderRegisterParams", value)
}

/// Parses and validates the closed `ProviderUpdateParams` data record.
pub fn parse_provider_update_params(
    value: serde_json::Value,
) -> Result<ProviderUpdateParams, ContractError> {
    parse_model("ProviderUpdateParams", value)
}

/// Parses and validates the closed `ProviderUnregisterParams` data record.
pub fn parse_provider_unregister_params(
    value: serde_json::Value,
) -> Result<ProviderUnregisterParams, ContractError> {
    parse_model("ProviderUnregisterParams", value)
}

/// Parses and validates the closed `ProviderCatalogCompleteParams` data record.
pub fn parse_provider_catalog_complete_params(
    value: serde_json::Value,
) -> Result<ProviderCatalogCompleteParams, ContractError> {
    parse_model("ProviderCatalogCompleteParams", value)
}

/// Parses and validates the closed `ProviderCatalogResult` data record.
pub fn parse_provider_catalog_result(
    value: serde_json::Value,
) -> Result<ProviderCatalogResult, ContractError> {
    parse_model("ProviderCatalogResult", value)
}

/// Parses and validates the closed `ProviderStreamRequest` data record.
pub fn parse_provider_stream_request(
    value: serde_json::Value,
) -> Result<ProviderStreamRequest, ContractError> {
    parse_model("ProviderStreamRequest", value)
}

/// Parses and validates the closed `ProviderStreamAccepted` data record.
pub fn parse_provider_stream_accepted(
    value: serde_json::Value,
) -> Result<ProviderStreamAccepted, ContractError> {
    parse_model("ProviderStreamAccepted", value)
}

/// Parses and validates the closed `ProviderStreamEvent` data record.
pub fn parse_provider_stream_event(
    value: serde_json::Value,
) -> Result<ProviderStreamEvent, ContractError> {
    parse_model("ProviderStreamEvent", value)
}

/// Parses and validates the closed `ProviderStreamCancelParams` data record.
pub fn parse_provider_stream_cancel_params(
    value: serde_json::Value,
) -> Result<ProviderStreamCancelParams, ContractError> {
    parse_model("ProviderStreamCancelParams", value)
}

/// Parses and validates the closed `ProviderAuthorizationRequest` data record.
pub fn parse_provider_authorization_request(
    value: serde_json::Value,
) -> Result<ProviderAuthorizationRequest, ContractError> {
    parse_model("ProviderAuthorizationRequest", value)
}

/// Parses and validates the closed `ProviderAuthorizationResult` data record.
pub fn parse_provider_authorization_result(
    value: serde_json::Value,
) -> Result<ProviderAuthorizationResult, ContractError> {
    parse_model("ProviderAuthorizationResult", value)
}

fn serialized_value<T: Serialize>(value: &T) -> Result<serde_json::Value, ContractError> {
    let value = serde_json::to_value(value)
        .map_err(|error| ContractError::named("internal_error", error.to_string()))?;
    canonical_value(&value, 0)?;
    Ok(value)
}

/// Checks the fixed host code/message pair and bounded data.
pub fn validate_error_object(error: &ErrorObject) -> Result<(), ContractError> {
    let value = serialized_value(error)?;
    validate_model_value("ErrorObject", &value)?;
    let Some(spec) = ERRORS.iter().find(|spec| spec.code == error.code) else {
        return Err(ContractError::named(
            "invalid_params",
            "error code is absent from API 0.3 table",
        ));
    };
    if spec.message != error.message {
        return Err(ContractError::named(
            "invalid_params",
            "error message does not match its API 0.3 code",
        ));
    }
    Ok(())
}

/// Builds a fixed host error; None omits data, Some(null) preserves null.
pub fn error_object(
    name: &str,
    data: Option<serde_json::Value>,
) -> Result<ErrorObject, ContractError> {
    let spec = error_spec(name).ok_or_else(|| {
        ContractError::named("internal_error", "unknown generated error semantic")
    })?;
    let error = ErrorObject {
        code: spec.code,
        message: spec.message.to_owned(),
        data: match data {
            Some(value) => Presence::Value(value),
            None => Presence::Absent,
        },
    };
    validate_error_object(&error)?;
    Ok(error)
}

/// Validates the common session-hook payload before 0.4 context is attached.
pub fn parse_session_hook_params(
    value: serde_json::Value,
) -> Result<SessionHookParams, ContractError> {
    parse_model("SessionHookParams", value)
}

/// Validates the bounded session identifier in a session-control completion.
pub fn parse_session_lifecycle_result(
    value: serde_json::Value,
) -> Result<SessionLifecycleResult, ContractError> {
    parse_model("SessionLifecycleResult", value)
}

/// Validates host-built session/model-control responses before atomic delivery.
///
/// This intentionally retains the checks already used by these services. It
/// is not a validator for all 0.4 envelopes: other services allow their own
/// result shapes and error vocabularies.
pub fn validate_session_response(value: &serde_json::Value) -> Result<(), ContractError> {
    let invalid = |error: ContractError| ContractError::named("invalid_request", error.message);
    canonical_value(value, 0).map_err(invalid)?;
    let object = value.as_object().ok_or_else(|| {
        ContractError::named("invalid_request", "JSON-RPC envelope must be an object")
    })?;
    let fields = match (
        object.contains_key("id"),
        object.contains_key("method"),
        object.contains_key("result"),
        object.contains_key("error"),
    ) {
        (true, false, true, false) => FIELDS_JSON_RPC_SUCCESS_RESPONSE,
        (true, false, false, true) => FIELDS_JSON_RPC_ERROR_RESPONSE,
        _ => {
            return Err(ContractError::named(
                "invalid_request",
                "JSON-RPC envelope has an invalid request/response shape",
            ))
        }
    };
    validate_record(value, fields).map_err(invalid)?;
    if let Some(error) = object.get("error") {
        let error = parse_error_object(error.clone()).map_err(invalid)?;
        validate_error_object(&error).map_err(invalid)?;
    }
    Ok(())
}
