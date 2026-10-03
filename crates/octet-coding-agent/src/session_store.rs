#![allow(missing_docs)]

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use octet_agent::tools::deferred::{DeferredRunLimits, DeferredRunRecord};
use octet_agent::{EntryId, EntryValue, Session};
use octet_ai::{EndpointId, Message, ModelId, Protocol, UserPart};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::session_catalog::{
    CachedTranscriptSummary, CatalogFingerprint, CatalogUpdate, IndexedEntry, IndexedEntryKind,
    IndexedEntryUpdate, SessionCatalog, MAX_INDEXED_ENTRIES_PER_SESSION, MAX_INDEXED_ENTRY_CHARS,
};

mod accounting_index;
mod operations;
mod search_projection;

static NEXT_SESSION_SUFFIX: AtomicU64 = AtomicU64::new(1);

// Keep the picker scanner under the same documented bounds as Session::open.
// Unlike a semantic replay, this path retains only IDs, parents, entry kinds,
// and one clipped user title per entry.
pub(crate) const MAX_SESSION_FILE_BYTES: usize = 256 * 1024 * 1024;
const MAX_SESSION_RECORDS: usize = 1_000_000;
const MAX_SESSION_METADATA_BYTES: usize = 64 * 1024;
const MAX_SESSION_NAME_CHARS: usize = 120;
const MAX_SESSION_TAGS: usize = 32;
const MAX_SESSION_TAG_CHARS: usize = 48;
/// Marker file recording the canonical workspace path in a workspace
/// directory. Plain text (one path); older binaries ignore it.
const WORKSPACE_MARKER: &str = ".workspace";
/// Private delegation directory this workspace's session store owns, beside the
/// transcripts. The owning session lays out `<session-dir>/.delegation/team-*/`
/// and its durable roster there (`DelegationConfig::new(session_parent.join(".delegation"))`).
const DELEGATION_DIRECTORY: &str = ".delegation";
/// Host-published opaque handle prefix for one session-owned delegated child
/// (`octet_agent::delegated_session_reference`). The token is path-free and
/// argv-safe: the launcher passes `octet --resume <handle>` as separate argv
/// elements.
const DELEGATED_SESSION_HANDLE_PREFIX: &str = "agent-session:";

/// Filesystem-backed sessions scoped to one canonical workspace.
#[derive(Clone, Debug)]
pub struct SessionStore {
    dir: PathBuf,
    root: PathBuf,
    workspace: Option<PathBuf>,
}

/// Metadata used by startup and session pickers.
#[derive(Clone, Debug)]
pub struct SessionMeta {
    pub id: String,
    pub path: PathBuf,
    pub title: String,
    pub name: Option<String>,
    pub tags: Vec<String>,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub pinned: bool,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub archived: bool,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub trashed_at_ms: Option<u64>,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub purge_after_ms: Option<u64>,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub forked_from_session_id: Option<String>,
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub forked_from_entry_id: Option<String>,
    /// Number of persisted message entries in the active branch.
    pub message_count: usize,
    pub modified: SystemTime,
    /// Canonical workspace path recorded in the store's `.workspace` marker,
    /// when known. Enables cross-workspace browsing without reversing the
    /// workspace-key hash.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub workspace: Option<PathBuf>,
}

/// Compact active-branch state derived without constructing a full `Session`.
///
/// This is intentionally limited to data needed for catalog inventory and
/// lifetime usage recovery. Opening a session for mutation still performs the
/// authoritative descriptor-bound `Session` replay.
#[cfg_attr(not(feature = "serve"), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct SessionCatalogEntry {
    pub meta: Option<SessionMeta>,
    pub configured_model: Option<String>,
    pub configured_reasoning: Option<String>,
}

/// One compact usage projection retained by the lightweight catalog replay.
#[cfg_attr(not(feature = "serve"), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct SessionUsageRecord {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub completed_at_unix_ms: Option<u64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cache_write_1h_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
}

/// Result of one bounded, graph-validating transcript scan.
#[cfg_attr(not(feature = "serve"), allow(dead_code))]
#[derive(Clone, Debug)]
pub(crate) struct SessionCatalogInspection {
    pub catalog: SessionCatalogEntry,
    pub usage_records: Vec<SessionUsageRecord>,
    pub usage_uncertainty_records: Vec<octet_agent::UsageUncertaintyRecord>,
}

/// Small user-owned metadata kept next to, but separate from, append-only
/// session records. Sidecars let older octet binaries continue to open JSONL
/// sessions while catalog metadata remains easy to export and recover.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionUserMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_after_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forked_from_entry_id: Option<String>,
}

#[cfg_attr(not(feature = "serve"), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionStorageLifecycle {
    Active,
    Archived,
    Trash,
}

#[cfg_attr(not(feature = "serve"), allow(dead_code))]
pub const SESSION_TRASH_RETENTION_MS: u64 = 30 * 24 * 60 * 60 * 1_000;

#[derive(Debug)]
struct SessionCandidate {
    path: PathBuf,
    modified: SystemTime,
    file_size: u64,
}

fn catalog_fingerprint(candidate: &SessionCandidate) -> Option<CatalogFingerprint> {
    if candidate.file_size > MAX_SESSION_FILE_BYTES as u64 {
        return None;
    }
    let modified_ns = i64::try_from(
        candidate
            .modified
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos(),
    )
    .ok()?;
    Some(CatalogFingerprint {
        file_size: candidate.file_size,
        modified_ns,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryEntryKind {
    User,
    Assistant,
    Other,
}

#[derive(Debug)]
struct SummaryEntry {
    parent: Option<EntryId>,
    kind: SummaryEntryKind,
    title: Option<String>,
    position: u32,
    assistant_model: Option<ModelId>,
    assistant_protocol: Option<Protocol>,
    configured_model: Option<String>,
    configured_reasoning: Option<String>,
}

fn summary_ancestry_intervals(entries: &HashMap<EntryId, SummaryEntry>) -> (Vec<u32>, Vec<u32>) {
    const NONE: u32 = u32::MAX;

    let mut first_child = vec![NONE; entries.len()];
    let mut next_sibling = vec![NONE; entries.len()];
    for entry in entries.values() {
        let Some(parent) = entry.parent.as_ref() else {
            continue;
        };
        let parent = entries
            .get(parent)
            .expect("summary replay validates every parent before ancestry")
            .position;
        next_sibling[entry.position as usize] = first_child[parent as usize];
        first_child[parent as usize] = entry.position;
    }

    let mut entered = vec![0u32; entries.len()];
    let mut exited = vec![0u32; entries.len()];
    let mut clock = 0u32;
    let mut stack = Vec::<(u32, bool)>::new();
    for entry in entries.values().filter(|entry| entry.parent.is_none()) {
        stack.push((entry.position, false));
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

/// A title-only JSON string. serde_json can lend ordinary strings directly to
/// this visitor, so the common path never allocates the complete prompt merely
/// to retain its first 60 normalized characters.
struct TitleText(String);

impl<'de> Deserialize<'de> for TitleText {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TitleVisitor;

        impl Visitor<'_> for TitleVisitor {
            type Value = TitleText;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a session-title string")
            }

            fn visit_borrowed_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TitleText(trim_title(value)))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TitleText(trim_title(value)))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(TitleText(trim_title(&value)))
            }
        }

        deserializer.deserialize_string(TitleVisitor)
    }
}

