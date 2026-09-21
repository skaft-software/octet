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
    fn into_value(&self) -> serde_json::Value {
        serde_json::json!({ "entry_type": self.entry_type, "data": self.data })
    }

    /// Decodes a namespace value envelope written by [`Self::into_value`].
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
    let mut nodes = 0usize;
    if !valid_extension_metadata_value(&entry.data, 0, &mut nodes) {
        return None;
    }
    let encoded = serde_json::to_vec(&entry.into_value()).ok()?;
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
        (self.prompt_model.is_some()
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

fn result_invocation_scopes<'a>(
    value: &EntryValue,
    mut cursor: Option<&'a EntryId>,
    entries: &'a [Entry],
    index: &HashMap<EntryId, usize>,
) -> Vec<InvocationScope> {
    let EntryValue::Message(Message::User(user)) = value else {
        return Vec::new();
    };
    let results = user
        .content
        .iter()
        .filter_map(|part| match part {
            UserPart::ToolResult(result) => Some(&result.tool_call_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    if results.is_empty() {
        return Vec::new();
    }
    while let Some(id) = cursor {
        let Some(entry) = index.get(id).and_then(|position| entries.get(*position)) else {
            break;
        };
        if let EntryValue::Message(Message::Assistant(assistant)) = &entry.value {
            return assistant
                .content
                .iter()
                .filter_map(|part| match part {
                    octet_ai::AssistantPart::ToolCall(call) => Some(call),
                    _ => None,
                })
                .enumerate()
                .filter(|(_, call)| results.contains(&&call.id))
                .filter_map(|(position, _)| {
                    InvocationScope::new(entry.id.0.clone(), position.to_string()).ok()
                })
                .collect();
        }
        cursor = entry.parent.as_ref();
    }
    Vec::new()
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

impl Session {
    /// Creates a new empty session file on disk. Fails if the file exists.
    pub fn create(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        let path = path.into();
        let mut options = OpenOptions::new();
        options.create_new(true).read(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&path)?;
        Self::create_with_file(path, file)
    }

    /// Create an empty session through a caller-supplied read/append file
    /// descriptor that was opened with exclusive-create semantics.
    ///
    /// The descriptor must have been opened with exclusive-create semantics
    /// and must still be empty. This lets a host securely create the file
    /// relative to a validated parent directory before handing it here.
    pub fn create_with_file(path: impl Into<PathBuf>, file: File) -> Result<Self, SessionError> {
        if !file.metadata()?.file_type().is_file() {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session descriptor is not a regular file",
            )));
        }
        if file.metadata()?.len() != 0 {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "new session descriptor is not empty",
            )));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        let writer = Arc::new(SessionWriter::new(file.try_clone()?, 0, 0, true));
        let invocations = Arc::new(DurableInvocationStore::with_journal(Arc::clone(&writer)));
        let deferred_runs = Arc::new(DeferredRunStore::with_journal(Arc::clone(&writer)));
        Ok(Self {
            path: path.into(),
            file,
            writer,
            invocations,
            deferred_runs,
            entries: Vec::new(),
            index: HashMap::new(),
            head: None,
            next_id: 1,
            context_cache: RefCell::new(None),
            responses_replay_cache: RefCell::new(None),
            #[cfg(test)]
            responses_replay_work: std::cell::Cell::new((0, 0)),
            total_cost_microdollars: 0,
            total_cost_picodollars_remainder: 0,
            checkpoints: Vec::new(),
            usage_records: Vec::new(),
            usage_uncertainty_records: Vec::new(),
            entry_labels: BTreeMap::new(),
        })
    }

    /// Opens an existing session, replaying all records and restoring the
    /// head from the last recorded head.
    ///
    /// A torn *final* line (an interrupted write during an unclean exit) is
    /// dropped — and physically truncated from the file, so subsequent
    /// appends start on a fresh line instead of merging into the torn bytes.
    /// A *valid* final record that merely lost its trailing newline is kept,
    /// and the missing newline is written to complete it. Any malformed
    /// record *before* the final line is corruption and is rejected, as are
    /// duplicate entry IDs and references to unknown entries.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        Self::open_impl(path.into(), true)
    }

    /// Open an existing session through a caller-supplied read/append file
    /// descriptor.
    ///
    /// This lets a host bind path authorization and opening into one
    /// descriptor-relative operation. The descriptor must refer to a regular
    /// file and permit reads, locking, permission repair, and durable appends.
    pub fn open_with_file(path: impl Into<PathBuf>, file: File) -> Result<Self, SessionError> {
        Self::open_file_impl_with_limits(
            path.into(),
            file,
            true,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    /// Inspect an existing session without repairing, truncating, appending,
    /// or otherwise changing its bytes. The returned snapshot is intended for
    /// listing and reporting only; mutation methods fail because its file
    /// descriptor is read-only.
    pub fn open_read_only(path: impl Into<PathBuf>) -> Result<Self, SessionError> {
        Self::open_impl(path.into(), false)
    }

    /// Inspect an existing session through a caller-supplied read-only file
    /// descriptor without repairing or mutating its bytes.
    ///
    /// This is the descriptor-bound counterpart to [`Self::open_read_only`].
    /// The descriptor must refer to a regular file and permit reads.
    pub fn open_read_only_with_file(
        path: impl Into<PathBuf>,
        file: File,
    ) -> Result<Self, SessionError> {
        Self::open_file_impl_with_limits(
            path.into(),
            file,
            false,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    fn open_impl(path: PathBuf, recover_tail: bool) -> Result<Self, SessionError> {
        Self::open_impl_with_limits(
            path,
            recover_tail,
            MAX_SESSION_FILE_BYTES,
            MAX_SESSION_RECORDS,
        )
    }

    fn open_impl_with_limits(
        path: PathBuf,
        recover_tail: bool,
        max_file_bytes: u64,
        max_records: usize,
    ) -> Result<Self, SessionError> {
        let mut options = OpenOptions::new();
        options.read(true);
        if recover_tail {
            // Windows tail repair needs FILE_WRITE_DATA in addition to append
            // access so `set_len` can remove a torn final record.
            options.write(true).append(true);
        }
        let file = options.open(&path)?;
        Self::open_file_impl_with_limits(path, file, recover_tail, max_file_bytes, max_records)
    }

    fn open_file_impl_with_limits(
        path: PathBuf,
        mut file: File,
        recover_tail: bool,
        max_file_bytes: u64,
        max_records: usize,
    ) -> Result<Self, SessionError> {
        if !file.metadata()?.file_type().is_file() {
            return Err(SessionError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "session descriptor is not a regular file",
            )));
        }
        // Replay and tail handling must observe one stable snapshot. Without
        // this lock, a writer could append after the read but before the
        // observed length is captured, pairing stale IDs with a newer length.
        if recover_tail {
            file.lock_exclusive()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = file.metadata()?.permissions().mode() & 0o777;
                if mode != 0o600 {
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
            }
        } else {
            FileExt::lock_shared(&file)?;
        }
        let file_len = file.metadata()?.len();
        if file_len > max_file_bytes {
            return Err(SessionError::Limit(format!(
                "{} is {file_len} bytes (limit {max_file_bytes})",
                path.display()
            )));
        }
        let mut reader = file.try_clone()?;
        reader.seek(std::io::SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(1024 * 1024, reader);

        let mut entries: Vec<Entry> = Vec::new();
        let mut index: HashMap<EntryId, usize> = HashMap::new();
        let mut head: Option<EntryId> = None;
        let mut max_id: u64 = 0;
        let mut total_cost_microdollars: u64 = 0;
        let mut total_cost_picodollars_remainder: u32 = 0;
        let mut checkpoints: Vec<Checkpoint> = Vec::new();
        let mut checkpoint_lines: Vec<usize> = Vec::new();
        let mut usage_records: Vec<UsageRecord> = Vec::new();
        let mut usage_uncertainty_records = Vec::new();
        let restored_invocations = DurableInvocationStore::new();
        let restored_deferred_runs = DeferredRunStore::new();
        let mut entry_labels: BTreeMap<EntryId, String> = BTreeMap::new();

        // Byte offset of the end of the last accepted record, so a torn tail
        // can be truncated away below. Only one physical line is buffered at a
        // time; parsed entries remain the authoritative in-memory replay.
        let mut valid_end = 0u64;
        let mut observed_end = 0u64;
        let mut final_record_had_newline = true;
        let mut line_bytes = Vec::new();
        let mut line_no = 0usize;
        let mut persisted_records = 0usize;
        loop {
            line_bytes.clear();
            let read_limit = max_file_bytes
                .saturating_sub(observed_end)
                .saturating_add(1);
            let bytes_read = reader
                .by_ref()
                .take(read_limit)
                .read_until(b'\n', &mut line_bytes)?;
            if bytes_read == 0 {
                break;
            }
            observed_end = observed_end
                .checked_add(u64::try_from(bytes_read).map_err(|_| {
                    SessionError::Limit("session read length does not fit u64".to_owned())
                })?)
                .ok_or_else(|| SessionError::Limit("session read length overflow".to_owned()))?;
            if observed_end > max_file_bytes {
                return Err(SessionError::Limit(format!(
                    "session exceeds the {max_file_bytes}-byte limit while being read"
                )));
            }
            line_no += 1;
            if line_no > max_records {
                return Err(SessionError::Limit(format!(
                    "session has more than {max_records} records"
                )));
            }
            let has_newline = line_bytes.last() == Some(&b'\n');
            let line_bytes = if has_newline {
                &line_bytes[..line_bytes.len() - 1]
            } else {
                line_bytes.as_slice()
            };
            let line = match std::str::from_utf8(line_bytes) {
                Ok(line) => line,
                // A crash may tear the final write in the middle of a UTF-8
                // scalar. Newline-terminated records remain strict UTF-8.
                Err(_) if !has_newline => break,
                Err(error) => {
                    return Err(SessionError::Corrupt {
                        line: line_no,
                        message: format!("invalid UTF-8: {error}"),
                    })
                }
            };
            let record: SessionRecord = match serde_json::from_str(line) {
                Ok(record) => record,
                // `read_until` returns a non-newline-terminated segment only
                // at EOF, so malformed bytes are recoverable only here.
                Err(_) if !has_newline => break,
                Err(error) => {
                    return Err(SessionError::Corrupt {
                        line: line_no,
                        message: error.to_string(),
                    })
                }
            };
            valid_end = observed_end;
            final_record_had_newline = has_newline;
            persisted_records += 1;
            match record {
                SessionRecord::ToolInvocation { scope, record } => {
                    restored_invocations
                        .restore(scope, record)
                        .map_err(|error| SessionError::Corrupt {
                            line: line_no,
                            message: error.to_string(),
                        })?;
                }
                SessionRecord::EntryLabel { entry_id, label } => {
                    if !index.contains_key(&entry_id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!(
                                "entry label references unknown entry {:?}",
                                entry_id.0
                            ),
                        });
                    }
                    if !valid_entry_label(&label) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "entry label exceeds its bound or contains control characters"
                                .to_owned(),
                        });
                    }
                    if label.is_empty() {
                        entry_labels.remove(&entry_id);
                    } else {
                        entry_labels.insert(entry_id, label);
                    }
                }
                SessionRecord::DeferredRun { record } => {
                    restored_deferred_runs
                        .restore(record)
                        .map_err(|error| SessionError::Corrupt {
                            line: line_no,
                            message: error.to_string(),
                        })?;
                }
                SessionRecord::Entry(entry) => {
                    if index.contains_key(&entry.id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!("duplicate entry id {:?}", entry.id.0),
                        });
                    }
                    // Track the maximum numeric ID so we can safely resume
                    // appending even if the in-memory vector diverges from
                    // disk state.
                    if let Ok(n) = entry.id.0.parse::<u64>() {
                        max_id = max_id.max(n);
                    }
                    if let Some(parent) = &entry.parent {
                        if !index.contains_key(parent) {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message: format!(
                                    "entry {:?} references unknown parent {:?}",
                                    entry.id.0, parent.0
                                ),
                            });
                        }
                    }
                    match &entry.value {
                        EntryValue::Compaction { first_kept, .. } => {
                            if !index.contains_key(first_kept) {
                                return Err(SessionError::Corrupt {
                                    line: line_no,
                                    message: format!(
                                        "compaction {:?} references unknown first_kept {:?}",
                                        entry.id.0, first_kept.0
                                    ),
                                });
                            }
                        }
                        EntryValue::ResponsesTurn {
                            assistant,
                            model,
                            output,
                            ..
                        } => {
                            let valid_assistant = index
                                .get(assistant)
                                .and_then(|position| entries.get(*position))
                                .is_some_and(|candidate| {
                                    matches!(
                                        &candidate.value,
                                        EntryValue::Message(Message::Assistant(message))
                                            if message.protocol == octet_ai::Protocol::OpenAiResponses
                                                && &message.model == model
                                    )
                                });
                            if !valid_assistant
                                || entry.parent.as_ref() != Some(assistant)
                                || output.is_empty()
                            {
                                return Err(SessionError::Corrupt {
                                    line: line_no,
                                    message: format!(
                                        "Responses turn {:?} is not a direct sidecar of assistant {:?}",
                                        entry.id.0, assistant.0
                                    ),
                                });
                            }
                        }
                        EntryValue::ResponsesCompaction {
                            covered_through,
                            output,
                            ..
                        } if !index.contains_key(covered_through)
                            || entry.parent.as_ref() != Some(covered_through)
                            || !output.has_valid_compaction() =>
                        {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message: format!(
                                    "Responses compaction {:?} is not a direct checkpoint of {:?}",
                                    entry.id.0, covered_through.0
                                ),
                            });
                        }
                        _ => {}
                    }
                    for scope in result_invocation_scopes(
                        &entry.value,
                        entry.parent.as_ref(),
                        &entries,
                        &index,
                    ) {
                        restored_invocations.restore_result(&scope);
                    }
                    index.insert(entry.id.clone(), entries.len());
                    entries.push(*entry);
                }
                SessionRecord::Head {
                    id,
                    total_cost_microdollars: cost,
                    total_cost_picodollars_remainder: remainder,
                } => {
                    if !index.contains_key(&id) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: format!("head references unknown entry {:?}", id.0),
                        });
                    }
                    head = Some(id);
                    total_cost_microdollars = cost;
                    total_cost_picodollars_remainder = remainder;
                }
                SessionRecord::RootHead {
                    total_cost_microdollars: cost,
                    total_cost_picodollars_remainder: remainder,
                } => {
                    head = None;
                    total_cost_microdollars = cost;
                    total_cost_picodollars_remainder = remainder;
                }
                SessionRecord::Checkpoint {
                    prompt,
                    head: checkpoint_head,
                    usage,
                    run_cost_microdollars,
                } => {
                    let prompt_is_user = index
                        .get(&prompt)
                        .and_then(|position| entries.get(*position))
                        .is_some_and(|entry| {
                            matches!(&entry.value, EntryValue::Message(Message::User(_)))
                        });
                    if !prompt_is_user || !index.contains_key(&checkpoint_head) {
                        return Err(SessionError::Corrupt {
                            line: line_no,
                            message: "checkpoint references unknown or non-user entries"
                                .to_string(),
                        });
                    }
                    checkpoint_lines.push(line_no);
                    checkpoints.push(Checkpoint {
                        prompt,
                        head: checkpoint_head,
                        usage,
                        run_cost_microdollars,
                    });
                }
                SessionRecord::UsageUncertainty { record } => {
                    record.validate().map_err(|_| SessionError::Corrupt {
                        line: line_no,
                        message: "invalid usage uncertainty identifiers".into(),
                    })?;
                    usage_uncertainty_records.push(record);
                }
                SessionRecord::Usage { record } => {
                    if let UsageRecordKind::AssistantTurn { assistant } = &record.kind {
                        let valid_assistant = index
                            .get(assistant)
                            .and_then(|position| entries.get(*position))
                            .is_some_and(|entry| {
                                matches!(&entry.value, EntryValue::Message(Message::Assistant(_)))
                            });
                        if !valid_assistant {
                            return Err(SessionError::Corrupt {
                                line: line_no,
                                message:
                                    "usage record references an unknown or non-assistant entry"
                                        .to_string(),
                            });
                        }
                    }
                    if let Some(cost) = record.session_cost_microdollars {
                        total_cost_microdollars = cost;
                        total_cost_picodollars_remainder = record
                            .session_cost_picodollars_remainder
                            .unwrap_or_default();
                    } else {
                        // Usage records written before cumulative session
                        // accounting was introduced have only their request
                        // total. Rebuild that legacy tally while replaying so
                        // reports and limits work for resumed sessions too.
                        let request_cost = record
                            .cost_microdollars
                            .or_else(|| record.cost.map(|cost| cost.total))
                            .unwrap_or_default();
                        total_cost_microdollars =
                            total_cost_microdollars.saturating_add(request_cost);
                    }
                    usage_records.push(record);
                }
            }
        }

        if !checkpoints.is_empty() {
            let (entered, exited) = entry_ancestry_intervals(&entries, &index);
            for (checkpoint, checkpoint_line) in checkpoints.iter().zip(checkpoint_lines) {
                let prompt = index[&checkpoint.prompt];
                let checkpoint_head = index[&checkpoint.head];
                let prompt_is_ancestor = entered[prompt] <= entered[checkpoint_head]
                    && exited[checkpoint_head] <= exited[prompt];
                if !prompt_is_ancestor {
                    return Err(SessionError::Corrupt {
                        line: checkpoint_line,
                        message: "checkpoint prompt is not an ancestor of its head".to_string(),
                    });
                }
            }
        }

        // Validate the ID counter before repairing any tail bytes. A
        // syntactically valid record can still be semantically corrupt, and
        // opening such a file must not normalize or otherwise mutate it before
        // returning the corruption error.
        let next_id = max_id.checked_add(1).ok_or_else(|| SessionError::Corrupt {
            line: line_no,
            message: "numeric entry ID exhausts the u64 ID space".to_owned(),
        })?;

        if recover_tail && valid_end < observed_end {
            // Torn final line: truncate it away so the next append starts on
            // a fresh line rather than merging into the torn bytes (which
            // would corrupt the record for every later reopen).
            file.set_len(valid_end)?;
        }

        if recover_tail && valid_end > 0 && !final_record_had_newline {
            // The final record parsed but lost its newline in an interrupted
            // write; complete the line so the next append cannot merge into it.
            let repaired_len = valid_end.checked_add(1).ok_or_else(|| {
                SessionError::Limit("repaired session file length overflow".to_owned())
            })?;
            if repaired_len > max_file_bytes {
                return Err(SessionError::Limit(format!(
                    "repair would grow session to {repaired_len} bytes (limit {max_file_bytes})"
                )));
            }
            file.seek(std::io::SeekFrom::End(0))?;
            file.write_all(b"\n")?;
        }
        let persisted_len = file.metadata()?.len();
        FileExt::unlock(&file)?;
        let writer = Arc::new(SessionWriter::new(
            file.try_clone()?,
            persisted_len,
            persisted_records,
            recover_tail,
        ));
        let invocations = Arc::new(restored_invocations.attach_journal(Arc::clone(&writer)));
        let deferred_runs = Arc::new(restored_deferred_runs.attach_journal(Arc::clone(&writer)));
        Ok(Self {
            path,
            file,
            writer,
            invocations,
            deferred_runs,
            entries,
            next_id,
            index,
            head,
            context_cache: RefCell::new(None),
            responses_replay_cache: RefCell::new(None),
            #[cfg(test)]
            responses_replay_work: std::cell::Cell::new((0, 0)),
            total_cost_microdollars,
            total_cost_picodollars_remainder,
            checkpoints,
            usage_records,
            usage_uncertainty_records,
            entry_labels,
        })
    }

    /// The path of the underlying JSONL file.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Clones the already-authorized session descriptor for identity-stable
    /// inspection without reopening its path.
    pub(crate) fn try_clone_file(&self) -> std::io::Result<File> {
        self.file.try_clone()
    }

    /// Returns a stable, provider-safe cache-affinity key for this session.
    ///
    /// The key is derived from the full session path, so two sessions with the
    /// same filename in different workspaces cannot share a provider cache.
    /// Reopening the same file preserves the key across process restarts.
    pub fn cache_key(&self) -> String {
        const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
        const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
        let mut hash = FNV_OFFSET;
        for byte in self.path.to_string_lossy().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        format!("octet-{hash:016x}")
    }

    /// Returns the durable authorization namespace for extension-owned
    /// resources. Unlike the compact provider cache key, this uses SHA-256 of
    /// the canonical session descriptor path so alias paths converge and the
    /// collision bound is suitable for ownership checks.
    pub fn resource_owner_key(&self) -> String {
        let identity = self
            .path
            .canonicalize()
            .or_else(|_| std::path::absolute(&self.path))
            .unwrap_or_else(|_| self.path.clone());
        let digest = Sha256::digest(identity.to_string_lossy().as_bytes());
        format!("session-{digest:x}")
    }

    /// Append bytes only if this handle still reflects the complete file.
    ///
    /// The length check and write happen under one OS advisory lock. This is
    /// deliberately per-write rather than a lifetime lock: read-only session
    /// listing can still open active sessions, while a second writer fails
    /// before it can reuse stale entry IDs.
    fn persist(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        self.writer.persist(bytes)
    }

    #[cfg(test)]
    pub(crate) fn fail_next_append(&self) {
        self.writer.fail_next_append();
    }

    /// Issues a durable memo/checkpoint capability for one unresolved call in
    /// the current assistant batch. Identity is assistant entry + source index,
    /// so a provider's reused call ID never aliases another invocation.
    pub fn tool_invocation(&self, call_index: usize) -> Result<InvocationHandle, SessionError> {
        self.invocations
            .open(self.invocation_scope(call_index)?)
            .map_err(|e| SessionError::Limit(e.to_string()))
    }

    /// The durable store for suspended/effect-pending deferred runs.
    ///
    /// The store shares this session's descriptor-bound append line, so a
    /// parked leaf, a poll admitted before provider work, and a terminal
    /// tombstone are each one synced session record. It is replaceable state:
    /// the last record for one operation is authoritative on replay, never
    /// model-visible context and never usage accounting.
    pub fn deferred_run_store(&self) -> Arc<DeferredRunStore> {
        Arc::clone(&self.deferred_runs)
    }

    /// Durable state of one suspended deferred run, if any.
    pub fn deferred_run(&self, operation_id: &str) -> Option<DeferredRunRecord> {
        self.deferred_runs.record(operation_id)
    }

    /// Every durable deferred-run record, including terminal tombstones.
    pub fn deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.deferred_runs.records()
    }

    /// Every non-terminal deferred run that may still be resumed.
    pub fn parked_deferred_runs(&self) -> Vec<DeferredRunRecord> {
        self.deferred_runs.parked_records()
    }

    /// Recovery refusals may retain existing progress without allocating a
    /// pending-effect slot for a call that will never execute.
    pub(crate) fn invocation_partial_output(
        &self,
        call_index: usize,
    ) -> Result<Option<String>, SessionError> {
        self.invocations
            .partial_output_for_scope(&self.invocation_scope(call_index)?)
            .map_err(|e| SessionError::Limit(e.to_string()))
    }

    fn invocation_scope(&self, call_index: usize) -> Result<InvocationScope, SessionError> {
        let mut cursor = self.head_ref();
        let mut completed = std::collections::HashSet::new();
        while let Some(id) = cursor {
            let entry = self.entry(id).expect("session ancestry is valid");
            match &entry.value {
                EntryValue::Message(Message::Assistant(assistant)) => {
                    let call = assistant
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            octet_ai::AssistantPart::ToolCall(call) => Some(call),
                            _ => None,
                        })
                        .nth(call_index)
                        .ok_or_else(|| SessionError::Limit("unknown tool invocation".into()))?;
                    if completed.contains(&call.id) {
                        return Err(SessionError::Limit(
                            "tool invocation already settled".into(),
                        ));
                    }
                    return InvocationScope::new(id.0.clone(), call_index.to_string())
                        .map_err(|e| SessionError::Limit(e.to_string()));
                }
                EntryValue::Message(Message::User(user)) => {
                    for part in &user.content {
                        if let UserPart::ToolResult(result) = part {
                            completed.insert(result.tool_call_id.clone());
                        }
                    }
                }
                _ => {}
            }
            cursor = entry.parent.as_ref();
        }
        Err(SessionError::Limit(
            "no pending assistant tool batch".into(),
        ))
    }

    /// Appends an entry (parented on the current head) and records the new
    /// head. Writes two JSONL records — the entry, then a head record — in a
    /// single synced write to the append-only file.
    pub fn append(&mut self, value: EntryValue) -> Result<EntryId, SessionError> {
        self.append_with_metadata(value, None)
    }

    /// Append a durable, non-model-visible terminal marker for a frontend run.
    ///
    /// The marker uses the long-standing configuration entry envelope for
    /// backwards-compatible replay. Its typed outcome lives in presentation
    /// metadata and therefore never enters provider-visible context.
    pub fn append_run_outcome(
        &mut self,
        outcome: SessionRunOutcome,
    ) -> Result<EntryId, SessionError> {
        self.append_with_metadata(
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(EntryMetadata {
                run_outcome: Some(outcome),
                ..EntryMetadata::default()
            }),
        )
    }

    /// Appends an entry with stable semantic presentation metadata.
    ///
    /// Metadata is intentionally kept outside [`EntryValue`] so model-visible
    /// conversation messages remain provider-independent and legacy readers can
    /// continue to ignore presentation details.
    pub fn append_with_metadata(
        &mut self,
        value: EntryValue,
        metadata: Option<EntryMetadata>,
    ) -> Result<EntryId, SessionError> {
        match &value {
            EntryValue::ResponsesTurn {
                assistant,
                model,
                output,
                ..
            } => {
                let valid_assistant = self.head.as_ref() == Some(assistant)
                    && self.entry(assistant).is_some_and(|entry| {
                        matches!(
                            &entry.value,
                            EntryValue::Message(Message::Assistant(message))
                                if message.protocol == octet_ai::Protocol::OpenAiResponses
                                    && &message.model == model
                        )
                    });
                if !valid_assistant || output.is_empty() {
                    return Err(SessionError::InvalidResponsesSidecar(format!(
                        "Responses turn is not a direct sidecar of a Responses assistant from model {}",
                        model.0
                    )));
                }
            }
            EntryValue::ResponsesCompaction {
                covered_through,
                output,
                ..
            } if self.head.as_ref() != Some(covered_through) || !output.has_valid_compaction() => {
                return Err(SessionError::InvalidResponsesSidecar(format!(
                    "Responses compaction is not a direct checkpoint of {:?}",
                    covered_through.0
                )));
            }
            _ => {}
        }
        let id = EntryId(format!("{:03}", self.next_id));
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| SessionError::Limit("session entry ID space is exhausted".to_owned()))?;
        let accepts_tool_output_details = matches!(
            &value,
            EntryValue::Message(Message::User(message))
                if message
                    .content
                    .iter()
                    .filter(|part| matches!(part, UserPart::ToolResult(_)))
                    .count()
                    == 1
        );
        let metadata = metadata
            .map(|mut metadata| {
                if !accepts_tool_output_details {
                    metadata.tool_output = None;
                }
                metadata
            })
            .and_then(EntryMetadata::sanitized);
        let entry = Entry {
            id: id.clone(),
            parent: self.head.clone(),
            metadata,
            timestamp_unix_ms: Some(now_unix_millis()),
            value,
        };
        let mut buf = Vec::with_capacity(256);
        write_json_line(&mut buf, &SessionRecordRef::Entry(&entry))?;
        write_json_line(
            &mut buf,
            &SessionRecordRef::Head {
                id: &id,
                total_cost_microdollars: &self.total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        // The immutable paired result doubles as the invocation tombstone.
        // Hold the store fence across its synced append so late memos cannot
        // revive state; replay performs the same cleanup from this entry.
        let settled = result_invocation_scopes(
            &entry.value,
            entry.parent.as_ref(),
            &self.entries,
            &self.index,
        );
        self.invocations
            .commit_results(&settled, || self.writer.persist(&buf))?;

        self.index.insert(id.clone(), self.entries.len());
        self.entries.push(entry);
        self.head = Some(id.clone());
        self.next_id = next_id;

        let cache = self.context_cache.get_mut();
        match &self.entries.last().expect("just appended").value {
            EntryValue::Message(message) => {
                if let Some(messages) = cache {
                    append_context_message(messages, message);
                }
            }
            EntryValue::Config { .. }
            | EntryValue::PromptTemplateSelected { .. }
            | EntryValue::ResponsesTurn { .. }
            | EntryValue::ResponsesCompaction { .. } => {}
            EntryValue::Compaction { .. }
            | EntryValue::SkillActivated { .. }
            | EntryValue::SkillResourceRead { .. }
            | EntryValue::SkillDeactivated { .. } => *cache = None,
        }
        Ok(id)
    }

    /// Appends one durable, non-model-visible extension-owned entry.
    ///
    /// The payload is retained exactly like [`Session::append_run_outcome`]:
    /// the entry uses the long-standing non-context configuration marker and
    /// the typed data lives in entry metadata, so no provider projection can
    /// observe it and older readers can still replay the record. The entry's
    /// `extension_metadata` carries the host-attested provenance envelope for
    /// `namespace`, mirroring the persistence-metadata hook path, and the
    /// payload itself is stored in that namespace's bounded value slot.
    ///
    /// Returns the new entry ID, which resolves again after reopening the
    /// session from disk (see [`Session::extension_entry`]). An invalid
    /// namespace, entry type, or payload is refused with a typed error before
    /// anything is written; nothing is truncated or silently dropped.
    pub fn append_extension_entry(
        &mut self,
        namespace: &str,
        process_generation: Option<u64>,
        entry_type: &str,
        data: serde_json::Value,
    ) -> Result<EntryId, SessionError> {
        if !is_valid_extension_metadata_namespace(namespace) {
            return Err(SessionError::Limit(format!(
                "invalid extension metadata namespace {namespace:?}"
            )));
        }
        let payload = ExtensionEntry {
            entry_type: entry_type.to_owned(),
            data,
        };
        let Some(_data_bytes) = valid_extension_entry_payload(&payload) else {
            return Err(SessionError::Limit(format!(
                "extension entry type must be 1..={MAX_EXTENSION_ENTRY_TYPE_BYTES} non-control bytes and its data must be an inert JSON value within {MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES} encoded bytes"
            )));
        };
        let mut extension_metadata = BTreeMap::new();
        extension_metadata.insert(
            namespace.to_owned(),
            ExtensionEntryMetadata {
                // The append protocol carries no public flag, so extension
                // payloads stay private: exports and frontend projections must
                // not surface extension-owned data implicitly.
                public: false,
                value: payload.into_value(),
                provenance: ExtensionMetadataProvenance {
                    extension: namespace.to_owned(),
                    process_generation,
                },
            },
        );
        self.append_with_metadata(
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            },
            Some(EntryMetadata {
                extension_metadata,
                ..EntryMetadata::default()
            }),
        )
    }

    /// Changes the head to an existing entry and appends a head record (same
    /// persistence semantics as [`Session::append`]). Future appends fork a
    /// new branch from this point.
    pub fn checkout(&mut self, id: EntryId) -> Result<(), SessionError> {
        if !self.index.contains_key(&id) {
            return Err(SessionError::UnknownEntry(id));
        }
        let mut buf = Vec::with_capacity(64);
        write_json_line(
            &mut buf,
            &SessionRecordRef::Head {
                id: &id,
                total_cost_microdollars: &self.total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        self.persist(&buf)?;
        self.head = Some(id);
        *self.context_cache.get_mut() = None;
        *self.responses_replay_cache.get_mut() = None;
        Ok(())
    }

    /// Durably selects the empty pre-entry boundary. Future appends create a
    /// new root branch; all existing roots and descendants remain preserved.
    pub fn checkout_root(&mut self) -> Result<(), SessionError> {
        let mut buf = Vec::with_capacity(64);
        write_json_line(
            &mut buf,
            &SessionRecordRef::RootHead {
                total_cost_microdollars: &self.total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        self.persist(&buf)?;
        self.head = None;
        *self.context_cache.get_mut() = None;
        *self.responses_replay_cache.get_mut() = None;
        Ok(())
    }

    /// Copies exactly one committed ancestor chain into a new session file.
    ///
    /// Entry IDs and semantic sidecar references are preserved, but sibling
    /// branches, usage telemetry, and later checkpoints are deliberately not
    /// copied. When the selected chain crosses a compaction boundary, entries
    /// older than that boundary's `first_kept` are omitted: the fork replays
    /// from the compaction summary exactly like the source session, so the
    /// replaced history is never copied again. The boundary's root-side entry
    /// keeps a source-side parent that was not copied, so it is detached and
    /// re-rooted in the destination.
    ///
    /// A `None` checkpoint copies no entries at all: the destination is an
    /// empty session (for forking "before" a root message).
    ///
    /// The destination is created atomically enough to remain absent on
    /// every validation/write failure.
    pub fn fork_to(
        &self,
        path: impl Into<PathBuf>,
        checkpoint: Option<&EntryId>,
    ) -> Result<Self, SessionError> {
        let path = path.into();
        let mut newest_first = Vec::<&Entry>::new();
        let mut stop_at: Option<&EntryId> = None;
        let mut cursor = checkpoint;
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            newest_first.push(entry);
            if stop_at == Some(id) {
                break;
            }
            if let EntryValue::Compaction { first_kept, .. } = &entry.value {
                stop_at = Some(first_kept);
            }
            cursor = entry.parent.as_ref();
        }
        newest_first.reverse();

        let mut destination = Session::create(path.clone())?;
        let result = (|| {
            let mut bytes = Vec::new();
            let mut remaining = newest_first.into_iter();
            if let Some(first) = remaining.next() {
                if first.parent.is_some() {
                    // The chain starts inside a compacted span: detach the
                    // root-side entry from its source-side parent, which was
                    // deliberately not copied.
                    let mut detached = (*first).clone();
                    detached.parent = None;
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(&detached))?;
                } else {
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(first))?;
                }
                for entry in remaining {
                    write_json_line(&mut bytes, &SessionRecordRef::Entry(entry))?;
                }
            }
            if let Some(id) = checkpoint {
                write_json_line(
                    &mut bytes,
                    &SessionRecordRef::Head {
                        id,
                        total_cost_microdollars: &0,
                        total_cost_picodollars_remainder: &0,
                    },
                )?;
            }
            destination.persist(&bytes)?;
            drop(destination);
            Session::open(&path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&path);
        }
        result
    }

    /// Persist a restore point for a completed prompt without changing the
    /// current head or model-visible context.
    ///
    /// The prompt must be a user-message ancestor of the current head. The
    /// returned record can later be restored with [`Self::restore_checkpoint`].
    pub fn checkpoint(&mut self, prompt: EntryId) -> Result<Checkpoint, SessionError> {
        self.checkpoint_with_telemetry(prompt, None, None)
    }

    /// Persist a completed-prompt restore point together with exact aggregate
    /// usage and current-run cost for UI/status rehydration.
    ///
    /// `run_cost_microdollars` is `Some(0)` for explicitly zero-priced models
    /// and `None` when pricing was unavailable.
    pub fn checkpoint_with_telemetry(
        &mut self,
        prompt: EntryId,
        usage: Option<Usage>,
        run_cost_microdollars: Option<u64>,
    ) -> Result<Checkpoint, SessionError> {
        let head = self.head.clone().ok_or(SessionError::EmptySession)?;
        let prompt_is_user = self
            .entry(&prompt)
            .is_some_and(|entry| matches!(&entry.value, EntryValue::Message(Message::User(_))));
        if !prompt_is_user {
            return Err(SessionError::UnknownEntry(prompt));
        }
        if !self.is_ancestor_of_head(&prompt) {
            return Err(SessionError::NotAncestor(prompt));
        }

        let checkpoint = Checkpoint {
            prompt,
            head,
            usage,
            run_cost_microdollars,
        };
        let mut buffer = Vec::with_capacity(192);
        write_json_line(
            &mut buffer,
            &SessionRecordRef::Checkpoint {
                prompt: &checkpoint.prompt,
                head: &checkpoint.head,
                usage: &checkpoint.usage,
                run_cost_microdollars: &checkpoint.run_cost_microdollars,
            },
        )?;
        self.persist(&buffer)?;
        self.checkpoints.push(checkpoint.clone());
        Ok(checkpoint)
    }

    /// Durable completed-prompt restore points in append order, across all
    /// preserved branches.
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// Provider usage records in append order, across all preserved branches.
    ///
    /// Assistant-turn records point at their exact durable assistant entry,
    /// unlike checkpoint usage which is aggregated for a whole user prompt.
    pub fn usage_records(&self) -> &[UsageRecord] {
        &self.usage_records
    }

    /// Newest provider usage record for an assistant turn on the active
    /// branch. Unlike checkpoint usage, this is one request rather than the
    /// sum of every autonomous tool turn in a submitted prompt.
    pub fn latest_active_assistant_usage(&self) -> Option<&UsageRecord> {
        let mut active = std::collections::HashSet::<&str>::new();
        let mut cursor = self.head.as_ref();
        while let Some(id) = cursor {
            active.insert(id.0.as_str());
            cursor = self.entry(id).and_then(|entry| entry.parent.as_ref());
        }
        self.usage_records.iter().rev().find(|record| {
            matches!(
                &record.kind,
                UsageRecordKind::AssistantTurn { assistant }
                    if active.contains(assistant.0.as_str())
            )
        })
    }

    /// Persist usage for one completed assistant turn.
    pub fn record_assistant_usage(
        &mut self,
        assistant: EntryId,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        self.record_assistant_usage_inner(assistant, endpoint, model, usage, cost, None)
    }

    /// Persist usage and the provider-authoritative stop reason for one
    /// completed assistant turn.
    pub fn record_assistant_usage_with_stop_reason(
        &mut self,
        assistant: EntryId,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        stop_reason: StopReason,
    ) -> Result<(), SessionError> {
        self.record_assistant_usage_inner(
            assistant,
            endpoint,
            model,
            usage,
            cost,
            Some(stop_reason),
        )
    }

    fn record_assistant_usage_inner(
        &mut self,
        assistant: EntryId,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        stop_reason: Option<StopReason>,
    ) -> Result<(), SessionError> {
        let valid_assistant = self.entry(&assistant).is_some_and(|entry| {
            matches!(&entry.value, EntryValue::Message(Message::Assistant(_)))
        });
        if !valid_assistant {
            return Err(SessionError::UnknownEntry(assistant));
        }
        self.record_usage(UsageRecord {
            kind: UsageRecordKind::AssistantTurn { assistant },
            usage,
            stop_reason,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        })
    }

    /// Persist root-ledger usage for one bounded delegated child session.
    ///
    /// `cost` is the exact aggregate of the child's durable provider records,
    /// including its picodollar remainder. The child session remains the
    /// detailed source of truth; this root record makes cumulative accounting
    /// and cost limits include delegated work without replaying child files.
    pub(crate) fn record_delegated_agent_usage(
        &mut self,
        delegated: DelegatedUsage,
    ) -> Result<(), SessionError> {
        let DelegatedUsage {
            agent_id,
            turn_count,
            tool_call_count,
            endpoint,
            model,
            usage,
            cost,
        } = delegated;
        self.record_usage(UsageRecord {
            kind: UsageRecordKind::DelegatedAgent {
                agent_id,
                turn_count,
                tool_call_count,
            },
            usage,
            stop_reason: None,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        })
    }

    /// Persist usage for a context-compaction provider call.
    pub fn record_compaction_usage(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        self.record_usage(UsageRecord {
            kind: UsageRecordKind::Compaction,
            usage,
            stop_reason: None,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        })
    }

    /// Persist usage for a Responses turn whose terminal output could not
    /// satisfy explicit native replay mode.
    pub fn record_rejected_responses_turn_usage(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        self.record_usage(UsageRecord {
            kind: UsageRecordKind::RejectedResponsesTurn,
            usage,
            stop_reason: None,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        })
    }

    /// Persist usage for an isolated terminal-gate provider call.
    pub fn record_terminal_gate_usage(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        returned: Option<bool>,
    ) -> Result<(), SessionError> {
        self.record_usage(UsageRecord {
            kind: UsageRecordKind::TerminalGate { returned },
            usage,
            stop_reason: None,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        })
    }

    fn record_usage(&mut self, mut record: UsageRecord) -> Result<(), SessionError> {
        let request_remainder = record
            .cost
            .map(|cost| cost.total_picodollars_remainder)
            .unwrap_or_default();
        let remainder_sum = u64::from(self.total_cost_picodollars_remainder)
            .saturating_add(u64::from(request_remainder));
        let carry = remainder_sum / u64::from(PICODOLLARS_PER_MICRODOLLAR);
        let new_total = self
            .total_cost_microdollars
            .saturating_add(record.cost_microdollars.unwrap_or_default())
            .saturating_add(carry);
        let new_remainder = (remainder_sum % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
        record.session_cost_microdollars = Some(new_total);
        record.session_cost_picodollars_remainder = Some(new_remainder);
        let mut buffer = Vec::with_capacity(224);
        write_json_line(&mut buffer, &SessionRecordRef::Usage { record: &record })?;
        self.persist(&buffer)?;
        self.total_cost_microdollars = new_total;
        self.total_cost_picodollars_remainder = new_remainder;
        self.usage_records.push(record);
        Ok(())
    }

    /// Persist unknown usage for one accepted attempt before replacing it.
    ///
    /// Supply only trusted endpoint/model/operation identifiers (1..=128 ASCII
    /// letters, digits, `-`, `_`, `.`, `:`, `/`; URLs are forbidden). Call once
    /// per failed physical attempt, not once per observer or retry notification.
    /// A failed append leaves in-memory accounting unchanged and must stop
    /// recovery. Successful appends use the session's ordinary private, locked,
    /// synced persistence path and change neither head nor known usage subtotal.
    pub fn record_usage_uncertainty(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        operation: impl Into<String>,
    ) -> Result<(), SessionError> {
        let record = UsageUncertaintyRecord {
            endpoint,
            model,
            operation: operation.into(),
        };
        record.validate()?;
        let mut buffer = Vec::with_capacity(256);
        write_json_line(
            &mut buffer,
            &SessionRecordRef::UsageUncertainty { record: &record },
        )?;
        self.persist(&buffer)?;
        self.usage_uncertainty_records.push(record);
        Ok(())
    }

    /// Drop cannot return a persistence failure to its caller. Retain the same
    /// uncertainty in memory on failure so a later hard ceiling cannot mistake
    /// the abandoned accepted attempt for zero exposure. Disk failure still
    /// prevents any promise of recovery after process exit.
    pub(crate) fn record_abandoned_usage_uncertainty(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        operation: &str,
    ) -> Result<(), SessionError> {
        let result = self.record_usage_uncertainty(endpoint.clone(), model.clone(), operation);
        if result.is_err() {
            self.usage_uncertainty_records.push(UsageUncertaintyRecord {
                endpoint,
                model,
                operation: operation.to_owned(),
            });
        }
        result
    }

    /// Whether any completed operation lacks exact pricing. Token usage can
    /// still be known; catalog availability for the active model cannot price
    /// a historical request, provider-selected tier, or child retroactively.
    pub fn has_unpriced_usage(&self) -> bool {
        self.usage_records
            .iter()
            .any(|record| record.cost.is_none() && record.cost_microdollars.is_none())
    }

    /// Whether any durable accepted-attempt usage is unknown, on any branch.
    /// Known usage/cost totals are only subtotals while this is true. Hard
    /// cumulative ceilings must fail closed, including after reopening.
    pub fn has_uncertain_usage(&self) -> bool {
        !self.usage_uncertainty_records.is_empty()
    }

    /// Unknown-usage evidence in append order, independent of the active head.
    /// These records contain no token or cost estimates and are not usage totals.
    pub fn usage_uncertainty_records(&self) -> &[UsageUncertaintyRecord] {
        &self.usage_uncertainty_records
    }

    /// Newest completed-prompt checkpoint on the active branch.
    pub fn latest_active_checkpoint(&self) -> Option<&Checkpoint> {
        if self.checkpoints.is_empty() {
            return None;
        }
        let mut active_entry_ids = std::collections::HashSet::<&str>::new();
        let mut cursor = self.head.as_ref();
        while let Some(id) = cursor {
            active_entry_ids.insert(id.0.as_str());
            cursor = self.entry(id).and_then(|entry| entry.parent.as_ref());
        }
        self.checkpoints
            .iter()
            .rev()
            .find(|checkpoint| active_entry_ids.contains(checkpoint.head.0.as_str()))
    }

    /// Restore the newest checkpoint written for `prompt` and append the
    /// corresponding durable head update. Future appends branch from it.
    pub fn restore_checkpoint(&mut self, prompt: &EntryId) -> Result<(), SessionError> {
        let checkpoint = self
            .checkpoints
            .iter()
            .rev()
            .find(|checkpoint| &checkpoint.prompt == prompt)
            .cloned()
            .ok_or_else(|| SessionError::UnknownCheckpoint(prompt.clone()))?;
        self.checkout(checkpoint.head)
    }

    /// Returns the whole-microdollar portion of known cumulative session cost.
    /// This is only a subtotal when [`Self::has_uncertain_usage`] is true.
    pub fn total_cost_microdollars(&self) -> u64 {
        self.total_cost_microdollars
    }

    /// Returns the cumulative picodollar remainder below one microdollar.
    pub fn total_cost_picodollars_remainder(&self) -> u32 {
        self.total_cost_picodollars_remainder
    }

    /// Increments the cumulative session cost by `additional` microdollars
    /// and persists a new head record. Local/custom models that have no
    /// pricing should pass 0 so the tally stays unchanged.
    pub fn add_cost(&mut self, additional: u64) -> Result<(), SessionError> {
        if additional == 0 {
            return Ok(());
        }
        let new_total = self.total_cost_microdollars.saturating_add(additional);
        let mut buf = Vec::with_capacity(64);
        write_json_line(
            &mut buf,
            &SessionRecordRef::Head {
                id: self.head.as_ref().expect("head exists after first append"),
                total_cost_microdollars: &new_total,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        self.persist(&buf)?;
        self.total_cost_microdollars = new_total;
        Ok(())
    }

    /// Appends a manual compaction entry. `summary` is caller-provided text
    /// (this crate never generates summaries itself); `first_kept` must be an
    /// ancestor of — or equal to — the current head and marks the oldest
    /// entry kept in full fidelity by [`Session::context`].
    pub fn compact(
        &mut self,
        summary: impl Into<String>,
        first_kept: EntryId,
    ) -> Result<EntryId, SessionError> {
        self.compact_with_details(
            summary,
            first_kept,
            crate::compaction::CompactionDetails::default(),
        )
    }

    /// Appends a compaction checkpoint with cumulative Pi-compatible file
    /// operation details used by later iterative handoffs.
    pub fn compact_with_details(
        &mut self,
        summary: impl Into<String>,
        first_kept: EntryId,
        details: crate::compaction::CompactionDetails,
    ) -> Result<EntryId, SessionError> {
        if !self.is_ancestor_of_head(&first_kept) {
            return Err(SessionError::NotAncestor(first_kept));
        }
        let parent_id = self
            .entry(&first_kept)
            .ok_or_else(|| SessionError::UnknownEntry(first_kept.clone()))?
            .parent
            .clone();

        let (active_skills, skill_resources) = if let Some(p_id) = parent_id {
            let state = self.resolve_active_skills(&p_id)?;
            (state.active_skills, state.skill_resources)
        } else {
            (Vec::new(), Vec::new())
        };

        self.append(EntryValue::Compaction {
            summary: summary.into(),
            first_kept,
            active_skills,
            skill_resources,
            details,
        })
    }

    /// Appends a completed assistant turn, its usage record, and an optional
    /// authoritative Responses sidecar in one durable write.
    ///
    /// The assistant entry and its final head are written before usage, matching
    /// the historical record order. A Responses sidecar, when present, is then
    /// written as the direct child of that assistant. Keeping all records in one
    /// `persist` call removes redundant filesystem sync barriers without
    /// weakening the crash boundary: a successful return means the complete
    /// turn is durable, while a failed write mutates no in-memory state.
    #[allow(clippy::too_many_arguments)]
    pub fn append_assistant_turn(
        &mut self,
        assistant: octet_ai::AssistantMessage,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        stop_reason: StopReason,
        responses_output: Option<octet_ai::ResponsesOutput>,
    ) -> Result<EntryId, SessionError> {
        self.append_assistant_turn_with_metadata(
            assistant,
            endpoint,
            model,
            usage,
            cost,
            stop_reason,
            responses_output,
            None,
        )
    }

    /// Appends a completed assistant turn with host-validated, extension-owned
    /// metadata at the same durable boundary as the canonical message.
    ///
    /// This deliberately accepts only `extension_metadata`; host-owned prompt,
    /// tool, and run presentation fields are cleared before persistence.
    #[allow(clippy::too_many_arguments)]
    pub fn append_assistant_turn_with_metadata(
        &mut self,
        assistant: octet_ai::AssistantMessage,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        stop_reason: StopReason,
        responses_output: Option<octet_ai::ResponsesOutput>,
        metadata: Option<EntryMetadata>,
    ) -> Result<EntryId, SessionError> {
        let metadata = metadata
            .map(|mut metadata| {
                metadata.prompt_model = None;
                metadata.prompt_model_source = None;
                metadata.prompt_color = None;
                metadata.display_text = None;
                metadata.run_outcome = None;
                metadata.tool_output = None;
                metadata.tool_started_unix_ms = None;
                metadata.tool_finished_unix_ms = None;
                metadata.local_synthetic_assistant = false;
                metadata
            })
            .and_then(EntryMetadata::sanitized);
        let output_is_valid = responses_output.as_ref().is_none_or(|output| {
            !output.is_empty()
                && assistant.protocol == octet_ai::Protocol::OpenAiResponses
                && assistant.model == model
        });
        if !output_is_valid {
            return Err(SessionError::InvalidResponsesSidecar(format!(
                "Responses output is not attached to a non-empty Responses assistant from model {}",
                model.0
            )));
        }

        let assistant_id = EntryId(format!("{:03}", self.next_id));
        let sidecar_id = responses_output
            .as_ref()
            .map(|_| EntryId(format!("{:03}", self.next_id.saturating_add(1))));
        let ids_used = if sidecar_id.is_some() { 2 } else { 1 };
        let next_id = self
            .next_id
            .checked_add(ids_used)
            .ok_or_else(|| SessionError::Limit("session entry ID space is exhausted".to_owned()))?;
        let parent = self.head.clone();
        let assistant_message = Message::Assistant(assistant);
        let assistant_entry = Entry {
            id: assistant_id.clone(),
            parent,
            metadata,
            timestamp_unix_ms: Some(now_unix_millis()),
            value: EntryValue::Message(assistant_message.clone()),
        };

        let request_remainder = cost
            .map(|value| value.total_picodollars_remainder)
            .unwrap_or_default();
        let remainder_sum = u64::from(self.total_cost_picodollars_remainder)
            .saturating_add(u64::from(request_remainder));
        let carry = remainder_sum / u64::from(PICODOLLARS_PER_MICRODOLLAR);
        let new_total = self
            .total_cost_microdollars
            .saturating_add(cost.map(|value| value.total).unwrap_or_default())
            .saturating_add(carry);
        let new_remainder = (remainder_sum % u64::from(PICODOLLARS_PER_MICRODOLLAR)) as u32;
        let usage_record = UsageRecord {
            kind: UsageRecordKind::AssistantTurn {
                assistant: assistant_id.clone(),
            },
            usage,
            stop_reason: Some(stop_reason),
            endpoint: Some(endpoint),
            model: Some(model.clone()),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|value| value.total),
            session_cost_microdollars: Some(new_total),
            session_cost_picodollars_remainder: Some(new_remainder),
        };

        let mut buffer = Vec::with_capacity(512);
        write_json_line(&mut buffer, &SessionRecordRef::Entry(&assistant_entry))?;
        write_json_line(
            &mut buffer,
            &SessionRecordRef::Head {
                id: &assistant_id,
                total_cost_microdollars: &self.total_cost_microdollars,
                total_cost_picodollars_remainder: &self.total_cost_picodollars_remainder,
            },
        )?;
        write_json_line(
            &mut buffer,
            &SessionRecordRef::Usage {
                record: &usage_record,
            },
        )?;

        let sidecar_entry = responses_output.map(|output| Entry {
            id: sidecar_id
                .clone()
                .expect("sidecar id exists for Responses output"),
            parent: Some(assistant_id.clone()),
            metadata: None,
            timestamp_unix_ms: Some(now_unix_millis()),
            value: EntryValue::ResponsesTurn {
                assistant: assistant_id.clone(),
                endpoint: usage_record
                    .endpoint
                    .clone()
                    .expect("assistant usage endpoint exists"),
                model: usage_record
                    .model
                    .clone()
                    .expect("assistant usage model exists"),
                output,
            },
        });
        if let Some(sidecar_entry) = sidecar_entry.as_ref() {
            write_json_line(&mut buffer, &SessionRecordRef::Entry(sidecar_entry))?;
            let sidecar_id = &sidecar_entry.id;
            write_json_line(
                &mut buffer,
                &SessionRecordRef::Head {
                    id: sidecar_id,
                    total_cost_microdollars: &new_total,
                    total_cost_picodollars_remainder: &new_remainder,
                },
            )?;
        }

        self.persist(&buffer)?;

        self.entries.push(assistant_entry);
        self.index
            .insert(assistant_id.clone(), self.entries.len().saturating_sub(1));
        if let Some(sidecar_entry) = sidecar_entry {
            let id = sidecar_entry.id.clone();
            self.index.insert(id, self.entries.len());
            self.entries.push(sidecar_entry);
            self.head = Some(
                self.entries
                    .last()
                    .expect("sidecar just appended")
                    .id
                    .clone(),
            );
        } else {
            self.head = Some(assistant_id.clone());
        }
        self.next_id = next_id;
        self.total_cost_microdollars = new_total;
        self.total_cost_picodollars_remainder = new_remainder;
        self.usage_records.push(usage_record);
        if let Some(messages) = self.context_cache.get_mut() {
            append_context_message(messages, &assistant_message);
        }
        Ok(assistant_id)
    }

    /// Appends an authoritative Responses turn sidecar.
    ///
    /// The canonical assistant must be the current head. Requiring the sidecar
    /// to be its direct child makes association branch-local: checkout to the
    /// assistant or to another child cannot accidentally inherit this opaque
    /// provider state.
    pub fn append_responses_turn(
        &mut self,
        assistant: EntryId,
        endpoint: EndpointId,
        model: ModelId,
        output: octet_ai::ResponsesOutput,
    ) -> Result<EntryId, SessionError> {
        if self.head.as_ref() != Some(&assistant) {
            return Err(SessionError::InvalidResponsesSidecar(format!(
                "assistant {:?} is not the current head",
                assistant.0
            )));
        }
        let valid_assistant = self.entry(&assistant).is_some_and(|entry| {
            matches!(
                &entry.value,
                EntryValue::Message(Message::Assistant(message))
                    if message.protocol == octet_ai::Protocol::OpenAiResponses
                        && message.model == model
            )
        });
        if !valid_assistant {
            return Err(SessionError::InvalidResponsesSidecar(format!(
                "entry {:?} is not a Responses assistant from model {}",
                assistant.0, model.0
            )));
        }
        self.append(EntryValue::ResponsesTurn {
            assistant,
            endpoint,
            model,
            output,
        })
    }

    /// Appends a native Responses compaction checkpoint at the current head.
    ///
    /// The opaque output covers the selected branch replay root through
    /// `covered_through`. Because the marker is appended directly after that
    /// head and remains context-invisible, sibling branches never observe it
    /// and non-matching routes can always fall back to canonical history.
    pub fn append_responses_compaction(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        output: octet_ai::ResponsesOutput,
    ) -> Result<EntryId, SessionError> {
        let covered_through = self.head.clone().ok_or(SessionError::EmptySession)?;
        self.append(EntryValue::ResponsesCompaction {
            endpoint,
            model,
            covered_through,
            output,
        })
    }

    fn active_branch_entries(&self) -> Result<Vec<&Entry>, SessionError> {
        let mut newest_first = Vec::new();
        let mut cursor = self.head.as_ref();
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            newest_first.push(entry);
            cursor = entry.parent.as_ref();
        }
        newest_first.reverse();
        Ok(newest_first)
    }

    /// Builds the exact active-branch input sequence for durable Responses
    /// replay on `endpoint`/`model`.
    ///
    /// `Some` means every assistant in the selected model-visible window has a
    /// route-affine authoritative sidecar. `None` is the safe legacy/crash
    /// fallback when any assistant lacks one. A sidecar that exists but belongs
    /// to another route is rejected explicitly rather than silently replayed.
    /// The nearest matching native compaction checkpoint after the latest local
    /// compaction becomes the opaque base for subsequent user/assistant turns.
    pub fn responses_replay_items(
        &self,
        endpoint: &EndpointId,
        model: &ModelId,
    ) -> Result<Option<Vec<octet_ai::responses::ResponsesReplayItem>>, SessionError> {
        Ok(self
            .responses_replay_snapshot(endpoint, model)?
            .map(|items| (*items).clone()))
    }

    /// Shares the route-affine active replay window without cloning its prefix.
    /// A retained snapshot stays immutable when the session advances. Encoding
    /// a complete provider request still necessarily visits the complete input.
    pub fn responses_replay_snapshot(
        &self,
        endpoint: &EndpointId,
        model: &ModelId,
    ) -> Result<Option<Arc<Vec<octet_ai::responses::ResponsesReplayItem>>>, SessionError> {
        let mut cache = self.responses_replay_cache.borrow_mut();
        if let Some(current) = cache
            .as_mut()
            .filter(|current| &current.endpoint == endpoint && &current.model == model)
        {
            let mut cursor = self.head_ref();
            let mut appended = Vec::new();
            let mut rebuild = false;
            while cursor != current.head.as_ref() {
                let Some(entry) = cursor.and_then(|id| self.entry(id)) else {
                    rebuild = true;
                    break;
                };
                #[cfg(test)]
                self.responses_replay_work.set((
                    self.responses_replay_work.get().0,
                    self.responses_replay_work.get().1 + 1,
                ));
                if matches!(
                    entry.value,
                    EntryValue::Compaction { .. } | EntryValue::ResponsesCompaction { .. }
                ) {
                    rebuild = true;
                    break;
                }
                // A late sidecar can repair a previously queried legacy/crash
                // gap. New turns cannot repair a missing older assistant.
                if current.items.is_none() {
                    if let EntryValue::ResponsesTurn { assistant, .. } = &entry.value {
                        if current
                            .head
                            .as_ref()
                            .is_some_and(|head| self.index[assistant] <= self.index[head])
                        {
                            rebuild = true;
                            break;
                        }
                    }
                }
                appended.push(entry);
                cursor = entry.parent.as_ref();
            }
            if !rebuild {
                if let Some(items) = &mut current.items {
                    appended.reverse();
                    // Validate the suffix before changing the cached prefix.
                    let mut suffix = Vec::new();
                    if self.append_responses_replay(
                        &appended,
                        &appended,
                        endpoint,
                        model,
                        &mut suffix,
                    )? {
                        if !suffix.is_empty() {
                            Arc::make_mut(items).extend(suffix);
                        }
                    } else {
                        // Cache the fallback too: a permanent legacy gap must
                        // not rescan an ever-growing suffix on every turn. A
                        // late sidecar repairs it through the rebuild path.
                        current.items = None;
                        current.head = self.head();
                        return Ok(None);
                    }
                }
                current.head = self.head();
                return Ok(current.items.clone());
            }
        }
        #[cfg(test)]
        self.responses_replay_work.set((
            self.responses_replay_work.get().0 + 1,
            self.responses_replay_work.get().1,
        ));
        let items = self
            .rebuild_responses_replay(endpoint, model)?
            .map(Arc::new);
        *cache = Some(ResponsesReplayCache {
            endpoint: endpoint.clone(),
            model: model.clone(),
            head: self.head(),
            items: items.clone(),
        });
        Ok(items)
    }

    fn rebuild_responses_replay(
        &self,
        endpoint: &EndpointId,
        model: &ModelId,
    ) -> Result<Option<Vec<octet_ai::responses::ResponsesReplayItem>>, SessionError> {
        let branch = self.active_branch_entries()?;
        if branch.is_empty() {
            return Ok(Some(Vec::new()));
        }

        let local_compaction =
            branch
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, entry)| match &entry.value {
                    EntryValue::Compaction {
                        summary,
                        first_kept,
                        ..
                    } => Some((index, summary, first_kept)),
                    _ => None,
                });
        let local_marker_index = local_compaction.map(|(index, _, _)| index);
        let native_search_start = local_marker_index.map_or(0, |index| index.saturating_add(1));
        let native_compaction = branch
            .iter()
            .enumerate()
            .skip(native_search_start)
            .rev()
            .find_map(|(index, entry)| match &entry.value {
                EntryValue::ResponsesCompaction {
                    endpoint: recorded_endpoint,
                    model: recorded_model,
                    output,
                    ..
                } if recorded_endpoint == endpoint && recorded_model == model => {
                    Some((index, output))
                }
                _ => None,
            });

        let mut replay = Vec::new();
        let start = if let Some((index, output)) = native_compaction {
            replay.push(octet_ai::responses::ResponsesReplayItem::Compacted(
                output.clone(),
            ));
            index.saturating_add(1)
        } else if let Some((_marker_index, summary, first_kept)) = local_compaction {
            let first_kept_index = branch
                .iter()
                .position(|entry| &entry.id == first_kept)
                .ok_or_else(|| SessionError::UnknownEntry(first_kept.clone()))?;
            replay.push(octet_ai::responses::ResponsesReplayItem::User(
                UserMessage {
                    content: vec![UserPart::Text(format!(
                        "[summary of earlier conversation]\n{summary}"
                    ))],
                },
            ));
            first_kept_index
        } else {
            0
        };

        if self.append_responses_replay(&branch[start..], &branch, endpoint, model, &mut replay)? {
            Ok(Some(replay))
        } else {
            Ok(None)
        }
    }

    fn append_responses_replay(
        &self,
        entries: &[&Entry],
        sidecar_entries: &[&Entry],
        endpoint: &EndpointId,
        model: &ModelId,
        replay: &mut Vec<octet_ai::responses::ResponsesReplayItem>,
    ) -> Result<bool, SessionError> {
        let mut sidecars =
            HashMap::<&EntryId, (&EndpointId, &ModelId, &octet_ai::ResponsesOutput)>::new();
        for entry in sidecar_entries {
            if let EntryValue::ResponsesTurn {
                assistant,
                endpoint,
                model,
                output,
            } = &entry.value
            {
                sidecars.insert(assistant, (endpoint, model, output));
            }
        }

        for entry in entries {
            match &entry.value {
                EntryValue::Message(Message::User(user)) => {
                    replay.push(octet_ai::responses::ResponsesReplayItem::User(user.clone()));
                }
                EntryValue::Message(Message::Assistant(assistant))
                    if entry.metadata.as_ref().is_some_and(|metadata| {
                        metadata.local_synthetic_assistant
                            && assistant.protocol == octet_ai::Protocol::OpenAiResponses
                            && assistant.model == *model
                    }) =>
                {
                    replay.push(octet_ai::responses::ResponsesReplayItem::LocalAssistant(
                        assistant.clone(),
                    ));
                }
                EntryValue::Message(Message::Assistant(_)) => {
                    let Some((recorded_endpoint, recorded_model, output)) =
                        sidecars.get(&entry.id).copied()
                    else {
                        return Ok(false);
                    };
                    if recorded_endpoint != endpoint || recorded_model != model {
                        return Err(SessionError::ResponsesRouteMismatch {
                            assistant: entry.id.clone(),
                            expected_endpoint: endpoint.0.clone(),
                            expected_model: model.0.clone(),
                            actual_endpoint: recorded_endpoint.0.clone(),
                            actual_model: recorded_model.0.clone(),
                        });
                    }
                    replay.push(octet_ai::responses::ResponsesReplayItem::Output(
                        output.clone(),
                    ));
                }
                EntryValue::Compaction { .. }
                | EntryValue::ResponsesTurn { .. }
                | EntryValue::ResponsesCompaction { .. }
                | EntryValue::Config { .. }
                | EntryValue::PromptTemplateSelected { .. }
                | EntryValue::SkillActivated { .. }
                | EntryValue::SkillResourceRead { .. }
                | EntryValue::SkillDeactivated { .. } => {}
            }
        }
        Ok(true)
    }

    /// Returns the current head entry ID (`None` for an empty session).
    pub fn head(&self) -> Option<EntryId> {
        self.head.clone()
    }

    /// Borrows the current head entry ID without allocating.
    pub fn head_ref(&self) -> Option<&EntryId> {
        self.head.as_ref()
    }

    /// Returns all entries in insertion order, across all branches.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// Returns the entry with the given ID.
    pub fn entry(&self, id: &EntryId) -> Option<&Entry> {
        self.index.get(id).map(|&i| &self.entries[i])
    }

    /// Durably sets or clears the label of an existing entry.
    ///
    /// Labels are a replaceable mutation of an immutable JSONL entry: a new
    /// label record is appended and the last record for one entry wins on
    /// replay. An empty `label` clears the entry's label. Unknown entry IDs,
    /// labels longer than [`MAX_ENTRY_LABEL_BYTES`], and control characters are
    /// refused with a typed error and leave the session state unchanged.
    pub fn set_entry_label(&mut self, id: &EntryId, label: &str) -> Result<(), SessionError> {
        if !self.index.contains_key(id) {
            return Err(SessionError::UnknownEntry(id.clone()));
        }
        if !valid_entry_label(label) {
            return Err(SessionError::Limit(format!(
                "entry label must be at most {MAX_ENTRY_LABEL_BYTES} bytes without control characters"
            )));
        }
        let mut buf = Vec::with_capacity(64 + label.len());
        write_json_line(
            &mut buf,
            &SessionRecordRef::EntryLabel { entry_id: id, label },
        )?;
        self.persist(&buf)?;
        if label.is_empty() {
            self.entry_labels.remove(id);
        } else {
            self.entry_labels.insert(id.clone(), label.to_owned());
        }
        Ok(())
    }

    /// The durable label of `id`, if one is currently set.
    pub fn entry_label(&self, id: &EntryId) -> Option<&str> {
        self.entry_labels.get(id).map(String::as_str)
    }

    /// Every durable entry label, keyed by entry ID.
    ///
    /// At most one label exists per entry, so the map never outgrows the
    /// session's entries.
    pub fn entry_labels(&self) -> &BTreeMap<EntryId, String> {
        &self.entry_labels
    }

    /// The durable extension-owned payload appended for `id` by `namespace`.
    ///
    /// Returns a detached decoded payload, or `None` when the entry has no
    /// extension value in that namespace (or the value was written by another
    /// host path that does not use the entry envelope).
    pub fn extension_entry(&self, id: &EntryId, namespace: &str) -> Option<ExtensionEntry> {
        let metadata = self.entry(id)?.metadata.as_ref()?;
        ExtensionEntry::from_value(&metadata.extension_metadata.get(namespace)?.value)
    }

    /// Reconstructs the model-visible context from the current head.
    ///
    /// Walks the parent chain from the head, stopping at the nearest
    /// compaction's `first_kept` boundary, and returns messages in
    /// chronological order. Compaction summaries are injected in front as
    /// synthetic user messages (`octet-ai` has no system role inside
    /// [`Message`]; the request-level system prompt belongs to the agent).
    /// Config entries are skipped. Consecutive tool-result messages (including
    /// protocol-required adjacent media) are coalesced into a single user
    /// message with every result before the media, matching provider-required
    /// wire ordering. Once materialized, the result is incrementally updated
    /// for ordinary appends and reused until checkout or compaction changes
    /// the active branch semantics.
    fn reconstruct_context(&self) -> Result<Vec<Message>, SessionError> {
        let mut newest_first: Vec<Message> = Vec::new();
        let mut summary: Option<String> = None;
        let mut boundary: Option<EntryId> = None;

        let mut cursor = self.head.as_ref();
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            match &entry.value {
                EntryValue::Message(m) => newest_first.push(m.clone()),
                EntryValue::Config { .. }
                | EntryValue::PromptTemplateSelected { .. }
                | EntryValue::ResponsesTurn { .. }
                | EntryValue::ResponsesCompaction { .. } => {}
                EntryValue::Compaction {
                    summary: compaction_summary,
                    first_kept,
                    ..
                } => {
                    // A compaction summary represents everything it replaces,
                    // including any older summary in that range. Therefore only
                    // the marker nearest the head is model-visible; injecting
                    // older summaries again duplicates overlapping history.
                    if boundary.is_none() {
                        summary = Some(compaction_summary.clone());
                        boundary = Some(first_kept.clone());
                    }
                }
                EntryValue::SkillActivated { .. }
                | EntryValue::SkillResourceRead { .. }
                | EntryValue::SkillDeactivated { .. } => {}
            }
            if boundary.as_ref() == Some(id) {
                break;
            }
            cursor = entry.parent.as_ref();
        }

        let mut messages: Vec<Message> = summary
            .into_iter()
            .map(|summary| {
                Message::User(UserMessage {
                    content: vec![UserPart::Text(format!(
                        "[summary of earlier conversation]\n{summary}"
                    ))],
                })
            })
            .collect();
        messages.extend(newest_first.into_iter().rev());
        Ok(coalesce_tool_results(messages))
    }

    /// Borrows the cached model-visible context without deep-cloning message
    /// text, tool output, or media. The first call reconstructs the active
    /// branch; ordinary appends update that cache incrementally.
    pub fn context_ref(&self) -> Result<Ref<'_, [Message]>, SessionError> {
        if self.context_cache.borrow().is_none() {
            let messages = self.reconstruct_context()?;
            *self.context_cache.borrow_mut() = Some(messages);
        }
        Ok(Ref::map(self.context_cache.borrow(), |cache| {
            cache
                .as_deref()
                .expect("context cache initialized immediately above")
        }))
    }

    /// Returns an owned model-visible context snapshot.
    ///
    /// Call [`Self::context_ref`] for estimates and inspection that do not
    /// require ownership; it avoids copying the complete conversation.
    pub fn context(&self) -> Result<Vec<Message>, SessionError> {
        Ok(self.context_ref()?.to_vec())
    }

    /// Reconstructs the model-visible messages represented strictly before an
    /// active-branch boundary. This is used by autonomous context recovery to
    /// summarize exactly what a compaction record will replace.
    pub fn context_before(&self, first_kept: &EntryId) -> Result<Vec<Message>, SessionError> {
        let entry = self
            .entry(first_kept)
            .ok_or_else(|| SessionError::UnknownEntry(first_kept.clone()))?;
        let mut reverse = Vec::new();
        let mut cursor = entry.parent.as_ref();
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            reverse.push(entry);
            cursor = entry.parent.as_ref();
        }
        reverse.reverse();

        let mut messages = Vec::new();
        for entry in reverse {
            match &entry.value {
                EntryValue::Message(message) => messages.push(message.clone()),
                EntryValue::Compaction { summary, .. } => {
                    messages.clear();
                    messages.push(Message::User(UserMessage {
                        content: vec![UserPart::Text(format!(
                            "[summary of earlier conversation]\n{summary}"
                        ))],
                    }));
                }
                EntryValue::Config { .. }
                | EntryValue::PromptTemplateSelected { .. }
                | EntryValue::ResponsesTurn { .. }
                | EntryValue::ResponsesCompaction { .. }
                | EntryValue::SkillActivated { .. }
                | EntryValue::SkillResourceRead { .. }
                | EntryValue::SkillDeactivated { .. } => {}
            }
        }
        Ok(coalesce_tool_results(messages))
    }

    /// True when `id` is the head or one of its persistent-tree ancestors.
    ///
    /// Compaction markers deliberately do not sever parent-link ancestry: they
    /// change model-visible context reconstruction, not which branch an entry
    /// belongs to. This predicate protects `compact()` from abandoned-branch
    /// references; it is not a context-visibility query.
    fn is_ancestor_of_head(&self, id: &EntryId) -> bool {
        let mut cursor = self.head.as_ref();
        while let Some(current) = cursor {
            if current == id {
                return true;
            }
            cursor = self.entry(current).and_then(|entry| entry.parent.as_ref());
        }
        false
    }

    /// Active skills resolved for a given leaf entry along its branch ancestry.
    pub fn resolve_active_skills(
        &self,
        leaf_id: &EntryId,
    ) -> Result<ActiveSkillState, SessionError> {
        let mut cursor = Some(leaf_id);
        let mut deactivated = std::collections::HashSet::new();
        let mut active_skills: Vec<SkillActivatedSnapshot> = Vec::new();
        let mut skill_resources: Vec<SkillResourceSnapshot> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        let mut boundary: Option<&EntryId> = None;
        let mut saw_compaction = false;

        while let Some(id) = cursor {
            if boundary == Some(id) {
                break;
            }
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            match &entry.value {
                EntryValue::SkillDeactivated {
                    activation_id,
                    skill_id,
                } => {
                    deactivated.insert(activation_id.clone());
                    // Deactivation resolves the skill ID, not merely one
                    // historical activation. Otherwise walking farther back
                    // resurrects the activation that a reload superseded.
                    seen_ids.insert(skill_id.clone());
                }
                EntryValue::SkillActivated {
                    descriptor,
                    instructions_hash,
                    instructions,
                } => {
                    let act_id = id.clone();
                    if !deactivated.contains(&act_id) && seen_ids.insert(descriptor.id.clone()) {
                        active_skills.push(SkillActivatedSnapshot {
                            activation_id: act_id,
                            descriptor: descriptor.clone(),
                            instructions_hash: instructions_hash.clone(),
                            instructions: instructions.clone(),
                        });
                    }
                }
                EntryValue::SkillResourceRead {
                    activation_id,
                    skill_id,
                    resource_path,
                    start_line,
                    line_count,
                    content_hash,
                    content,
                } => {
                    skill_resources.push(SkillResourceSnapshot {
                        activation_id: activation_id.clone(),
                        skill_id: skill_id.clone(),
                        resource_path: resource_path.clone(),
                        start_line: *start_line,
                        line_count: *line_count,
                        content_hash: content_hash.clone(),
                        content: content.clone(),
                    });
                }
                EntryValue::Compaction {
                    active_skills: comp_skills,
                    skill_resources: comp_res,
                    first_kept,
                    ..
                } if !saw_compaction => {
                    // The nearest compaction snapshot replaces all older
                    // snapshots in its range, just like its model-visible
                    // summary. Kept-range events are still traversed normally.
                    saw_compaction = true;
                    // The ancestry walk is newest-to-oldest, while cached
                    // skills are stored oldest-to-newest. Push this boundary
                    // in reverse so the final reversal restores chronology.
                    for skill in comp_skills.iter().rev() {
                        if !deactivated.contains(&skill.activation_id)
                            && seen_ids.insert(skill.descriptor.id.clone())
                        {
                            active_skills.push(skill.clone());
                        }
                    }
                    for res in comp_res {
                        skill_resources.push(res.clone());
                    }
                    boundary = self
                        .entry(first_kept)
                        .and_then(|entry| entry.parent.as_ref());
                }
                _ => {}
            }
            cursor = entry.parent.as_ref();
        }

        let active_activation_ids: std::collections::HashSet<crate::skills::SkillActivationId> =
            active_skills
                .iter()
                .map(|s| s.activation_id.clone())
                .collect();

        skill_resources.retain(|r| active_activation_ids.contains(&r.activation_id));
        active_skills.reverse();

        Ok(ActiveSkillState {
            active_skills,
            skill_resources,
        })
    }
}

