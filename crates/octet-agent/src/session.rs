//! One concrete append-only JSONL session: parent-linked entries, durable
//! head records, branching via checkout, and context reconstruction.
//!
//! The session *is* the conversation history model — there is no separate
//! store trait or conversation manager. Only semantic boundaries are
//! persisted (complete user messages, complete assistant messages, individual
//! tool results, config markers, compaction records); streaming deltas never
//! enter the session log.
//!
//! The one exception is a **separate, bounded sidecar**: while an assistant
//! attempt streams, [`Session::begin_assistant_frame_journal`] records
//! [`octet_ai::AssistantMessageFrame`]s to an owner-only file beside the
//! session so a killed process can republish partial progress through
//! [`Session::take_partial_assistant`]. The journal is never a session record,
//! never provider-visible context, and is removed at terminal settlement.
//!
//! # Crash semantics
//!
//! Every append writes the entry record and a head record to the append-only
//! file before returning. This makes records **process-crash safe**: once
//! `append` returns, the bytes are in the kernel and survive the process
//! dying. Every semantic record is followed by `sync_data` before success is
//! returned, so completed session commits survive ordinary OS crashes and
//! power loss subject to the filesystem's durability guarantees.
//!
//! After an unclean exit the file may end in a torn final line;
//! [`Session::open`] drops (and truncates away) an unparseable *final* line
//! and resumes from the last recorded head, while corruption in any earlier
//! (completed) record is rejected. Because there is an unavoidable window
//! between a tool's external side effect and the write of its result entry,
//! unresolved mutating calls are reported as **indeterminate** after an
//! unclean crash and are not automatically replayed. Read-only tools may opt
//! into safe replay. This avoids claiming exactly-once execution while also
//! avoiding silent at-least-once mutations.
//!
//! # Module layout
//!
//! This file owns the vocabulary — [`SessionError`], the entry and record
//! types, and the [`Session`] struct itself — and nothing else. Each concern
//! that has a failure mode of its own lives in a sibling so it can be read
//! (and fixed) on its own:
//!
//! - [`store`] — durable file lifecycle: create, open, replay, write fence.
//! - [`entries`] — the entry tree: append, checkout, fork, checkpoints.
//! - [`usage`] — the usage ledger and the picodollar carry.
//! - [`context`] — the model-visible projection, compaction, skill resolution.
//! - [`replay`] — the Responses API sidecar projection and its cache.
//! - [`journal`] — the bounded partial-assistant frame sidecar.
//! - `tests` — one module per concern, mirroring the split above.

use std::cell::{Ref, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::session_writer::SessionWriter;
use crate::tools::deferred::{DeferredRunRecord, DeferredRunStore};
use crate::tools::durability::{
    DurableInvocationStore, InvocationHandle, InvocationRecord, InvocationScope,
};

use fs2::FileExt;
use octet_ai::{
    Cost, EndpointId, Message, ModelId, StopReason, Usage, UserMessage, UserPart,
    PICODOLLARS_PER_MICRODOLLAR,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Identifier of a session entry. Unique within one session file.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct EntryId(pub String);

/// A durable restore point written after one submitted prompt completes.
///
/// `prompt` identifies the user entry that began the completed interaction;
/// `head` is the exact session entry restored by [`Session::restore_checkpoint`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    /// User-message entry that began this completed interaction.
    pub prompt: EntryId,
    /// Session head after the interaction completed.
    pub head: EntryId,
    /// Provider-reported cumulative usage for this completed interaction.
    #[serde(default)]
    pub usage: Option<Usage>,
    /// Cost accrued by this completed interaction, including explicit zero.
    #[serde(default)]
    pub run_cost_microdollars: Option<u64>,
}

/// The operation to which a durable usage record belongs.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UsageRecordKind {
    /// One completed provider turn persisted as an assistant message.
    AssistantTurn {
        /// The assistant entry that received the provider response.
        assistant: EntryId,
    },
    /// A billable Responses turn rejected before assistant persistence because
    /// explicit native replay mode did not receive authoritative raw output.
    RejectedResponsesTurn,
    /// Provider usage produced by one bounded delegated child during the
    /// owning root interaction. The child session remains independently
    /// durable; this record is the root session's accounting ledger entry.
    DelegatedAgent {
        /// Stable host-created child identifier within the owning run.
        agent_id: String,
        /// Completed model turns observed for the child.
        turn_count: u64,
        /// Host-observed child tool calls.
        tool_call_count: u64,
    },
    /// A tool-free call used to produce a context-compaction summary.
    Compaction,
    /// Isolated Anthropic prompt-cache keepalive; never an assistant turn.
    CacheWarm,
    /// A bounded one-token decision about whether a candidate response may
    /// return control to the user. `None` records a billable malformed answer.
    TerminalGate {
        /// `Some(true)` returns, `Some(false)` continues, and `None` is invalid.
        returned: Option<bool>,
    },
}

/// One child session's exact usage mirror for the owning root ledger.
#[derive(Clone, Debug)]
pub(crate) struct DelegatedUsage {
    pub(crate) agent_id: String,
    pub(crate) turn_count: u64,
    pub(crate) tool_call_count: u64,
    pub(crate) endpoint: EndpointId,
    pub(crate) model: ModelId,
    pub(crate) usage: Usage,
    pub(crate) cost: Option<Cost>,
}