#[derive(Deserialize)]
enum SummaryUserPart {
    Text(TitleText),
    Media(IgnoredAny),
    ToolResult(IgnoredAny),
}

#[derive(Deserialize)]
struct SummaryUserMessage {
    content: Vec<SummaryUserPart>,
}

#[derive(Deserialize)]
struct SummaryEntryMetadata {
    #[serde(default)]
    display_text: Option<TitleText>,
}

#[derive(Deserialize)]
enum SummaryMessage {
    User(SummaryUserMessage),
    Assistant(SummaryAssistantMessage),
}

#[derive(Deserialize)]
struct SummaryAssistantMessage {
    model: ModelId,
    protocol: Protocol,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SummaryResponsesField {
    Type,
    EncryptedContent,
    Other,
}

impl<'de> Deserialize<'de> for SummaryResponsesField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct FieldVisitor;

        impl Visitor<'_> for FieldVisitor {
            type Value = SummaryResponsesField;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Responses item field")
            }

            fn visit_borrowed_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(value)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(match value {
                    "type" => SummaryResponsesField::Type,
                    "encrypted_content" => SummaryResponsesField::EncryptedContent,
                    _ => SummaryResponsesField::Other,
                })
            }
        }

        deserializer.deserialize_identifier(FieldVisitor)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct IsCompaction(bool);

#[derive(Clone, Copy, Debug, Default)]
struct IsNonEmptyString(bool);

macro_rules! impl_summary_string_probe {
    ($name:ident, $predicate:expr, $expected:literal) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                struct ProbeVisitor;

                impl<'de> Visitor<'de> for ProbeVisitor {
                    type Value = $name;

                    fn expecting(
                        &self,
                        formatter: &mut std::fmt::Formatter<'_>,
                    ) -> std::fmt::Result {
                        formatter.write_str($expected)
                    }

                    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        Ok($name(($predicate)(value)))
                    }

                    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
                    where
                        E: serde::de::Error,
                    {
                        Ok($name(($predicate)(value)))
                    }

                    fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_none<E>(self) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_unit<E>(self) -> Result<Self::Value, E> {
                        Ok($name(false))
                    }

                    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
                    where
                        D: Deserializer<'de>,
                    {
                        deserializer.deserialize_any(self)
                    }

                    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
                    where
                        A: SeqAccess<'de>,
                    {
                        while sequence.next_element::<IgnoredAny>()?.is_some() {}
                        Ok($name(false))
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                        Ok($name(false))
                    }
                }

                deserializer.deserialize_any(ProbeVisitor)
            }
        }
    };
}

impl_summary_string_probe!(
    IsCompaction,
    |value: &str| value == "compaction",
    "the Responses item type"
);
impl_summary_string_probe!(
    IsNonEmptyString,
    |value: &str| !value.is_empty(),
    "opaque encrypted Responses content"
);

#[derive(Clone, Copy, Debug, Default)]
struct SummaryResponsesItem {
    is_compaction: bool,
    has_encrypted_content: bool,
}

impl<'de> Deserialize<'de> for SummaryResponsesItem {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ItemVisitor;

        impl<'de> Visitor<'de> for ItemVisitor {
            type Value = SummaryResponsesItem;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Responses item object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut item = SummaryResponsesItem::default();
                while let Some(field) = map.next_key::<SummaryResponsesField>()? {
                    match field {
                        SummaryResponsesField::Type => {
                            item.is_compaction = map.next_value::<IsCompaction>()?.0;
                        }
                        SummaryResponsesField::EncryptedContent => {
                            item.has_encrypted_content = map.next_value::<IsNonEmptyString>()?.0;
                        }
                        SummaryResponsesField::Other => {
                            let _ = map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(item)
            }
        }

        deserializer.deserialize_map(ItemVisitor)
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct SummaryResponsesOutput {
    is_empty: bool,
    has_valid_compaction: bool,
}

impl SummaryResponsesOutput {
    fn is_empty(&self) -> bool {
        self.is_empty
    }

    fn has_valid_compaction(&self) -> bool {
        self.has_valid_compaction
    }
}

impl<'de> Deserialize<'de> for SummaryResponsesOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OutputVisitor;

        impl<'de> Visitor<'de> for OutputVisitor {
            type Value = SummaryResponsesOutput;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a Responses output array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut item_count = 0usize;
                let mut compaction_count = 0usize;
                let mut valid_compaction_count = 0usize;
                while let Some(item) = sequence.next_element::<SummaryResponsesItem>()? {
                    item_count = item_count.saturating_add(1);
                    if item.is_compaction {
                        compaction_count = compaction_count.saturating_add(1);
                        if item.has_encrypted_content {
                            valid_compaction_count = valid_compaction_count.saturating_add(1);
                        }
                    }
                }
                Ok(SummaryResponsesOutput {
                    is_empty: item_count == 0,
                    has_valid_compaction: compaction_count == 1 && valid_compaction_count == 1,
                })
            }
        }