/// Maximum bytes retained in one durable partial-assistant frame journal.
///
/// The journal is a bounded, disposable recovery aid: once the bound is
/// reached the prefix already written is kept and later frames are dropped, so
/// a crash mid-stream can never leave an unbounded file behind.
pub const MAX_PARTIAL_FRAME_JOURNAL_BYTES: usize = 1024 * 1024;

/// Maximum frames retained in one durable partial-assistant frame journal.
pub const MAX_PARTIAL_FRAME_JOURNAL_FRAMES: usize = 8192;

/// Durable, bounded journal of [`octet_ai::AssistantMessageFrame`]s for one
/// in-flight assistant attempt.
///
/// Frame deltas are *not* session entries: they never enter provider-visible
/// context and never affect usage, cost, or uncertainty accounting. A journal
/// is an owner-only sidecar file beside the session log that exists only while
/// an attempt is streaming. It is removed at terminal settlement, so a
/// completed turn is never replayed as partial progress.
///
/// Writes are deliberately not `fsync`ed: the journal must survive a killed
/// *process* (the bytes are already in the kernel), not power loss, because it
/// is discarded the moment the authoritative assistant entry is durably
/// appended. The sidecar is never parsed as a session record.
pub struct AssistantFrameJournal {
    path: PathBuf,
    file: File,
    bytes: usize,
    frames: usize,
    bounded: bool,
    settled: bool,
}