/// Durable evidence that an accepted provider attempt has unreported usage.
///
/// This is not a zero-token or zero-cost usage record. Only host-selected route,
/// model and operation identifiers belong here, never URLs, errors or payloads.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageUncertaintyRecord {
    /// Host-selected endpoint identifier, not its URL or credentials.
    pub endpoint: EndpointId,
    /// Host-selected canonical model identifier.
    pub model: ModelId,
    /// Host-selected operation identifier (for example `assistant_turn`).
    pub operation: String,
}

/// Conservative admission exposure for an attempt whose actual usage is unknown.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UsageUncertaintyBound {
    /// Input estimate plus the provider-enforced output cap.
    pub tokens: u64,
    /// That many tokens at the route's worst-case price; None when unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microdollars: Option<u64>,
}

impl UsageUncertaintyRecord {
    fn validate(&self) -> Result<(), SessionError> {
        // Keep persisted diagnostics bounded and exclude URL/query/header and
        // control syntax. Identifiers are trusted host metadata, not redacted
        // arbitrary provider content; never echo an invalid value in errors.
        for value in [&self.endpoint.0, &self.model.0, &self.operation] {
            if value.is_empty()
                || value.len() > 128
                || value.contains("://")
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
            {
                return Err(SessionError::Limit(
                    "usage uncertainty identifiers must be 1..=128 ASCII identifier bytes".into(),
                ));
            }
        }
        Ok(())
    }
}

/// Durable, payload-free status of an Anthropic prompt-cache keepalive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheWarmState {
    /// Written before the provider request; an unsettled attempt is uncertain.
    Started,
    /// A complete terminal response with separately recorded usage.
    Completed,
    /// Deadline expired after possible dispatch; usage is unknown.
    TimedOut,
    /// Provider failed after possible dispatch; usage is unknown.
    Failed,
}

/// One bounded status record, separate from provider usage and model context.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheWarmRecord {
    /// Monotonic attempt number within this session.
    pub attempt: u64,
    /// Host-selected route identifier (never URL or credentials).
    pub endpoint: EndpointId,
    /// Host-selected model identifier.
    pub model: ModelId,
    /// Attempt lifecycle.
    pub state: CacheWarmState,
    /// Wall-clock observation time.
    pub at_unix_ms: u64,
    /// Durable request-prefix head. Absent on older experimental warm records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<EntryId>,
    /// Whether an extension overrode the economic decision for this attempt.
    #[serde(default)]
    pub extension_override: bool,
}

/// Provider usage and cost recorded for one durable operation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct UsageRecord {
    /// The operation that produced this usage.
    pub kind: UsageRecordKind,
    /// Provider-reported, disjoint token buckets.
    pub usage: Usage,
    /// Provider-authoritative terminal reason for an assistant turn.
    /// Legacy usage records and non-assistant operations omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    /// Endpoint/provider route used for this operation.
    #[serde(default)]
    pub endpoint: Option<EndpointId>,
    /// Canonical selected model used for this operation.
    #[serde(default)]
    pub model: Option<ModelId>,
    /// Completion wall-clock time in milliseconds since the Unix epoch.
    #[serde(default)]
    pub completed_at_unix_ms: Option<u64>,
    /// Per-category request cost when pricing was available.
    #[serde(default)]
    pub cost: Option<Cost>,
    /// Request total retained explicitly for lightweight readers and backwards
    /// compatibility with the first usage-record format.
    #[serde(default)]
    pub cost_microdollars: Option<u64>,
    /// Cumulative whole-microdollar session cost after this operation. Keeping
    /// it on the same JSONL record makes usage and accounting one durable
    /// update rather than two crash-separable writes.
    #[serde(default)]
    pub session_cost_microdollars: Option<u64>,
    /// Picodollar remainder paired with `session_cost_microdollars`.
    #[serde(default)]
    pub session_cost_picodollars_remainder: Option<u32>,
}

/// Maximum separately namespaced extension metadata values attached to one
/// durable entry.
pub const MAX_EXTENSION_ENTRY_METADATA_NAMESPACES: usize = 32;
/// Maximum encoded JSON bytes retained for one extension metadata value.
pub const MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES: usize = 16 * 1024;
/// Maximum aggregate encoded JSON bytes retained for extension metadata on one
/// durable entry.
pub const MAX_EXTENSION_ENTRY_METADATA_BYTES: usize = 128 * 1024;
const MAX_EXTENSION_ENTRY_METADATA_DEPTH: usize = 16;
const MAX_EXTENSION_ENTRY_METADATA_NODES: usize = 256;
const MAX_EXTENSION_ENTRY_METADATA_KEY_BYTES: usize = 256;
/// Maximum bytes retained for one extension-owned entry type identifier.
pub const MAX_EXTENSION_ENTRY_TYPE_BYTES: usize = 128;
/// Maximum bytes retained for one durable entry label.
pub const MAX_ENTRY_LABEL_BYTES: usize = 4 * 1024;

/// Durable extension-owned payload from the extension append protocol.
///
/// This is opaque, inert data owned by the extension namespace that appended
/// it. It is retained in that namespace's entry metadata exactly like every
/// other extension-owned value — bounded by the same rules and never part of
/// provider context — and decoded again by [`Session::extension_entry`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionEntry {
    /// Caller-declared entry type from the extension protocol.
    pub entry_type: String,
    /// Bounded inert JSON payload owned by the entry type.
    pub data: serde_json::Value,
}

impl ExtensionEntry {
    /// Encodes this payload into its namespace value envelope.
    fn to_value(&self) -> serde_json::Value {
        serde_json::json!({ "entry_type": self.entry_type, "data": self.data })
    }