        deserializer.deserialize_seq(OutputVisitor)
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SummaryEntryValue {
    Message(SummaryMessage),
    Compaction {
        first_kept: EntryId,
    },
    ResponsesTurn {
        assistant: EntryId,
        #[serde(rename = "endpoint")]
        _endpoint: EndpointId,
        model: ModelId,
        output: SummaryResponsesOutput,
    },
    ResponsesCompaction {
        covered_through: EntryId,
        #[serde(rename = "endpoint")]
        _endpoint: EndpointId,
        #[serde(rename = "model")]
        _model: ModelId,
        output: SummaryResponsesOutput,
    },
    ResponsesReasoning {
        model: ModelId,
        baseline: octet_ai::ReasoningConfig,
        update: Option<octet_ai::ResponsesConfigurationUpdate>,
    },
    Config {
        model: Option<String>,
        reasoning: Option<String>,
    },
    ResponsesSteering {},
    PromptTemplateSelected {},
    SkillActivated {},
    SkillResourceRead {},
    SkillDeactivated {},
}

#[derive(Deserialize)]
struct SummaryUsage {
    input_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    cache_write_1h_tokens: u64,
    output_tokens: u64,
    reasoning_tokens: u64,
    total_tokens: u64,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SummaryUsageKind {
    AssistantTurn {
        assistant: EntryId,
    },
    Compaction,
    RejectedResponsesTurn,
    TerminalGate {
        #[serde(rename = "returned")]
        _returned: Option<bool>,
    },
}

#[derive(Deserialize)]
struct SummaryUsageRecord {
    kind: SummaryUsageKind,
    usage: SummaryUsage,
    #[serde(default)]
    endpoint: Option<EndpointId>,
    #[serde(default)]
    model: Option<ModelId>,
    #[serde(default)]
    completed_at_unix_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SummaryRecord {
    Entry {
        id: EntryId,
        parent: Option<EntryId>,
        #[serde(default)]
        metadata: Option<SummaryEntryMetadata>,
        value: SummaryEntryValue,
    },
    Head {
        id: EntryId,
    },
    RootHead {},
    Checkpoint {
        prompt: EntryId,
        head: EntryId,
    },
    UsageUncertainty {
        record: octet_agent::UsageUncertaintyRecord,
    },
    Usage {
        record: SummaryUsageRecord,
    },
    DeferredRun {
        record: DeferredRunRecord,
    },
    EntryLabel {
        entry_id: EntryId,
        label: String,
    },
    ToolInvocation {
        #[serde(rename = "scope")]
        _scope: octet_agent::tools::durability::InvocationScope,
        #[serde(rename = "record")]
        _record: octet_agent::tools::durability::InvocationRecord,
    },
}

/// Derive the oldest user title on the active branch, if one exists.
fn active_branch_catalog_config(session: &Session) -> (Option<String>, Option<String>) {
    let mut model = None;
    let mut reasoning = None;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(id) else {
            break;
        };
        if let EntryValue::ResponsesReasoning {
            model: selected,
            baseline,
            update,
            ..
        } = &entry.value
        {
            if model.is_none() {
                model = Some(selected.0.clone());
            }
            if reasoning.is_none() {
                reasoning = Some(crate::app::reasoning_label(
                    update.as_ref().map_or(baseline, |update| &update.reasoning),
                ));
            }
        }
        if let EntryValue::Config {
            model: configured_model,
            reasoning: configured_reasoning,
            ..
        } = &entry.value
        {
            if model.is_none() {
                model = configured_model.clone();
            }
            if reasoning.is_none() {
                reasoning = configured_reasoning.clone();
            }
            if model.is_some() && reasoning.is_some() {
                break;
            }
        }
        cursor = entry.parent.as_ref();
    }
    (model, reasoning)
}

fn active_branch_catalog_title(session: &Session) -> Option<String> {
    let mut oldest: Option<&str> = None;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(id) else {
            break;
        };
        if let EntryValue::Message(Message::User(user)) = &entry.value {
            if let Some(display) = entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.display_text.as_deref())
            {
                oldest = Some(display);
            } else if let Some(UserPart::Text(text)) = user
                .content
                .iter()
                .find(|part| matches!(part, UserPart::Text(_)))
            {
                oldest = Some(text);
            }
        }
        cursor = entry.parent.as_ref();
    }
    oldest.map(trim_title)
}

/// Derive a compact title from the oldest user text on the active branch.
pub fn active_branch_title(session: &Session) -> String {
    active_branch_catalog_title(session).unwrap_or_else(|| "(empty session)".to_owned())
}

fn active_branch_message_count(session: &Session) -> usize {
    let mut count = 0usize;
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let Some(entry) = session.entry(id) else {
            break;
        };
        if matches!(entry.value, EntryValue::Message(_)) {
            count = count.saturating_add(1);
        }
        cursor = entry.parent.as_ref();
    }
    count
}

pub(crate) fn trim_title(title: &str) -> String {
    const LIMIT: usize = 60;
    let mut normalized = String::with_capacity(LIMIT + 3);
    let mut length = 0usize;
    for word in title.split_whitespace() {
        if !normalized.is_empty() {
            if length == LIMIT {
                normalized.push('…');
                return normalized;
            }
            normalized.push(' ');
            length += 1;
        }
        for character in word.chars() {
            if length == LIMIT {
                normalized.push('…');
                return normalized;
            }
            normalized.push(character);
            length += 1;
        }
    }
    normalized
}

fn workspace_key(workspace: &Path) -> String {
    // FNV-1a is small, deterministic, and avoids another hashing dependency.
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET;
    for byte in workspace.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:012x}")
}

fn session_id_is_valid(id: &str) -> bool {
    if id.is_empty() || id.chars().any(char::is_control) {
        return false;
    }
    let mut components = Path::new(id).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == id
    )
}

/// Bounded, typed refusal for a session-owned worker handle that cannot be
/// opened as an interactive session.
///
/// Every variant is a deliberate fail-closed verdict, so an unlaunchable worker
/// is never silently resolved to a different session (the parent, a stale
/// transcript) and never panics. The rendered reason is fixed text: it carries
/// no transcript path, no roster path, no credential, and no session secret,
/// and never echoes the caller-supplied handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DelegatedHandleRefusal {
    /// The handle is not `agent-session:` plus exactly 64 lowercase hex digits.
    /// Rejected before any filesystem work, so a shell metacharacter, a control
    /// byte, or a path traversal can never reach a path join.
    MalformedHandle,
    /// This session has no readable, supported, owned delegation roster, so no
    /// worker handle can be resolved from it.
    RosterUnavailable,
    /// The roster of this session does not know this handle.
    UnknownWorker,
    /// The worker is parked at the approval boundary (`awaiting_approval`).
    /// Opening it elsewhere would be unattended mutation.
    ParkedAtApprovalBoundary,
    /// The roster still records a live worker (`pending`/`running`) that owns
    /// this transcript in the owning process.
    LiveInOwningProcess {
        /// Bounded roster state label (`pending` or `running`).
        status: &'static str,
    },
    /// The roster knows the worker but its transcript is gone.
    VanishedTranscript,
    /// The roster resolved to a transcript outside this store's private
    /// delegation directory: refused as a path escape.
    OutsideDelegationDirectory,
}

impl DelegatedHandleRefusal {
    /// Stable, bounded, machine-readable code for frontends and diagnostics.
    #[cfg(test)]
    pub fn code(self) -> &'static str {
        match self {
            Self::MalformedHandle => "malformed_worker_handle",
            Self::RosterUnavailable => "delegation_roster_unavailable",
            Self::UnknownWorker => "unknown_worker_handle",
            Self::ParkedAtApprovalBoundary => "worker_awaiting_approval",
            Self::LiveInOwningProcess { .. } => "worker_live_in_owning_process",
            Self::VanishedTranscript => "worker_transcript_missing",
            Self::OutsideDelegationDirectory => "worker_handle_outside_delegation",
        }
    }
}