impl AssistantFrameJournal {
    /// Records one frame unless the journal's bounds are already reached.
    ///
    /// A bounded journal keeps its existing prefix and silently drops later
    /// frames: a recovery aid must never fail or destabilize the provider
    /// stream it observes.
    pub fn append(&mut self, frame: &octet_ai::AssistantMessageFrame) -> Result<(), SessionError> {
        if self.settled || self.bounded {
            return Ok(());
        }
        let mut line = serde_json::to_vec(frame)
            .map_err(|error| SessionError::Serde(error.to_string()))?;
        line.push(b'\n');
        if self.frames >= MAX_PARTIAL_FRAME_JOURNAL_FRAMES
            || self.bytes.saturating_add(line.len()) > MAX_PARTIAL_FRAME_JOURNAL_BYTES
        {
            self.bounded = true;
            return Ok(());
        }
        self.file.write_all(&line)?;
        self.bytes += line.len();
        self.frames += 1;
        Ok(())
    }

    /// Removes the journal after terminal settlement.
    ///
    /// Called once an attempt reaches its terminal event, so the sequence of
    /// partial frames is never mistaken for an in-flight turn on the next
    /// start. Idempotent.
    pub fn settle(&mut self) {
        if self.settled {
            return;
        }
        self.settled = true;
        let _ = self.file.sync_data();
        // Read from the descriptor we created, then compare-and-delete. A
        // replaced pathname or parent must not redirect cleanup to a new file.
        if self.file.rewind().is_ok() {
            let mut bytes = Vec::new();
            if Read::by_ref(&mut self.file)
                .take((MAX_PARTIAL_FRAME_JOURNAL_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .is_ok()
            {
                let _ = crate::secure_fs::remove_private_file_if_unchanged(
                    &self.path,
                    &bytes,
                    MAX_PARTIAL_FRAME_JOURNAL_BYTES,
                );
            }
        }
    }

    /// Durable sidecar path of this journal.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Bytes written so far.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }

    /// Frames written so far.
    pub fn retained_frames(&self) -> usize {
        self.frames
    }

    /// Whether the journal stopped accepting frames at its bound.
    pub fn is_bounded(&self) -> bool {
        self.bounded
    }
}

impl Session {
    /// Durable sidecar path used for partial-assistant recovery.
    ///
    /// Kept beside the session so a journal and the log it belongs to move
    /// together. It is never a session record and is never replayed as one.
    fn partial_assistant_frames_path(&self) -> Result<PathBuf, SessionError> {
        let name = self.path.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "session has no filename")
        })?;
        let mut name = name.to_os_string();
        name.push(".partial-assistant-frames");
        let parent = self
            .path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        Ok(parent.canonicalize()?.join(name))
    }

    /// Opens a fresh durable journal for one in-flight assistant attempt.
    ///
    /// A stale journal must first be consumed through [`Self::take_partial_assistant`].
    /// Exclusive, descriptor-bound creation never follows links or truncates an
    /// existing target. The file is created owner-only next to the session log.
    pub fn begin_assistant_frame_journal(&mut self) -> Result<AssistantFrameJournal, SessionError> {
        let path = self.partial_assistant_frames_path()?;
        let file = crate::secure_fs::create_regular_file_for_append(&path)
            .map_err(partial_journal_file_error)?;
        Ok(AssistantFrameJournal {
            path,
            file,
            bytes: 0,
            frames: 0,
            bounded: false,
            settled: false,
        })
    }

    /// Consumes any partial assistant turn left by a killed stream.
    ///
    /// Reduces the durable frame prefix into an [`octet_ai::AssistantMessage`]
    /// (partial text/reasoning content only — a partial tool call is not a
    /// result and never becomes one), then removes the journal so a partial is
    /// published exactly once. `Ok(None)` means the last attempt settled
    /// terminally (or never started), so there is no progress to republish.
    pub fn take_partial_assistant(
        &mut self,
    ) -> Result<Option<octet_ai::AssistantMessage>, SessionError> {
        let path = self.partial_assistant_frames_path()?;
        let bytes = match crate::secure_fs::read_private_file_bounded(
            &path,
            MAX_PARTIAL_FRAME_JOURNAL_BYTES,
        ) {
            Ok(bytes) => bytes,
            Err(crate::secure_fs::SecureFileError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                return Ok(None)
            }
            Err(error) => return Err(partial_journal_file_error(error)),
        };
        let frames = read_partial_assistant_frames(&bytes)?;
        // Never acknowledge consumption if cleanup failed or a replacement
        // changed the snapshot. Otherwise a later start could republish it.
        crate::secure_fs::remove_private_file_if_unchanged(
            &path,
            &bytes,
            MAX_PARTIAL_FRAME_JOURNAL_BYTES,
        )
        .map_err(partial_journal_file_error)?;
        if frames.is_empty() {
            return Ok(None);
        }
        octet_ai::reduce_assistant_message_frames(&frames)
            .map_err(|error| SessionError::Serde(error.to_string()))
    }
}