    /// Decodes a namespace value envelope written by [`Self::to_value`].
    ///
    /// A namespace may also carry values from other host paths, so only the
    /// exact two-key envelope decodes; anything else is not an entry.
    fn from_value(value: &serde_json::Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 2 {
            return None;
        }
        let entry_type = object.get("entry_type")?.as_str()?.to_owned();
        let data = object.get("data")?.clone();
        valid_extension_entry_type(&entry_type).then_some(Self { entry_type, data })
    }
}

/// Returns whether `entry_type` is a usable extension-owned entry type.
fn valid_extension_entry_type(entry_type: &str) -> bool {
    !entry_type.is_empty()
        && entry_type.len() <= MAX_EXTENSION_ENTRY_TYPE_BYTES
        && !entry_type.chars().any(char::is_control)
}

/// Validates an extension entry payload and returns its encoded envelope size.
///
/// The shape rules are exactly [`valid_extension_metadata_value`] and the size
/// bound is exactly [`MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES`], so a payload
/// accepted here is retained verbatim by the namespace metadata sanitizer — an
/// extension entry can never widen what durable extension state may contain.
fn valid_extension_entry_payload(entry: &ExtensionEntry) -> Option<usize> {
    if !valid_extension_entry_type(&entry.entry_type) {
        return None;
    }
    let value = entry.to_value();
    let mut nodes = 0usize;
    if !valid_extension_metadata_value(&value, 0, &mut nodes) {
        return None;
    }
    let encoded = serde_json::to_vec(&value).ok()?;
    (encoded.len() <= MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES).then_some(encoded.len())
}

/// Returns whether `label` is a bounded, control-character-free entry label.
///
/// An empty label is valid and clears the entry's label.
fn valid_entry_label(label: &str) -> bool {
    label.len() <= MAX_ENTRY_LABEL_BYTES && !label.chars().any(char::is_control)
}

/// Host-attested provenance for one extension-owned metadata value.
///
/// The extension never controls this envelope: the host attaches it while
/// collecting a declared typed hook response before the entry is persisted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionMetadataProvenance {
    /// Stable extension namespace selected during host registration.
    pub extension: String,
    /// Process generation that supplied the value, when the source is an
    /// executable extension. Native extensions omit this fence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_generation: Option<u64>,
}

/// One bounded extension-owned value retained beside a durable entry.
///
/// Values are never included in provider context. Frontends and exports must
/// use [`EntryMetadata::public_extension_metadata`] rather than exposing this
/// map directly, because private values are intentionally retained only for
/// the owning extension's recovery/diagnostic boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionEntryMetadata {
    /// Whether ordinary frontend/export projections may expose this value.
    #[serde(default)]
    pub public: bool,
    /// Bounded inert JSON value owned by the extension namespace.
    pub value: serde_json::Value,
    /// Host-attested source identity.
    pub provenance: ExtensionMetadataProvenance,
}

impl ExtensionEntryMetadata {
    fn sanitized(self, namespace: &str) -> Option<Self> {
        if !is_valid_extension_metadata_namespace(namespace)
            || !is_valid_extension_metadata_namespace(&self.provenance.extension)
            || self.provenance.extension != namespace
        {
            return None;
        }
        let mut nodes = 0usize;
        if !valid_extension_metadata_value(&self.value, 0, &mut nodes) {
            return None;
        }
        let encoded = serde_json::to_vec(&self.value).ok()?;
        (encoded.len() <= MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES).then_some(self)
    }
}

/// Returns whether `namespace` is a stable extension-owned metadata namespace.
///
/// Namespaces are lowercase ASCII segments separated by dots. This permits
/// durable ownership checks without treating arbitrary user-facing text as a
/// storage key.
pub fn is_valid_extension_metadata_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.len() <= 128
        && namespace.split('.').all(|segment| {
            !segment.is_empty()
                && segment.len() <= 64
                && segment.bytes().enumerate().all(|(index, byte)| match byte {
                    b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-' => {
                        index > 0 || byte.is_ascii_lowercase()
                    }
                    _ => false,
                })
        })
}

fn valid_extension_metadata_value(
    value: &serde_json::Value,
    depth: usize,
    nodes: &mut usize,
) -> bool {
    if depth > MAX_EXTENSION_ENTRY_METADATA_DEPTH || *nodes >= MAX_EXTENSION_ENTRY_METADATA_NODES {
        return false;
    }
    *nodes = nodes.saturating_add(1);
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => true,
        serde_json::Value::String(value) => {
            value.len() <= MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES
                && !value.chars().any(char::is_control)
        }
        serde_json::Value::Array(values) => values
            .iter()
            .all(|value| valid_extension_metadata_value(value, depth.saturating_add(1), nodes)),
        serde_json::Value::Object(values) => values.iter().all(|(key, value)| {
            key.len() <= MAX_EXTENSION_ENTRY_METADATA_KEY_BYTES
                && !key.chars().any(char::is_control)
                && valid_extension_metadata_value(value, depth.saturating_add(1), nodes)
        }),
    }
}