impl std::fmt::Display for DelegatedHandleRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MalformedHandle => formatter.write_str(
                "not a launchable worker handle: expected `agent-session:` followed by exactly 64 lowercase hex digits, with no shell metacharacter, control byte, or path component",
            ),
            Self::RosterUnavailable => formatter.write_str(
                "this session has no readable session-owned delegation roster, so the worker handle cannot be resolved; a handle is launchable only from the session store that owns it",
            ),
            Self::UnknownWorker => formatter.write_str(
                "the session-owned delegation roster of this session does not know this worker handle",
            ),
            Self::ParkedAtApprovalBoundary => formatter.write_str(
                "the worker is parked at the approval boundary (awaiting_approval), so opening it would be unattended mutation; approve or stop it in an interactive session first",
            ),
            Self::LiveInOwningProcess { status } => write!(
                formatter,
                "the roster still records a live worker (state `{status}`) that owns this transcript in the owning process; stop or detach it before opening the session elsewhere, because one session has one writer",
            ),
            Self::VanishedTranscript => formatter.write_str(
                "the worker is in the session-owned roster but its transcript is gone, so there is nothing to open",
            ),
            Self::OutsideDelegationDirectory => formatter.write_str(
                "the worker handle resolved outside this session store's private delegation directory, so it was refused as a path escape",
            ),
        }
    }
}

impl std::error::Error for DelegatedHandleRefusal {}

/// Strict `agent-session:<sha256>` shape check, run before any filesystem work.
///
/// One argv element, no shell metacharacter, no control byte, no path
/// separator, no traversal: exactly the boring identifier the host publishes.
fn delegated_handle_digest(handle: &str) -> Option<&str> {
    let digest = handle.strip_prefix(DELEGATED_SESSION_HANDLE_PREFIX)?;
    (digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some(digest)
}

/// The bounded roster state label of a worker that still owns its transcript in
/// the owning process, matching `DelegatedAgentStatus::label` for the two live
/// states. Liveness itself is process-local and cannot cross the roster, so a
/// live record is the fail-closed signal available to a separate process.
fn live_worker_state(status: &str) -> Option<&'static str> {
    match status {
        "pending" => Some("pending"),
        "running" => Some("running"),
        _ => None,
    }
}

/// Classify the host resolver's refusal into this store's typed, bounded
/// verdict.
///
/// The resolver's messages are fixed, but two of them interpolate an I/O or
/// JSON error that can name a private path. Only recognized bounded verdicts are
/// relayed; every unrecognized one (and every non-`Unlaunchable` variant) fails
/// closed as [`DelegatedHandleRefusal::RosterUnavailable`], so no dynamic text
/// can reach the error and no path, credential, or secret can leak into it.
fn classify_delegated_handle_error(
    error: &octet_agent::delegation::DelegationError,
) -> DelegatedHandleRefusal {
    let octet_agent::delegation::DelegationError::Unlaunchable(reason) = error else {
        return DelegatedHandleRefusal::RosterUnavailable;
    };
    let reason = reason.as_str();
    if reason.starts_with("worker handle must") {
        DelegatedHandleRefusal::MalformedHandle
    } else if reason.starts_with("worker is parked at the approval boundary") {
        DelegatedHandleRefusal::ParkedAtApprovalBoundary
    } else if reason.starts_with("the worker session file is gone") {
        DelegatedHandleRefusal::VanishedTranscript
    } else if reason.starts_with("unknown worker handle") {
        DelegatedHandleRefusal::UnknownWorker
    } else {
        DelegatedHandleRefusal::RosterUnavailable
    }
}

/// Confine a roster-resolved child transcript to this store's private
/// delegation directory.
///
/// The handle is derived from the opaquely random team directory name plus the
/// host-generated child filename, so a forged or copied roster entry can name
/// the same pair somewhere else and hashes to the same handle. The resolved path
/// is therefore re-checked here: it must be `<delegation>/team-*/<child>.jsonl`,
/// with no `..` component, no symlinked team directory, and no non-regular final
/// entry. Anything else fails closed as a path escape.
fn confine_delegated_session_path(
    delegation_directory: &Path,
    path: &Path,
) -> Result<(), DelegatedHandleRefusal> {
    let relative = path
        .strip_prefix(delegation_directory)
        .map_err(|_| DelegatedHandleRefusal::OutsideDelegationDirectory)?;
    let mut components = relative.components();
    let (team, child) = match (components.next(), components.next(), components.next()) {
        (Some(Component::Normal(team)), Some(Component::Normal(child)), None) => (team, child),
        _ => return Err(DelegatedHandleRefusal::OutsideDelegationDirectory),
    };
    let team_name = team
        .to_str()
        .ok_or(DelegatedHandleRefusal::OutsideDelegationDirectory)?;
    let child_name = child
        .to_str()
        .ok_or(DelegatedHandleRefusal::OutsideDelegationDirectory)?;
    if !team_name.starts_with("team-") || !child_name.ends_with(".jsonl") {
        return Err(DelegatedHandleRefusal::OutsideDelegationDirectory);
    }
    let team_path = delegation_directory.join(team);
    let Ok(team_metadata) = team_path.symlink_metadata() else {
        return Err(DelegatedHandleRefusal::VanishedTranscript);
    };
    if team_metadata.file_type().is_symlink() || !team_metadata.file_type().is_dir() {
        return Err(DelegatedHandleRefusal::OutsideDelegationDirectory);
    }
    let Ok(child_metadata) = path.symlink_metadata() else {
        return Err(DelegatedHandleRefusal::VanishedTranscript);
    };
    if child_metadata.file_type().is_symlink() || !child_metadata.file_type().is_file() {
        return Err(DelegatedHandleRefusal::OutsideDelegationDirectory);
    }
    Ok(())
}

fn sanitize_session_name(name: &str) -> anyhow::Result<Option<String>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_SESSION_NAME_CHARS || name.chars().any(char::is_control) {
        anyhow::bail!(
            "session name must be at most {MAX_SESSION_NAME_CHARS} characters and contain no control characters"
        );
    }
    Ok(Some(name.to_owned()))
}

fn sanitize_session_tags(tags: &[String]) -> anyhow::Result<Vec<String>> {
    if tags.len() > MAX_SESSION_TAGS {
        anyhow::bail!("a session may have at most {MAX_SESSION_TAGS} tags");
    }
    let mut sanitized = Vec::with_capacity(tags.len());
    for tag in tags {
        let tag = tag.trim();
        if tag.is_empty()
            || tag.chars().count() > MAX_SESSION_TAG_CHARS
            || !tag.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.' | '/')
            })
        {
            anyhow::bail!(
                "session tags must be 1-{MAX_SESSION_TAG_CHARS} ASCII letters/digits or '-', '_', '.', '/'"
            );
        }
        if !sanitized.iter().any(|existing| existing == tag) {
            sanitized.push(tag.to_owned());
        }
    }
    Ok(sanitized)
}

fn validate_session_metadata(metadata: &SessionUserMetadata) -> anyhow::Result<()> {
    if metadata.trashed_at_ms.is_some() != metadata.purge_after_ms.is_some()
        || metadata
            .trashed_at_ms
            .zip(metadata.purge_after_ms)
            .is_some_and(|(trashed, purge)| trashed == 0 || purge <= trashed)
        || metadata.trashed_at_ms.is_some() && !metadata.archived
    {
        anyhow::bail!("invalid session trash retention metadata");
    }
    match (
        metadata.forked_from_session_id.as_deref(),
        metadata.forked_from_entry_id.as_deref(),
    ) {
        (None, None) => Ok(()),
        (Some(session), Some(entry))
            if session_id_is_valid(session)
                && !entry.is_empty()
                && entry.len() <= 256
                && !entry.chars().any(char::is_control) =>
        {
            Ok(())
        }
        _ => anyhow::bail!("invalid session fork provenance metadata"),
    }
}