/// Reads a partial-assistant frame journal, keeping the valid prefix.
///
/// A torn final line is the expected crash shape; it is dropped rather than
/// treated as corruption. An oversized file is rejected instead of read.
fn read_partial_assistant_frames(
    bytes: &[u8],
) -> Result<Vec<octet_ai::AssistantMessageFrame>, SessionError> {
    if bytes.len() > MAX_PARTIAL_FRAME_JOURNAL_BYTES {
        return Err(SessionError::Limit(
            "partial assistant frame journal exceeded its byte bound".into(),
        ));
    }
    let mut frames = Vec::new();
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        // Even a syntactically valid final value is uncommitted without the
        // newline; invalid UTF-8 in a torn tail is discarded the same way.
        if !line.ends_with(b"\n") {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        if frames.len() == MAX_PARTIAL_FRAME_JOURNAL_FRAMES {
            return Err(SessionError::Limit(
                "partial assistant frame journal exceeded its frame bound".into(),
            ));
        }
        match serde_json::from_slice::<octet_ai::AssistantMessageFrame>(line) {
            Ok(frame) => frames.push(frame),
            Err(_) => break,
        }
    }
    Ok(frames)
}

fn partial_journal_file_error(error: crate::secure_fs::SecureFileError) -> SessionError {
    match error {
        crate::secure_fs::SecureFileError::Io(error) => SessionError::Io(error),
        crate::secure_fs::SecureFileError::TooLarge { .. } => {
            SessionError::Limit(error.to_string())
        }
        error => SessionError::Io(std::io::Error::other(error)),
    }
}

/// Active skills resolved for a given leaf entry along its branch ancestry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveSkillState {
    /// Ordered snapshots of active skills.
    pub active_skills: Vec<SkillActivatedSnapshot>,
    /// Snapshots of lazy resource reads active at the compaction boundary.
    pub skill_resources: Vec<SkillResourceSnapshot>,
}

fn is_tool_result_turn(m: &UserMessage) -> bool {
    !m.content.is_empty()
        && m.content
            .iter()
            .any(|part| matches!(part, UserPart::ToolResult(_)))
        && m.content
            .iter()
            .all(|part| matches!(part, UserPart::ToolResult(_) | UserPart::Media(_)))
}

/// Adds one persisted tool-result turn to an adjacent one while keeping every
/// provider-paired result ahead of OpenAI Chat's adjacent media messages.
fn merge_tool_result_turn(previous: &mut UserMessage, current: &[UserPart]) {
    let media_start = previous
        .content
        .iter()
        .position(|part| matches!(part, UserPart::Media(_)))
        .unwrap_or(previous.content.len());
    let current_results = current
        .iter()
        .filter(|part| matches!(part, UserPart::ToolResult(_)))
        .cloned();
    drop(
        previous
            .content
            .splice(media_start..media_start, current_results),
    );
    previous.content.extend(
        current
            .iter()
            .filter(|part| matches!(part, UserPart::Media(_)))
            .cloned(),
    );
}

/// Appends one newly persisted message to an already materialized context.
fn append_context_message(messages: &mut Vec<Message>, message: &Message) {
    if let Message::User(current) = message {
        if is_tool_result_turn(current) {
            if let Some(Message::User(previous)) = messages.last_mut() {
                if is_tool_result_turn(previous) {
                    merge_tool_result_turn(previous, &current.content);
                    return;
                }
            }
        }
    }
    messages.push(message.clone());
}