/// Stable presentation metadata attached to a durable session entry.
///
/// Values are inert data, never terminal escape sequences. In addition to the
/// semantic model identity, a user prompt may retain the exact sRGB highlight
/// assigned when it was appended. Persisting that value keeps old prompts
/// visually immutable across model and theme changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryMetadata {
    /// Atomic provenance for a materialized native steering user message.
    /// The tuple is (operation identifier, prepared local submission id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_steering: Option<(String, u64)>,
    /// Canonical model that received a user prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_model: Option<ModelId>,
    /// Stable model creator/source key (for example `openai` or `deepseek`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_model_source: Option<String>,
    /// Exact normalized sRGB highlight assigned to this prompt (`#rrggbb`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_color: Option<String>,
    /// User-visible transcript text when model-only prompt composition added
    /// context around the submitted draft. The message body remains the exact
    /// replayable model input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// Durable terminal state for a completed frontend run.
    ///
    /// This is presentation-only metadata attached to a non-model-visible
    /// marker entry. Keeping it on a known entry variant lets older octet
    /// binaries safely ignore the additional field while newer frontends can
    /// reconstruct run boundaries after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_outcome: Option<SessionRunOutcome>,
    /// Structured content and inert metadata retained beside a tool-result
    /// message. These values are presentation/session data and never enter the
    /// canonical provider-visible message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_output: Option<crate::tool::ToolOutputDetails>,
    /// Unix milliseconds just before the tool's effects were admitted
    /// (`null` when the call never reached the effect gate). Together with
    /// `tool_finished_unix_ms` this is the durable per-tool timing window
    /// persisted beside the tool result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_started_unix_ms: Option<u64>,
    /// Unix milliseconds when the tool call's outcome was finalized.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_finished_unix_ms: Option<u64>,
    /// Marks a locally generated assistant boundary that intentionally has no
    /// authoritative provider sidecar.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub local_synthetic_assistant: bool,
    /// Extension-owned metadata keyed by a host-validated namespace.
    ///
    /// This is deliberately separate from host-owned presentation fields and
    /// canonical message content. It is never reconstructed into provider
    /// context.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extension_metadata: BTreeMap<String, ExtensionEntryMetadata>,
}

/// Durable terminal state for one frontend-owned agent run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRunOutcome {
    /// Coarse terminal status shared by graphical and native frontends.
    pub status: SessionRunOutcomeStatus,
    /// Optional bounded user-safe explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Coarse terminal status persisted at a frontend run boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunOutcomeStatus {
    /// The run completed successfully.
    Completed,
    /// The user stopped the run.
    Stopped,
    /// The run failed.
    Failed,
}

impl EntryMetadata {
    fn sanitized(mut self) -> Option<Self> {
        self.native_steering = self.native_steering.filter(|(operation, id)| {
            !operation.is_empty()
                && operation.len() <= 128
                && *id < 64
                && operation
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte))
        });
        self.prompt_model = self.prompt_model.filter(|model| {
            !model.0.is_empty() && !model.0.chars().any(|character| character.is_control())
        });
        self.prompt_model_source = self.prompt_model_source.and_then(|source| {
            let source = source.trim();
            (!source.is_empty()
                && source.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '-' | '_' | '.' | ':' | '/')
                }))
            .then(|| source.to_owned())
        });
        self.prompt_color = self.prompt_color.and_then(|color| {
            let color = color.trim();
            let digits = color.strip_prefix('#')?;
            (digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .then(|| format!("#{}", digits.to_ascii_lowercase()))
        });
        self.display_text = self.display_text.and_then(|text| {
            (text.len() <= 256 * 1024
                && !text
                    .chars()
                    .any(|character| character.is_control() && !matches!(character, '\n' | '\t')))
            .then_some(text)
        });
        if let Some(outcome) = self.run_outcome.as_mut() {
            outcome.message = outcome.message.take().and_then(|message| {
                (message.len() <= 8 * 1024
                    && !message.chars().any(|character| {
                        character.is_control() && !matches!(character, '\n' | '\t')
                    }))
                .then_some(message)
            });
        }
        self.tool_output = self
            .tool_output
            .and_then(|details| details.into_validated().ok())
            .filter(|details| !details.is_empty());
        // The timing window is only meaningful beside a retained tool result.
        self.tool_started_unix_ms = None;
        self.tool_finished_unix_ms = None;
        let mut encoded_metadata_bytes = 0usize;
        let mut sanitized_extension_metadata = BTreeMap::new();
        for (namespace, entry_metadata) in std::mem::take(&mut self.extension_metadata)
            .into_iter()
            .take(MAX_EXTENSION_ENTRY_METADATA_NAMESPACES)
        {
            let Some(entry_metadata) = entry_metadata.sanitized(&namespace) else {
                continue;
            };
            let encoded = match serde_json::to_vec(&entry_metadata.value) {
                Ok(encoded) => encoded,
                Err(_) => continue,
            };
            let next = encoded_metadata_bytes.saturating_add(encoded.len());
            if next > MAX_EXTENSION_ENTRY_METADATA_BYTES {
                continue;
            }
            encoded_metadata_bytes = next;
            sanitized_extension_metadata.insert(namespace, entry_metadata);
        }
        self.extension_metadata = sanitized_extension_metadata;
        (self.native_steering.is_some()
            || self.prompt_model.is_some()
            || self.prompt_model_source.is_some()
            || self.prompt_color.is_some()
            || self.display_text.is_some()
            || self.run_outcome.is_some()
            || self.tool_output.is_some()
            || self.local_synthetic_assistant
            || !self.extension_metadata.is_empty())
        .then_some(self)
    }

    /// Returns only extension metadata explicitly marked public by its owner.
    ///
    /// The returned map is a detached projection; private metadata remains
    /// durable but is deliberately omitted.
    pub fn public_extension_metadata(&self) -> BTreeMap<String, ExtensionEntryMetadata> {
        self.extension_metadata
            .iter()
            .filter(|(_, value)| value.public)
            .map(|(namespace, value)| (namespace.clone(), value.clone()))
            .collect()
    }
}