pub(crate) fn absolute_read_path(path: &Path) -> anyhow::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session path has no parent: {}", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("session path has no filename: {}", path.display()))?;
    Ok(parent.canonicalize()?.join(name))
}

fn corrupt_summary(line: usize, message: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("corrupt session record at line {line}: {message}")
}

/// Result of a bounded transcript scan before filesystem catalog metadata is applied.
#[derive(Debug)]
struct TranscriptSummary {
    title: Option<String>,
    configured_model: Option<String>,
    configured_reasoning: Option<String>,
    message_count: usize,
    usage_records: Vec<SessionUsageRecord>,
    usage_uncertainty_records: Vec<octet_agent::UsageUncertaintyRecord>,
    #[cfg(test)]
    deferred_run_records: Vec<DeferredRunRecord>,
}

/// Replay of the deferred-run replaceable session state.
///
/// Mirrors `octet_agent::tools::deferred::DeferredRunStore::restore` (the
/// validation `Session::open_read_only` applies) so the lightweight mirror
/// cannot bless a file a normal resume rejects: every record is validated
/// against the store's default hard bounds, a terminal record is never followed
/// by another record for the same operation, and generations only move forward.
/// The last record per operation is authoritative, exactly like the store, and
/// retention is bounded by the store's own `max_runs` bound.
#[derive(Default)]
struct SummaryDeferredRuns {
    records: BTreeMap<String, DeferredRunRecord>,
    terminal: VecDeque<String>,
    limits: DeferredRunLimits,
}