/// Merges consecutive user messages that contain tool results and their
/// protocol-required adjacent media into one user message. All tool results
/// remain ahead of adjacent media so OpenAI Chat serializes every `role:tool`
/// message before the next `role:user` media message. Individual tool results
/// stay individual *entries* on disk; coalescing happens only during context
/// reconstruction.
fn coalesce_tool_results(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        if let Message::User(current) = &message {
            if is_tool_result_turn(current) {
                if let Some(Message::User(previous)) = out.last_mut() {
                    if is_tool_result_turn(previous) {
                        merge_tool_result_turn(previous, &current.content);
                        continue;
                    }
                }
            }
        }
        out.push(message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use octet_ai::{AssistantMessage, AssistantPart, ModelId, Protocol, ToolCallId, ToolResult};

    fn user(text: &str) -> EntryValue {
        EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text(text.to_string())],
        }))
    }

    fn assistant(text: &str) -> EntryValue {
        EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text(text.to_string())],
            model: ModelId("m".to_string()),
            protocol: Protocol::AnthropicMessages,
        }))
    }

    fn responses_assistant(text: &str) -> EntryValue {
        EntryValue::Message(Message::Assistant(AssistantMessage {
            content: vec![AssistantPart::Text(text.to_string())],
            model: ModelId("m".to_string()),
            protocol: Protocol::OpenAiResponses,
        }))
    }

    fn responses_output(id: &str) -> octet_ai::ResponsesOutput {
        octet_ai::ResponsesOutput::new(vec![octet_ai::ResponsesItem::new(serde_json::json!({
            "type": "message",
            "id": id,
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": id,
                "annotations": []
            }],
            "unknown": {"preserved": true}
        }))
        .unwrap()])
    }

    fn responses_compact_output(id: &str) -> octet_ai::ResponsesOutput {
        octet_ai::ResponsesOutput::new(vec![
            octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "message",
                "id": format!("leading-{id}")
            }))
            .unwrap(),
            octet_ai::ResponsesItem::new(serde_json::json!({
                "type": "compaction",
                "id": id,
                "encrypted_content": "opaque"
            }))
            .unwrap(),
        ])
    }

    fn tool_result(call_id: &str, text: &str) -> EntryValue {
        EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::ToolResult(ToolResult {
                tool_call_id: ToolCallId(call_id.to_string()),
                content: vec![octet_ai::ToolResultPart::Text(text.to_string())],
                is_error: false,
                added_tool_names: None,
            })],
        }))
    }

    fn text_of(m: &Message) -> String {
        match m {
            Message::User(u) => u
                .content
                .iter()
                .map(|p| match p {
                    UserPart::Text(t) => t.clone(),
                    UserPart::ToolResult(r) => format!("result:{}", r.tool_call_id.0),
                    UserPart::Media(_) => "media".to_string(),
                })
                .collect::<Vec<_>>()
                .join("|"),
            Message::Assistant(a) => a
                .content
                .iter()
                .map(|p| match p {
                    AssistantPart::Text(t) => t.clone(),
                    _ => "other".to_string(),
                })
                .collect::<Vec<_>>()
                .join("|"),
        }
    }

    fn temp_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("session.jsonl")
    }

    fn skill_descriptor(id: &str) -> crate::skills::SkillDescriptor {
        crate::skills::SkillDescriptor {
            id: id.to_string(),
            name: id.to_string(),
            description: String::new(),
            license: None,
            compatibility: None,
            metadata: Default::default(),
            allowed_tools: vec![],
            disable_model_invocation: false,
            version: None,
            source: crate::skills::SkillSource::BuiltIn,
            trust: crate::skills::SkillTrust::BuiltIn,
            required_tools: Vec::new(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn prompt_metadata_persists_safe_identity_and_exact_normalized_color() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let valid = session
            .append_with_metadata(
                user("valid"),
                Some(EntryMetadata {
                    prompt_model: Some(ModelId("custom/model-a".into())),
                    prompt_model_source: Some("  deepseek  ".into()),
                    prompt_color: Some("  #22AACC  ".into()),
                    display_text: Some("visible\ndraft".into()),
                    run_outcome: None,
                    tool_output: None,
                    tool_started_unix_ms: None,
                    tool_finished_unix_ms: None,
                    local_synthetic_assistant: false,
                    extension_metadata: Default::default(),
                }),
            )
            .unwrap();
        let invalid = session
            .append_with_metadata(
                user("invalid"),
                Some(EntryMetadata {
                    prompt_model: Some(ModelId("model\u{1b}[31m".into())),
                    prompt_model_source: Some("#2243e6".into()),
                    prompt_color: Some("rgb(1,2,3)\u{1b}".into()),
                    display_text: Some("bad\u{1b}".into()),
                    run_outcome: None,
                    tool_output: None,
                    tool_started_unix_ms: None,
                    tool_finished_unix_ms: None,
                    local_synthetic_assistant: false,
                    extension_metadata: Default::default(),
                }),
            )
            .unwrap();
        drop(session);

        let session = Session::open(&path).unwrap();
        assert_eq!(
            session.entry(&valid).unwrap().metadata,
            Some(EntryMetadata {
                prompt_model: Some(ModelId("custom/model-a".into())),
                prompt_model_source: Some("deepseek".into()),
                prompt_color: Some("#22aacc".into()),
                display_text: Some("visible\ndraft".into()),
                run_outcome: None,
                tool_output: None,
                tool_started_unix_ms: None,
                tool_finished_unix_ms: None,
                local_synthetic_assistant: false,
                extension_metadata: Default::default(),
            })
        );
        assert_eq!(session.entry(&invalid).unwrap().metadata, None);
        let persisted = std::fs::read_to_string(path).unwrap();
        assert!(!persisted.contains("#2243e6"));
        assert!(persisted.contains("#22aacc"));
        assert!(!persisted.contains("[31m"));
    }

    #[test]
    fn structured_tool_output_details_survive_reopen_without_entering_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let details = crate::tool::ToolOutputDetails::try_new(
            Some(serde_json::json!({
                "sources": [{"title": "Primary", "url": "https://example.test"}]
            })),
            Some(serde_json::json!({"cache": "miss", "elapsed_ms": 12})),
        )
        .unwrap();
        let id = session
            .append_with_metadata(
                EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        tool_call_id: ToolCallId("call-structured".into()),
                        content: vec![octet_ai::ToolResultPart::Text("Found one source.".into())],
                        is_error: false,
                        added_tool_names: None,
                    })],
                })),
                Some(EntryMetadata {
                    tool_output: Some(details.clone()),
                    ..EntryMetadata::default()
                }),
            )
            .unwrap();
        let invalid_target = session
            .append_with_metadata(
                user("ordinary user message"),
                Some(EntryMetadata {
                    tool_output: Some(details.clone()),
                    ..EntryMetadata::default()
                }),
            )
            .unwrap();
        drop(session);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened
                .entry(&id)
                .and_then(|entry| entry.metadata.as_ref())
                .and_then(|metadata| metadata.tool_output.as_ref()),
            Some(&details)
        );
        assert_eq!(reopened.entry(&invalid_target).unwrap().metadata, None);
        let context = reopened.context().unwrap();
        let Message::User(message) = &context[0] else {
            panic!("expected user tool-result message");
        };
        let UserPart::ToolResult(result) = &message.content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(result.content.len(), 1);
        let persisted = std::fs::read_to_string(path).unwrap();
        assert!(persisted.contains("structured_content"));
        assert!(persisted.contains("elapsed_ms"));
    }

    #[test]
    fn explicit_null_structured_tool_output_survives_session_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let details =
            crate::tool::ToolOutputDetails::try_new(Some(serde_json::Value::Null), None).unwrap();
        let id = session
            .append_with_metadata(
                EntryValue::Message(Message::User(UserMessage {
                    content: vec![UserPart::ToolResult(ToolResult {
                        tool_call_id: ToolCallId("call-null".into()),
                        content: vec![octet_ai::ToolResultPart::Text("No value.".into())],
                        is_error: false,
                        added_tool_names: None,
                    })],
                })),
                Some(EntryMetadata {
                    tool_output: Some(details),
                    ..EntryMetadata::default()
                }),
            )
            .unwrap();
        drop(session);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(
            reopened
                .entry(&id)
                .and_then(|entry| entry.metadata.as_ref())
                .and_then(|metadata| metadata.tool_output.as_ref())
                .and_then(crate::tool::ToolOutputDetails::structured_content),
            Some(&serde_json::Value::Null)
        );
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .contains("\"structured_content\":null"));
    }

    #[test]
    fn run_outcome_marker_is_durable_and_not_model_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.append(user("question")).unwrap();
        session.append(assistant("answer")).unwrap();
        let outcome_id = session
            .append_run_outcome(SessionRunOutcome {
                status: SessionRunOutcomeStatus::Failed,
                message: Some("bounded failure".into()),
            })
            .unwrap();
        drop(session);

        let session = Session::open(&path).unwrap();
        let marker = session.entry(&outcome_id).expect("outcome marker");
        assert!(matches!(
            marker.value,
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            }
        ));
        assert_eq!(
            marker
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.run_outcome.as_ref()),
            Some(&SessionRunOutcome {
                status: SessionRunOutcomeStatus::Failed,
                message: Some("bounded failure".into()),
            })
        );
        assert_eq!(session.context().unwrap().len(), 2);
    }

    #[test]
    fn prompt_colors_are_immutable_across_checkout_branch_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let first = session
            .append_with_metadata(
                user("first model"),
                Some(EntryMetadata {
                    prompt_model: Some(ModelId("model-a".into())),
                    prompt_color: Some("#123456".into()),
                    ..EntryMetadata::default()
                }),
            )
            .unwrap();
        let abandoned = session.append(assistant("old branch")).unwrap();
        session.checkout(first.clone()).unwrap();
        let second = session
            .append_with_metadata(
                user("second model"),
                Some(EntryMetadata {
                    prompt_model: Some(ModelId("model-b".into())),
                    prompt_color: Some("#abcdef".into()),
                    ..EntryMetadata::default()
                }),
            )
            .unwrap();
        assert_ne!(session.head(), Some(abandoned));
        drop(session);

        let session = Session::open(path).unwrap();
        assert_eq!(
            session
                .entry(&first)
                .and_then(|entry| entry.metadata.as_ref())
                .and_then(|metadata| metadata.prompt_color.as_deref()),
            Some("#123456")
        );
        assert_eq!(
            session
                .entry(&second)
                .and_then(|entry| entry.metadata.as_ref())
                .and_then(|metadata| metadata.prompt_color.as_deref()),
            Some("#abcdef")
        );
    }

    #[test]
    fn create_append_reopen_and_reconstruct() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        let e1 = s.append(user("hello")).unwrap();
        let e2 = s.append(assistant("hi there")).unwrap();
        assert_eq!(s.head(), Some(e2.clone()));
        assert_eq!(s.entries()[1].parent, Some(e1.clone()));
        drop(s);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.head(), Some(e2));
        let ctx = reopened.context().unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(text_of(&ctx[0]), "hello");
        assert_eq!(text_of(&ctx[1]), "hi there");
    }

    #[test]
    fn caller_supplied_descriptor_is_not_reopened_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let moved = dir.path().join("authorized.jsonl");

        let mut original = Session::create(&path).unwrap();
        original.append(user("authorized")).unwrap();
        drop(original);
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .unwrap();

        std::fs::rename(&path, &moved).unwrap();
        let mut replacement = Session::create(&path).unwrap();
        replacement.append(user("replacement")).unwrap();
        drop(replacement);

        let mut adopted = Session::open_with_file(&path, file).unwrap();
        assert_eq!(text_of(&adopted.context().unwrap()[0]), "authorized");
        adopted.append(assistant("bound descriptor")).unwrap();
        drop(adopted);

        let authorized = Session::open(&moved).unwrap();
        assert_eq!(
            text_of(&authorized.context().unwrap()[1]),
            "bound descriptor"
        );
        let replacement = Session::open(&path).unwrap();
        assert_eq!(replacement.context().unwrap().len(), 1);
        assert_eq!(text_of(&replacement.context().unwrap()[0]), "replacement");
    }

    #[test]
    fn cache_key_is_stable_and_path_scoped() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let first_path = first_dir.path().join("session.jsonl");
        let second_path = second_dir.path().join("session.jsonl");
        let first = Session::create(&first_path).unwrap();
        let first_key = first.cache_key();
        let first_owner = first.resource_owner_key();
        assert_eq!(first_key, first.cache_key());
        assert_eq!(first_owner, first.resource_owner_key());
        assert_eq!(first_owner.len(), "session-".len() + 64);
        drop(first);
        let reopened = Session::open(&first_path).unwrap();
        assert_eq!(first_key, reopened.cache_key());
        assert_eq!(first_owner, reopened.resource_owner_key());
        let other = Session::create(&second_path).unwrap();
        assert_ne!(first_key, other.cache_key());
        assert_ne!(first_owner, other.resource_owner_key());
    }

    #[test]
    fn create_refuses_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        std::fs::write(&path, "").unwrap();
        assert!(matches!(Session::create(&path), Err(SessionError::Io(_))));
    }

    #[cfg(unix)]
    #[test]
    fn newly_created_session_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let _session = Session::create(&path).unwrap();
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn append_seeks_to_the_durable_end_on_writable_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        let mut session = Session::create_with_file(&path, file).unwrap();

        session.append(user("first")).unwrap();
        session.file.seek(std::io::SeekFrom::Start(0)).unwrap();
        session.append(user("second")).unwrap();
        drop(session);

        let reopened = Session::open_read_only(path).unwrap();
        let context = reopened.context().unwrap();
        assert_eq!(context.len(), 2);
        assert_eq!(text_of(&context[0]), "first");
        assert_eq!(text_of(&context[1]), "second");
    }

    #[test]
    fn stale_handle_cannot_append_a_duplicate_entry_id() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let original = Session::create(&path).unwrap();
        let mut stale = Session::open(&path).unwrap();
        let mut current = original;

        assert_eq!(
            current.append(user("first")).unwrap(),
            EntryId("001".into())
        );
        assert!(matches!(
            stale.append(user("stale")),
            Err(SessionError::ConcurrentModification)
        ));

        drop(stale);
        drop(current);
        let reopened = Session::open(path).unwrap();
        assert_eq!(reopened.entries().len(), 1);
        assert_eq!(reopened.head(), Some(EntryId("001".into())));
    }

    #[test]
    fn append_preflights_the_file_limit_without_partial_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let near_limit = MAX_SESSION_FILE_BYTES - 1;
        session.file.set_len(near_limit).unwrap();
        session.writer.state.lock().unwrap().len = near_limit;
        let before_next_id = session.next_id;
        let before_records = session.writer.state.lock().unwrap().records;

        let error = session.append(user("must not be written")).unwrap_err();

        assert!(matches!(error, SessionError::Limit(_)), "{error}");
        assert_eq!(session.file.metadata().unwrap().len(), near_limit);
        assert_eq!(session.writer.state.lock().unwrap().len, near_limit);
        assert_eq!(session.writer.state.lock().unwrap().records, before_records);
        assert_eq!(session.next_id, before_next_id);
        assert!(session.entries.is_empty());
        assert!(session.index.is_empty());
        assert!(session.head.is_none());
    }

    #[test]
    fn append_preflights_the_record_limit_without_partial_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.writer.state.lock().unwrap().records = MAX_SESSION_RECORDS - 1;
        let before_next_id = session.next_id;

        let error = session
            .append(user("two records would exceed the limit"))
            .unwrap_err();

        assert!(matches!(error, SessionError::Limit(_)), "{error}");
        assert_eq!(session.file.metadata().unwrap().len(), 0);
        assert_eq!(session.writer.state.lock().unwrap().len, 0);
        assert_eq!(
            session.writer.state.lock().unwrap().records,
            MAX_SESSION_RECORDS - 1
        );
        assert_eq!(session.next_id, before_next_id);
        assert!(session.entries.is_empty());
        assert!(session.index.is_empty());
        assert!(session.head.is_none());
    }

    #[test]
    fn missing_newline_repair_never_grows_past_the_file_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.append(user("valid unterminated record")).unwrap();
        drop(session);

        let mut unterminated = std::fs::read(&path).unwrap();
        assert_eq!(unterminated.pop(), Some(b'\n'));
        std::fs::write(&path, &unterminated).unwrap();
        let exact_limit = u64::try_from(unterminated.len()).unwrap();

        let error =
            Session::open_impl_with_limits(path.clone(), true, exact_limit, MAX_SESSION_RECORDS)
                .unwrap_err();
        assert!(matches!(error, SessionError::Limit(_)), "{error}");
        assert!(error.to_string().contains("repair would grow session"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            unterminated,
            "a rejected repair must leave the source bytes untouched"
        );

        let repaired = Session::open_impl_with_limits(
            path.clone(),
            true,
            exact_limit + 1,
            MAX_SESSION_RECORDS,
        )
        .unwrap();
        drop(repaired);
        let repaired = std::fs::read(path).unwrap();
        assert_eq!(repaired.len() as u64, exact_limit + 1);
        assert_eq!(repaired.last(), Some(&b'\n'));
    }

    #[test]
    fn maximum_numeric_entry_id_is_corruption_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.append(user("ordinary")).unwrap();
        drop(session);
        let original = std::fs::read_to_string(&path).unwrap();
        let mut corrupt = original
            .replace("\"001\"", &format!("\"{}\"", u64::MAX))
            .into_bytes();
        assert_eq!(corrupt.pop(), Some(b'\n'));
        std::fs::write(&path, &corrupt).unwrap();

        let opened = std::panic::catch_unwind(|| Session::open(&path));
        assert!(opened.is_ok(), "opening a corrupt ID must never unwind");
        let error = opened.unwrap().unwrap_err();
        assert!(matches!(error, SessionError::Corrupt { .. }), "{error}");
        assert!(error.to_string().contains("exhausts the u64 ID space"));
        assert_eq!(
            std::fs::read(path).unwrap(),
            corrupt,
            "semantic corruption must be rejected before tail repair mutates bytes"
        );
    }

    #[test]
    fn durable_head_survives_reopen_after_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        let e1 = s.append(user("one")).unwrap();
        let _e2 = s.append(assistant("two")).unwrap();
        s.checkout(e1.clone()).unwrap();
        drop(s);

        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.head(), Some(e1));
        let ctx = reopened.context().unwrap();
        assert_eq!(ctx.len(), 1);
        assert_eq!(text_of(&ctx[0]), "one");
    }

    #[test]
    fn checkout_ancestor_and_continue_forms_branch_preserving_old_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        let e1 = s.append(user("root")).unwrap();
        let e2 = s.append(assistant("branch-a")).unwrap();
        s.checkout(e1.clone()).unwrap();
        let e3 = s.append(assistant("branch-b")).unwrap();

        // The new entry forks from the ancestor, the old branch is intact.
        assert_eq!(s.entry(&e3).unwrap().parent, Some(e1.clone()));
        assert_eq!(s.entry(&e2).unwrap().parent, Some(e1));
        assert_eq!(s.entries().len(), 3);

        let ctx = s.context().unwrap();
        assert_eq!(ctx.len(), 2);
        assert_eq!(text_of(&ctx[1]), "branch-b");

        // Reopen: both branches still present, head on the new branch.
        drop(s);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.entries().len(), 3);
        assert_eq!(reopened.head(), Some(e3));
    }

    #[test]
    fn checkout_root_and_continue_preserves_the_original_root_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut session = Session::create(&path).unwrap();
        let original_user = session.append(user("original prompt")).unwrap();
        let original_assistant = session.append(assistant("original answer")).unwrap();
        session.checkout_root().unwrap();
        let edited_user = session.append(user("edited prompt")).unwrap();

        assert_eq!(session.entry(&original_user).unwrap().parent, None);
        assert_eq!(
            session.entry(&original_assistant).unwrap().parent,
            Some(original_user.clone())
        );
        assert_eq!(session.entry(&edited_user).unwrap().parent, None);
        assert_eq!(session.entries().len(), 3);
        assert_eq!(session.context().unwrap().len(), 1);
        assert_eq!(
            text_of(&session.context().unwrap()[0]),
            "edited prompt",
            "the active branch starts at the edited root"
        );

        drop(session);
        let reopened = Session::open(path).unwrap();
        assert_eq!(reopened.entries().len(), 3);
        assert_eq!(reopened.head(), Some(edited_user));
        assert!(reopened.entry(&original_assistant).is_some());
    }

    #[test]
    fn fork_to_copies_only_the_selected_committed_ancestor_chain() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.jsonl");
        let fork_path = dir.path().join("fork.jsonl");
        let mut source = Session::create(&source_path).unwrap();

        let root = source.append(user("root")).unwrap();
        let selected = source.append(assistant("selected answer")).unwrap();
        let later = source.append(user("later work")).unwrap();
        source.checkout(root.clone()).unwrap();
        let sibling = source.append(assistant("sibling answer")).unwrap();

        let mut fork = source.fork_to(&fork_path, Some(&selected)).unwrap();
        assert_eq!(fork.head(), Some(selected.clone()));
        assert_eq!(fork.entries().len(), 2);
        assert!(fork.entry(&root).is_some());
        assert!(fork.entry(&selected).is_some());
        assert!(fork.entry(&later).is_none());
        assert!(fork.entry(&sibling).is_none());
        let context = fork.context().unwrap();
        assert_eq!(context.len(), 2);
        assert_eq!(text_of(&context[0]), "root");
        assert_eq!(text_of(&context[1]), "selected answer");

        let continuation = fork.append(user("fork-only continuation")).unwrap();
        assert_eq!(
            fork.entry(&continuation).unwrap().parent,
            Some(selected.clone())
        );
        drop(fork);
        let reopened = Session::open(fork_path).unwrap();
        assert_eq!(reopened.head(), Some(continuation));
        assert_eq!(reopened.entries().len(), 3);

        assert_eq!(source.head(), Some(sibling));
        assert_eq!(source.entries().len(), 4);
    }

    #[test]
    fn fork_to_stops_at_the_compaction_boundary_and_reroots_the_span() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.jsonl");
        let fork_path = dir.path().join("fork.jsonl");
        let mut source = Session::create(&source_path).unwrap();

        let root = source.append(user("root")).unwrap();
        let first_answer = source.append(assistant("first answer")).unwrap();
        let first_kept = source.append(user("second prompt")).unwrap();
        source.append(assistant("second answer")).unwrap();
        source.append(user("third prompt")).unwrap();
        source.append(assistant("third answer")).unwrap();
        let compaction = source
            .compact("summary of the replaced history", first_kept.clone())
            .unwrap();

        let fork = source.fork_to(&fork_path, Some(&compaction)).unwrap();
        assert_eq!(fork.head(), Some(compaction.clone()));
        // Only the retained span plus the boundary itself was copied.
        assert!(fork.entry(&root).is_none());
        assert!(fork.entry(&first_answer).is_none());
        assert!(fork.entry(&first_kept).is_some());
        assert_eq!(fork.entries().len(), 5);
        // The root-side entry of the retained span was detached from the
        // replaced history and re-rooted in the fork.
        assert_eq!(
            fork.entry(&first_kept).unwrap().parent,
            None,
            "retained span must be re-rooted"
        );

        // The fork replays exactly like the source: summary, then the span.
        let texts = |messages: &[Message]| messages.iter().map(text_of).collect::<Vec<_>>();
        let fork_context = fork.context().unwrap();
        assert_eq!(
            texts(&fork_context),
            texts(&source.context().unwrap()),
            "fork context must match the source context"
        );
        assert_eq!(
            texts(&fork_context),
            vec![
                "[summary of earlier conversation]\nsummary of the replaced history".to_string(),
                "second prompt".to_string(),
                "second answer".to_string(),
                "third prompt".to_string(),
                "third answer".to_string(),
            ]
        );

        // The re-rooted span survives a reopen: the file validates on its own.
        drop(fork);
        let reopened = Session::open(&fork_path).unwrap();
        assert_eq!(reopened.head(), Some(compaction));
        assert_eq!(reopened.entries().len(), 5);
        assert_eq!(
            reopened.entry(&first_kept).unwrap().parent,
            None,
            "re-rooting must be durable"
        );
    }

    #[test]
    fn fork_to_stops_at_the_oldest_compaction_on_the_chain() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.jsonl");
        let fork_path = dir.path().join("fork.jsonl");
        let mut source = Session::create(&source_path).unwrap();

        let root = source.append(user("root")).unwrap();
        source.append(assistant("first answer")).unwrap();
        let first_kept = source.append(user("second prompt")).unwrap();
        source.append(assistant("second answer")).unwrap();
        source.append(user("third prompt")).unwrap();
        let first_compaction = source.compact("first summary", first_kept.clone()).unwrap();
        let second_kept = source.append(user("fourth prompt")).unwrap();
        source.append(assistant("fourth answer")).unwrap();
        // A second, newer compaction whose boundary is the prompt appended
        // after the first one. Its replaced span subsumes the first boundary.
        let second_compaction = source
            .compact("second summary", second_kept.clone())
            .unwrap();

        let fork = source
            .fork_to(&fork_path, Some(&second_compaction))
            .unwrap();
        assert_eq!(fork.head(), Some(second_compaction));
        // The walk stops at the oldest boundary on the chain, so the first
        // compaction and its retained span are not copied: the newer summary
        // already subsumes them.
        assert!(fork.entry(&first_compaction).is_none());
        assert!(fork.entry(&first_kept).is_none());
        assert!(fork.entry(&root).is_none());
        assert_eq!(fork.entries().len(), 3);
        // The retained prompt's source-side parent is the first compaction,
        // which was not copied, so it must be re-rooted.
        assert_eq!(
            fork.entry(&second_kept).unwrap().parent,
            None,
            "retained span must be re-rooted"
        );
    }

    #[test]
    fn fork_to_with_none_creates_an_empty_session() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("source.jsonl");
        let fork_path = dir.path().join("fork.jsonl");
        let mut source = Session::create(&source_path).unwrap();
        source.append(user("root")).unwrap();

        let mut fork = source.fork_to(&fork_path, None).unwrap();
        assert!(fork.head().is_none());
        assert!(fork.entries().is_empty());

        // The empty fork continues as a fresh root branch.
        let new_root = fork.append(user("fresh start")).unwrap();
        assert_eq!(
            fork.entry(&new_root).unwrap().parent,
            None,
            "first entry of an empty fork is a root"
        );
        drop(fork);
        let reopened = Session::open(&fork_path).unwrap();
        assert_eq!(reopened.entries().len(), 1);
        assert_eq!(reopened.head(), Some(new_root));
    }

    fn record_unknown_attempt(session: &mut Session) -> Result<(), SessionError> {
        session.record_usage_uncertainty(
            EndpointId("codex".into()),
            ModelId("openai/gpt-5.4".into()),
            "assistant_turn",
        )
    }

    #[test]
    fn usage_uncertainty_serializes_without_fictional_usage_or_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        assert!(!session.has_uncertain_usage());
        record_unknown_attempt(&mut session).unwrap();
        let bytes = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({
                "type": "usage_uncertainty",
                "record": {
                    "endpoint": "codex",
                    "model": "openai/gpt-5.4",
                    "operation": "assistant_turn"
                }
            })
        );
        let record: SessionRecord = serde_json::from_str(&bytes).unwrap();
        let SessionRecord::UsageUncertainty { record } = record else {
            panic!("expected uncertainty, not known usage");
        };
        assert_eq!(session.usage_uncertainty_records(), &[record]);
        assert!(session.head().is_none());
        assert!(session.entries().is_empty());
        assert!(session.usage_records().is_empty());
        assert!(session.context().unwrap().is_empty());
        let reopened = Session::open_read_only(&path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(
            reopened.usage_uncertainty_records(),
            session.usage_uncertainty_records()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn usage_uncertainty_survives_success_checkpoint_checkout_and_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let prompt = session.append(user("first prompt")).unwrap();
        let completed = session.append(assistant("first completion")).unwrap();
        session.add_cost(17).unwrap();
        let checkpoint = session.checkpoint(prompt.clone()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let context = serde_json::to_value(&*session.context().unwrap()).unwrap();
        record_unknown_attempt(&mut session).unwrap();
        record_unknown_attempt(&mut session).unwrap();
        assert!(std::fs::read(&path).unwrap().starts_with(&before));
        assert_eq!(
            serde_json::to_value(&*session.context().unwrap()).unwrap(),
            context
        );
        assert_eq!(session.head(), Some(completed));
        assert!(session.usage_records().is_empty());
        // A later completed response does not erase earlier accepted exposure.
        let later = session.append(assistant("replacement completed")).unwrap();
        session
            .record_assistant_usage(
                later.clone(),
                EndpointId("codex".into()),
                ModelId("m".into()),
                Usage {
                    total_tokens: 31,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session.checkpoint(prompt.clone()).unwrap();
        session.compact("summary", later).unwrap();
        assert!(session.has_uncertain_usage());
        session.checkout(checkpoint.head).unwrap();
        assert!(session.has_uncertain_usage());
        session.restore_checkpoint(&prompt).unwrap();
        assert!(session.has_uncertain_usage());
        session.checkout_root().unwrap();
        assert!(session.has_uncertain_usage());
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(reopened.head().is_none());
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.usage_uncertainty_records().len(), 2);
        assert_eq!(reopened.usage_records().len(), 1);
        assert_eq!(reopened.usage_records()[0].usage.total_tokens, 31);
        assert_eq!(reopened.total_cost_microdollars(), 17);
        assert_eq!(reopened.total_cost_picodollars_remainder(), 0);
    }

    #[test]
    fn usage_uncertainty_legacy_sessions_remain_certain() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let prompt = session.append(user("legacy prompt")).unwrap();
        session.append(assistant("legacy completion")).unwrap();
        session.checkpoint(prompt).unwrap();
        drop(session);
        let reopened = Session::open(path).unwrap();
        assert!(!reopened.has_uncertain_usage());
        assert!(reopened.usage_uncertainty_records().is_empty());
    }

    #[test]
    fn usage_uncertainty_fork_projection_starts_independent_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut source = Session::create(&path).unwrap();
        let head = source.append(user("forkable prompt")).unwrap();
        source.add_cost(17).unwrap();
        record_unknown_attempt(&mut source).unwrap();
        let fork_path = dir.path().join("fork.jsonl");
        let fork = source.fork_to(&fork_path, Some(&head)).unwrap();
        assert_eq!(fork.head(), Some(head));
        assert!(!fork.has_uncertain_usage());
        assert_eq!(fork.total_cost_microdollars(), 0);
        assert!(fork.usage_records().is_empty());
        assert!(source.has_uncertain_usage());
        assert!(Session::open_read_only(&path)
            .unwrap()
            .has_uncertain_usage());
        assert!(!Session::open_read_only(&fork_path)
            .unwrap()
            .has_uncertain_usage());
    }

    #[test]
    fn usage_uncertainty_append_failures_leave_memory_and_disk_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut writer = Session::create(&path).unwrap();
        let mut stale = Session::open(&path).unwrap();
        writer.append(user("concurrent writer")).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            record_unknown_attempt(&mut stale),
            Err(SessionError::ConcurrentModification)
        ));
        assert!(!stale.has_uncertain_usage());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let mut read_only = Session::open_read_only(&path).unwrap();
        assert!(record_unknown_attempt(&mut read_only).is_err());
        assert!(!read_only.has_uncertain_usage());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        writer.writer.state.lock().unwrap().records = MAX_SESSION_RECORDS;
        assert!(matches!(
            record_unknown_attempt(&mut writer),
            Err(SessionError::Limit(_))
        ));
        assert!(!writer.has_uncertain_usage());
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn usage_uncertainty_identifiers_are_bounded_and_validated_on_replay() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        for invalid in [
            "".to_string(),
            "x".repeat(129),
            "https://host/path".into(),
            "secret?token=value".into(),
            "Authorization: secret".into(),
            "line\nfeed".into(),
        ] {
            for field in 0..3 {
                let mut ids = [
                    "codex".to_string(),
                    "m".to_string(),
                    "assistant_turn".to_string(),
                ];
                ids[field] = invalid.clone();
                let error = session
                    .record_usage_uncertainty(
                        EndpointId(ids[0].clone()),
                        ModelId(ids[1].clone()),
                        ids[2].clone(),
                    )
                    .unwrap_err();
                assert!(matches!(error, SessionError::Limit(_)));
                assert!(!session.has_uncertain_usage());
                assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
                let record = serde_json::json!({"type":"usage_uncertainty", "record": {
                    "endpoint": ids[0], "model": ids[1], "operation": ids[2]
                }});
                std::fs::write(&path, format!("{record}\n")).unwrap();
                assert!(matches!(
                    Session::open_read_only(&path),
                    Err(SessionError::Corrupt { line: 1, .. })
                ));
                std::fs::write(&path, "").unwrap();
            }
        }
        let with_payload = serde_json::json!({"type":"usage_uncertainty", "record": {
            "endpoint":"codex", "model":"m", "operation":"assistant_turn", "body":"forbidden"
        }});
        assert!(serde_json::from_value::<SessionRecord>(with_payload).is_err());
        session
            .record_usage_uncertainty(
                EndpointId("x".repeat(128)),
                ModelId("x".repeat(128)),
                "x".repeat(128),
            )
            .unwrap();
        assert!(Session::open(&path).unwrap().has_uncertain_usage());
    }

    #[test]
    fn usage_uncertainty_survives_repair_of_a_later_torn_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        record_unknown_attempt(&mut session).unwrap();
        drop(session);
        let before = std::fs::read(&path).unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"entry\"")
            .unwrap();
        let reopened = Session::open(&path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.usage_uncertainty_records().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn completed_prompt_checkpoint_round_trips_and_restores_a_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let prompt = session.append(user("make a change")).unwrap();
        let completed = session.append(assistant("done")).unwrap();

        let checkpoint = session.checkpoint(prompt.clone()).unwrap();
        assert_eq!(checkpoint.head, completed);
        assert_eq!(session.head(), Some(completed.clone()));
        session.append(user("later branch")).unwrap();
        drop(session);

        let mut reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.checkpoints(), &[checkpoint]);
        reopened.restore_checkpoint(&prompt).unwrap();
        assert_eq!(reopened.head(), Some(completed.clone()));
        let branch = reopened.append(user("new branch")).unwrap();
        assert_eq!(reopened.entry(&branch).unwrap().parent, Some(completed));
        assert_eq!(
            text_of(reopened.context().unwrap().last().unwrap()),
            "new branch"
        );
    }

    #[test]
    fn checkpoint_telemetry_round_trips_and_follows_the_active_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let first_prompt = session.append(user("first")).unwrap();
        session.append(assistant("first answer")).unwrap();
        let first_usage = Usage {
            input_tokens: 120,
            output_tokens: 30,
            total_tokens: 150,
            ..Usage::default()
        };
        let first = session
            .checkpoint_with_telemetry(first_prompt, Some(first_usage), Some(8_600))
            .unwrap();

        let second_prompt = session.append(user("second")).unwrap();
        session.append(assistant("second answer")).unwrap();
        session
            .checkpoint_with_telemetry(second_prompt, Some(Usage::default()), Some(0))
            .unwrap();
        session.checkout(first.head.clone()).unwrap();
        drop(session);

        let reopened = Session::open(path).unwrap();
        let active = reopened.latest_active_checkpoint().unwrap();
        assert_eq!(active, &first);
        assert_eq!(active.usage, Some(first_usage));
        assert_eq!(active.run_cost_microdollars, Some(8_600));
    }

    #[test]
    fn latest_active_assistant_usage_is_per_request_and_branch_aware() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        let root = session.append(user("root")).unwrap();
        let abandoned = session.append(assistant("abandoned")).unwrap();
        session
            .record_assistant_usage(
                abandoned,
                EndpointId("provider".into()),
                ModelId("m".into()),
                Usage {
                    total_tokens: 900,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session.checkout(root).unwrap();
        session.append(user("active")).unwrap();
        let active = session.append(assistant("active answer")).unwrap();
        session
            .record_assistant_usage(
                active,
                EndpointId("provider".into()),
                ModelId("m".into()),
                Usage {
                    total_tokens: 100,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session
            .record_compaction_usage(
                EndpointId("provider".into()),
                ModelId("m".into()),
                Usage {
                    total_tokens: 500,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();

        assert_eq!(
            session
                .latest_active_assistant_usage()
                .unwrap()
                .usage
                .total_tokens,
            100
        );
    }

    #[test]
    fn per_operation_usage_round_trips_for_turns_and_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.append(user("first")).unwrap();
        let assistant = session.append(assistant("answer")).unwrap();
        let turn_usage = Usage {
            input_tokens: 50,
            cache_read_tokens: 100,
            output_tokens: 20,
            total_tokens: 170,
            ..Usage::default()
        };
        let turn_cost = Cost {
            total: 42,
            total_picodollars_remainder: 600_000,
            ..Cost::default()
        };
        session
            .record_assistant_usage(
                assistant.clone(),
                EndpointId("provider".to_string()),
                ModelId("m".to_string()),
                turn_usage,
                Some(turn_cost),
            )
            .unwrap();
        let compaction_usage = Usage {
            input_tokens: 75,
            output_tokens: 10,
            total_tokens: 85,
            ..Usage::default()
        };
        session
            .record_compaction_usage(
                EndpointId("provider".to_string()),
                ModelId("m".to_string()),
                compaction_usage,
                Some(Cost {
                    total_picodollars_remainder: 600_000,
                    ..Cost::default()
                }),
            )
            .unwrap();
        let expected = session.usage_records().to_vec();
        assert_eq!(expected[0].cost, Some(turn_cost));
        assert_eq!(expected[0].cost_microdollars, Some(42));
        assert_eq!(expected[0].session_cost_microdollars, Some(42));
        assert_eq!(
            expected[0].session_cost_picodollars_remainder,
            Some(600_000)
        );
        assert_eq!(expected[1].session_cost_microdollars, Some(43));
        assert_eq!(
            expected[1].session_cost_picodollars_remainder,
            Some(200_000)
        );
        assert!(expected[0].completed_at_unix_ms.is_some());
        assert_eq!(session.total_cost_microdollars(), 43);
        assert_eq!(session.total_cost_picodollars_remainder(), 200_000);
        drop(session);

        let reopened = Session::open(path).unwrap();
        assert_eq!(reopened.usage_records(), expected);
        assert_eq!(reopened.total_cost_microdollars(), 43);
        assert_eq!(reopened.total_cost_picodollars_remainder(), 200_000);
    }

    #[test]
    fn usage_totals_fold_tool_turns_and_summaries_and_preserve_uncertainty() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        session.append(user("prompt")).unwrap();
        let assistant = session.append(assistant("answer")).unwrap();
        session
            .record_assistant_usage(
                assistant,
                EndpointId("provider".into()),
                ModelId("m".into()),
                Usage {
                    input_tokens: 100,
                    cache_read_tokens: 50,
                    cache_write_tokens: 30,
                    cache_write_1h_tokens: 25,
                    output_tokens: 20,
                    reasoning_tokens: 5,
                    total_tokens: 200,
                },
                None,
            )
            .unwrap();
        session
            .record_compaction_usage(
                EndpointId("provider".into()),
                ModelId("m".into()),
                Usage {
                    input_tokens: 75,
                    output_tokens: 10,
                    total_tokens: 85,
                    ..Usage::default()
                },
                None,
            )
            .unwrap();
        session
            .record_delegated_agent_usage(DelegatedUsage {
                agent_id: "child".into(),
                turn_count: 1,
                tool_call_count: 2,
                endpoint: EndpointId("provider".into()),
                model: ModelId("m".into()),
                usage: Usage {
                    total_tokens: 40,
                    ..Usage::default()
                },
                cost: None,
            })
            .unwrap();

        let totals = crate::telemetry::schema::UsageTotals::from_records(session.usage_records());
        assert_eq!(totals.assistant_records, 1);
        assert_eq!(totals.summary_records, 1);
        assert_eq!(totals.delegated_records, 1);
        assert_eq!(totals.total_tokens, 200 + 85 + 40);
        assert_eq!(totals.own_context_total_tokens, 200 + 85);
        assert_eq!(totals.cache_write_tokens, 30);
        assert_eq!(totals.cache_write_1h_tokens, 25);
        assert_eq!(totals.cache_hit_rate(), Some(50.0 / 255.0));

        // Known usage is only a subtotal: durable uncertainty is preserved and
        // never rewritten as fabricated zero usage.
        assert!(!session.has_uncertain_usage());
        record_unknown_attempt(&mut session).unwrap();
        assert!(session.has_uncertain_usage());
        assert_eq!(
            crate::telemetry::schema::UsageTotals::from_records(session.usage_records()),
            totals,
            "recording uncertainty must not fabricate or alter known totals"
        );
        let durable = session.usage_records().to_vec();
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.usage_records(), durable);
    }

    #[test]
    fn delegated_usage_is_durable_and_contributes_exact_session_cost() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let usage = Usage {
            input_tokens: 1_000,
            cache_read_tokens: 250,
            output_tokens: 80,
            reasoning_tokens: 20,
            total_tokens: 1_330,
            ..Usage::default()
        };
        let cost = Cost {
            input: 10,
            output: 4,
            reasoning: 1,
            cache_read: 1,
            total: 15,
            total_picodollars_remainder: 750_000,
            ..Cost::default()
        };
        session
            .record_delegated_agent_usage(DelegatedUsage {
                agent_id: "agent-1".into(),
                turn_count: 3,
                tool_call_count: 7,
                endpoint: EndpointId("provider".into()),
                model: ModelId("worker-model".into()),
                usage,
                cost: Some(cost),
            })
            .unwrap();
        assert_eq!(session.total_cost_microdollars(), 15);
        assert_eq!(session.total_cost_picodollars_remainder(), 750_000);
        assert!(matches!(
            &session.usage_records()[0].kind,
            UsageRecordKind::DelegatedAgent {
                agent_id,
                turn_count: 3,
                tool_call_count: 7,
            } if agent_id == "agent-1"
        ));
        drop(session);

        let reopened = Session::open(path).unwrap();
        assert_eq!(reopened.total_cost_microdollars(), 15);
        assert_eq!(reopened.usage_records()[0].usage, usage);
        assert_eq!(reopened.usage_records()[0].cost, Some(cost));
    }

    #[test]
    fn checkpoint_rejects_non_user_and_non_ancestor_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        let root = session.append(user("root")).unwrap();
        let old_prompt = session.append(user("old branch")).unwrap();
        let assistant_entry = session.append(assistant("done")).unwrap();
        assert!(matches!(
            session.checkpoint(assistant_entry),
            Err(SessionError::UnknownEntry(_))
        ));
        session.checkout(root).unwrap();
        session.append(user("new branch")).unwrap();
        assert!(matches!(
            session.checkpoint(old_prompt),
            Err(SessionError::NotAncestor(_))
        ));
    }

    #[test]
    fn replay_rejects_a_checkpoint_whose_prompt_is_on_another_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let root = session.append(user("root")).unwrap();
        let abandoned_prompt = session.append(user("abandoned prompt")).unwrap();
        session.append(assistant("abandoned answer")).unwrap();
        session.checkout(root).unwrap();
        session.append(user("active prompt")).unwrap();
        let active_head = session.append(assistant("active answer")).unwrap();
        drop(session);

        let mut bytes = Vec::new();
        write_json_line(
            &mut bytes,
            &SessionRecord::Checkpoint {
                prompt: abandoned_prompt,
                head: active_head,
                usage: None,
                run_cost_microdollars: None,
            },
        )
        .unwrap();
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&bytes)
            .unwrap();

        let error = Session::open(&path).unwrap_err();
        assert!(
            matches!(error, SessionError::Corrupt { line: 12, .. }),
            "{error}"
        );
        assert!(error.to_string().contains("not an ancestor"), "{error}");
    }

    #[test]
    fn replay_validates_many_checkpoints_with_one_linear_ancestry_index() {
        const ENTRY_COUNT: u64 = 4_096;
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut bytes = Vec::new();
        let mut parent = None;
        let root = EntryId("001".into());
        for number in 1..=ENTRY_COUNT {
            let id = EntryId(format!("{number:03}"));
            let entry = Entry {
                id: id.clone(),
                parent: parent.clone(),
                metadata: None,
                timestamp_unix_ms: None,
                value: if number == 1 {
                    user("checkpoint root")
                } else {
                    EntryValue::Config {
                        model: None,
                        reasoning: None,
                        reasoning_mode: None,
                    }
                },
            };
            write_json_line(&mut bytes, &SessionRecordRef::Entry(&entry)).unwrap();
            parent = Some(id);
        }
        let head = parent.unwrap();
        let total_cost = 0u64;
        let remainder = 0u32;
        write_json_line(
            &mut bytes,
            &SessionRecordRef::Head {
                id: &head,
                total_cost_microdollars: &total_cost,
                total_cost_picodollars_remainder: &remainder,
            },
        )
        .unwrap();
        let usage = None;
        let run_cost = None;
        for _ in 0..ENTRY_COUNT {
            write_json_line(
                &mut bytes,
                &SessionRecordRef::Checkpoint {
                    prompt: &root,
                    head: &head,
                    usage: &usage,
                    run_cost_microdollars: &run_cost,
                },
            )
            .unwrap();
        }
        std::fs::write(&path, bytes).unwrap();

        let session = Session::open_read_only(path).unwrap();
        assert_eq!(session.entries().len(), ENTRY_COUNT as usize);
        assert_eq!(session.checkpoints().len(), ENTRY_COUNT as usize);
    }

    #[test]
    fn checkout_of_unknown_entry_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(temp_path(&dir)).unwrap();
        s.append(user("x")).unwrap();
        let err = s.checkout(EntryId("999".to_string())).unwrap_err();
        assert!(matches!(err, SessionError::UnknownEntry(_)));
    }

    #[test]
    fn malformed_parent_reference_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let entry = r#"{"type":"entry","id":"001","parent":"000","value":{"type":"config","model":null,"reasoning":null}}"#;
        std::fs::write(&path, format!("{entry}\n{entry}\n")).unwrap();
        let err = Session::open(&path).unwrap_err();
        assert!(
            matches!(err, SessionError::Corrupt { line: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn incomplete_trailing_record_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        let e1 = s.append(user("kept")).unwrap();
        drop(s);

        // Simulate a torn write: a partial JSON record with no newline.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(br#"{"type":"entry","id":"002","paren"#)
            .unwrap();
        drop(f);

        let mut reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.entries().len(), 1);
        assert_eq!(reopened.head(), Some(e1));
        // The session remains appendable after recovery.
        let e2 = reopened.append(assistant("next")).unwrap();
        assert_eq!(reopened.head(), Some(e2.clone()));
        drop(reopened);

        // Regression: recovery must truncate the torn bytes, so the
        // post-recovery append starts a fresh line — a second reopen must not
        // see a merged/corrupt record.
        let reopened_again = Session::open(&path).unwrap();
        assert_eq!(reopened_again.entries().len(), 2);
        assert_eq!(reopened_again.head(), Some(e2));
        assert!(!std::fs::read_to_string(&path).unwrap().contains("paren\""));
    }

    #[test]
    fn invalid_utf8_in_an_unterminated_final_record_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut session = Session::create(&path).unwrap();
        let durable_head = session.append(user("kept")).unwrap();
        drop(session);
        let durable_len = std::fs::metadata(&path).unwrap().len();

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"text\":\"").unwrap();
        file.write_all(&[0xf0, 0x9f]).unwrap();
        drop(file);
        let torn_bytes = std::fs::read(&path).unwrap();

        let read_only = Session::open_read_only(&path).unwrap();
        assert_eq!(read_only.head(), Some(durable_head.clone()));
        drop(read_only);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            torn_bytes,
            "read-only inspection must not repair the tail"
        );

        let recovered = Session::open(&path).unwrap();
        assert_eq!(recovered.head(), Some(durable_head));
        drop(recovered);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_len);
    }

    #[test]
    fn invalid_utf8_in_a_newline_terminated_record_is_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut session = Session::create(&path).unwrap();
        session.append(user("kept")).unwrap();
        drop(session);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(&[0xff, b'\n']).unwrap();
        drop(file);
        let original = std::fs::read(&path).unwrap();

        let error = Session::open(&path).unwrap_err();
        assert!(
            matches!(error, SessionError::Corrupt { line: 3, .. }),
            "{error}"
        );
        assert!(error.to_string().contains("invalid UTF-8"), "{error}");
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn malformed_newline_terminated_final_record_is_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut session = Session::create(&path).unwrap();
        session.append(user("kept")).unwrap();
        drop(session);

        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"{\"type\":\"entry\"\n").unwrap();
        drop(file);
        let original = std::fs::read(&path).unwrap();

        let read_only_error = Session::open_read_only(&path).unwrap_err();
        assert!(
            matches!(read_only_error, SessionError::Corrupt { line: 3, .. }),
            "{read_only_error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), original);

        let recovery_error = Session::open(&path).unwrap_err();
        assert!(
            matches!(recovery_error, SessionError::Corrupt { line: 3, .. }),
            "{recovery_error}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "completed corrupt records must never be truncated as torn tails"
        );
    }

    #[test]
    fn valid_final_record_without_trailing_newline_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        s.append(user("one")).unwrap();
        let e2 = s.append(assistant("two")).unwrap();
        drop(s);

        // Simulate losing only the final newline of an otherwise complete
        // write: the record is valid and must be kept, not discarded.
        let content = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, content.strip_suffix('\n').unwrap()).unwrap();

        let mut reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.entries().len(), 2);
        assert_eq!(reopened.head(), Some(e2));

        // And the completed newline keeps subsequent appends line-separated.
        let e3 = reopened.append(user("three")).unwrap();
        drop(reopened);
        let reopened_again = Session::open(&path).unwrap();
        assert_eq!(reopened_again.entries().len(), 3);
        assert_eq!(reopened_again.head(), Some(e3));
    }

    #[test]
    fn corruption_before_the_trailing_record_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        s.append(user("a")).unwrap();
        s.append(assistant("b")).unwrap();
        drop(s);

        let content = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<String> = content.lines().map(String::from).collect();
        // Corrupt a completed (non-final) record.
        lines[1] = lines[1][..lines[1].len() / 2].to_string();
        std::fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

        let err = Session::open(&path).unwrap_err();
        assert!(
            matches!(err, SessionError::Corrupt { line: 2, .. }),
            "{err}"
        );
    }

    #[test]
    fn config_entries_persist_but_are_not_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);

        let mut s = Session::create(&path).unwrap();
        s.append(user("hi")).unwrap();
        s.append(EntryValue::Config {
            model: Some("claude".to_string()),
            reasoning: Some("high".to_string()),
            reasoning_mode: None,
        })
        .unwrap();
        s.append(assistant("hello")).unwrap();
        drop(s);

        let reopened = Session::open(&path).unwrap();
        assert!(matches!(
            reopened.entries()[1].value,
            EntryValue::Config { .. }
        ));
        let ctx = reopened.context().unwrap();
        assert_eq!(ctx.len(), 2, "config entries are not model-visible");
    }

    #[test]
    fn active_skills_keep_chronological_order_across_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        for id in ["a", "b"] {
            session
                .append(EntryValue::SkillActivated {
                    descriptor: skill_descriptor(id),
                    instructions_hash: format!("{id}-hash"),
                    instructions: format!("{id}-instructions"),
                })
                .unwrap();
        }
        // Compaction caches the state before this entry: [a, b].
        let first_kept = session.append(user("keep this")).unwrap();
        session.compact("summary", first_kept).unwrap();
        session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor("c"),
                instructions_hash: "c-hash".to_string(),
                instructions: "c-instructions".to_string(),
            })
            .unwrap();

        let state = session
            .resolve_active_skills(&session.head().unwrap())
            .unwrap();
        let ids: Vec<_> = state
            .active_skills
            .iter()
            .map(|skill| skill.descriptor.id.as_str())
            .collect();
        assert_eq!(ids, ["a", "b", "c"]);
    }

    #[test]
    fn deactivating_latest_skill_activation_does_not_resurrect_an_older_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor("audit"),
                instructions_hash: "old-hash".to_string(),
                instructions: "old instructions".to_string(),
            })
            .unwrap();
        let latest = session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor("audit"),
                instructions_hash: "new-hash".to_string(),
                instructions: "new instructions".to_string(),
            })
            .unwrap();
        session
            .append(EntryValue::SkillDeactivated {
                activation_id: latest,
                skill_id: "audit".to_string(),
            })
            .unwrap();

        let state = session
            .resolve_active_skills(&session.head().unwrap())
            .unwrap();
        assert!(state.active_skills.is_empty());
    }

    #[test]
    fn repeated_compaction_uses_only_the_nearest_skill_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        let activation_id = session
            .append(EntryValue::SkillActivated {
                descriptor: skill_descriptor("audit"),
                instructions_hash: "instructions-hash".into(),
                instructions: "audit instructions".into(),
            })
            .unwrap();
        session
            .append(EntryValue::SkillResourceRead {
                activation_id: activation_id.clone(),
                skill_id: "audit".into(),
                resource_path: "reference.txt".into(),
                start_line: None,
                line_count: None,
                content_hash: "resource-hash".into(),
                content: "resource content".into(),
            })
            .unwrap();
        session.append(user("old user")).unwrap();
        let old = session.append(assistant("old assistant")).unwrap();
        session.append(user("recent user")).unwrap();
        let recent = session.append(assistant("recent assistant")).unwrap();

        // Append both markers after the same completed history, matching
        // repeated provider rejection before another assistant can be added.
        session.compact("first summary", old).unwrap();
        session.compact("replacement summary", recent).unwrap();

        let state = session
            .resolve_active_skills(&session.head().unwrap())
            .unwrap();
        assert_eq!(state.active_skills.len(), 1);
        assert_eq!(state.skill_resources.len(), 1);
        assert_eq!(state.skill_resources[0].resource_path, "reference.txt");
    }

    #[test]
    fn compaction_reconstruction_matches_design_example() {
        // Entries: E1, E2, E3, C(first_kept=E2), E5, E6 — context must be
        // [summary, E2, E3, E5, E6].
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(temp_path(&dir)).unwrap();
        let _e1 = s.append(user("E1")).unwrap();
        let e2 = s.append(assistant("E2")).unwrap();
        let _e3 = s.append(user("E3")).unwrap();
        let _c = s.compact("what came before", e2).unwrap();
        let _e5 = s.append(assistant("E5")).unwrap();
        let _e6 = s.append(user("E6")).unwrap();

        let ctx = s.context().unwrap();
        let texts: Vec<String> = ctx.iter().map(text_of).collect();
        assert_eq!(
            texts,
            vec![
                "[summary of earlier conversation]\nwhat came before".to_string(),
                "E2".to_string(),
                "E3".to_string(),
                "E5".to_string(),
                "E6".to_string(),
            ]
        );
    }

    #[test]
    fn successive_compactions_expose_only_the_newest_summary() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        session.append(user("old user")).unwrap();
        session.append(assistant("old assistant")).unwrap();
        session.append(user("middle user")).unwrap();
        let middle = session.append(assistant("middle assistant")).unwrap();
        session.append(user("recent user")).unwrap();
        let recent = session.append(assistant("recent assistant")).unwrap();

        session
            .compact("first overlapping summary", middle)
            .unwrap();
        session
            .compact("replacement summary including prior history", recent)
            .unwrap();

        let texts: Vec<String> = session.context().unwrap().iter().map(text_of).collect();
        assert_eq!(
            texts,
            [
                "[summary of earlier conversation]\nreplacement summary including prior history",
                "recent assistant",
            ]
        );
        assert!(texts.iter().all(|text| !text.contains("first overlapping")));
    }

    #[test]
    fn compact_rejects_non_ancestor_first_kept() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(temp_path(&dir)).unwrap();
        let e1 = s.append(user("root")).unwrap();
        let e2 = s.append(assistant("side")).unwrap();
        s.checkout(e1).unwrap();
        let _e3 = s.append(assistant("main")).unwrap();
        // e2 is on the abandoned branch, not an ancestor of the head.
        let err = s.compact("s", e2).unwrap_err();
        assert!(matches!(err, SessionError::NotAncestor(_)));
    }

    #[test]
    fn compaction_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut s = Session::create(&path).unwrap();
        let e1 = s.append(user("old")).unwrap();
        let e2 = s.append(user("kept")).unwrap();
        assert_eq!(e1.0, "001");
        s.compact("summary text", e2).unwrap();
        drop(s);

        let reopened = Session::open(&path).unwrap();
        let ctx = reopened.context().unwrap();
        let texts: Vec<String> = ctx.iter().map(text_of).collect();
        assert_eq!(
            texts,
            vec![
                "[summary of earlier conversation]\nsummary text".to_string(),
                "kept".to_string(),
            ]
        );
    }

    #[test]
    fn tool_results_persist_individually_and_coalesce_in_context() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(temp_path(&dir)).unwrap();
        s.append(user("do things")).unwrap();
        s.append(assistant("calling tools")).unwrap();
        s.append(tool_result("call_1", "one")).unwrap();
        s.append(tool_result("call_2", "two")).unwrap();

        // Individual persistence: two separate entries on disk.
        assert_eq!(s.entries().len(), 4);

        // Coalesced reconstruction: one user message with both results.
        let ctx = s.context().unwrap();
        assert_eq!(ctx.len(), 3);
        match &ctx[2] {
            Message::User(u) => {
                assert_eq!(u.content.len(), 2);
                assert!(u
                    .content
                    .iter()
                    .all(|p| matches!(p, UserPart::ToolResult(_))));
            }
            _ => panic!("expected coalesced user message"),
        }
    }

    #[test]
    fn plain_user_text_does_not_coalesce_with_tool_results() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Session::create(temp_path(&dir)).unwrap();
        s.append(assistant("calling tool")).unwrap();
        s.append(tool_result("call_1", "one")).unwrap();
        s.append(user("interjection")).unwrap();
        let ctx = s.context().unwrap();
        assert_eq!(ctx.len(), 3);
    }

    #[test]
    fn parallel_tool_results_stay_ahead_of_adjacent_media() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut s = Session::create(&path).unwrap();
        s.append(assistant("calling tools")).unwrap();
        s.append(EntryValue::Message(Message::User(UserMessage {
            content: vec![
                UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call_1".into()),
                    content: vec![],
                    is_error: false,
                    added_tool_names: None,
                }),
                UserPart::Media(octet_ai::Media::image_bytes(
                    bytes::Bytes::from_static(b"first"),
                    "image/png".parse().unwrap(),
                )),
            ],
        })))
        .unwrap();
        s.append(tool_result("call_2", "two")).unwrap();

        let assert_order = |context: Vec<Message>| {
            let Message::User(turn) = context.last().unwrap() else {
                panic!("expected a coalesced user turn");
            };
            assert_eq!(turn.content.len(), 3);
            assert!(matches!(
                &turn.content[0],
                UserPart::ToolResult(result) if result.tool_call_id.0 == "call_1"
            ));
            assert!(matches!(
                &turn.content[1],
                UserPart::ToolResult(result) if result.tool_call_id.0 == "call_2"
            ));
            assert!(matches!(&turn.content[2], UserPart::Media(_)));
        };
        assert_order(s.context().unwrap());

        drop(s);
        assert_order(Session::open(path).unwrap().context().unwrap());
    }

    #[test]
    fn responses_replay_cache_advances_suffix_without_copying_settled_payloads() {
        for turns in [16, 64, 256] {
            let directory = tempfile::tempdir().unwrap();
            let mut session = Session::create(directory.path().join("replay.jsonl")).unwrap();
            let endpoint = EndpointId("responses".into());
            let model = ModelId("m".into());
            session
                .append(user(&"settled prefix".repeat(1024)))
                .unwrap();
            let first = session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap();
            let octet_ai::responses::ResponsesReplayItem::User(first_user) = &first[0] else {
                panic!("user")
            };
            let UserPart::Text(first_text) = &first_user.content[0] else {
                panic!("text")
            };
            let first_text_ptr = first_text.as_ptr();
            drop(first);
            for turn in 0..turns {
                session.append(user("new request")).unwrap();
                session
                    .append_assistant_turn(
                        AssistantMessage {
                            content: vec![AssistantPart::Text("answer".into())],
                            model: model.clone(),
                            protocol: Protocol::OpenAiResponses,
                        },
                        endpoint.clone(),
                        model.clone(),
                        Usage::default(),
                        None,
                        StopReason::EndTurn,
                        Some(responses_output(&format!("output-{turn}"))),
                    )
                    .unwrap();
                let replay = session
                    .responses_replay_snapshot(&endpoint, &model)
                    .unwrap()
                    .unwrap();
                let again = session
                    .responses_replay_snapshot(&endpoint, &model)
                    .unwrap()
                    .unwrap();
                assert!(Arc::ptr_eq(&replay, &again));
                let octet_ai::responses::ResponsesReplayItem::User(first_user) = &replay[0] else {
                    panic!("user")
                };
                let UserPart::Text(first_text) = &first_user.content[0] else {
                    panic!("text")
                };
                assert_eq!(
                    first_text.as_ptr(),
                    first_text_ptr,
                    "settled payload was cloned"
                );
            }
            assert_eq!(session.responses_replay_work.get(), (1, turns * 3));
            let replay = session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap();
            let full = session
                .rebuild_responses_replay(&endpoint, &model)
                .unwrap()
                .unwrap();
            assert_eq!(replay_debug_json(&replay), replay_debug_json(&full));
            // An externally retained snapshot remains immutable on append.
            session.append(user("after snapshot")).unwrap();
            let newer = session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap();
            assert_eq!(newer.len(), replay.len() + 1);
        }
    }

    #[test]
    fn responses_replay_cache_invalidates_routes_branches_compactions_and_repairs_gaps() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("boundaries.jsonl");
        let mut session = Session::create(&path).unwrap();
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        let root = session.append(user("root")).unwrap();
        assert_eq!(
            session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        let assistant = session.append(responses_assistant("answer")).unwrap();
        assert!(session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .is_none());
        session
            .append_responses_turn(
                assistant,
                endpoint.clone(),
                model.clone(),
                responses_output("raw"),
            )
            .unwrap();
        assert_eq!(
            session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap()
                .len(),
            2
        );
        assert!(matches!(
            session.responses_replay_snapshot(&EndpointId("other".into()), &model),
            Err(SessionError::ResponsesRouteMismatch { .. })
        ));
        session
            .append_responses_compaction(
                endpoint.clone(),
                model.clone(),
                responses_compact_output("native"),
            )
            .unwrap();
        let native = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert!(matches!(
            &native[0],
            octet_ai::responses::ResponsesReplayItem::Compacted(_)
        ));
        assert_eq!(native.len(), 1);
        let kept = session.append(user("kept")).unwrap();
        session.compact("local summary", kept).unwrap();
        let local = session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(
            replay_debug_json(&local),
            replay_debug_json(
                &session
                    .rebuild_responses_replay(&endpoint, &model)
                    .unwrap()
                    .unwrap()
            )
        );
        session.checkout(root).unwrap();
        assert_eq!(
            session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        session.checkout_root().unwrap();
        assert!(session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap()
            .is_empty());
        drop(session);
        assert!(Session::open(&path)
            .unwrap()
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn responses_replay_legacy_fallback_does_not_rescan_history_per_turn() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("legacy.jsonl")).unwrap();
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        session.append(user("root")).unwrap();
        session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap();
        session.append(responses_assistant("legacy gap")).unwrap();
        assert!(session
            .responses_replay_snapshot(&endpoint, &model)
            .unwrap()
            .is_none());
        for turn in 0..64 {
            session.append(user("new request")).unwrap();
            let assistant = session.append(responses_assistant("answer")).unwrap();
            session
                .append_responses_turn(
                    assistant,
                    endpoint.clone(),
                    model.clone(),
                    responses_output(&format!("{turn}")),
                )
                .unwrap();
            assert!(session
                .responses_replay_snapshot(&endpoint, &model)
                .unwrap()
                .is_none());
        }
        assert_eq!(session.responses_replay_work.get(), (1, 1 + 64 * 3));
    }

    #[test]
    fn responses_replay_survives_restart_and_uses_only_the_active_branch() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        let mut session = Session::create(&path).unwrap();
        let root = session.append(user("root")).unwrap();
        let abandoned_assistant = session.append(responses_assistant("old")).unwrap();
        session
            .append_responses_turn(
                abandoned_assistant,
                endpoint.clone(),
                model.clone(),
                responses_output("old_raw"),
            )
            .unwrap();

        session.checkout(root).unwrap();
        let active_assistant = session.append(responses_assistant("new")).unwrap();
        session
            .append_responses_turn(
                active_assistant,
                endpoint.clone(),
                model.clone(),
                responses_output("new_raw"),
            )
            .unwrap();
        session.append(user("follow up")).unwrap();
        drop(session);

        let reopened = Session::open(&path).unwrap();
        let replay = reopened
            .responses_replay_items(&endpoint, &model)
            .unwrap()
            .expect("the active branch is fully reconstructible");
        assert_eq!(replay.len(), 3);
        let octet_ai::responses::ResponsesReplayItem::Output(output) = &replay[1] else {
            panic!("assistant must be represented by raw output");
        };
        assert_eq!(output.items()[0].as_json()["id"], "new_raw");
        assert!(serde_json::to_string(&replay_debug_json(&replay))
            .unwrap()
            .contains("follow up"));
        assert!(!serde_json::to_string(&replay_debug_json(&replay))
            .unwrap()
            .contains("old_raw"));
    }

    #[test]
    fn responses_replay_rejects_route_mismatch_and_legacy_gaps() {
        let dir = tempfile::tempdir().unwrap();
        let mut legacy = Session::create(dir.path().join("legacy.jsonl")).unwrap();
        legacy.append(user("prompt")).unwrap();
        legacy.append(responses_assistant("legacy answer")).unwrap();
        assert!(legacy
            .responses_replay_items(&EndpointId("responses".into()), &ModelId("m".into()))
            .unwrap()
            .is_none());

        let mut mismatch = Session::create(dir.path().join("mismatch.jsonl")).unwrap();
        mismatch.append(user("prompt")).unwrap();
        let assistant = mismatch
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("answer".into())],
                model: ModelId("other-model".into()),
                protocol: Protocol::OpenAiResponses,
            })))
            .unwrap();
        mismatch
            .append_responses_turn(
                assistant,
                EndpointId("other-endpoint".into()),
                ModelId("other-model".into()),
                responses_output("raw"),
            )
            .unwrap();
        let error = mismatch
            .responses_replay_items(&EndpointId("responses".into()), &ModelId("m".into()))
            .unwrap_err();
        assert!(matches!(error, SessionError::ResponsesRouteMismatch { .. }));
    }

    #[test]
    fn responses_sidecars_reject_non_authoritative_output() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(temp_path(&dir)).unwrap();
        session.append(user("prompt")).unwrap();
        let assistant = session.append(responses_assistant("answer")).unwrap();
        let error = session
            .append_responses_turn(
                assistant,
                EndpointId("responses".into()),
                ModelId("m".into()),
                octet_ai::ResponsesOutput::default(),
            )
            .unwrap_err();
        assert!(matches!(error, SessionError::InvalidResponsesSidecar(_)));

        let error = session
            .append_responses_compaction(
                EndpointId("responses".into()),
                ModelId("m".into()),
                responses_output("not-a-compaction"),
            )
            .unwrap_err();
        assert!(matches!(error, SessionError::InvalidResponsesSidecar(_)));
    }

    #[test]
    fn reopening_rejects_a_semantically_invalid_compact_checkpoint() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let covered = session.append(user("prompt")).unwrap();
        drop(session);

        let malformed = serde_json::json!({
            "type": "entry",
            "id": "999",
            "parent": covered,
            "value": {
                "type": "responses_compaction",
                "endpoint": "responses",
                "model": "m",
                "covered_through": covered,
                "output": [{"type": "message", "id": "not-compacted"}]
            }
        });
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        serde_json::to_writer(&mut file, &malformed).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);

        let error = Session::open(&path).unwrap_err();
        assert!(matches!(error, SessionError::Corrupt { .. }), "{error}");
        assert!(error.to_string().contains("direct checkpoint"), "{error}");
    }

    #[test]
    fn native_responses_compaction_is_a_branch_local_replay_base() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = EndpointId("responses".into());
        let model = ModelId("m".into());
        let mut session = Session::create(temp_path(&dir)).unwrap();
        session.append(user("old prompt")).unwrap();
        let assistant = session.append(responses_assistant("old answer")).unwrap();
        let covered = session
            .append_responses_turn(
                assistant,
                endpoint.clone(),
                model.clone(),
                responses_output("old_raw"),
            )
            .unwrap();
        session
            .append_responses_compaction(
                endpoint.clone(),
                model.clone(),
                responses_compact_output("compact_raw"),
            )
            .unwrap();
        session.append(user("after compact")).unwrap();

        let replay = session
            .responses_replay_items(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(replay.len(), 2);
        let octet_ai::responses::ResponsesReplayItem::Compacted(output) = &replay[0] else {
            panic!("native compact output must be the replay base");
        };
        assert_eq!(output.items()[1].as_json()["id"], "compact_raw");

        // Checking out the covered head abandons the checkpoint. The new
        // sibling reconstructs from canonical messages plus the turn sidecar.
        session.checkout(covered).unwrap();
        session.append(user("sibling")).unwrap();
        let sibling = session
            .responses_replay_items(&endpoint, &model)
            .unwrap()
            .unwrap();
        assert_eq!(sibling.len(), 3);
        let octet_ai::responses::ResponsesReplayItem::Output(output) = &sibling[1] else {
            panic!("ordinary turn output must be restored on the sibling");
        };
        assert_eq!(output.items()[0].as_json()["id"], "old_raw");
    }

    fn replay_debug_json(
        replay: &[octet_ai::responses::ResponsesReplayItem],
    ) -> Vec<serde_json::Value> {
        replay
            .iter()
            .map(|item| match item {
                octet_ai::responses::ResponsesReplayItem::User(user) => {
                    serde_json::to_value(user).unwrap()
                }
                octet_ai::responses::ResponsesReplayItem::LocalAssistant(assistant) => {
                    serde_json::to_value(assistant).unwrap()
                }
                octet_ai::responses::ResponsesReplayItem::Output(output) => {
                    serde_json::to_value(output).unwrap()
                }
                octet_ai::responses::ResponsesReplayItem::Compacted(output) => {
                    serde_json::to_value(output).unwrap()
                }
            })
            .collect()
    }

    fn frame_stream(prefix: &str) -> Vec<octet_ai::AssistantMessageFrame> {
        use octet_ai::AssistantMessageFrame as Frame;
        use octet_ai::StreamEvent;
        let mut encoder = octet_ai::AssistantMessageFrameEncoder::new(
            ModelId("frame-model".to_string()),
            Protocol::AnthropicMessages,
        );
        let events = [
            StreamEvent::Started {
                response_id: Some("resp-1".to_string()),
            },
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                delta: prefix.to_string(),
            },
        ];
        let mut frames = Vec::new();
        for event in &events {
            if let Some(frame) = encoder.encode(event).unwrap() {
                frames.push(frame);
            }
        }
        // A terminal event contributes no frame.
        assert!(encoder
            .encode(&StreamEvent::Finished(octet_ai::Response {
                message: AssistantMessage {
                    content: vec![AssistantPart::Text(prefix.to_string())],
                    model: ModelId("frame-model".to_string()),
                    protocol: Protocol::AnthropicMessages,
                },
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                cost: None,
                response_id: Some("resp-1".to_string()),
                responses_output: None,
                deferred: None,
                diagnostics: Vec::new(),
            }))
            .unwrap()
            .is_none());
        let Frame::Start { .. } = &frames[0] else {
            panic!("first frame is the stream start");
        };
        frames
    }

    #[test]
    fn partial_assistant_frames_survive_reopen_and_republish_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let journal_path = {
            let mut session = Session::create(&path).unwrap();
            session.append(user("hi")).unwrap();
            let mut journal = session.begin_assistant_frame_journal().unwrap();
            for frame in frame_stream("partial progress") {
                journal.append(&frame).unwrap();
            }
            let journal_path = journal.path().to_path_buf();
            assert!(journal_path.is_file());
            assert!(journal.retained_frames() > 0);
            // Dropping the session (process kill) leaves the journal behind.
            journal_path
        };

        // Restart: a fresh handle sees the partial exactly once.
        let mut reopened = Session::open(&path).unwrap();
        let partial = reopened.take_partial_assistant().unwrap().unwrap();
        assert_eq!(partial.model, ModelId("frame-model".to_string()));
        assert_eq!(partial.protocol, Protocol::AnthropicMessages);
        match partial.content.as_slice() {
            [AssistantPart::Text(text)] => assert_eq!(text, "partial progress"),
            other => panic!("unexpected partial content: {other:?}"),
        }
        // Republish is exactly once and removes the sidecar.
        assert!(reopened.take_partial_assistant().unwrap().is_none());
        assert!(!journal_path.exists());
        // The partial never entered the session log or its context.
        assert!(reopened
            .context()
            .unwrap()
            .iter()
            .all(|message| !matches!(message, Message::Assistant(_))));
    }

    #[test]
    fn settled_assistant_frames_are_not_republished_as_progress() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let journal_path = {
            let mut session = Session::create(&path).unwrap();
            let mut journal = session.begin_assistant_frame_journal().unwrap();
            for frame in frame_stream("complete turn") {
                journal.append(&frame).unwrap();
            }
            let journal_path = journal.path().to_path_buf();
            // Terminal settlement removes the partial.
            journal.settle();
            assert!(!journal_path.exists());
            journal_path
        };

        let mut reopened = Session::open(&path).unwrap();
        assert!(reopened.take_partial_assistant().unwrap().is_none());
        assert!(!journal_path.exists());
    }

    #[test]
    fn partial_assistant_journal_never_truncates_an_existing_target() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("keep this prefix") {
            journal.append(&frame).unwrap();
        }
        let original = std::fs::read(journal.path()).unwrap();
        assert!(session.begin_assistant_frame_journal().is_err());
        assert_eq!(std::fs::read(journal.path()).unwrap(), original);
        drop(journal);
        assert!(session.take_partial_assistant().unwrap().is_some());
        assert!(session.begin_assistant_frame_journal().is_ok());
    }

    #[test]
    fn partial_assistant_journal_bounds_reads_and_discards_torn_utf8_tail() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("valid prefix") {
            journal.append(&frame).unwrap();
        }
        journal.file.write_all(b"{\"torn\":\"\xff").unwrap();
        drop(journal);
        assert!(session.take_partial_assistant().unwrap().is_some());

        let journal = session.begin_assistant_frame_journal().unwrap();
        journal
            .file
            .set_len((MAX_PARTIAL_FRAME_JOURNAL_BYTES + 1) as u64)
            .unwrap();
        drop(journal);
        assert!(matches!(
            session.take_partial_assistant(),
            Err(SessionError::Limit(_))
        ));
        assert!(session.partial_assistant_frames_path().unwrap().exists());

        let frame = octet_ai::AssistantMessageFrame::TextDelta {
            index: 0,
            delta: "x".into(),
        };
        let mut line = serde_json::to_vec(&frame).unwrap();
        // A JSON value without its newline is still an uncommitted tail.
        assert!(read_partial_assistant_frames(&line).unwrap().is_empty());
        line.push(b'\n');
        let bytes = line.repeat(MAX_PARTIAL_FRAME_JOURNAL_FRAMES + 1);
        assert!(bytes.len() < MAX_PARTIAL_FRAME_JOURNAL_BYTES);
        assert!(matches!(
            read_partial_assistant_frames(&bytes),
            Err(SessionError::Limit(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn partial_assistant_journal_rejects_symlinks_hardlinks_and_special_files() {
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let path = session.partial_assistant_frames_path().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, b"do not overwrite").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &path).unwrap();
        assert!(session.begin_assistant_frame_journal().is_err());
        assert!(session.take_partial_assistant().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"do not overwrite");
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&target, &path).unwrap();
        assert!(session.begin_assistant_frame_journal().is_err());
        assert!(session.take_partial_assistant().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"do not overwrite");
        std::fs::remove_file(&path).unwrap();
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo is a valid NUL-terminated pathname, with a valid mode.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(session.begin_assistant_frame_journal().is_err());
        assert!(session.take_partial_assistant().is_err());
    }

    #[test]
    fn partial_assistant_journal_settlement_preserves_a_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("original") {
            journal.append(&frame).unwrap();
        }
        let path = journal.path().to_owned();
        std::fs::rename(&path, path.with_extension("old")).unwrap();
        let mut replacement = session.begin_assistant_frame_journal().unwrap();
        for frame in frame_stream("replacement") {
            replacement.append(&frame).unwrap();
        }
        let bytes = std::fs::read(&path).unwrap();
        journal.settle();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        replacement.settle();
        assert!(!path.exists());
    }

    #[test]
    fn partial_frame_journal_is_bounded_and_does_not_grow_without_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let mut session = Session::create(&path).unwrap();
        let mut journal = session.begin_assistant_frame_journal().unwrap();
        let mut frame = octet_ai::AssistantMessageFrame::TextDelta {
            index: 0,
            delta: "x".repeat(MAX_PARTIAL_FRAME_JOURNAL_BYTES / 4 + 1),
        };
        let mut accepted = 0usize;
        for _ in 0..64 {
            journal.append(&frame).unwrap();
            accepted += 1;
        }
        assert!(journal.is_bounded(), "journal must stop at its byte bound");
        assert!(
            journal.retained_bytes() <= MAX_PARTIAL_FRAME_JOURNAL_BYTES,
            "retained journal bytes stay bounded"
        );
        let bytes_before = journal.retained_bytes();
        frame = octet_ai::AssistantMessageFrame::TextDelta {
            index: 0,
            delta: "y".to_string(),
        };
        journal.append(&frame).unwrap();
        assert_eq!(journal.retained_bytes(), bytes_before);
        let path = journal.path().to_path_buf();
        drop(journal);
        assert!(accepted > 1);
        assert!(
            std::fs::metadata(&path).unwrap().len() as usize <= MAX_PARTIAL_FRAME_JOURNAL_BYTES
        );
    }

    #[test]
    fn extension_entries_are_durable_and_never_model_visible() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let id = session
            .append_extension_entry(
                "octet.todo",
                Some(7),
                "todo.created",
                serde_json::json!({ "text": "ship wave 1" }),
            )
            .unwrap();
        // The payload rides the same non-context marker envelope as
        // `append_run_outcome`, so every provider projection skips it and
        // older readers can still replay the record.
        assert!(matches!(
            &session.entry(&id).unwrap().value,
            EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            }
        ));
        assert!(session.context().unwrap().is_empty());
        let prompt = session.append(user("hello")).unwrap();
        let context = session.context().unwrap();
        assert_eq!(context.len(), 1, "only the user message is model-visible");
        assert!(
            !format!("{context:?}").contains("ship wave 1"),
            "extension payload text must never reach provider context"
        );
        // Nothing before the prompt is model-visible: the extension marker is
        // skipped exactly like every other configuration marker.
        assert!(session.context_before(&prompt).unwrap().is_empty());
        // A marker that is itself the active head still adds no context, and
        // the prior message remains the only model-visible contribution.
        let second = session
            .append_extension_entry(
                "octet.todo",
                None,
                "todo.updated",
                serde_json::json!({ "text": "ship wave 1" }),
            )
            .unwrap();
        assert_eq!(session.head(), Some(second.clone()));
        assert_eq!(session.context().unwrap().len(), 1);
        assert_eq!(session.context_before(&second).unwrap().len(), 1);
        assert!(!format!("{:?}", session.context().unwrap()).contains("ship wave 1"));
    }

    #[test]
    fn extension_entry_round_trips_through_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let id = {
            let mut session = Session::create(&path).unwrap();
            session
                .append_extension_entry(
                    "octet.todo",
                    Some(11),
                    "todo.created",
                    serde_json::json!({ "items": ["a", "b"] }),
                )
                .unwrap()
        };
        let session = Session::open(&path).unwrap();
        let entry = session
            .extension_entry(&id, "octet.todo")
            .expect("payload resolves");
        assert_eq!(entry.entry_type, "todo.created");
        assert_eq!(entry.data, serde_json::json!({ "items": ["a", "b"] }));
        let metadata = session.entry(&id).unwrap().metadata.as_ref().unwrap();
        let stored = &metadata.extension_metadata["octet.todo"];
        assert!(!stored.public);
        assert_eq!(stored.provenance.extension, "octet.todo");
        assert_eq!(stored.provenance.process_generation, Some(11));
        assert_eq!(
            stored.value,
            serde_json::json!({
                "entry_type": "todo.created",
                "data": { "items": ["a", "b"] },
            })
        );
        assert!(metadata.public_extension_metadata().is_empty());
        assert!(session.extension_entry(&id, "other.namespace").is_none());
        assert!(session.context().unwrap().is_empty());
    }

    #[test]
    fn extension_entry_refuses_invalid_input_without_state_change() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let anchor = session.append(user("anchor")).unwrap();
        let durable_bytes = std::fs::metadata(&path).unwrap().len();
        let long_type = "t".repeat(MAX_EXTENSION_ENTRY_TYPE_BYTES + 1);
        let mut deep = serde_json::json!(1);
        for _ in 0..=MAX_EXTENSION_ENTRY_METADATA_DEPTH {
            deep = serde_json::json!({ "a": deep });
        }
        // 255 single-kilobyte strings plus their array is exactly the node
        // bound, but whose encoded form exceeds the namespace value bound.
        let oversize = serde_json::Value::Array(
            (0..255)
                .map(|_| serde_json::json!("x".repeat(1024)))
                .collect(),
        );
        let cases = vec![
            ("Bad.Namespace", "note", serde_json::json!(1)),
            ("", "note", serde_json::json!(1)),
            ("octet..todo", "note", serde_json::json!(1)),
            ("octet.todo", "", serde_json::json!(1)),
            ("octet.todo", long_type.as_str(), serde_json::json!(1)),
            ("octet.todo", "no\nte", serde_json::json!(1)),
            (
                "octet.todo",
                "note",
                serde_json::json!("x".repeat(MAX_EXTENSION_ENTRY_METADATA_VALUE_BYTES)),
            ),
            ("octet.todo", "note", deep),
            ("octet.todo", "note", oversize),
        ];
        for (namespace, entry_type, data) in cases {
            let error = session
                .append_extension_entry(namespace, None, entry_type, data)
                .expect_err("invalid extension entry must be refused");
            assert!(matches!(error, SessionError::Limit(_)), "{error}");
        }
        assert_eq!(session.head(), Some(anchor));
        assert_eq!(session.entries().len(), 1);
        assert_eq!(session.context().unwrap().len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_bytes);
        // A payload that fits the namespace value bound is retained verbatim.
        let accepted = serde_json::json!({ "note": "x".repeat(8 * 1024) });
        let id = session
            .append_extension_entry("octet.todo", None, "note", accepted.clone())
            .unwrap();
        assert_eq!(session.extension_entry(&id, "octet.todo").unwrap().data, accepted);
    }

    #[test]
    fn entry_labels_are_replaceable_durable_and_clearable() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let first;
        let second;
        {
            let mut session = Session::create(&path).unwrap();
            first = session.append(user("one")).unwrap();
            second = session.append(assistant("two")).unwrap();
            session.set_entry_label(&first, "planning").unwrap();
            session.set_entry_label(&second, "answer").unwrap();
            session.set_entry_label(&first, "replanned").unwrap();
            assert_eq!(session.entry_label(&first), Some("replanned"));
            assert_eq!(session.entry_label(&second), Some("answer"));
            assert_eq!(session.entry_labels().len(), 2);
            assert!(session.entry_labels().len() <= session.entries().len());
            // Labels never move the head or change model-visible context.
            assert_eq!(session.head(), Some(second.clone()));
            assert_eq!(session.context().unwrap().len(), 2);
        }
        let mut session = Session::open(&path).unwrap();
        assert_eq!(session.entry_label(&first), Some("replanned"));
        assert_eq!(session.entry_label(&second), Some("answer"));
        session.set_entry_label(&first, "").unwrap();
        assert_eq!(session.entry_label(&first), None);
        assert!(session.entry_labels().contains_key(&second));
        drop(session);
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.entry_label(&first), None);
        assert_eq!(reopened.entry_label(&second), Some("answer"));
    }

    #[test]
    fn entry_label_refuses_unknown_entry_and_invalid_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = temp_path(&dir);
        let mut session = Session::create(&path).unwrap();
        let entry = session.append(user("one")).unwrap();
        let durable_bytes = std::fs::metadata(&path).unwrap().len();
        let unknown = EntryId("999".into());
        let error = session.set_entry_label(&unknown, "ghost").unwrap_err();
        assert!(matches!(error, SessionError::UnknownEntry(id) if id == unknown));
        let error = session
            .set_entry_label(&entry, &"x".repeat(MAX_ENTRY_LABEL_BYTES + 1))
            .unwrap_err();
        assert!(matches!(error, SessionError::Limit(_)), "{error}");
        let error = session.set_entry_label(&entry, "two\nlines").unwrap_err();
        assert!(matches!(error, SessionError::Limit(_)), "{error}");
        assert_eq!(session.entry_label(&entry), None);
        assert!(session.entry_labels().is_empty());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), durable_bytes);
        // The exact byte bound is accepted without control characters.
        let label = "y".repeat(MAX_ENTRY_LABEL_BYTES);
        session.set_entry_label(&entry, &label).unwrap();
        assert_eq!(session.entry_label(&entry), Some(label.as_str()));
    }
}