/// A parent-linked session entry. `parent: None` marks a root entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    /// This entry's ID.
    pub id: EntryId,
    /// The entry this one follows; `None` for a conversation root.
    pub parent: Option<EntryId>,
    /// Stable presentation metadata. Legacy sessions omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<EntryMetadata>,
    /// Wall-clock creation time for protocol/session presentation. Legacy
    /// entries omit it because historical append times cannot be recovered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_unix_ms: Option<u64>,
    /// The payload.
    pub value: EntryValue,
}

/// Durable bitmap checkpoint; source text is retained for later re-compaction.
/// The lead-in and frames enter vision-model context, not this source text.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SnapcompactCheckpoint {
    /// Source of the frames, including any earlier bitmap checkpoint.
    pub source_text: String,
    /// Inline PNGs using the existing base64 media codec.
    pub frames: Vec<octet_ai::Media>,
}

/// Payload of a session entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryValue {
    /// A complete conversation message (user, assistant, or tool results
    /// carried as a user message).
    Message(Message),
    /// A manual compaction record: everything on the parent chain older than
    /// `first_kept` is replaced by `summary` during context reconstruction.
    Compaction {
        /// Caller-provided summary of the replaced history.
        summary: String,
        /// Optional deterministic bitmap replacement for this summary.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        snapcompact: Option<SnapcompactCheckpoint>,
        /// The oldest entry still kept in full-fidelity context.
        first_kept: EntryId,
        /// Snapshots of active skills at the compaction boundary.
        #[serde(default)]
        active_skills: Vec<SkillActivatedSnapshot>,
        /// Snapshots of lazy resource reads active at the compaction boundary.
        #[serde(default)]
        skill_resources: Vec<SkillResourceSnapshot>,
        /// Pi-compatible cumulative read/modified file lists for handoff.
        #[serde(default)]
        details: crate::compaction::CompactionDetails,
    },
    /// Opaque complete Responses output attached beside its canonical assistant
    /// message. It is not model-visible canonical context.
    ResponsesTurn {
        /// Assistant message entry produced by this provider turn.
        assistant: EntryId,
        /// Exact endpoint that produced the opaque output.
        endpoint: EndpointId,
        /// Exact model that produced the opaque output.
        model: ModelId,
        /// Complete terminal output, preserved without normalization.
        output: octet_ai::ResponsesOutput,
    },
    /// Opaque output from native `POST /responses/compact`.
    ///
    /// This is a provider checkpoint sidecar, not canonical model-visible
    /// context. The marker is appended directly after `covered_through`, so it
    /// covers the selected active-branch replay root through that entry and can
    /// never affect a sibling branch after checkout.
    ResponsesCompaction {
        /// Exact endpoint that produced the compact output.
        endpoint: EndpointId,
        /// Exact model that produced the compact output.
        model: ModelId,
        /// Active-branch head included at the end of the compacted input.
        covered_through: EntryId,
        /// Complete, unpruned compact output used as the next replay base.
        output: octet_ai::ResponsesOutput,
    },
    /// Durable native steering intent/outcome. Never canonical user input until
    /// the matching application is appended after its completed response prefix.
    ResponsesSteering {
        /// Exact endpoint owning the live connection.
        endpoint: EndpointId,
        /// Exact model owning the live connection.
        model: ModelId,
        /// Run/response operation identifier; local ids are operation-local.
        operation: String,
        /// Transport preparation receipt, durable before dispatch.
        local_id: u64,
        /// User input on the initial intent record only.
        input: Option<UserMessage>,
        /// Observed transport outcome on subsequent records.
        state: Option<octet_ai::SteeringUpdate>,
        /// Assistant whose committed usage settles this materialized input.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        completed: Option<EntryId>,
    },
    /// Host-authoritative, route-affine Responses reasoning cache state.
    /// Baselines never come from opaque provider output.
    ResponsesReasoning {
        /// Exact endpoint owning this cache prefix.
        endpoint: EndpointId,
        /// Exact model owning this cache prefix.
        model: ModelId,
        /// Pinned request-level reasoning for this replay window.
        baseline: octet_ai::ReasoningConfig,
        /// Ordered effective-reasoning change; None establishes a new baseline
        /// and supersedes earlier updates without discarding conversation items.
        update: Option<octet_ai::ResponsesConfigurationUpdate>,
    },
    /// A configuration marker (not part of model-visible context).
    Config {
        /// Model selection recorded at this point, if any.
        model: Option<String>,
        /// Reasoning setting recorded at this point, if any.
        reasoning: Option<String>,
        /// Reasoning execution mode recorded at this point, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reasoning_mode: Option<String>,
    },
    /// A named prompt template was expanded before a user prompt. Keeping this
    /// as a non-model-visible append-only marker makes template provenance
    /// inspectable without changing the submitted text or replay semantics.
    PromptTemplateSelected {
        /// Stable template name used by the command or CLI option.
        name: String,
        /// SHA-256 of the complete template file, including frontmatter.
        content_hash: String,
    },
    /// A skill was explicitly activated.
    SkillActivated {
        /// The skill metadata descriptor.
        descriptor: crate::skills::SkillDescriptor,
        /// Deterministic content hash of SKILL.md.
        instructions_hash: crate::skills::ContentHash,
        /// Raw core instructions content.
        instructions: String,
    },
    /// A resource associated with an active skill was loaded.
    SkillResourceRead {
        /// The unique ID of the activation that read this resource.
        activation_id: crate::skills::SkillActivationId,
        /// The unique ID of the skill.
        skill_id: crate::skills::SkillId,
        /// Relative path of the resource (e.g. "references/semver.md").
        resource_path: String,
        /// The optional start line.
        start_line: Option<u32>,
        /// The optional line count.
        line_count: Option<u32>,
        /// Content hash of the retrieved text.
        content_hash: crate::skills::ContentHash,
        /// Text content of the resource range.
        content: String,
    },
    /// A skill was explicitly deactivated.
    SkillDeactivated {
        /// The activation identifier that is being deactivated.
        activation_id: crate::skills::SkillActivationId,
        /// The unique ID of the skill.
        skill_id: crate::skills::SkillId,
    },
}