impl SummaryDeferredRuns {
    fn restore(&mut self, record: DeferredRunRecord) -> Result<(), String> {
        record
            .validate(&self.limits)
            .map_err(|error| error.to_string())?;
        if let Some(existing) = self.records.get(&record.operation_id) {
            if existing.is_terminal() {
                return Err("a terminal deferred record may not be followed".to_owned());
            }
            if record.generation <= existing.generation {
                return Err(format!(
                    "deferred run {} generation regressed from {} to {}",
                    record.operation_id, existing.generation, record.generation
                ));
            }
        }
        let operation_id = record.operation_id.clone();
        let terminal = record.is_terminal();
        if !self.records.contains_key(&operation_id) && self.records.len() >= self.limits.max_runs {
            let mut evicted = false;
            while let Some(oldest) = self.terminal.pop_front() {
                if self.records.remove(&oldest).is_some() {
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                return Err(format!(
                    "{} deferred runs retained (limit {})",
                    self.records.len(),
                    self.limits.max_runs
                ));
            }
        }
        if terminal {
            self.terminal.push_back(operation_id.clone());
        }
        self.records.insert(operation_id, record);
        Ok(())
    }

    #[cfg(test)]
    fn into_records(self) -> Vec<DeferredRunRecord> {
        self.records.into_values().collect()
    }
}

/// Replay only the graph metadata needed by the session picker and serve
/// catalog. Large model, tool, media, skill, and compaction bodies are
/// consumed by serde without being retained. This deliberately mirrors
/// Session::open_read_only's graph checks and torn-final-record handling so the
/// fast path cannot bless a file that normal resume would reject.
fn summarize_session(path: &Path) -> anyhow::Result<TranscriptSummary> {
    summarize_session_with_usage(path, true)
}

fn summarize_catalog_session(path: &Path) -> anyhow::Result<TranscriptSummary> {
    summarize_session_with_usage(path, false)
}

fn summarize_session_with_usage(
    path: &Path,
    retain_usage_records: bool,
) -> anyhow::Result<TranscriptSummary> {
    let path = absolute_read_path(path)?;
    let file = octet_agent::secure_fs::open_regular_file_for_read(&path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_SESSION_FILE_BYTES as u64 {
        anyhow::bail!("session is {file_len} bytes (limit {MAX_SESSION_FILE_BYTES})");
    }
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut entries = HashMap::<EntryId, SummaryEntry>::new();
    let mut head = None;
    let mut checkpoints = Vec::<(EntryId, EntryId, usize)>::new();
    let mut usage_records = Vec::<SessionUsageRecord>::new();
    let mut usage_uncertainty_records = Vec::new();
    let mut deferred_runs = SummaryDeferredRuns::default();
    let mut line_bytes = Vec::new();
    let mut observed_bytes = 0usize;
    let mut line_no = 0usize;

    loop {
        line_bytes.clear();
        let read_limit = MAX_SESSION_FILE_BYTES
            .saturating_sub(observed_bytes)
            .saturating_add(1);
        let bytes_read = reader
            .by_ref()
            .take(u64::try_from(read_limit).expect("session byte limit fits u64"))
            .read_until(b'\n', &mut line_bytes)?;
        if bytes_read == 0 {
            break;
        }
        observed_bytes = observed_bytes
            .checked_add(bytes_read)
            .ok_or_else(|| anyhow::anyhow!("session read length overflow"))?;
        if observed_bytes > MAX_SESSION_FILE_BYTES {
            anyhow::bail!(
                "session exceeds the {MAX_SESSION_FILE_BYTES}-byte limit while being read"
            );
        }
        line_no += 1;
        if line_no > MAX_SESSION_RECORDS {
            anyhow::bail!("session has more than {MAX_SESSION_RECORDS} records");
        }
        let has_newline = line_bytes.last() == Some(&b'\n');
        let line_bytes = if has_newline {
            &line_bytes[..line_bytes.len() - 1]
        } else {
            line_bytes.as_slice()
        };
        let line = match std::str::from_utf8(line_bytes) {
            Ok(line) => line,
            Err(_) if !has_newline => break,
            Err(error) => return Err(corrupt_summary(line_no, format!("invalid UTF-8: {error}"))),
        };
        let record: SummaryRecord = match serde_json::from_str(line) {
            Ok(record) => record,
            // `read_until` returns a non-newline-terminated segment only at
            // EOF, so malformed bytes are recoverable only at that boundary.
            Err(_) if !has_newline => break,
            Err(error) => return Err(corrupt_summary(line_no, error)),
        };

        match record {
            SummaryRecord::Entry {
                id,
                parent,
                metadata,
                value,
            } => {
                if entries.contains_key(&id) {
                    return Err(corrupt_summary(
                        line_no,
                        format!("duplicate entry id {:?}", id.0),
                    ));
                }
                if let Some(parent) = &parent {
                    if !entries.contains_key(parent) {
                        return Err(corrupt_summary(
                            line_no,
                            format!("entry {:?} references unknown parent {:?}", id.0, parent.0),
                        ));
                    }
                }

                let (kind, title, assistant_route, configured_model, configured_reasoning) =
                    match value {
                        SummaryEntryValue::Message(SummaryMessage::User(message)) => {
                            let title = message.content.into_iter().find_map(|part| match part {
                                SummaryUserPart::Text(TitleText(title)) => Some(title),
                                SummaryUserPart::Media(_) | SummaryUserPart::ToolResult(_) => None,
                            });
                            (
                                SummaryEntryKind::User,
                                metadata
                                    .and_then(|metadata| metadata.display_text)
                                    .map(|text| text.0)
                                    .or(title),
                                None,
                                None,
                                None,
                            )
                        }
                        SummaryEntryValue::Message(SummaryMessage::Assistant(message)) => (
                            SummaryEntryKind::Assistant,
                            None,
                            Some((message.model, message.protocol)),
                            None,
                            None,
                        ),
                        SummaryEntryValue::Compaction { first_kept } => {
                            if !entries.contains_key(&first_kept) {
                                return Err(corrupt_summary(
                                    line_no,
                                    format!(
                                        "compaction {:?} references unknown first_kept {:?}",
                                        id.0, first_kept.0
                                    ),
                                ));
                            }
                            (SummaryEntryKind::Other, None, None, None, None)
                        }
                        SummaryEntryValue::ResponsesTurn {
                            assistant,
                            model,
                            output,
                            ..
                        } => {
                            let valid_assistant =
                                entries.get(&assistant).is_some_and(|candidate| {
                                    candidate.kind == SummaryEntryKind::Assistant
                                        && candidate.assistant_protocol
                                            == Some(Protocol::OpenAiResponses)
                                        && candidate.assistant_model.as_ref() == Some(&model)
                                });
                            if !valid_assistant
                                || parent.as_ref() != Some(&assistant)
                                || output.is_empty()
                            {
                                return Err(corrupt_summary(
                                    line_no,
                                    format!(
                                        "Responses turn {:?} is not a direct sidecar of assistant {:?}",
                                        id.0, assistant.0
                                    ),
                                ));
                            }
                            (SummaryEntryKind::Other, None, None, None, None)
                        }
                        SummaryEntryValue::ResponsesCompaction {
                            covered_through,
                            output,
                            ..
                        } => {
                            if !entries.contains_key(&covered_through)
                                || parent.as_ref() != Some(&covered_through)
                                || !output.has_valid_compaction()
                            {
                                return Err(corrupt_summary(
                                    line_no,
                                    format!(
                                        "Responses compaction {:?} is not a direct checkpoint of {:?}",
                                        id.0, covered_through.0
                                    ),
                                ));
                            }
                            (SummaryEntryKind::Other, None, None, None, None)
                        }
                        SummaryEntryValue::ResponsesReasoning {
                            model,
                            baseline,
                            update,
                        } => {
                            let reasoning = crate::app::reasoning_label(
                                update
                                    .as_ref()
                                    .map_or(&baseline, |update| &update.reasoning),
                            );
                            (
                                SummaryEntryKind::Other,
                                None,
                                None,
                                Some(model.0),
                                Some(reasoning),
                            )
                        }
                        SummaryEntryValue::Config { model, reasoning } => {
                            (SummaryEntryKind::Other, None, None, model, reasoning)
                        }
                        SummaryEntryValue::ResponsesSteering {}
                        | SummaryEntryValue::PromptTemplateSelected {}
                        | SummaryEntryValue::SkillActivated {}
                        | SummaryEntryValue::SkillResourceRead {}
                        | SummaryEntryValue::SkillDeactivated {} => {
                            (SummaryEntryKind::Other, None, None, None, None)
                        }
                    };
                let (assistant_model, assistant_protocol) = assistant_route
                    .map_or((None, None), |(model, protocol)| {
                        (Some(model), Some(protocol))
                    });
                let position = u32::try_from(entries.len()).expect("session record limit fits u32");
                entries.insert(
                    id,
                    SummaryEntry {
                        parent,
                        kind,
                        title,
                        position,
                        assistant_model,
                        assistant_protocol,
                        configured_model,
                        configured_reasoning,
                    },
                );
            }
            SummaryRecord::Head { id } => {
                if !entries.contains_key(&id) {
                    return Err(corrupt_summary(
                        line_no,
                        format!("head references unknown entry {:?}", id.0),
                    ));
                }
                head = Some(id);
            }
            SummaryRecord::RootHead {} => {
                head = None;
            }
            SummaryRecord::Checkpoint {
                prompt,
                head: checkpoint_head,
            } => {
                let prompt_is_user = entries
                    .get(&prompt)
                    .is_some_and(|entry| entry.kind == SummaryEntryKind::User);
                if !prompt_is_user || !entries.contains_key(&checkpoint_head) {
                    return Err(corrupt_summary(
                        line_no,
                        "checkpoint references unknown or non-user entries",
                    ));
                }
                checkpoints.push((prompt, checkpoint_head, line_no));
            }
            SummaryRecord::UsageUncertainty { record } => {
                // Mirror Session replay validation without retaining conversation bodies.
                for value in [&record.endpoint.0, &record.model.0, &record.operation] {
                    if value.is_empty()
                        || value.len() > 128
                        || value.contains("://")
                        || !value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.:/".contains(&byte))
                    {
                        return Err(corrupt_summary(
                            line_no,
                            "invalid usage uncertainty identifiers",
                        ));
                    }
                }
                if retain_usage_records {
                    usage_uncertainty_records.push(record);
                }
            }
            SummaryRecord::EntryLabel { entry_id, label } => {
                if !entries.contains_key(&entry_id)
                    || label.len() > octet_agent::session::MAX_ENTRY_LABEL_BYTES
                    || label.chars().any(char::is_control)
                {
                    return Err(corrupt_summary(line_no, "invalid entry label"));
                }
            }
            // Invocation memos/checkpoints are auxiliary, not transcript,
            // model selection, or usage. Their wire shape is still decoded.
            SummaryRecord::ToolInvocation { .. } => {}
            SummaryRecord::DeferredRun { record } => {
                // Replaceable state, not model-visible context: the record still
                // has to be valid and monotonic, and the last one per operation
                // wins, exactly as the durable store replays it.
                deferred_runs
                    .restore(record)
                    .map_err(|message| corrupt_summary(line_no, message))?;
            }
            SummaryRecord::Usage { record } => {
                if let SummaryUsageKind::AssistantTurn { assistant } = &record.kind {
                    let valid_assistant = entries
                        .get(assistant)
                        .is_some_and(|entry| entry.kind == SummaryEntryKind::Assistant);
                    if !valid_assistant {
                        return Err(corrupt_summary(
                            line_no,
                            "usage record references an unknown or non-assistant entry",
                        ));
                    }
                }
                if retain_usage_records {
                    usage_records.push(SessionUsageRecord {
                        endpoint: record.endpoint.map(|endpoint| endpoint.0),
                        model: record.model.map(|model| model.0),
                        completed_at_unix_ms: record.completed_at_unix_ms,
                        input_tokens: record.usage.input_tokens,
                        output_tokens: record.usage.output_tokens,
                        cache_read_tokens: record.usage.cache_read_tokens,
                        cache_write_tokens: record.usage.cache_write_tokens,
                        cache_write_1h_tokens: record.usage.cache_write_1h_tokens,
                        reasoning_tokens: record.usage.reasoning_tokens,
                        total_tokens: record.usage.total_tokens,
                    });
                }
            }
        }
    }

    if !checkpoints.is_empty() {
        let (entered, exited) = summary_ancestry_intervals(&entries);
        for (prompt, checkpoint_head, checkpoint_line) in checkpoints {
            let prompt = entries[&prompt].position as usize;
            let checkpoint_head = entries[&checkpoint_head].position as usize;
            let prompt_is_ancestor = entered[prompt] <= entered[checkpoint_head]
                && exited[checkpoint_head] <= exited[prompt];
            if !prompt_is_ancestor {
                return Err(corrupt_summary(
                    checkpoint_line,
                    "checkpoint prompt is not an ancestor of its head",
                ));
            }
        }
    }

    let mut oldest_title = None;
    let mut configured_model = None;
    let mut configured_reasoning = None;
    let mut message_count = 0usize;
    let mut cursor = head.as_ref();
    while let Some(id) = cursor {
        let Some(entry) = entries.get(id) else {
            break;
        };
        if matches!(
            entry.kind,
            SummaryEntryKind::User | SummaryEntryKind::Assistant
        ) {
            message_count = message_count.saturating_add(1);
        }
        if entry.kind == SummaryEntryKind::User {
            if let Some(title) = &entry.title {
                oldest_title = Some(title.clone());
            }
        }
        if configured_model.is_none() {
            configured_model = entry.configured_model.clone();
        }
        if configured_reasoning.is_none() {
            configured_reasoning = entry.configured_reasoning.clone();
        }
        cursor = entry.parent.as_ref();
    }
    Ok(TranscriptSummary {
        title: oldest_title,
        configured_model,
        configured_reasoning,
        message_count,
        usage_records,
        usage_uncertainty_records,
        #[cfg(test)]
        deferred_run_records: deferred_runs.into_records(),
    })
}

/// One session-entry search hit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EntrySearchHit {
    /// Workspace-local session id.
    pub session_id: String,
    /// Durable entry id inside that session.
    pub entry_id: String,
    /// Which conversation role produced the entry.
    pub kind: EntryKind,
    /// Bounded matching entry text.
    pub text: String,
}

/// Which conversation role produced an indexed entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    User,
    Assistant,
}

/// Result of one bounded incremental entry search.
#[derive(Clone, Debug, Default)]
pub struct EntrySearchOutcome {
    /// Matching entries, ordered by `(session_id, entry ordinal)`.
    pub hits: Vec<EntrySearchHit>,
    /// Whether the disposable index changed during this search.
    pub index_changed: bool,
    /// The entry-index revision observed after the search.
    pub revision: i64,
    /// How many transcripts this search had to re-read (the incremental bound).
    pub scanned_sessions: usize,
}

/// Watches the disposable entry-index revision so a caller is notified exactly
/// when the index changed since its previous observation.
///
/// The revision only advances when a session's fingerprint changed or a session
/// vanished, so a repeated observation with no transcript change is silent.
#[derive(Clone, Debug, Default)]
#[cfg(test)]
pub struct SessionSearchWatcher {
    last_revision: Option<i64>,
}

#[cfg(test)]
impl SessionSearchWatcher {
    /// Observe the current revision. Returns `true` exactly once per change.
    pub fn observe(&mut self, revision: i64) -> bool {
        let changed = self.last_revision != Some(revision);
        self.last_revision = Some(revision);
        changed
    }
}

/// Extract a bounded, user-visible entry projection for the incremental search
/// index.
///
/// Only submitted user text and assistant-visible text are retained; reasoning,
/// tool calls/arguments, media, provider metadata and private answers are never
/// indexed. The index is disposable and rebuilt from JSONL, so this is a lenient
/// scan that does not re-run the graph validation `summarize_session` performs;
/// it still honours the same byte and record bounds.
pub(crate) fn index_session_entries(path: &Path) -> anyhow::Result<Vec<IndexedEntry>> {
    search_projection::index(path)
}

#[cfg(test)]
fn indexed_entry_from_record(record: &serde_json::Value) -> Option<IndexedEntry> {
    search_projection::from_value(record)
}

/// Directory (inside the workspace session store) holding accounting-only
/// records for ephemeral `--no-session` runs.
const EPHEMERAL_ACCOUNTING_DIRECTORY: &str = ".accounting";
const EPHEMERAL_ACCOUNTING_FILE: &str = "ephemeral-sessions.jsonl";
const EPHEMERAL_ACCOUNTING_RECOVERY: &str = ".accounting-recovery.json";
const MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES: usize = 256 * 1024;
const MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES: u64 = 64 * 1024 * 1024;

/// Durable, conversation-free accounting for one ephemeral (`--no-session`) run.
///
/// The transcript is discarded, but provider usage, cost and any
/// usage-uncertainty exposure are recorded so cost accounting stays complete
/// and fail-closed across the run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EphemeralAccountingRecord {
    /// Stable invocation key, allowing recovery after an ambiguous append.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accounting_id: Option<String>,
    /// Wall-clock time the record was durably appended.
    pub recorded_at_unix_ms: u64,
    /// Cumulative session cost after the run, in microdollars.
    pub session_cost_microdollars: u64,
    /// Whether the run has unknown usage or completed operations without exact pricing.
    pub has_uncertain_usage: bool,
    /// Provider usage records, exactly as the transcript recorded them.
    pub usage_records: Vec<octet_agent::UsageRecord>,
    /// Unknown-usage exposure records.
    pub usage_uncertainty_records: Vec<octet_agent::UsageUncertaintyRecord>,
}

impl EphemeralAccountingRecord {
    /// Derive price uncertainty from the retained receipts as well as the flag:
    /// historical accounting-only ledgers predate unpriced-call reporting.
    fn retain_accounting_uncertainty(&mut self) {
        self.has_uncertain_usage |= !self.usage_uncertainty_records.is_empty()
            || self
                .usage_records
                .iter()
                .any(|record| record.cost.is_none() && record.cost_microdollars.is_none());
    }
}

/// Aggregate durable ephemeral accounting for one workspace store.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EphemeralAccountingSummary {
    /// Number of ephemeral runs recorded.
    pub runs: usize,
    /// Sum of every recorded run's cumulative session cost.
    pub total_cost_microdollars: u64,
    /// Whether any recorded run had unknown usage or absent exact pricing.
    pub has_uncertain_usage: bool,
    /// Provider usage records kept across every ephemeral run.
    pub usage_records: usize,
    /// Input tokens across every kept usage record.
    pub input_tokens: u64,
    /// Output tokens across every kept usage record.
    pub output_tokens: u64,
    /// Unknown-usage exposure records kept across every ephemeral run.
    pub uncertainty_records: usize,
}