/// Snapshot of an activated skill, used in compaction records.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillActivatedSnapshot {
    /// The activation ID (which corresponds to the EntryId of the activation event).
    pub activation_id: crate::skills::SkillActivationId,
    /// The skill metadata descriptor.
    pub descriptor: crate::skills::SkillDescriptor,
    /// Deterministic content hash of the skill instructions.
    pub instructions_hash: crate::skills::ContentHash,
    /// Raw core instructions content.
    pub instructions: String,
}

/// Snapshot of a loaded skill resource, used in compaction records.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillResourceSnapshot {
    /// The activation ID.
    pub activation_id: crate::skills::SkillActivationId,
    /// The skill ID.
    pub skill_id: crate::skills::SkillId,
    /// The resource path.
    pub resource_path: String,
    /// Optional start line.
    pub start_line: Option<u32>,
    /// Optional line count.
    pub line_count: Option<u32>,
    /// Content hash of the resource text.
    pub content_hash: crate::skills::ContentHash,
    /// Raw text content.
    pub content: String,
}

/// One line of the session JSONL file.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionRecord {
    /// Replaceable auxiliary state for an unresolved tool call. Never context.
    ToolInvocation {
        /// Host-derived assistant entry and source position, not provider call ID.
        scope: InvocationScope,
        /// Bounded memos and the latest partial-output snapshot.
        record: InvocationRecord,
    },
    /// An appended entry.
    Entry(Box<Entry>),
    /// A durable head update: the current head entry ID and cumulative cost.
    Head {
        /// The entry the head now points at.
        id: EntryId,
        /// Cumulative whole-microdollar session cost.
        #[serde(default)]
        total_cost_microdollars: u64,
        /// Picodollar remainder paired with `total_cost_microdollars`.
        #[serde(default)]
        total_cost_picodollars_remainder: u32,
    },
    /// A durable checkout before the first entry. Subsequent appends create a
    /// new root branch while preserving every existing root and descendant.
    RootHead {
        /// Cumulative whole-microdollar session cost.
        #[serde(default)]
        total_cost_microdollars: u64,
        /// Picodollar remainder paired with `total_cost_microdollars`.
        #[serde(default)]
        total_cost_picodollars_remainder: u32,
    },
    /// A completed prompt's durable restore point. Checkpoints do not alter
    /// the active head or model-visible context.
    Checkpoint {
        /// User-message entry that began the completed interaction.
        prompt: EntryId,
        /// Exact completed head to restore.
        head: EntryId,
        /// Provider-reported cumulative usage for this interaction.
        #[serde(default)]
        usage: Option<Usage>,
        /// Cost accrued by this interaction; `None` for legacy records.
        #[serde(default)]
        run_cost_microdollars: Option<u64>,
    },
    /// An accepted attempt with unknown usage. Session-global, not branch state.
    UsageUncertainty {
        /// Bounded host-selected identifiers only; no invented usage or cost.
        record: UsageUncertaintyRecord,
        /// Optional worst-case admission exposure; absent in legacy records.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bound: Option<UsageUncertaintyBound>,
    },
    /// Cache-warm lifecycle; independent from provider usage and context.
    CacheWarm {
        /// Sanitized status record.
        record: CacheWarmRecord,
    },
    /// Usage for one assistant turn or compaction operation. This does not
    /// alter the active head or model-visible context.
    Usage {
        /// The durable usage record.
        record: UsageRecord,
    },
    /// Replaceable suspended/effect-pending deferred-run state. This never
    /// changes the active head or model-visible context, and the last record
    /// for one operation is authoritative on replay.
    DeferredRun {
        /// The durable deferred-run record, keyed by operation id.
        record: DeferredRunRecord,
    },
    /// Replaceable label for one existing entry. JSONL entries are immutable,
    /// so this record never rewrites history: the last record for one entry is
    /// authoritative on replay and an empty `label` clears it. Labels never
    /// change the active head, branch ancestry, or model-visible context.
    EntryLabel {
        /// The labeled entry. Unknown IDs are refused on append and on replay.
        entry_id: EntryId,
        /// Bounded, control-character-free label text; empty clears.
        label: String,
    },
}

/// Borrowed serialization view used by append/checkout. Keeping the entry
/// borrowed avoids cloning large message payloads solely to write JSONL.
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SessionRecordRef<'a> {
    UsageUncertainty {
        record: &'a UsageUncertaintyRecord,
        #[serde(skip_serializing_if = "Option::is_none")]
        bound: Option<UsageUncertaintyBound>,
    },
    Entry(&'a Entry),
    Head {
        id: &'a EntryId,
        total_cost_microdollars: &'a u64,
        total_cost_picodollars_remainder: &'a u32,
    },
    RootHead {
        total_cost_microdollars: &'a u64,
        total_cost_picodollars_remainder: &'a u32,
    },
    Checkpoint {
        prompt: &'a EntryId,
        head: &'a EntryId,
        usage: &'a Option<Usage>,
        run_cost_microdollars: &'a Option<u64>,
    },
    Usage {
        record: &'a UsageRecord,
    },
    CacheWarm {
        record: &'a CacheWarmRecord,
    },
    EntryLabel {
        entry_id: &'a EntryId,
        label: &'a str,
    },
}

fn write_json_line<T: Serialize>(buf: &mut Vec<u8>, record: &T) -> Result<(), SessionError> {
    serde_json::to_writer(&mut *buf, record).map_err(|e| SessionError::Serde(e.to_string()))?;
    buf.push(b'\n');
    Ok(())
}

pub(crate) const MAX_SESSION_FILE_BYTES: u64 = 256 * 1024 * 1024;
pub(crate) const MAX_SESSION_RECORDS: usize = 1_000_000;

/// Immutable-entry index shared by replay and live appends. Parent links and
/// call IDs are indexed once, rather than rewalking a batch for every result.
#[derive(Default)]
struct InvocationEntryIndex {
    nearest_assistant: Vec<Option<usize>>,
    calls: HashMap<usize, HashMap<octet_ai::ToolCallId, Vec<usize>>>,
    results: HashMap<InvocationScope, (usize, usize)>,
}

impl InvocationEntryIndex {
    fn result_scopes(
        &self,
        entry: &Entry,
        entries: &[Entry],
        index: &HashMap<EntryId, usize>,
    ) -> Vec<(InvocationScope, usize)> {
        let EntryValue::Message(Message::User(user)) = &entry.value else {
            return Vec::new();
        };
        let Some(assistant) = entry
            .parent
            .as_ref()
            .and_then(|parent| self.nearest_assistant[index[parent]])
        else {
            return Vec::new();
        };
        let Some(calls) = self.calls.get(&assistant) else {
            return Vec::new();
        };
        let mut scopes = Vec::new();
        for (part_index, part) in user.content.iter().enumerate() {
            if let UserPart::ToolResult(result) = part {
                if let Some(positions) = calls.get(&result.tool_call_id) {
                    for position in positions {
                        if let Ok(scope) = InvocationScope::new(
                            entries[assistant].id.0.clone(),
                            position.to_string(),
                        ) {
                            scopes.push((scope, part_index));
                        }
                    }
                }
            }
        }
        scopes
    }

    fn record(
        &mut self,
        entry: &Entry,
        index: &HashMap<EntryId, usize>,
        results: &[(InvocationScope, usize)],
    ) {
        let position = self.nearest_assistant.len();
        let nearest = if let EntryValue::Message(Message::Assistant(assistant)) = &entry.value {
            let mut calls: HashMap<_, Vec<usize>> = HashMap::new();
            for (call_index, call) in assistant
                .content
                .iter()
                .filter_map(|part| match part {
                    octet_ai::AssistantPart::ToolCall(call) => Some(call),
                    _ => None,
                })
                .enumerate()
            {
                calls.entry(call.id.clone()).or_default().push(call_index);
            }
            if !calls.is_empty() {
                self.calls.insert(position, calls);
            }
            Some(position)
        } else {
            entry
                .parent
                .as_ref()
                .and_then(|parent| self.nearest_assistant[index[parent]])
        };
        self.nearest_assistant.push(nearest);
        for (scope, part) in results {
            // Reconciliation may copy an existing immutable result onto a new
            // branch; the first durable result remains the source of truth.
            self.results
                .entry(scope.clone())
                .or_insert((position, *part));
        }
    }
}

/// Build DFS entry/exit times for the parent-linked entry forest in linear
/// time. Parent records are guaranteed to precede children during replay, but
/// branches may be interleaved in insertion order, so numeric entry positions
/// alone cannot answer ancestry queries.
fn entry_ancestry_intervals(
    entries: &[Entry],
    index: &HashMap<EntryId, usize>,
) -> (Vec<u32>, Vec<u32>) {
    const NONE: u32 = u32::MAX;

    let mut first_child = vec![NONE; entries.len()];
    let mut next_sibling = vec![NONE; entries.len()];
    for (child, entry) in entries.iter().enumerate() {
        let Some(parent) = entry.parent.as_ref() else {
            continue;
        };
        let parent = *index
            .get(parent)
            .expect("replay validates every parent before building ancestry");
        let child = u32::try_from(child).expect("session record limit fits u32");
        next_sibling[child as usize] = first_child[parent];
        first_child[parent] = child;
    }

    let mut entered = vec![0u32; entries.len()];
    let mut exited = vec![0u32; entries.len()];
    let mut clock = 0u32;
    let mut stack = Vec::<(u32, bool)>::new();
    for (root, entry) in entries.iter().enumerate() {
        if entry.parent.is_some() {
            continue;
        }
        stack.push((
            u32::try_from(root).expect("session record limit fits u32"),
            false,
        ));
        while let Some((node, leaving)) = stack.pop() {
            let position = node as usize;
            if leaving {
                exited[position] = clock;
                continue;
            }
            entered[position] = clock;
            clock = clock.saturating_add(1);
            stack.push((node, true));
            let mut child = first_child[position];
            while child != NONE {
                stack.push((child, false));
                child = next_sibling[child as usize];
            }
        }
    }
    (entered, exited)
}