struct EphemeralRun {
    transcript_root: PathBuf,
    accounting_session_dir: PathBuf,
    workspace: PathBuf,
    pending: Option<EphemeralAccountingRecord>,
}

/// The one active ephemeral run, if any. A shared process may take a single
/// `--no-session` run at a time.
static EPHEMERAL_RUN: Mutex<Option<EphemeralRun>> = Mutex::new(None);

// Unit tests share the process-wide invocation slot even when the test runner
// executes unrelated cases concurrently. Keep their separate fixtures serialized.
#[cfg(test)]
pub(crate) static EPHEMERAL_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Register an ephemeral run: its transcript lives under `transcript_root` and
/// is deleted afterwards, while accounting is persisted into
/// `SessionStore::new(accounting_session_dir, workspace)`.
pub fn begin_ephemeral_run(
    transcript_root: PathBuf,
    accounting_session_dir: PathBuf,
    workspace: PathBuf,
) {
    *EPHEMERAL_RUN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(EphemeralRun {
        transcript_root,
        accounting_session_dir,
        workspace,
        pending: None,
    });
}

/// Persist all sessions in the active ephemeral invocation and discard conversations.
///
/// Failure keeps the run registered for retry. A private accounting-only snapshot
/// is staged before append, so an append failure never requires a transcript to
/// survive. The returned error retains the original append failure and identifies
/// the recovery file (which also survives process exit).
pub fn finish_ephemeral_run() -> anyhow::Result<Option<EphemeralAccountingRecord>> {
    let mut active = EPHEMERAL_RUN
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(run) = active.as_mut() else {
        return Ok(None);
    };
    let result = finish_ephemeral_run_state(run);
    if result.is_ok() {
        *active = None;
    }
    result
}

fn finish_ephemeral_run_state(
    run: &mut EphemeralRun,
) -> anyhow::Result<Option<EphemeralAccountingRecord>> {
    use anyhow::Context as _;

    let workspace_dir = run.transcript_root.join(workspace_key(&run.workspace));
    let recovery = run.transcript_root.join(EPHEMERAL_ACCOUNTING_RECOVERY);
    let snapshot = (|| -> anyhow::Result<()> {
        if run.pending.is_none() {
            // A caller may re-register the same recovery root after process exit.
            run.pending = if recovery.exists() {
                Some(serde_json::from_slice(
                    &octet_agent::secure_fs::read_private_file_bounded(
                        &recovery,
                        MAX_SESSION_FILE_BYTES,
                    )?,
                )?)
            } else {
                collect_ephemeral_accounting(&workspace_dir, &run.transcript_root)?
            };
        }
        if let Some(record) = &mut run.pending {
            record.retain_accounting_uncertainty();
            let bytes = serde_json::to_vec(record)?;
            octet_agent::secure_fs::write_private_atomic(
                &recovery,
                &bytes,
                MAX_SESSION_FILE_BYTES,
            )?;
        }
        Ok(())
    })();
    // Privacy does not depend on the ledger (or recovery filesystem) being writable.
    // If even staging fails, the in-process accounting-only snapshot still permits
    // retry; report that failure, rather than falsely claiming durable recovery.
    let cleanup = match std::fs::remove_dir_all(&workspace_dir) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    snapshot.context("could not stage ephemeral accounting recovery; retry before exit")?;
    cleanup.context("could not remove ephemeral conversation directory")?;
    if let Some(record) = &run.pending {
        let store = SessionStore::new(&run.accounting_session_dir, &run.workspace);
        store.append_ephemeral_accounting(record).with_context(|| {
            format!(
                "ephemeral accounting append failed; accounting-only recovery retained at {}",
                recovery.display()
            )
        })?;
    }
    std::fs::remove_dir_all(&run.transcript_root)?;
    Ok(run.pending.clone())
}

fn collect_ephemeral_accounting(
    directory: &Path,
    invocation: &Path,
) -> anyhow::Result<Option<EphemeralAccountingRecord>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    let mut combined: Option<EphemeralAccountingRecord> = None;
    for path in paths {
        let record = read_ephemeral_accounting(&path)?;
        if let Some(total) = &mut combined {
            total.session_cost_microdollars = total
                .session_cost_microdollars
                .saturating_add(record.session_cost_microdollars);
            total.has_uncertain_usage |= record.has_uncertain_usage;
            total.usage_records.extend(record.usage_records);
            total
                .usage_uncertainty_records
                .extend(record.usage_uncertainty_records);
        } else {
            combined = Some(record);
        }
    }
    if let Some(record) = &mut combined {
        record.accounting_id = Some(workspace_key(invocation));
    }
    Ok(combined)
}

fn read_ephemeral_accounting(transcript: &Path) -> anyhow::Result<EphemeralAccountingRecord> {
    let session = Session::open_read_only(transcript.to_path_buf())
        .map_err(|error| anyhow::anyhow!("ephemeral accounting could not read the run: {error}"))?;
    let usage_uncertainty_records = session.usage_uncertainty_records().to_vec();
    Ok(EphemeralAccountingRecord {
        accounting_id: None,
        recorded_at_unix_ms: now_unix_ms(),
        session_cost_microdollars: session.total_cost_microdollars(),
        has_uncertain_usage: session.has_uncertain_usage() || session.has_unpriced_usage(),
        usage_records: session.usage_records().to_vec(),
        usage_uncertainty_records,
    })
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn remove_regular_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_file() => {
            std::fs::remove_file(path)?;
            Ok(())
        }
        Ok(_) => anyhow::bail!("session deletion path is not a regular file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Best-effort directory durability sync after a session-store rename.
///
/// `File::open` yields a read-only directory handle on Windows, and
/// `FlushFileBuffers` on it fails with `ERROR_ACCESS_DENIED`. File data is
/// already synced before these renames, so directory durability stays
/// best-effort there instead of failing the operation.
fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // `FILE_FLAG_BACKUP_SEMANTICS` (Win32 constant; `windows-sys` is not
        // available with the Storage feature in this crate).
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        let result = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .and_then(|directory| directory.sync_all());
        let _ = result;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open(path)?.sync_all()
    }
}

fn remove_staged_deletion_files(directory: &Path, id: &str) -> anyhow::Result<()> {
    for path in staged_deletion_files(directory, id)? {
        remove_regular_file_if_exists(&path)?;
    }
    Ok(())
}

fn staged_deletion_files(directory: &Path, id: &str) -> anyhow::Result<Vec<PathBuf>> {
    let prefix = format!(".delete-{id}-");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(suffix) = name.to_str().and_then(|name| name.strip_prefix(&prefix)) else {
            continue;
        };
        if suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

#[cfg(test)]
mod tests;