pub(crate) fn now_unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// Session errors.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// Filesystem failure.
    #[error("session io error: {0}")]
    Io(#[from] std::io::Error),
    /// A record failed to serialize.
    #[error("session serialization error: {0}")]
    Serde(String),
    /// A completed (non-final) record failed to parse or violated an invariant.
    #[error("corrupt session record at line {line}: {message}")]
    Corrupt {
        /// 1-based line number of the offending record.
        line: usize,
        /// What was wrong with it.
        message: String,
    },
    /// A configured session parsing/resource bound was exceeded.
    #[error("session limit exceeded: {0}")]
    Limit(String),
    /// Another session handle changed the file after this one was opened.
    #[error("session was modified by another process; reopen it before writing")]
    ConcurrentModification,
    /// An operation referenced an entry ID that does not exist.
    #[error("unknown session entry: {0:?}")]
    UnknownEntry(EntryId),
    /// A compaction/checkpoint entry is not an ancestor of the current head.
    #[error("entry {0:?} is not an ancestor of the current head")]
    NotAncestor(EntryId),
    /// An operation requires a non-empty session.
    #[error("the session has no current head")]
    EmptySession,
    /// No durable checkpoint exists for the supplied prompt entry.
    #[error("no checkpoint exists for prompt entry {0:?}")]
    UnknownCheckpoint(EntryId),
    /// A Responses assistant sidecar belongs to another endpoint/model route.
    #[error(
        "Responses replay route mismatch for assistant {assistant:?}: expected {expected_endpoint}/{expected_model}, found {actual_endpoint}/{actual_model}"
    )]
    ResponsesRouteMismatch {
        /// Assistant entry whose sidecar was inspected.
        assistant: EntryId,
        /// Endpoint selected for the new request.
        expected_endpoint: String,
        /// Model selected for the new request.
        expected_model: String,
        /// Endpoint recorded on the sidecar.
        actual_endpoint: String,
        /// Model recorded on the sidecar.
        actual_model: String,
    },
    /// A Responses sidecar was not appended at its required branch boundary.
    #[error("invalid Responses sidecar: {0}")]
    InvalidResponsesSidecar(String),
}

/// One route's immutable replay projection. Ordinary appends inspect only the
/// suffix below `head`; checkout, compaction and route changes rebuild it.
struct ResponsesReplayCache {
    endpoint: EndpointId,
    model: ModelId,
    head: Option<EntryId>,
    items: Option<Arc<Vec<octet_ai::responses::ResponsesReplayItem>>>,
}

/// An append-only JSONL session file.
///
/// Entries form a tree via parent links; the durable head selects the active
/// branch. [`Session::checkout`] moves the head to any existing entry, and
/// subsequent appends fork a new branch from there — earlier branches are
/// preserved verbatim in the file.
pub struct Session {
    path: PathBuf,
    file: File,
    // Shared only with host-issued invocation handles. Every append uses the
    // same descriptor-bound mutation line and stale-length fence.
    writer: Arc<SessionWriter>,
    invocations: Arc<DurableInvocationStore>,
    invocation_entries: InvocationEntryIndex,
    entries: Vec<Entry>,
    index: HashMap<EntryId, usize>,
    head: Option<EntryId>,
    /// Monotonically-increasing counter for the next entry ID. Derived from
    /// the maximum ID replayed during [`Session::open`] (or 0 for a new
    /// session) and incremented on every [`Session::append`]. Using an
    /// explicit counter instead of `entries.len()` avoids collisions when the
    /// in-memory entry vector diverges from the on-disk state (e.g. after an
    /// unclean reopen).
    next_id: u64,
    /// Cached model-visible context. Message appends update it in place;
    /// checkout and compaction invalidate it because they can change the
    /// active branch or summary boundary.
    context_cache: RefCell<Option<Vec<Message>>>,
    responses_replay_cache: RefCell<Option<ResponsesReplayCache>>,
    #[cfg(test)]
    responses_replay_work: std::cell::Cell<(usize, usize)>,
    /// Cumulative whole-microdollar session cost.
    /// Persisted in Head/Usage records and restored on open.
    total_cost_microdollars: u64,
    /// Picodollar remainder carried across provider operations.
    total_cost_picodollars_remainder: u32,
    /// Durable completed-prompt restore points in append order.
    checkpoints: Vec<Checkpoint>,
    /// Usage records for every completed provider operation, in append order.
    usage_records: Vec<UsageRecord>,
    /// Session-global exposure; checkout and compaction never clear it.
    usage_uncertainty_records: Vec<UsageUncertaintyRecord>,
    /// Admission bounds in the same append order; None fails closed.
    usage_uncertainty_bounds: Vec<Option<UsageUncertaintyBound>>,
    /// Separate cache-warm lifecycle, session-global and never model-visible.
    cache_warm_records: Vec<CacheWarmRecord>,
    /// Replaceable parked deferred-run leaves, keyed by operation id. The
    /// store owns its own descriptor-bound append line so a durable change is
    /// one synced record.
    deferred_runs: Arc<DeferredRunStore>,
    /// Replaceable durable entry labels. Keyed by an existing entry ID, so the
    /// map can never grow past the session's entry count (one label per entry).
    entry_labels: BTreeMap<EntryId, String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // A retained callback cannot keep writing after the owning session
        // closes. Durable pending state remains for a newly opened session;
        // a parked deferred run is replayed by the next owning session.
        self.invocations.close();
        self.deferred_runs.close();
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("path", &self.path)
            .field("entries", &self.entries.len())
            .field("head", &self.head)
            .finish()
    }
}

pub use context::ActiveSkillState;
// Only Windows session creation maps secure-file errors in `store`.
#[cfg(windows)]
use journal::partial_journal_file_error;
pub use journal::{
    AssistantFrameJournal, MAX_PARTIAL_FRAME_JOURNAL_BYTES, MAX_PARTIAL_FRAME_JOURNAL_FRAMES,
};

mod context;
mod entries;
mod journal;
mod replay;
mod store;
mod usage;

#[cfg(test)]
mod tests;
