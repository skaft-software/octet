#![allow(missing_docs)]

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
    Config {
        model: Option<String>,
        reasoning: Option<String>,
    },
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
                        SummaryEntryValue::Config { model, reasoning } => {
                            (SummaryEntryKind::Other, None, None, model, reasoning)
                        }
                        SummaryEntryValue::PromptTemplateSelected {}
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
    let path = absolute_read_path(path)?;
    let file = octet_agent::secure_fs::open_regular_file_for_read(&path)?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_SESSION_FILE_BYTES as u64 {
        anyhow::bail!("session is {file_len} bytes (limit {MAX_SESSION_FILE_BYTES})");
    }
    let mut reader = BufReader::with_capacity(1024 * 1024, file);
    let mut line_bytes = Vec::new();
    let mut observed_bytes = 0usize;
    let mut line_no = 0usize;
    let mut entries = Vec::new();
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
            Err(_) => continue,
        };
        let record = match serde_json::from_str::<serde_json::Value>(line) {
            Ok(record) => record,
            Err(_) if !has_newline => break,
            Err(_) => continue,
        };
        if let Some(entry) = indexed_entry_from_record(&record) {
            entries.push(entry);
            if entries.len() >= MAX_INDEXED_ENTRIES_PER_SESSION {
                break;
            }
        }
    }
    Ok(entries)
}

fn indexed_entry_from_record(record: &serde_json::Value) -> Option<IndexedEntry> {
    if record.get("type").and_then(|value| value.as_str()) != Some("entry") {
        return None;
    }
    let entry_id = record.get("id")?.as_str()?.to_owned();
    let value = record.get("value")?;
    if value.get("type").and_then(|value| value.as_str()) != Some("message") {
        return None;
    }
    let (role, kind) = if value.get("User").is_some() {
        ("User", IndexedEntryKind::User)
    } else if value.get("Assistant").is_some() {
        ("Assistant", IndexedEntryKind::Assistant)
    } else {
        return None;
    };
    let parts = value.get(role)?.get("content")?.as_array()?;
    let mut text = String::new();
    for part in parts {
        let Some(part_text) = part.get("Text").and_then(|value| value.as_str()) else {
            continue;
        };
        if !text.is_empty() {
            text.push('\n');
        }
        text.extend(part_text.chars().take(MAX_INDEXED_ENTRY_CHARS));
        if text.chars().count() >= MAX_INDEXED_ENTRY_CHARS {
            break;
        }
    }
    let text = text
        .chars()
        .take(MAX_INDEXED_ENTRY_CHARS)
        .collect::<String>();
    if text.trim().is_empty() {
        return None;
    }
    Some(IndexedEntry {
        entry_id,
        kind,
        text,
    })
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

impl SessionStore {
    /// Create a store rooted at `<session_dir>/<workspace-key>`.
    pub fn new(session_dir: &Path, workspace: &Path) -> Self {
        Self {
            dir: session_dir.join(workspace_key(workspace)),
            root: session_dir.to_path_buf(),
            workspace: Some(workspace.to_path_buf()),
        }
    }

    /// Create a store for an already-known workspace directory, recovering the
    /// workspace path from its `.workspace` marker when present.
    ///
    /// Used for cross-workspace browsing and mutation (the workspace-key hash
    /// is one-way, so the marker is the only way to learn which workspace a
    /// directory belongs to). Older binaries ignore the marker file.
    pub fn for_directory(dir: &Path, root: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            root: root.to_path_buf(),
            workspace: Self::read_workspace_marker(dir),
        }
    }

    /// The workspace-scoped session directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The shared sessions root containing every workspace directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical workspace path, when this store knows it.
    pub fn workspace(&self) -> Option<&Path> {
        self.workspace.as_deref()
    }

    /// Write the workspace path marker so future (and other) processes can
    /// display and scope sessions from this directory.
    pub fn write_workspace_marker(&self) -> anyhow::Result<()> {
        let Some(workspace) = self.workspace.as_ref() else {
            anyhow::bail!("store has no workspace to record");
        };
        std::fs::create_dir_all(&self.dir)?;
        let marker = self.dir.join(WORKSPACE_MARKER);
        crate::auth::write_private_atomic(
            &marker,
            format!("{}\n", workspace.display()).as_bytes(),
            ".workspace-",
        )?;
        Ok(())
    }

    /// Read the workspace path marker from a workspace directory, if present.
    pub(crate) fn read_workspace_marker(dir: &Path) -> Option<PathBuf> {
        let bytes = std::fs::read(dir.join(WORKSPACE_MARKER)).ok()?;
        let text = String::from_utf8_lossy(&bytes);
        let path = text.lines().next()?.trim().to_owned();
        if path.is_empty() {
            return None;
        }
        Some(PathBuf::from(path))
    }

    /// List sessions across every workspace under the shared root, newest
    /// first. Each row carries its workspace path when the store marker is
    /// readable; otherwise `workspace` is `None`.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn list_all(&self) -> Vec<SessionMeta> {
        let mut all = Vec::new();
        for entry in std::fs::read_dir(&self.root).into_iter().flatten() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            let is_dir = match entry.file_type() {
                Ok(file_type) => file_type.is_dir(),
                Err(_) => false,
            };
            if !is_dir {
                continue;
            }
            let dir = entry.path();
            let store = if dir == self.dir {
                self.clone()
            } else {
                Self::for_directory(&dir, &self.root)
            };
            all.extend(store.list());
        }
        all.sort_by_key(|meta| std::cmp::Reverse(meta.modified));
        all
    }

    /// Allocate a new JSONL path. The caller supplies a timestamp for testability.
    pub fn new_path(&self, stamp: &str) -> PathBuf {
        let suffix = NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed);
        self.dir.join(format!("{stamp}-{suffix:04x}.jsonl"))
    }

    /// Bounded incremental entry search over this workspace's sessions.
    ///
    /// Only transcripts whose fingerprint changed since the last search are
    /// re-read; unchanged sessions are served from the disposable catalog, so a
    /// repeat search does not re-scan the workspace.
    pub fn search_entries(&self, query: &str, limit: usize) -> anyhow::Result<EntrySearchOutcome> {
        self.search_entries_with(query, limit, index_session_entries)
    }

    pub(crate) fn search_entries_with<F>(
        &self,
        query: &str,
        limit: usize,
        extractor: F,
    ) -> anyhow::Result<EntrySearchOutcome>
    where
        F: Fn(&Path) -> anyhow::Result<Vec<IndexedEntry>>,
    {
        let candidates = self.candidates();
        let (mut catalog, _) = SessionCatalog::open_loaded(&self.dir)?;
        let indexed = catalog.entry_fingerprints()?;
        let current_ids = candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            })
            .collect::<HashSet<_>>();
        let stale_ids = indexed
            .keys()
            .filter(|id| !current_ids.contains(*id))
            .cloned()
            .collect::<HashSet<_>>();
        let mut updates = Vec::new();
        for candidate in &candidates {
            let Some(id) = candidate
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let Some(fingerprint) = catalog_fingerprint(candidate) else {
                continue;
            };
            if indexed.get(&id) == Some(&fingerprint) {
                continue;
            }
            if let Ok(entries) = extractor(&candidate.path) {
                updates.push(IndexedEntryUpdate {
                    session_id: id,
                    fingerprint,
                    entries,
                });
            }
        }
        let scanned_sessions = updates.len();
        let index_changed = catalog.apply_entries(&updates, &stale_ids)?;
        let revision = catalog.entry_revision()?;
        let hits = catalog
            .search_entries(query, limit)?
            .into_iter()
            .map(|hit| EntrySearchHit {
                session_id: hit.session_id,
                entry_id: hit.entry_id,
                kind: match hit.kind {
                    IndexedEntryKind::User => EntryKind::User,
                    IndexedEntryKind::Assistant => EntryKind::Assistant,
                },
                text: hit.text,
            })
            .collect();
        Ok(EntrySearchOutcome {
            hits,
            index_changed,
            revision,
            scanned_sessions,
        })
    }

    /// Persist only the durable accounting for one ephemeral transcript.
    ///
    /// Reads the run's usage and unknown-usage records plus its cumulative cost
    /// and appends them to the workspace's accounting ledger. The conversation
    /// itself is never copied.
    #[cfg(test)]
    pub fn record_ephemeral_accounting(
        &self,
        transcript: &Path,
    ) -> anyhow::Result<EphemeralAccountingRecord> {
        let record = read_ephemeral_accounting(transcript)?;
        self.append_ephemeral_accounting(&record)?;
        Ok(record)
    }

    fn append_ephemeral_accounting(
        &self,
        record: &EphemeralAccountingRecord,
    ) -> anyhow::Result<()> {
        let mut record = record.clone();
        record.retain_accounting_uncertainty();
        let directory = self.dir.join(EPHEMERAL_ACCOUNTING_DIRECTORY);
        octet_agent::secure_fs::create_private_directory_all(&directory)?;
        let path = directory.join(EPHEMERAL_ACCOUNTING_FILE);
        let mut line = serde_json::to_vec(&record)?;
        if line.len() > MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES {
            anyhow::bail!(
                "ephemeral accounting record is {} bytes (limit {MAX_EPHEMERAL_ACCOUNTING_LINE_BYTES})",
                line.len()
            );
        }
        line.push(b'\n');
        let mut file = match octet_agent::secure_fs::open_regular_file_for_append(&path) {
            Ok(file) => file,
            Err(octet_agent::secure_fs::SecureFileError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                octet_agent::secure_fs::create_regular_file_for_append(&path)?
            }
            Err(error) => return Err(error.into()),
        };
        // Serialize read/deduplicate/repair/append across concurrent invocations.
        // A complete record whose fsync failed is retried by syncing, not appending
        // it again. A torn trailing write is discarded before retrying its snapshot.
        fs2::FileExt::lock_exclusive(&file)?;
        let mut existing = Vec::new();
        (&mut file)
            .take(MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES + 1)
            .read_to_end(&mut existing)?;
        if existing.len() as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
        }
        if !existing.is_empty() && existing.last() != Some(&b'\n') {
            let tail = existing
                .iter()
                .rposition(|byte| *byte == b'\n')
                .map_or(0, |i| i + 1);
            if serde_json::from_slice::<EphemeralAccountingRecord>(&existing[tail..]).is_ok() {
                file.write_all(b"\n")?;
                existing.push(b'\n');
            } else {
                file.set_len(tail as u64)?;
                existing.truncate(tail);
            }
        }
        if let Some(id) = &record.accounting_id {
            for prior in existing
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                let mut prior: EphemeralAccountingRecord = serde_json::from_slice(prior)?;
                prior.retain_accounting_uncertainty();
                if prior.accounting_id.as_ref() == Some(id) {
                    if serde_json::to_value(&prior)? != serde_json::to_value(&record)? {
                        anyhow::bail!("ephemeral accounting recovery key has conflicting data");
                    }
                    file.sync_all()?;
                    return Ok(());
                }
            }
        }
        if (existing.len() + line.len()) as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!("ephemeral accounting ledger exceeds its byte limit");
        }
        file.write_all(&line)?;
        file.sync_all()?;
        Ok(())
    }

    /// Aggregate durable accounting for every ephemeral run in this workspace.
    ///
    /// `has_uncertain_usage` is fail-closed: while any recorded run exposed
    /// unknown usage or absent exact pricing, the workspace total remains uncertain.
    pub fn ephemeral_accounting_summary(&self) -> anyhow::Result<EphemeralAccountingSummary> {
        let path = self
            .dir
            .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
            .join(EPHEMERAL_ACCOUNTING_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(EphemeralAccountingSummary::default())
            }
            Err(error) => return Err(error.into()),
        };
        if bytes.len() as u64 > MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES {
            anyhow::bail!(
                "ephemeral accounting ledger is {} bytes (limit {MAX_EPHEMERAL_ACCOUNTING_LEDGER_BYTES})",
                bytes.len()
            );
        }
        let mut summary = EphemeralAccountingSummary::default();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Ok(mut record) = serde_json::from_str::<EphemeralAccountingRecord>(line) else {
                continue;
            };
            record.retain_accounting_uncertainty();
            summary.runs += 1;
            summary.total_cost_microdollars = summary
                .total_cost_microdollars
                .saturating_add(record.session_cost_microdollars);
            summary.has_uncertain_usage |= record.has_uncertain_usage;
            summary.usage_records += record.usage_records.len();
            summary.uncertainty_records += record.usage_uncertainty_records.len();
            for usage in &record.usage_records {
                summary.input_tokens = summary
                    .input_tokens
                    .saturating_add(usage.usage.input_tokens);
                summary.output_tokens = summary
                    .output_tokens
                    .saturating_add(usage.usage.output_tokens);
            }
        }
        Ok(summary)
    }

    fn candidates(&self) -> Vec<SessionCandidate> {
        let mut candidates = std::fs::read_dir(&self.dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if !entry.file_type().ok()?.is_file() {
                    return None;
                }
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                    return None;
                }
                let metadata = entry.metadata().ok()?;
                let modified = metadata.modified().ok()?;
                Some(SessionCandidate {
                    path,
                    modified,
                    file_size: metadata.len(),
                })
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.modified));
        candidates
    }

    /// Lists safe regular JSONL filename stems without parsing transcript content.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn session_file_ids(&self) -> Vec<String> {
        self.candidates()
            .into_iter()
            .filter_map(|candidate| {
                let id = candidate.path.file_stem()?.to_str()?.to_owned();
                session_id_is_valid(&id).then_some(id)
            })
            .collect()
    }

    /// Sort named, already-authorized session IDs by transcript mtime without
    /// enumerating or parsing other workspace sessions.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn session_ids_newest_first<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Vec<String> {
        let mut candidates = ids
            .into_iter()
            .filter_map(|id| {
                self.candidate_by_id(id)
                    .ok()
                    .map(|candidate| (id.to_owned(), candidate.modified))
            })
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
        candidates.into_iter().map(|(id, _)| id).collect()
    }

    fn candidate_by_id(&self, id: &str) -> anyhow::Result<SessionCandidate> {
        let path = self.path_by_id(id)?;
        let metadata = path.symlink_metadata().map_err(|error| {
            anyhow::anyhow!("session {id:?} could not be inspected after lookup: {error}")
        })?;
        if !metadata.file_type().is_file() {
            anyhow::bail!("session {id:?} is not a regular file");
        }
        Ok(SessionCandidate {
            path,
            modified: metadata.modified()?,
            file_size: metadata.len(),
        })
    }

    fn meta_from_parts(
        &self,
        candidate: SessionCandidate,
        id: String,
        fallback_title: String,
        metadata: SessionUserMetadata,
        message_count: usize,
    ) -> SessionMeta {
        let title = metadata
            .name
            .clone()
            .unwrap_or_else(|| fallback_title.clone());
        let modified = self
            .metadata_path(&id)
            .ok()
            .and_then(|path| path.symlink_metadata().ok())
            .filter(|metadata| metadata.file_type().is_file())
            .and_then(|metadata| metadata.modified().ok())
            .map_or(candidate.modified, |metadata_modified| {
                std::cmp::max(candidate.modified, metadata_modified)
            });
        SessionMeta {
            id,
            path: candidate.path,
            title,
            name: metadata.name,
            tags: metadata.tags,
            pinned: metadata.pinned,
            archived: metadata.archived,
            trashed_at_ms: metadata.trashed_at_ms,
            purge_after_ms: metadata.purge_after_ms,
            forked_from_session_id: metadata.forked_from_session_id,
            forked_from_entry_id: metadata.forked_from_entry_id,
            message_count,
            modified,
            workspace: self.workspace.clone(),
        }
    }

    fn inspect_candidate(
        &self,
        candidate: SessionCandidate,
        retain_usage_records: bool,
    ) -> anyhow::Result<SessionCatalogInspection> {
        let id = candidate
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .filter(|id| session_id_is_valid(id))
            .ok_or_else(|| anyhow::anyhow!("session has an invalid filename"))?
            .to_owned();
        let transcript = if retain_usage_records {
            summarize_session(&candidate.path)?
        } else {
            summarize_catalog_session(&candidate.path)?
        };
        let metadata = transcript
            .title
            .is_some()
            .then(|| self.load_metadata(&id))
            .transpose()?
            .unwrap_or_default();
        let meta = transcript.title.map(|title| {
            self.meta_from_parts(candidate, id, title, metadata, transcript.message_count)
        });
        Ok(SessionCatalogInspection {
            catalog: SessionCatalogEntry {
                meta,
                configured_model: transcript.configured_model,
                configured_reasoning: transcript.configured_reasoning,
            },
            usage_records: transcript.usage_records,
            usage_uncertainty_records: transcript.usage_uncertainty_records,
        })
    }

    /// Inspect one named transcript without enumerating or parsing unrelated
    /// sessions. The bounded scan validates its graph and torn tail before
    /// returning catalog and usage projections.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn inspect_by_id(&self, id: &str) -> anyhow::Result<SessionCatalogInspection> {
        self.inspect_candidate(self.candidate_by_id(id)?, true)
    }

    fn catalog_entry_from_cached_summary(
        &self,
        candidate: SessionCandidate,
        id: String,
        summary: CachedTranscriptSummary,
    ) -> anyhow::Result<SessionCatalogEntry> {
        let CachedTranscriptSummary::Summary {
            title,
            configured_model,
            configured_reasoning,
            message_count,
        } = summary
        else {
            anyhow::bail!("session {id:?} is unreadable");
        };
        let metadata = title
            .as_ref()
            .map(|_| self.load_metadata(&id))
            .transpose()?
            .unwrap_or_default();
        Ok(SessionCatalogEntry {
            meta: title
                .map(|title| self.meta_from_parts(candidate, id, title, metadata, message_count)),
            configured_model,
            configured_reasoning,
        })
    }

    /// Load catalog metadata for one named transcript without scanning the
    /// workspace catalog.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn catalog_by_id(&self, id: &str) -> anyhow::Result<SessionCatalogEntry> {
        self.catalog_by_ids([id])?
            .into_iter()
            .next()
            .map(|(_, entry)| entry)
            .ok_or_else(|| anyhow::anyhow!("session {id:?} is unavailable"))
    }

    /// Load catalog metadata for several already-authorized transcripts while
    /// opening the disposable catalog and reading its cached rows only once.
    ///
    /// The requested IDs are never expanded into a workspace-wide listing. A
    /// missing, invalid, or unreadable ID is omitted just as a failed targeted
    /// [`Self::catalog_by_id`] lookup is omitted by Serve callers.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn catalog_by_ids<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> anyhow::Result<Vec<(String, SessionCatalogEntry)>> {
        let (mut catalog, cached) = SessionCatalog::open_loaded(&self.dir)?;
        let mut updates = Vec::new();
        let mut entries = Vec::new();

        for id in ids {
            let Ok(candidate) = self.candidate_by_id(id) else {
                continue;
            };
            let fingerprint = catalog_fingerprint(&candidate);
            if let Some(summary) = fingerprint.and_then(|fingerprint| {
                cached
                    .get(id)
                    .filter(|cached| cached.fingerprint == fingerprint)
                    .map(|cached| cached.summary.clone())
            }) {
                if let Ok(entry) =
                    self.catalog_entry_from_cached_summary(candidate, id.to_owned(), summary)
                {
                    entries.push((id.to_owned(), entry));
                }
                continue;
            }

            let Ok(inspection) = self.inspect_candidate(candidate, false) else {
                continue;
            };
            if let Some(fingerprint) = fingerprint {
                let summary = CachedTranscriptSummary::Summary {
                    title: inspection
                        .catalog
                        .meta
                        .as_ref()
                        .map(|meta| meta.title.clone()),
                    configured_model: inspection.catalog.configured_model.clone(),
                    configured_reasoning: inspection.catalog.configured_reasoning.clone(),
                    message_count: inspection
                        .catalog
                        .meta
                        .as_ref()
                        .map_or(0, |meta| meta.message_count),
                };
                updates.push(CatalogUpdate {
                    id: id.to_owned(),
                    fingerprint,
                    summary,
                });
            }
            entries.push((id.to_owned(), inspection.catalog));
        }

        catalog.apply(&updates, &HashSet::new())?;
        Ok(entries)
    }

    /// Build catalog metadata from the already authorized, fully replayed
    /// session rather than reopening its pathname.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub(crate) fn meta_for_open_session(
        &self,
        id: &str,
        session: &Session,
    ) -> anyhow::Result<Option<SessionMeta>> {
        let candidate = self.candidate_by_id(id)?;
        if absolute_read_path(session.path())? != absolute_read_path(&candidate.path)? {
            anyhow::bail!("opened session does not match requested session id {id:?}");
        }
        let Some(title) = active_branch_catalog_title(session) else {
            return Ok(None);
        };
        Ok(Some(self.meta_from_parts(
            candidate,
            id.to_owned(),
            title,
            self.load_metadata(id)?,
            active_branch_message_count(session),
        )))
    }

    /// Refresh the disposable title projection from an already replayed session.
    /// This keeps a normally closed session warm without reopening its JSONL.
    pub(crate) fn refresh_catalog_for_open_session(&self, session: &Session) -> anyhow::Result<()> {
        let id = session
            .path()
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| anyhow::anyhow!("opened session path has no UTF-8 filename stem"))?;
        let candidate = self.candidate_by_id(id)?;
        if absolute_read_path(session.path())? != absolute_read_path(&candidate.path)? {
            anyhow::bail!("opened session does not belong to this workspace store");
        }
        let fingerprint = catalog_fingerprint(&candidate)
            .ok_or_else(|| anyhow::anyhow!("session fingerprint is outside catalog bounds"))?;
        let (configured_model, configured_reasoning) = active_branch_catalog_config(session);
        let update = CatalogUpdate {
            id: id.to_owned(),
            fingerprint,
            summary: CachedTranscriptSummary::Summary {
                title: active_branch_catalog_title(session),
                configured_model,
                configured_reasoning,
                message_count: active_branch_message_count(session),
            },
        };
        let (mut catalog, _) = SessionCatalog::open_loaded(&self.dir)?;
        catalog.apply(&[update], &HashSet::new())
    }

    /// Remove one disposable row after its authoritative transcript is removed.
    pub(crate) fn remove_catalog_entry(&self, id: &str) -> anyhow::Result<()> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        if !SessionCatalog::exists(&self.dir) {
            return Ok(());
        }
        let (mut catalog, _) = SessionCatalog::open_loaded(&self.dir)?;
        catalog.apply(&[], &HashSet::from([id.to_owned()]))
    }

    /// Load one session's validated catalog metadata without scanning unrelated
    /// transcripts.
    #[cfg(test)]
    pub(crate) fn get_by_id(&self, id: &str) -> anyhow::Result<Option<SessionMeta>> {
        Ok(self.catalog_by_id(id)?.meta)
    }

    fn meta_from_cached_summary(
        &self,
        candidate: SessionCandidate,
        id: String,
        summary: CachedTranscriptSummary,
    ) -> Option<SessionMeta> {
        let (fallback_title, message_count) = match summary {
            CachedTranscriptSummary::Summary {
                title,
                message_count,
                ..
            } => (title?, message_count),
            CachedTranscriptSummary::Unreadable => ("(unreadable session)".to_owned(), 0),
        };
        let metadata = self.load_metadata(&id).unwrap_or_default();
        Some(self.meta_from_parts(candidate, id, fallback_title, metadata, message_count))
    }

    fn discover_with_summarizer<F>(
        &self,
        candidates: Vec<SessionCandidate>,
        first_only: bool,
        summarizer: F,
    ) -> Vec<SessionMeta>
    where
        F: Fn(&Path) -> anyhow::Result<TranscriptSummary>,
    {
        let current_ids = candidates
            .iter()
            .filter_map(|candidate| {
                candidate
                    .path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .map(str::to_owned)
            })
            .collect::<HashSet<_>>();
        let (catalog, cached) = match SessionCatalog::open_loaded(&self.dir) {
            Ok((catalog, cached)) => (Some(catalog), cached),
            Err(_) => (None, HashMap::new()),
        };
        let stale_ids = cached
            .keys()
            .filter(|id| !current_ids.contains(*id))
            .cloned()
            .collect::<HashSet<_>>();
        let mut updates = Vec::new();
        let mut discovered = Vec::new();

        for candidate in candidates {
            let Some(id) = candidate
                .path
                .file_stem()
                .and_then(|value| value.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            let fingerprint = catalog_fingerprint(&candidate);
            let summary = fingerprint
                .and_then(|fingerprint| {
                    cached
                        .get(&id)
                        .filter(|cached| cached.fingerprint == fingerprint)
                })
                .map(|cached| cached.summary.clone())
                .unwrap_or_else(|| match summarizer(&candidate.path) {
                    Ok(transcript) => {
                        let summary = CachedTranscriptSummary::Summary {
                            title: transcript.title,
                            configured_model: transcript.configured_model,
                            configured_reasoning: transcript.configured_reasoning,
                            message_count: transcript.message_count,
                        };
                        if let Some(fingerprint) = fingerprint.filter(|_| catalog.is_some()) {
                            updates.push(CatalogUpdate {
                                id: id.clone(),
                                fingerprint,
                                summary: summary.clone(),
                            });
                        }
                        summary
                    }
                    // I/O failures can be transient, so unreadable projections
                    // are shown but deliberately not retained in the catalog.
                    Err(_) => CachedTranscriptSummary::Unreadable,
                });
            if let Some(meta) = self.meta_from_cached_summary(candidate, id, summary) {
                discovered.push(meta);
                if first_only {
                    break;
                }
            }
        }

        if let Some(mut catalog) = catalog {
            let _ = catalog.apply(&updates, &stale_ids);
        }
        discovered
    }

    /// List sessions newest-first by filesystem modification time.
    pub fn list(&self) -> Vec<SessionMeta> {
        let candidates = self.candidates();
        if candidates.is_empty() && !SessionCatalog::exists(&self.dir) {
            return Vec::new();
        }
        self.discover_with_summarizer(candidates, false, summarize_catalog_session)
    }

    /// Return the newest session or an actionable error when none exists.
    pub fn latest(&self) -> anyhow::Result<SessionMeta> {
        let candidates = self.candidates();
        if candidates.is_empty() && !SessionCatalog::exists(&self.dir) {
            anyhow::bail!("no sessions for this workspace yet");
        }
        self.discover_with_summarizer(candidates, true, summarize_catalog_session)
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("no sessions for this workspace yet"))
    }

    /// Reports whether the canonical transcript currently exists.
    ///
    /// A non-regular entry is an error, not absence. Permanent-deletion
    /// recovery uses this distinction so it never crosses the irreversible
    /// boundary merely because an existing transcript could not be validated.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn session_file_exists(&self, id: &str) -> anyhow::Result<bool> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        let path = self.dir.join(format!("{id}.jsonl"));
        match path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => Ok(true),
            Ok(_) => anyhow::bail!("session {id:?} is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(anyhow::anyhow!(
                "session {id:?} could not be inspected: {error}"
            )),
        }
    }

    /// Resolve a filename stem without enumerating or parsing unrelated sessions.
    ///
    /// A session-owned worker handle (`agent-session:<sha256>`) is resolved
    /// through this session's durable delegation roster instead of a flat
    /// `<session-dir>/<id>.jsonl` join, so `octet --resume <handle>` can open a
    /// detached delegated child as its own interactive session. Every
    /// non-launchable handle fails closed with a typed, bounded
    /// [`DelegatedHandleRefusal`]; an ordinary session id keeps exactly its
    /// previous resolution path.
    pub fn path_by_id(&self, id: &str) -> anyhow::Result<PathBuf> {
        if id.starts_with(DELEGATED_SESSION_HANDLE_PREFIX) {
            return self.path_for_delegated_handle(id);
        }
        if !self.session_file_exists(id)? {
            anyhow::bail!("session {id:?} was not found");
        }
        Ok(self.dir.join(format!("{id}.jsonl")))
    }

    /// Resolve one launchable session-owned worker handle to its transcript.
    ///
    /// The handle is the only reference an extension ever receives for a
    /// session-owned delegated child (`octet_agent::delegated_session_reference`),
    /// and it is deliberately path-free, argv-safe, and credential-free. It is
    /// resolved through `octet_agent::delegation::resolve_launchable_child_session`,
    /// which needs no live agent:
    ///
    /// 1. the token is validated *before any filesystem work*, so a shell
    ///    metacharacter, a control byte, or a path component can never reach a
    ///    path join;
    /// 2. the owning session's durable roster is read, and a parked
    ///    (`awaiting_approval`) worker, an unknown handle, a missing roster, and
    ///    a vanished transcript each refuse with their own bounded reason;
    /// 3. a worker the roster still records as live (`pending`/`running`) is
    ///    refused here too: process-local liveness cannot be read from the
    ///    roster, but a live record means another process owns this transcript,
    ///    and one session has one writer;
    /// 4. the resolved path is confined to this store's private delegation
    ///    directory, so a forged roster entry cannot escape it.
    pub fn path_for_delegated_handle(&self, handle: &str) -> anyhow::Result<PathBuf> {
        if delegated_handle_digest(handle).is_none() {
            return Err(DelegatedHandleRefusal::MalformedHandle.into());
        }
        let delegation_directory = self.dir.join(DELEGATION_DIRECTORY);
        let resolved = octet_agent::delegation::resolve_launchable_child_session(
            &delegation_directory,
            handle,
        )
        .map_err(|error| anyhow::Error::from(classify_delegated_handle_error(&error)))?;
        if let Some(status) = live_worker_state(&resolved.status) {
            return Err(DelegatedHandleRefusal::LiveInOwningProcess { status }.into());
        }
        confine_delegated_session_path(&delegation_directory, &resolved.session_path)?;
        Ok(resolved.session_path)
    }

    fn metadata_dir(&self) -> PathBuf {
        self.dir.join(".metadata")
    }

    fn metadata_path(&self, id: &str) -> anyhow::Result<PathBuf> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session id {id:?}");
        }
        Ok(self.metadata_dir().join(format!("{id}.json")))
    }

    /// Read optional user-owned session catalog metadata.
    pub fn load_metadata(&self, id: &str) -> anyhow::Result<SessionUserMetadata> {
        let path = self.metadata_path(id)?;
        let metadata_dir = self.metadata_dir();
        match metadata_dir.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            }
            Ok(_) => anyhow::bail!(
                "session metadata directory is not a real directory: {}",
                metadata_dir.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SessionUserMetadata::default());
            }
            Err(error) => return Err(error.into()),
        }

        let bytes = match crate::auth::read_bounded_private(&path, MAX_SESSION_METADATA_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Ok(SessionUserMetadata::default()),
            Err(error) => anyhow::bail!("cannot read session metadata {}: {error}", path.display()),
        };
        let parsed: SessionUserMetadata = serde_json::from_slice(&bytes).map_err(|error| {
            anyhow::anyhow!("invalid session metadata {}: {error}", path.display())
        })?;
        let metadata = SessionUserMetadata {
            name: parsed
                .name
                .as_deref()
                .map(sanitize_session_name)
                .transpose()?
                .flatten(),
            tags: sanitize_session_tags(&parsed.tags)?,
            pinned: parsed.pinned,
            archived: parsed.archived,
            trashed_at_ms: parsed.trashed_at_ms,
            purge_after_ms: parsed.purge_after_ms,
            forked_from_session_id: parsed.forked_from_session_id,
            forked_from_entry_id: parsed.forked_from_entry_id,
        };
        validate_session_metadata(&metadata)?;
        Ok(metadata)
    }

    /// Atomically replace user-owned catalog metadata. The target session must exist.
    pub fn save_metadata(&self, id: &str, metadata: &SessionUserMetadata) -> anyhow::Result<()> {
        self.path_by_id(id)?;
        let metadata = SessionUserMetadata {
            name: metadata
                .name
                .as_deref()
                .map(sanitize_session_name)
                .transpose()?
                .flatten(),
            tags: sanitize_session_tags(&metadata.tags)?,
            pinned: metadata.pinned,
            archived: metadata.archived,
            trashed_at_ms: metadata.trashed_at_ms,
            purge_after_ms: metadata.purge_after_ms,
            forked_from_session_id: metadata.forked_from_session_id.clone(),
            forked_from_entry_id: metadata.forked_from_entry_id.clone(),
        };
        validate_session_metadata(&metadata)?;
        let bytes = serde_json::to_vec_pretty(&metadata)?;
        if bytes.len() > MAX_SESSION_METADATA_BYTES {
            anyhow::bail!("session metadata exceeds {MAX_SESSION_METADATA_BYTES} bytes");
        }
        crate::auth::write_private_atomic(&self.metadata_path(id)?, &bytes, ".session-metadata-")
    }

    pub fn rename(&self, id: &str, name: &str) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.name = sanitize_session_name(name)?;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    pub fn set_tags(&self, id: &str, tags: Vec<String>) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.tags = sanitize_session_tags(&tags)?;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn set_pinned(&self, id: &str, pinned: bool) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.pinned = pinned;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn set_archived(&self, id: &str, archived: bool) -> anyhow::Result<SessionUserMetadata> {
        let mut metadata = self.load_metadata(id)?;
        metadata.archived = archived;
        metadata.trashed_at_ms = None;
        metadata.purge_after_ms = None;
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn set_lifecycle(
        &self,
        id: &str,
        lifecycle: SessionStorageLifecycle,
        changed_at_ms: u64,
    ) -> anyhow::Result<SessionUserMetadata> {
        if changed_at_ms == 0 {
            anyhow::bail!("session lifecycle timestamp must be positive");
        }
        let mut metadata = self.load_metadata(id)?;
        match lifecycle {
            SessionStorageLifecycle::Active => {
                metadata.archived = false;
                metadata.trashed_at_ms = None;
                metadata.purge_after_ms = None;
            }
            SessionStorageLifecycle::Archived => {
                metadata.archived = true;
                metadata.trashed_at_ms = None;
                metadata.purge_after_ms = None;
            }
            SessionStorageLifecycle::Trash => {
                metadata.archived = true;
                metadata.pinned = false;
                if metadata.trashed_at_ms.is_none() {
                    metadata.trashed_at_ms = Some(changed_at_ms);
                    metadata.purge_after_ms = changed_at_ms.checked_add(SESSION_TRASH_RETENTION_MS);
                }
                if metadata.purge_after_ms.is_none() {
                    anyhow::bail!("session trash retention timestamp overflow");
                }
            }
        }
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn set_fork_provenance(
        &self,
        id: &str,
        source_session_id: &str,
        source_entry_id: &str,
    ) -> anyhow::Result<SessionUserMetadata> {
        if !session_id_is_valid(source_session_id)
            || source_entry_id.is_empty()
            || source_entry_id.len() > 256
            || source_entry_id.chars().any(char::is_control)
        {
            anyhow::bail!("invalid session fork provenance");
        }
        let mut metadata = self.load_metadata(id)?;
        metadata.forked_from_session_id = Some(source_session_id.to_owned());
        metadata.forked_from_entry_id = Some(source_entry_id.to_owned());
        self.save_metadata(id, &metadata)?;
        Ok(metadata)
    }

    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn delete_permanently(&self, id: &str, expected_trashed_at_ms: u64) -> anyhow::Result<()> {
        let metadata = self.load_metadata(id)?;
        if metadata.trashed_at_ms != Some(expected_trashed_at_ms) {
            anyhow::bail!("session trash confirmation is stale");
        }
        let session_path = self.path_by_id(id)?;
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) => return Err(error.into()),
        }
        let suffix = NEXT_SESSION_SUFFIX.fetch_add(1, Ordering::Relaxed);
        let staged_session = self.dir.join(format!(".delete-{id}-{suffix:016x}"));
        let staged_metadata = self
            .metadata_dir()
            .join(format!(".delete-{id}-{suffix:016x}"));

        std::fs::rename(&session_path, &staged_session)?;
        if !staged_session
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            let _ = std::fs::rename(&staged_session, &session_path);
            anyhow::bail!("staged session transcript is not a regular file");
        }
        if let Err(error) = std::fs::rename(&metadata_path, &staged_metadata) {
            let _ = std::fs::rename(&staged_session, &session_path);
            return Err(error.into());
        }
        if !staged_metadata
            .symlink_metadata()
            .is_ok_and(|metadata| metadata.file_type().is_file())
        {
            let _ = std::fs::rename(&staged_metadata, &metadata_path);
            let _ = std::fs::rename(&staged_session, &session_path);
            anyhow::bail!("staged session metadata is not a regular file");
        }
        if let Err(error) = std::fs::remove_file(&staged_session) {
            let _ = std::fs::rename(&staged_metadata, &metadata_path);
            let _ = std::fs::rename(&staged_session, &session_path);
            return Err(error.into());
        }
        std::fs::remove_file(&staged_metadata)?;
        self.finish_permanent_delete(id)
    }

    /// Rolls back an interrupted permanent deletion while the canonical
    /// transcript still exists.
    ///
    /// The intent journal is written before the transcript rename. If a crash
    /// occurs before the irreversible transcript-removal boundary, metadata may
    /// already have been staged. This restores that metadata and removes only
    /// deletion staging files, making pre-commit recovery idempotent.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn rollback_permanent_delete(&self, id: &str) -> anyhow::Result<()> {
        self.path_by_id(id)?;
        let metadata_dir = self.metadata_dir();
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let staged = staged_deletion_files(&metadata_dir, id)?;
                let [staged_metadata] = staged.as_slice() else {
                    anyhow::bail!("interrupted session metadata cannot be restored");
                };
                std::fs::rename(staged_metadata, &metadata_path)?;
                std::fs::File::open(&metadata_dir)?.sync_all()?;
            }
            Err(error) => return Err(error.into()),
        }

        remove_staged_deletion_files(&self.dir, id)?;
        remove_staged_deletion_files(&metadata_dir, id)?;
        std::fs::File::open(&self.dir)?.sync_all()?;
        std::fs::File::open(metadata_dir)?.sync_all()?;
        Ok(())
    }

    /// Finishes an already-confirmed permanent deletion after interruption.
    ///
    /// This idempotently removes both canonical files and transaction staging
    /// files. Callers must establish the destructive confirmation boundary
    /// before invoking it.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn finish_permanent_delete(&self, id: &str) -> anyhow::Result<()> {
        if !session_id_is_valid(id) {
            anyhow::bail!("invalid session ID");
        }
        remove_regular_file_if_exists(&self.dir.join(format!("{id}.jsonl")))?;
        remove_regular_file_if_exists(&self.metadata_path(id)?)?;
        remove_staged_deletion_files(&self.dir, id)?;
        let metadata_dir = self.metadata_dir();
        remove_staged_deletion_files(&metadata_dir, id)?;
        std::fs::File::open(&self.dir)?.sync_all()?;
        std::fs::File::open(metadata_dir)?.sync_all()?;
        let _ = self.remove_catalog_entry(id);
        Ok(())
    }

    /// Removes a just-created session and sidecar during a higher-level
    /// transaction rollback. This is intentionally not a user-facing delete
    /// path and must only be used before the new session is acknowledged.
    #[cfg_attr(not(feature = "serve"), allow(dead_code))]
    pub fn discard_unacknowledged(&self, id: &str) -> anyhow::Result<()> {
        let session_path = self.path_by_id(id)?;
        std::fs::remove_file(session_path)?;
        let metadata_path = self.metadata_path(id)?;
        match metadata_path.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_file() => {
                std::fs::remove_file(metadata_path)?;
            }
            Ok(_) => anyhow::bail!("session metadata path is not a regular file"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let _ = self.remove_catalog_entry(id);
        Ok(())
    }
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
mod tests {
    use super::*;

    /// One transcript worth of usage plus an unknown-usage exposure, appended
    /// through the session's own durable path.
    fn write_ephemeral_transcript(path: &Path, uncertain: bool) -> PathBuf {
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("ephemeral prompt".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("ephemeral answer".into())],
                    model: ModelId("custom/model".into()),
                    protocol: Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        session
            .record_terminal_gate_usage(
                EndpointId("custom".into()),
                ModelId("probe".into()),
                octet_ai::Usage {
                    input_tokens: 40,
                    output_tokens: 10,
                    total_tokens: 50,
                    ..octet_ai::Usage::default()
                },
                Some(octet_ai::Cost {
                    total: 7,
                    ..octet_ai::Cost::default()
                }),
                Some(true),
            )
            .unwrap();
        if uncertain {
            // The operation id the Codex above-272K policy exports.
            session
                .record_usage_uncertainty(
                    EndpointId("custom".into()),
                    ModelId("probe".into()),
                    crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION,
                )
                .unwrap();
        }
        drop(session);
        path.to_path_buf()
    }

    #[test]
    fn ephemeral_accounting_keeps_usage_and_uncertainty_without_the_transcript() {
        let transcript_root = tempfile::tempdir().unwrap();
        let accounting_root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(accounting_root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();

        let transcript = transcript_root
            .path()
            .join(workspace_key(workspace.path()))
            .join("ephemeral.jsonl");
        std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
        write_ephemeral_transcript(&transcript, true);

        let record = store.record_ephemeral_accounting(&transcript).unwrap();
        assert_eq!(record.usage_records.len(), 1);
        assert_eq!(record.usage_records[0].usage.input_tokens, 40);
        assert_eq!(record.usage_uncertainty_records.len(), 1);
        assert_eq!(
            record.usage_uncertainty_records[0].operation,
            crate::codex_context::CODEX_ABOVE_STANDARD_TIER_OPERATION
        );
        assert!(record.has_uncertain_usage, "unknown usage must survive");

        // The conversation itself is never copied into the durable ledger.
        let ledger = store
            .dir()
            .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
            .join(EPHEMERAL_ACCOUNTING_FILE);
        let bytes = std::fs::read_to_string(&ledger).unwrap();
        assert!(!bytes.contains("ephemeral prompt"), "{bytes}");
        assert!(!bytes.contains("ephemeral answer"), "{bytes}");

        // A second run accumulates, and uncertainty stays fail-closed across the
        // whole workspace ledger.
        let clean = transcript_root
            .path()
            .join(workspace_key(workspace.path()))
            .join("ephemeral-two.jsonl");
        write_ephemeral_transcript(&clean, false);
        let second = store.record_ephemeral_accounting(&clean).unwrap();
        assert!(!second.has_uncertain_usage);

        let summary = store.ephemeral_accounting_summary().unwrap();
        assert_eq!(summary.runs, 2);
        assert!(
            summary.has_uncertain_usage,
            "one uncertain run keeps the total uncertain"
        );

        // The transcript can now be discarded: accounting still answers.
        std::fs::remove_file(&transcript).unwrap();
        std::fs::remove_file(&clean).unwrap();
        let after = store.ephemeral_accounting_summary().unwrap();
        assert_eq!(after.runs, 2);
        assert!(after.has_uncertain_usage);
    }

    #[test]
    fn unpriced_ephemeral_receipts_survive_legacy_ledger_and_recovery_flags() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let workspace = root_path.join("workspace");
        let transcript_root = root_path.join("transcripts");
        let accounting_root = root_path.join("durable");
        let store = SessionStore::new(&accounting_root, &workspace);
        let directory = transcript_root.join(workspace_key(&workspace));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("unpriced.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .record_compaction_usage(
                EndpointId("fixture".into()),
                ModelId("fixture".into()),
                octet_ai::Usage {
                    input_tokens: 4,
                    output_tokens: 2,
                    total_tokens: 6,
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        assert!(session.has_unpriced_usage());
        assert!(!session.has_uncertain_usage());
        drop(session);
        let mut record = read_ephemeral_accounting(&path).unwrap();
        assert!(record.has_uncertain_usage);
        assert!(record.usage_uncertainty_records.is_empty());
        record.accounting_id = Some(workspace_key(&transcript_root));
        store.append_ephemeral_accounting(&record).unwrap();
        // Persist the pre-unpriced-reporting flag to exercise old accounting-only
        // ledgers and recovery after the transcript itself is discarded.
        record.has_uncertain_usage = false;
        let legacy = serde_json::to_vec(&record).unwrap();
        let ledger = store
            .dir()
            .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
            .join(EPHEMERAL_ACCOUNTING_FILE);
        let mut line = legacy.clone();
        line.push(b'\n');
        std::fs::write(&ledger, &line).unwrap();
        octet_agent::secure_fs::write_private_atomic(
            &transcript_root.join(EPHEMERAL_ACCOUNTING_RECOVERY),
            &legacy,
            MAX_SESSION_FILE_BYTES,
        )
        .unwrap();
        let summary = store.ephemeral_accounting_summary().unwrap();
        assert!(summary.has_uncertain_usage);
        assert_eq!(summary.uncertainty_records, 0);
        let mut run = EphemeralRun {
            transcript_root: transcript_root.clone(),
            accounting_session_dir: accounting_root,
            workspace,
            pending: None,
        };
        let recovered = finish_ephemeral_run_state(&mut run).unwrap().unwrap();
        assert!(recovered.has_uncertain_usage);
        assert!(!transcript_root.exists());
        let summary = store.ephemeral_accounting_summary().unwrap();
        assert_eq!(
            summary.runs, 1,
            "normalizing the flag must not duplicate a recovery receipt"
        );
        assert_eq!(summary.input_tokens, 4);
        assert_eq!(summary.output_tokens, 2);
        assert!(summary.has_uncertain_usage);
        assert_eq!(
            std::fs::read(ledger).unwrap(),
            line,
            "historical receipts are not rewritten"
        );
    }

    #[test]
    fn ephemeral_finish_accounts_for_all_sessions_including_an_empty_newest_session() {
        for second_has_usage in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let workspace = root.path().join("workspace");
            let transcript_root = root.path().join("transcripts");
            let store = SessionStore::new(&root.path().join("durable"), &workspace);
            let directory = transcript_root.join(workspace_key(&workspace));
            std::fs::create_dir_all(&directory).unwrap();
            write_ephemeral_transcript(&directory.join("first.jsonl"), true);
            if second_has_usage {
                write_ephemeral_transcript(&directory.join("second.jsonl"), false);
            } else {
                Session::create(directory.join("second.jsonl")).unwrap();
            }
            let mut run = EphemeralRun {
                transcript_root: transcript_root.clone(),
                accounting_session_dir: root.path().join("durable"),
                workspace,
                pending: None,
            };
            let record = finish_ephemeral_run_state(&mut run).unwrap().unwrap();
            let count = if second_has_usage { 2 } else { 1 };
            assert_eq!(record.usage_records.len(), count);
            assert_eq!(record.session_cost_microdollars, 7 * count as u64);
            assert!(record.has_uncertain_usage);
            assert_eq!(record.usage_uncertainty_records.len(), 1);
            assert!(!transcript_root.exists());
            let summary = store.ephemeral_accounting_summary().unwrap();
            assert_eq!(
                summary.runs, 1,
                "one invocation, not one record per RPC session"
            );
            assert_eq!(summary.input_tokens, 40 * count as u64);
            assert_eq!(summary.usage_records, count);
        }
    }

    #[test]
    fn ephemeral_append_failure_keeps_private_accounting_only_and_retries_once() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let transcript_root = root.path().join("transcripts");
        let accounting_root = root.path().join("durable");
        let store = SessionStore::new(&accounting_root, &workspace);
        let directory = transcript_root.join(workspace_key(&workspace));
        std::fs::create_dir_all(&directory).unwrap();
        write_ephemeral_transcript(&directory.join("first.jsonl"), true);
        write_ephemeral_transcript(&directory.join("second.jsonl"), false);
        // A directory in place of the ledger fails deterministically, even as root.
        let ledger = store
            .dir()
            .join(EPHEMERAL_ACCOUNTING_DIRECTORY)
            .join(EPHEMERAL_ACCOUNTING_FILE);
        std::fs::create_dir_all(&ledger).unwrap();
        begin_ephemeral_run(
            transcript_root.clone(),
            accounting_root.clone(),
            workspace.clone(),
        );
        let error = finish_ephemeral_run().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("accounting-only recovery retained"),
            "{error:#}"
        );
        assert!(
            error.chain().count() > 1,
            "original append failure must be retained"
        );
        assert!(
            !directory.exists(),
            "no conversation survives failed accounting"
        );
        let recovery = transcript_root.join(EPHEMERAL_ACCOUNTING_RECOVERY);
        let bytes =
            octet_agent::secure_fs::read_private_file_bounded(&recovery, MAX_SESSION_FILE_BYTES)
                .unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("ephemeral prompt"));
        assert!(!text.contains("ephemeral answer"));
        let record: EphemeralAccountingRecord = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(record.usage_records.len(), 2);
        assert!(record.has_uncertain_usage);
        // Retrying before repair reports the original failure again, never a no-op.
        assert!(finish_ephemeral_run().is_err());
        std::fs::remove_dir(&ledger).unwrap();
        // Simulate a complete but unacknowledged append (e.g. sync failure), then
        // process-state loss. Disk-only recovery must not double-count that append.
        store.append_ephemeral_accounting(&record).unwrap();
        begin_ephemeral_run(transcript_root.clone(), accounting_root, workspace);
        let recovered = finish_ephemeral_run().unwrap().unwrap();
        assert_eq!(recovered.usage_records.len(), 2);
        assert!(!transcript_root.exists());
        assert!(finish_ephemeral_run().unwrap().is_none());
        let summary = store.ephemeral_accounting_summary().unwrap();
        assert_eq!(summary.runs, 1);
        assert_eq!(summary.usage_records, 2);
        assert_eq!(summary.total_cost_microdollars, 14);
        assert!(summary.has_uncertain_usage);
    }

    #[test]
    fn ephemeral_accounting_retry_repairs_a_torn_append() {
        let root = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), root.path());
        let transcript = root.path().join("source.jsonl");
        write_ephemeral_transcript(&transcript, true);
        let mut record = read_ephemeral_accounting(&transcript).unwrap();
        record.accounting_id = Some("retry-key".into());
        let directory = store.dir().join(EPHEMERAL_ACCOUNTING_DIRECTORY);
        std::fs::create_dir_all(&directory).unwrap();
        let ledger = directory.join(EPHEMERAL_ACCOUNTING_FILE);
        let bytes = serde_json::to_vec(&record).unwrap();
        std::fs::write(&ledger, &bytes[..bytes.len() / 2]).unwrap();
        store.append_ephemeral_accounting(&record).unwrap();
        store.append_ephemeral_accounting(&record).unwrap();
        assert_eq!(store.ephemeral_accounting_summary().unwrap().runs, 1);
        assert_eq!(std::fs::read_to_string(&ledger).unwrap().lines().count(), 1);
    }

    #[test]
    fn per_workspace_dirs_are_stable_and_distinct() {
        let root = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let first = SessionStore::new(root.path(), workspace_a.path());
        let second = SessionStore::new(root.path(), workspace_a.path());
        let other = SessionStore::new(root.path(), workspace_b.path());
        assert_eq!(first.dir(), second.dir());
        assert_ne!(first.dir(), other.dir());
        assert!(first.dir().starts_with(root.path()));
    }

    #[test]
    fn list_all_discovers_marked_workspace_stores_and_message_counts() {
        let root = tempfile::tempdir().unwrap();
        let workspace_a = tempfile::tempdir().unwrap();
        let workspace_b = tempfile::tempdir().unwrap();
        let store_a = SessionStore::new(root.path(), workspace_a.path());
        let store_b = SessionStore::new(root.path(), workspace_b.path());
        std::fs::create_dir_all(store_a.dir()).unwrap();
        std::fs::create_dir_all(store_b.dir()).unwrap();
        store_a.write_workspace_marker().unwrap();
        store_b.write_workspace_marker().unwrap();

        let path_a = store_a.new_path("a");
        let mut session_a = Session::create(&path_a).unwrap();
        session_a
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("a".into())],
            })))
            .unwrap();
        drop(session_a);
        let path_b = store_b.new_path("b");
        let mut session_b = Session::create(&path_b).unwrap();
        session_b
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("b".into())],
            })))
            .unwrap();
        session_b
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("b2".into())],
            })))
            .unwrap();
        drop(session_b);

        let all = store_a.list_all();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|meta| {
            meta.workspace.as_deref() == Some(workspace_a.path()) && meta.message_count == 1
        }));
        assert!(all.iter().any(|meta| {
            meta.workspace.as_deref() == Some(workspace_b.path()) && meta.message_count == 2
        }));
    }

    #[test]
    fn new_path_is_inside_dir_with_jsonl_extension_and_prefix() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        let path = store.new_path("2026-07-12T14-30-05Z");
        assert!(path.starts_with(store.dir()));
        assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("jsonl"));
        assert!(path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("2026-07-12T14-30-05Z-")));
    }

    #[test]
    fn catalog_metadata_round_trips_without_rewriting_the_session() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("metadata.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("original title".into())],
            })))
            .unwrap();
        drop(session);
        let session_bytes = std::fs::read(&path).unwrap();

        store
            .set_tags("metadata", vec!["work".into(), "active".into()])
            .unwrap();
        store.rename("metadata", "  Renamed session  ").unwrap();
        store.set_pinned("metadata", true).unwrap();
        store.set_archived("metadata", true).unwrap();

        let reopened = SessionStore::new(root.path(), workspace.path());
        let metadata = reopened.load_metadata("metadata").unwrap();
        assert_eq!(metadata.name.as_deref(), Some("Renamed session"));
        assert_eq!(metadata.tags, ["work", "active"]);
        assert!(metadata.pinned);
        assert!(metadata.archived);
        let listed = reopened.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Renamed session");
        assert!(listed[0].pinned);
        assert!(listed[0].archived);
        assert_eq!(std::fs::read(path).unwrap(), session_bytes);
    }

    #[test]
    fn trash_lifecycle_is_recoverable_and_preserves_its_retention_deadline() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("lifecycle.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("recover me".into())],
            })))
            .unwrap();
        drop(session);
        store.set_pinned("lifecycle", true).unwrap();

        let trashed = store
            .set_lifecycle("lifecycle", SessionStorageLifecycle::Trash, 1_000)
            .unwrap();
        assert!(trashed.archived);
        assert!(!trashed.pinned);
        assert_eq!(trashed.trashed_at_ms, Some(1_000));
        assert_eq!(
            trashed.purge_after_ms,
            Some(1_000 + SESSION_TRASH_RETENTION_MS)
        );

        let repeated = store
            .set_lifecycle("lifecycle", SessionStorageLifecycle::Trash, 9_000)
            .unwrap();
        assert_eq!(repeated.trashed_at_ms, trashed.trashed_at_ms);
        assert_eq!(repeated.purge_after_ms, trashed.purge_after_ms);
        let listed = store.list();
        assert_eq!(listed[0].trashed_at_ms, Some(1_000));
        assert_eq!(
            listed[0].purge_after_ms,
            Some(1_000 + SESSION_TRASH_RETENTION_MS)
        );

        let restored = store
            .set_lifecycle("lifecycle", SessionStorageLifecycle::Active, 10_000)
            .unwrap();
        assert!(!restored.archived);
        assert_eq!(restored.trashed_at_ms, None);
        assert_eq!(restored.purge_after_ms, None);

        let archived = store
            .set_lifecycle("lifecycle", SessionStorageLifecycle::Archived, 11_000)
            .unwrap();
        assert!(archived.archived);
        assert_eq!(archived.trashed_at_ms, None);
        assert!(path.is_file());
    }

    #[test]
    fn permanent_delete_requires_the_current_trash_confirmation() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("delete-me.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("delete me".into())],
            })))
            .unwrap();
        drop(session);
        store
            .set_lifecycle("delete-me", SessionStorageLifecycle::Trash, 2_000)
            .unwrap();
        let metadata_path = store.metadata_path("delete-me").unwrap();

        let error = store.delete_permanently("delete-me", 1_999).unwrap_err();
        assert!(error.to_string().contains("confirmation is stale"));
        assert!(path.is_file());
        assert!(metadata_path.is_file());

        std::fs::write(
            store.dir().join(".delete-delete-me-deadbeefdeadbeef"),
            b"staged transcript",
        )
        .unwrap();
        std::fs::write(
            store
                .metadata_dir()
                .join(".delete-delete-me-deadbeefdeadbeef"),
            b"staged metadata",
        )
        .unwrap();
        store.delete_permanently("delete-me", 2_000).unwrap();
        store.finish_permanent_delete("delete-me").unwrap();
        assert!(!path.exists());
        assert!(!metadata_path.exists());
        assert!(store.path_by_id("delete-me").is_err());
    }

    #[test]
    fn interrupted_pre_commit_delete_restores_metadata_idempotently() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("rollback-delete.jsonl");
        drop(Session::create(&path).unwrap());
        store.rename("rollback-delete", "Keep this name").unwrap();
        store
            .set_lifecycle("rollback-delete", SessionStorageLifecycle::Trash, 12_000)
            .unwrap();
        let metadata_path = store.metadata_path("rollback-delete").unwrap();
        let staged_metadata = store
            .metadata_dir()
            .join(".delete-rollback-delete-deadbeefdeadbeef");
        std::fs::rename(&metadata_path, &staged_metadata).unwrap();
        let staged_transcript = store.dir().join(".delete-rollback-delete-deadbeefdeadbeef");
        std::fs::write(&staged_transcript, b"stale staging file").unwrap();

        store.rollback_permanent_delete("rollback-delete").unwrap();
        store.rollback_permanent_delete("rollback-delete").unwrap();

        let metadata = store.load_metadata("rollback-delete").unwrap();
        assert_eq!(metadata.name.as_deref(), Some("Keep this name"));
        assert_eq!(metadata.trashed_at_ms, Some(12_000));
        assert!(path.is_file());
        assert!(metadata_path.is_file());
        assert!(!staged_metadata.exists());
        assert!(!staged_transcript.exists());
    }

    #[test]
    fn fork_provenance_round_trips_as_an_atomic_pair() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("fork.jsonl");
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("fork".into())],
            })))
            .unwrap();
        drop(session);

        let metadata = store
            .set_fork_provenance("fork", "source-session", "0042")
            .unwrap();
        assert_eq!(
            metadata.forked_from_session_id.as_deref(),
            Some("source-session")
        );
        assert_eq!(metadata.forked_from_entry_id.as_deref(), Some("0042"));
        let listed = store.list();
        assert_eq!(
            listed[0].forked_from_session_id.as_deref(),
            Some("source-session")
        );
        assert_eq!(listed[0].forked_from_entry_id.as_deref(), Some("0042"));

        let invalid = SessionUserMetadata {
            forked_from_session_id: Some("source-session".into()),
            ..SessionUserMetadata::default()
        };
        assert!(store.save_metadata("fork", &invalid).is_err());
    }

    #[test]
    fn latest_returns_newest_by_mtime() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let older_path = store.dir().join("2026-01-01T00-00-00Z-aaaa.jsonl");
        let mut older = Session::create(&older_path).unwrap();
        older
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("older".into())],
            })))
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        let newer_path = store.dir().join("2026-02-02T00-00-00Z-bbbb.jsonl");
        let mut newer = Session::create(&newer_path).unwrap();
        newer
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("newer".into())],
            })))
            .unwrap();
        assert_eq!(store.latest().unwrap().path, newer_path);
    }

    #[test]
    fn latest_skips_a_newer_config_only_session() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let older_path = store.dir().join("conversation.jsonl");
        let mut older = Session::create(&older_path).unwrap();
        older
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("resumable".into())],
            })))
            .unwrap();
        drop(older);
        std::thread::sleep(std::time::Duration::from_millis(15));
        let newer_path = store.dir().join("config-only.jsonl");
        let mut newer = Session::create(&newer_path).unwrap();
        newer
            .append(EntryValue::Config {
                model: Some("model".into()),
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();

        let latest = store.latest().unwrap();
        assert_eq!(latest.path, older_path);
        assert_eq!(latest.title, "resumable");
    }

    #[test]
    fn warm_catalog_avoids_transcript_scans_and_keeps_metadata_live() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("warm.jsonl");
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("warm title".into())],
            })))
            .unwrap();
        drop(session);

        let scans = std::cell::Cell::new(0);
        let first = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 1);
        assert_eq!(first[0].title, "warm title");
        assert!(SessionCatalog::path(store.dir()).is_file());

        scans.set(0);
        let second = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 0);
        assert_eq!(second[0].title, "warm title");

        store.rename("warm", "Renamed without replay").unwrap();
        let renamed = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 0);
        assert_eq!(renamed[0].title, "Renamed without replay");
    }

    #[test]
    fn catalog_fingerprint_rescans_only_changed_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("changed.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("old".into())],
            })))
            .unwrap();
        drop(session);
        assert_eq!(store.list()[0].title, "old");

        let replacement_path = store.dir().join("replacement.tmp");
        let mut replacement = Session::create(&replacement_path).unwrap();
        replacement
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("a distinct replacement title".into())],
            })))
            .unwrap();
        drop(replacement);
        std::fs::copy(&replacement_path, &path).unwrap();
        std::fs::remove_file(replacement_path).unwrap();

        let scans = std::cell::Cell::new(0);
        let changed = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 1);
        assert_eq!(changed[0].title, "a distinct replacement title");

        scans.set(0);
        let warm = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 0);
        assert_eq!(warm[0].title, "a distinct replacement title");
    }

    #[test]
    fn entry_search_is_incremental_and_notifies_only_on_change() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let mut first = Session::create(store.dir().join("one.jsonl")).unwrap();
        first
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("alpha needle".into())],
            })))
            .unwrap();
        drop(first);
        let mut second = Session::create(store.dir().join("two.jsonl")).unwrap();
        second
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("beta needle".into())],
            })))
            .unwrap();
        drop(second);

        let scans = std::cell::Cell::new(0usize);
        let cold = store
            .search_entries_with("needle", 10, |path| {
                scans.set(scans.get() + 1);
                index_session_entries(path)
            })
            .unwrap();
        assert_eq!(
            scans.get(),
            2,
            "a cold index reads every session exactly once"
        );
        assert_eq!(cold.scanned_sessions, 2);
        assert!(cold.index_changed);
        assert_eq!(cold.hits.len(), 2);
        assert!(cold.hits.iter().any(|hit| hit.session_id == "one"));
        assert!(cold
            .hits
            .iter()
            .any(|hit| hit.text.contains("alpha needle")));

        let mut watcher = SessionSearchWatcher::default();
        assert!(
            watcher.observe(cold.revision),
            "the first observation is a change"
        );
        assert!(
            !watcher.observe(cold.revision),
            "an unchanged index is silent"
        );

        scans.set(0);
        let warm = store
            .search_entries_with("needle", 10, |path| {
                scans.set(scans.get() + 1);
                index_session_entries(path)
            })
            .unwrap();
        assert_eq!(
            scans.get(),
            0,
            "a warm index must not re-read any transcript"
        );
        assert!(!warm.index_changed);
        assert!(!watcher.observe(warm.revision));

        // Only the new/changed transcript is re-read.
        let mut third = Session::create(store.dir().join("three.jsonl")).unwrap();
        third
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("gamma needle".into())],
            })))
            .unwrap();
        drop(third);
        scans.set(0);
        let delta = store
            .search_entries_with("needle", 10, |path| {
                scans.set(scans.get() + 1);
                index_session_entries(path)
            })
            .unwrap();
        assert_eq!(scans.get(), 1, "only the changed session is re-read");
        assert!(delta.index_changed);
        assert!(
            watcher.observe(delta.revision),
            "the change fires the notification"
        );
        assert_eq!(delta.hits.len(), 3);
    }

    #[test]
    fn indexed_entries_keep_only_user_and_assistant_text() {
        let record = serde_json::json!({
            "type": "entry",
            "id": "e1",
            "value": {"type": "message", "Assistant": {"content": [
                {"Text": "visible answer"},
                {"Reasoning": {"text": "hidden needle"}},
                {"ToolCall": {"name": "bash", "arguments": {"command": "secret needle"}}}
            ]}}
        });
        let entry = indexed_entry_from_record(&record).unwrap();
        assert_eq!(entry.kind, IndexedEntryKind::Assistant);
        assert!(entry.text.contains("visible answer"));
        assert!(!entry.text.contains("hidden needle"));
        assert!(!entry.text.contains("secret needle"));

        let user = serde_json::json!({
            "type": "entry",
            "id": "e2",
            "value": {"type": "message", "User": {"content": [
                {"Text": "user needle"},
                {"Media": {"mime": "image/png"}}
            ]}}
        });
        let entry = indexed_entry_from_record(&user).unwrap();
        assert_eq!(entry.kind, IndexedEntryKind::User);
        assert_eq!(entry.text, "user needle");
    }

    #[test]
    fn open_session_refresh_keeps_mutated_transcripts_warm() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("active.jsonl");
        let mut session = Session::create(path).unwrap();
        let branch_root = session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: Vec::new(),
                    model: ModelId("model".into()),
                    protocol: Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("first branch".into())],
            })))
            .unwrap();
        assert_eq!(store.list()[0].title, "first branch");

        session.checkout(branch_root).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("second branch".into())],
            })))
            .unwrap();
        store.refresh_catalog_for_open_session(&session).unwrap();
        drop(session);

        let scans = std::cell::Cell::new(0);
        let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 0);
        assert_eq!(listed[0].title, "second branch");
    }

    #[test]
    fn cold_catalog_accepts_labels_and_tool_invocation_records() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("with-metadata.jsonl");
        let mut session = Session::create(&path).unwrap();
        let prompt = session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("retained title".into())],
            })))
            .unwrap();
        session.set_entry_label(&prompt, "checkpoint").unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    model: ModelId("model".into()),
                    protocol: Protocol::OpenAiResponses,
                    content: vec![octet_ai::AssistantPart::ToolCall(octet_ai::ToolCall {
                        id: octet_ai::ToolCallId("call".into()),
                        name: "test".into(),
                        arguments_json: "{}".into(),
                        argument_error: None,
                    })],
                },
            )))
            .unwrap();
        session
            .tool_invocation(0)
            .unwrap()
            .set_memo("progress", serde_json::json!(true))
            .unwrap();
        drop(session);
        assert!(Session::open_read_only(&path).is_ok());
        assert!(summarize_catalog_session(&path).is_ok());
        assert_eq!(store.list()[0].title, "retained title");
    }

    #[test]
    fn corrupt_catalog_falls_back_without_touching_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("authoritative.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("authoritative".into())],
            })))
            .unwrap();
        drop(session);
        let authoritative_bytes = std::fs::read(&path).unwrap();
        assert_eq!(store.list().len(), 1);
        std::fs::write(SessionCatalog::path(store.dir()), b"not a sqlite database").unwrap();

        let scans = std::cell::Cell::new(0);
        let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 1);
        assert_eq!(listed[0].title, "authoritative");
        assert_eq!(std::fs::read(&path).unwrap(), authoritative_bytes);

        scans.set(0);
        let rebuilt = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 0);
        assert_eq!(rebuilt[0].title, "authoritative");
        assert_eq!(std::fs::read(path).unwrap(), authoritative_bytes);
    }

    #[test]
    fn catalog_removes_rows_for_missing_transcripts() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("removed.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("remove me".into())],
            })))
            .unwrap();
        drop(session);
        assert_eq!(store.list().len(), 1);
        assert!(SessionCatalog::open(store.dir())
            .unwrap()
            .load()
            .unwrap()
            .contains_key("removed"));

        std::fs::remove_file(path).unwrap();
        assert!(store.list().is_empty());
        assert!(!SessionCatalog::open(store.dir())
            .unwrap()
            .load()
            .unwrap()
            .contains_key("removed"));
    }

    #[test]
    fn newer_catalog_schema_falls_back_without_downgrading() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("future.jsonl");
        let mut session = Session::create(path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("future compatible".into())],
            })))
            .unwrap();
        drop(session);
        assert_eq!(store.list().len(), 1);
        let catalog_path = SessionCatalog::path(store.dir());
        let connection = rusqlite::Connection::open(&catalog_path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "DELETE")
            .unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);
        let future_catalog_bytes = std::fs::read(&catalog_path).unwrap();

        let scans = std::cell::Cell::new(0);
        let listed = store.discover_with_summarizer(store.candidates(), false, |path| {
            scans.set(scans.get() + 1);
            summarize_catalog_session(path)
        });
        assert_eq!(scans.get(), 1);
        assert_eq!(listed[0].title, "future compatible");
        assert_eq!(std::fs::read(&catalog_path).unwrap(), future_catalog_bytes);
        let connection = rusqlite::Connection::open(catalog_path).unwrap();
        let version: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 99);
        let journal_mode: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode, "delete");
    }

    #[test]
    fn active_branch_title_uses_oldest_active_user_text() {
        use octet_agent::{EntryValue, Session};
        use octet_ai::{
            AssistantMessage, AssistantPart, Message, ModelId, Protocol, UserMessage, UserPart,
        };

        let directory = tempfile::tempdir().unwrap();
        let mut session = Session::create(directory.path().join("session.jsonl")).unwrap();
        let root = session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("active title".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("abandoned".into())],
                model: ModelId("m".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session.checkout(root).unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("active".into())],
                model: ModelId("m".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        assert_eq!(active_branch_title(&session), "active title");
    }

    #[test]
    fn title_normalization_is_bounded_and_unicode_aware() {
        assert_eq!(trim_title("  one\n\ttwo  "), "one two");
        assert_eq!(
            trim_title(&format!("{}   ", "é".repeat(60))),
            "é".repeat(60)
        );
        assert_eq!(
            trim_title(&format!("{} next", "é".repeat(60))),
            format!("{}…", "é".repeat(60))
        );
        assert_eq!(trim_title(&"a".repeat(61)), format!("{}…", "a".repeat(60)));
    }

    #[test]
    fn listing_is_byte_for_byte_read_only_even_for_a_torn_tail() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("torn.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("durable title".into())],
            })))
            .unwrap();
        drop(session);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"type\":\"entry\"");
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(store.list().len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn listing_accepts_invalid_utf8_only_in_the_unterminated_tail() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("utf8-tail.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("durable title".into())],
            })))
            .unwrap();
        drop(session);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"text\":\"");
        bytes.extend_from_slice(&[0xf0, 0x9f]);
        std::fs::write(&path, &bytes).unwrap();

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "durable title");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn lightweight_summary_rejects_invalid_utf8_in_a_completed_record() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("utf8-corrupt.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("durable title".into())],
            })))
            .unwrap();
        drop(session);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(&[0xff, b'\n']);
        std::fs::write(&path, &bytes).unwrap();

        let error = summarize_session(&path).unwrap_err();
        assert!(error.to_string().contains("line 3"), "{error:#}");
        assert!(error.to_string().contains("invalid UTF-8"), "{error:#}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn lightweight_summary_rejects_a_malformed_completed_final_record() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("corrupt.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("durable title".into())],
            })))
            .unwrap();
        drop(session);
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.extend_from_slice(b"{\"type\":\"entry\"\n");
        std::fs::write(&path, &bytes).unwrap();

        let error = summarize_session(&path).unwrap_err();
        assert!(error.to_string().contains("line 3"), "{error:#}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn lightweight_summary_rejects_a_cross_branch_checkpoint() {
        use std::io::Write as _;

        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("cross-branch.jsonl");
        let mut session = Session::create(&path).unwrap();
        let root_entry = session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("root".into())],
            })))
            .unwrap();
        let abandoned_prompt = session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("abandoned".into())],
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("old answer".into())],
                    model: octet_ai::ModelId("model".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        session.checkout(root_entry).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("active".into())],
            })))
            .unwrap();
        let active_head = session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("new answer".into())],
                    model: octet_ai::ModelId("model".into()),
                    protocol: octet_ai::Protocol::OpenAiChat,
                },
            )))
            .unwrap();
        drop(session);

        let record = octet_agent::SessionRecord::Checkpoint {
            prompt: abandoned_prompt,
            head: active_head,
            usage: None,
            run_cost_microdollars: None,
        };
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        serde_json::to_writer(&mut file, &record).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);

        let error = summarize_session(&path).unwrap_err();
        assert!(error.to_string().contains("line 12"), "{error:#}");
        assert!(error.to_string().contains("not an ancestor"), "{error:#}");
    }

    #[test]
    fn lightweight_summary_validates_responses_sidecar_structure() {
        use std::io::Write as _;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bad-responses-turn.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("title".into())],
            })))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(
                octet_ai::AssistantMessage {
                    content: vec![octet_ai::AssistantPart::Text("answer".into())],
                    model: ModelId("model-a".into()),
                    protocol: Protocol::OpenAiResponses,
                },
            )))
            .unwrap();
        drop(session);

        let malformed = serde_json::json!({
            "type": "entry",
            "id": "999",
            "parent": assistant,
            "value": {
                "type": "responses_turn",
                "assistant": assistant,
                "endpoint": "responses",
                "model": "model-b",
                "output": [{"type": "message"}]
            }
        });
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        serde_json::to_writer(&mut file, &malformed).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);

        let error = summarize_session(&path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("is not a direct sidecar of assistant"),
            "{error:#}"
        );

        let compact_path = directory.path().join("bad-responses-compact.jsonl");
        let mut session = Session::create(&compact_path).unwrap();
        let first = session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("compact title".into())],
            })))
            .unwrap();
        let second = session
            .append(EntryValue::Config {
                model: None,
                reasoning: None,
                reasoning_mode: None,
            })
            .unwrap();
        drop(session);
        let malformed = serde_json::json!({
            "type": "entry",
            "id": "999",
            "parent": second,
            "value": {
                "type": "responses_compaction",
                "endpoint": "responses",
                "model": "model-a",
                "covered_through": first,
                "output": [{"type": "compaction"}]
            }
        });
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&compact_path)
            .unwrap();
        serde_json::to_writer(&mut file, &malformed).unwrap();
        file.write_all(b"\n").unwrap();
        drop(file);
        let error = summarize_session(&compact_path).unwrap_err();
        assert!(
            error.to_string().contains("is not a direct checkpoint"),
            "{error:#}"
        );
    }

    #[test]
    fn lightweight_responses_output_matches_full_compaction_validation() {
        let cases = [
            serde_json::json!([]),
            serde_json::json!([{"type": "message", "content": "ignored"}]),
            serde_json::json!([{"type": "compaction"}]),
            serde_json::json!([{"type": "compaction", "encrypted_content": ""}]),
            serde_json::json!([{"type": "compaction", "encrypted_content": 42}]),
            serde_json::json!([{"type": "compaction", "encrypted_content": "opaque"}]),
            serde_json::json!([
                {"type": "message", "future": {"large": [1, 2, 3]}},
                {"type": "compaction", "encrypted_content": "opaque"}
            ]),
            serde_json::json!([
                {"type": "compaction", "encrypted_content": "one"},
                {"type": "compaction", "encrypted_content": "two"}
            ]),
        ];

        for value in cases {
            let summary: SummaryResponsesOutput = serde_json::from_value(value.clone()).unwrap();
            let full: octet_ai::ResponsesOutput = serde_json::from_value(value).unwrap();
            assert_eq!(summary.is_empty(), full.is_empty());
            assert_eq!(summary.has_valid_compaction(), full.has_valid_compaction());
        }
    }

    #[test]
    fn lightweight_summary_accepts_non_assistant_usage_records() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage-kinds.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("usage title".into())],
            })))
            .unwrap();
        session
            .record_rejected_responses_turn_usage(
                octet_ai::EndpointId("responses".into()),
                ModelId("model".into()),
                octet_ai::Usage::default(),
                None,
            )
            .unwrap();
        session
            .record_terminal_gate_usage(
                octet_ai::EndpointId("responses".into()),
                ModelId("model".into()),
                octet_ai::Usage::default(),
                None,
                None,
            )
            .unwrap();
        drop(session);

        assert_eq!(
            summarize_session(&path).unwrap().title.as_deref(),
            Some("usage title")
        );
    }

    /// One durably parked deferred run, written through the session's own
    /// deferred-run store so the record lands in the transcript exactly as a
    /// real suspension would. The open session is returned so a test can drive
    /// the next durable change through the same store.
    fn park_deferred_run(path: &Path) -> (Session, DeferredRunRecord) {
        use octet_agent::tools::deferred::{
            DeferredHandle, DeferredResponseDeclaration, DeferredStopReason,
            DeferredSuspendDecision, ModelIdentity,
        };

        let mut session = Session::create(path).unwrap();
        let source = session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("parked prompt".into())],
            })))
            .unwrap();
        let identity = ModelIdentity::new("provider", "model");
        let declaration = DeferredResponseDeclaration {
            stop_reason: DeferredStopReason::Deferred,
            api: "anthropic_messages".into(),
            handle: Some(DeferredHandle::new(
                "provider",
                "model",
                "anthropic_messages",
                "resp-1",
            )),
        };
        let store = session.deferred_run_store();
        assert!(matches!(
            store
                .suspend(&identity, "op-1", &source.0, declaration)
                .unwrap(),
            DeferredSuspendDecision::Suspended(_)
        ));
        let record = store.record("op-1").expect("the suspension is durable");
        (session, record)
    }

    #[test]
    fn deferred_run_records_round_trip_through_the_lightweight_mirror() {
        use octet_agent::tools::deferred::{
            DeferredResumeIntent, DeferredResumeStart, DeferredRunState,
        };

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("deferred.jsonl");
        let (session, parked) = park_deferred_run(&path);

        // The parked leaf survives the mirror with its operation identity, grade
        // and provider handle intact.
        let mirrored = summarize_session(&path).unwrap();
        assert_eq!(mirrored.deferred_run_records, vec![parked.clone()]);
        let record = &mirrored.deferred_run_records[0];
        assert_eq!(record.operation_id, "op-1");
        assert_eq!(record.state_label(), "suspended");
        assert_eq!(record.generation, 0);
        let leaf = record.leaf().expect("a parked record keeps its leaf");
        assert_eq!(leaf.poll, 0);
        assert_eq!(leaf.handle.id, "resp-1");
        assert_eq!(leaf.response_api, "anthropic_messages");

        // A permitted poll replaces the leaf under a bumped generation before the
        // provider runs; the mirror must keep the last authoritative state and
        // never the abandoned one.
        let DeferredResumeStart::Admitted(poll) = session
            .deferred_run_store()
            .begin_pass("op-1", "pass-1", DeferredResumeIntent::Poll, 0)
            .unwrap()
        else {
            panic!("the first permitted poll must be admitted");
        };
        drop(session);

        let mirrored = summarize_session(&path).unwrap();
        assert_eq!(
            mirrored.deferred_run_records,
            vec![poll.effect_pending.clone()]
        );
        assert_eq!(
            mirrored.deferred_run_records[0].state_label(),
            "effect_pending"
        );
        assert!(matches!(
            mirrored.deferred_run_records[0].state,
            DeferredRunState::EffectPending { .. }
        ));
        assert_eq!(mirrored.deferred_run_records[0].generation, 1);

        // A reopened session replays the same replaceable state, so the mirror
        // and the authoritative store agree after a restart.
        let reopened = Session::open(&path).unwrap();
        assert_eq!(reopened.deferred_runs(), mirrored.deferred_run_records);
    }

    #[test]
    fn lightweight_mirror_refuses_deferred_records_normal_resume_rejects() {
        let directory = tempfile::tempdir().unwrap();

        // A record replaying a generation the store already holds is refused by
        // the durable store on reopen; the mirror must refuse it too.
        let stale_path = directory.path().join("stale-deferred.jsonl");
        let (session, parked) = park_deferred_run(&stale_path);
        drop(session);
        append_session_record(
            &stale_path,
            &octet_agent::SessionRecord::DeferredRun {
                record: parked.clone(),
            },
        );
        let error = summarize_session(&stale_path).unwrap_err();
        assert!(
            error.to_string().contains("generation regressed"),
            "{error:#}"
        );

        // A terminal tombstone is authoritative: no later record may follow it.
        let terminal_path = directory.path().join("terminal-deferred.jsonl");
        let (session, parked) = park_deferred_run(&terminal_path);
        drop(session);
        append_session_record(
            &terminal_path,
            &octet_agent::SessionRecord::DeferredRun {
                record: DeferredRunRecord::cancelled("op-1", parked.generation + 1),
            },
        );
        append_session_record(
            &terminal_path,
            &octet_agent::SessionRecord::DeferredRun {
                record: parked.clone(),
            },
        );
        let error = summarize_session(&terminal_path).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("terminal deferred record may not be followed"),
            "{error:#}"
        );
    }

    /// Append one already-built session record byte-for-byte, the way a torn or
    /// hostile transcript would carry it.
    fn append_session_record(path: &Path, record: &octet_agent::SessionRecord) {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        serde_json::to_writer(&mut file, record).unwrap();
        file.write_all(b"\n").unwrap();
    }

    #[test]
    fn list_omits_empty_and_config_only_sessions() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let _empty = Session::create(store.dir().join("empty.jsonl")).unwrap();
        let mut config_only = Session::create(store.dir().join("config.jsonl")).unwrap();
        config_only
            .append(EntryValue::Config {
                model: Some("model".into()),
                reasoning: Some("high".into()),
                reasoning_mode: None,
            })
            .unwrap();

        assert!(store.list().is_empty());
    }

    #[test]
    fn lightweight_listing_matches_the_active_branch_and_ignores_large_bodies() {
        use octet_ai::{
            AssistantMessage, AssistantPart, ModelId, Protocol, ToolCallId, ToolResult,
            ToolResultPart,
        };

        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("large.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append_with_metadata(
                EntryValue::Message(Message::User(octet_ai::UserMessage {
                    content: vec![UserPart::Text(
                        "model-only prompt text that must not title the session".into(),
                    )],
                })),
                Some(octet_agent::EntryMetadata {
                    display_text: Some(
                        "  title   with whitespace that the picker normalizes  ".into(),
                    ),
                    ..octet_agent::EntryMetadata::default()
                }),
            )
            .unwrap();
        session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("x".repeat(2 * 1024 * 1024))],
                model: ModelId("model".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::ToolResult(ToolResult {
                    tool_call_id: ToolCallId("call-1".into()),
                    content: vec![ToolResultPart::Text("y".repeat(2 * 1024 * 1024))],
                    is_error: false,
                    added_tool_names: None,
                })],
            })))
            .unwrap();
        let expected = active_branch_title(&session);
        drop(session);

        let listed = store.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, expected);
        assert_eq!(
            listed[0].title,
            "title with whitespace that the picker normalizes"
        );
    }

    #[test]
    fn listing_scales_across_many_session_files() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let template_path = store.dir().join("session-0000.jsonl");
        let mut template = Session::create(&template_path).unwrap();
        template
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("scale fixture".into())],
            })))
            .unwrap();
        drop(template);
        let bytes = std::fs::read(&template_path).unwrap();
        for index in 1..512 {
            std::fs::write(
                store.dir().join(format!("session-{index:04}.jsonl")),
                &bytes,
            )
            .unwrap();
        }

        let listed = store.list();
        assert_eq!(listed.len(), 512);
        assert!(listed
            .iter()
            .all(|session| session.title == "scale fixture"));
    }

    #[test]
    fn catalog_inspection_defaults_when_metadata_directory_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("unannotated.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("unannotated title".into())],
            })))
            .unwrap();
        drop(session);

        assert!(!store.metadata_dir().exists());
        assert_eq!(
            store.load_metadata("unannotated").unwrap(),
            SessionUserMetadata::default()
        );
        assert_eq!(
            store
                .inspect_by_id("unannotated")
                .unwrap()
                .catalog
                .meta
                .unwrap()
                .title,
            "unannotated title"
        );
        assert_eq!(
            store
                .catalog_by_id("unannotated")
                .unwrap()
                .meta
                .unwrap()
                .title,
            "unannotated title"
        );
    }

    #[test]
    fn uncertainty_reopens_and_keeps_warm_and_cold_catalogs_visible() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let path = store.dir().join("uncertain.jsonl");
        let mut session = Session::create(&path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("recoverable title".into())],
            })))
            .unwrap();
        assert_eq!(store.list().len(), 1); // Warm the index before the additive record.
        let head = session.head();
        for _ in 0..2 {
            session
                .record_usage_uncertainty(
                    EndpointId("openai".into()),
                    ModelId("test-model".into()),
                    "assistant_turn",
                )
                .unwrap();
        }
        drop(session);
        let bytes = std::fs::read(&path).unwrap();
        let reopened = Session::open_read_only(&path).unwrap();
        assert!(reopened.has_uncertain_usage());
        assert_eq!(reopened.head(), head);
        assert!(reopened.usage_records().is_empty());
        let inspection = store.inspect_by_id("uncertain").unwrap();
        assert_eq!(inspection.usage_uncertainty_records.len(), 2);
        assert!(inspection.usage_records.is_empty());
        assert_eq!(inspection.catalog.meta.unwrap().title, "recoverable title");
        assert_eq!(store.list()[0].title, "recoverable title");
        let cold = SessionStore::new(root.path(), workspace.path());
        assert_eq!(cold.list()[0].message_count, 1);
        assert_eq!(cold.list_all()[0].title, "recoverable title");
        assert!(cold.catalog_by_id("uncertain").unwrap().meta.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn targeted_catalog_inspection_validates_only_the_requested_session() {
        use octet_ai::{AssistantMessage, AssistantPart, Protocol, Usage, UserMessage};

        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let target = store.dir().join("target.jsonl");
        let mut session = Session::create(&target).unwrap();
        session
            .append(EntryValue::Message(Message::User(UserMessage {
                content: vec![UserPart::Text("target title".into())],
            })))
            .unwrap();
        let assistant = session
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                content: vec![AssistantPart::Text("done".into())],
                model: ModelId("target-model".into()),
                protocol: Protocol::OpenAiChat,
            })))
            .unwrap();
        session
            .record_assistant_usage(
                assistant,
                EndpointId("target-endpoint".into()),
                ModelId("target-model".into()),
                Usage {
                    input_tokens: 11,
                    cache_read_tokens: 2,
                    cache_write_tokens: 3,
                    cache_write_1h_tokens: 4,
                    output_tokens: 5,
                    reasoning_tokens: 6,
                    total_tokens: 31,
                },
                None,
            )
            .unwrap();
        session
            .append(EntryValue::Config {
                model: Some("target-config".into()),
                reasoning: Some("high".into()),
                reasoning_mode: None,
            })
            .unwrap();
        session
            .record_usage_uncertainty(
                EndpointId("target-endpoint".into()),
                ModelId("target-model".into()),
                "assistant_turn",
            )
            .unwrap();
        drop(session);
        store
            .set_lifecycle("target", SessionStorageLifecycle::Trash, 1_000)
            .unwrap();

        // A corrupt sibling must not affect a targeted operation.
        let corrupt = store.dir().join("corrupt.jsonl");
        drop(Session::create(&corrupt).unwrap());
        std::fs::write(&corrupt, b"{not valid json}\n").unwrap();

        let inspection = store.inspect_by_id("target").unwrap();
        assert_eq!(inspection.usage_uncertainty_records.len(), 1);
        let meta = inspection.catalog.meta.as_ref().unwrap();
        assert_eq!(meta.title, "target title");
        assert_eq!(meta.trashed_at_ms, Some(1_000));
        assert_eq!(
            inspection.catalog.configured_model.as_deref(),
            Some("target-config")
        );
        assert_eq!(
            inspection.catalog.configured_reasoning.as_deref(),
            Some("high")
        );
        assert_eq!(inspection.usage_records.len(), 1);
        assert_eq!(
            inspection.usage_records[0].endpoint.as_deref(),
            Some("target-endpoint")
        );
        assert_eq!(inspection.usage_records[0].total_tokens, 31);

        // Populate the catalog before the targeted Serve lookup. The lookup must
        // retain the persisted configuration without reopening the transcript.
        store.list();
        let catalogs = store.catalog_by_ids(["target", "corrupt"]).unwrap();
        assert_eq!(catalogs.len(), 1);
        assert_eq!(catalogs[0].0, "target");
        assert_eq!(catalogs[0].1.meta.as_ref().unwrap().title, "target title");
        assert_eq!(
            catalogs[0].1.configured_model.as_deref(),
            Some("target-config")
        );
        assert_eq!(catalogs[0].1.configured_reasoning.as_deref(), Some("high"));
        let catalog = store.catalog_by_id("target").unwrap();
        assert_eq!(catalog.meta.unwrap().title, "target title");
        assert_eq!(catalog.configured_model.as_deref(), Some("target-config"));
        assert_eq!(catalog.configured_reasoning.as_deref(), Some("high"));
        assert!(store.catalog_by_id("corrupt").is_err());
        assert!(store.get_by_id("corrupt").is_err());
    }

    #[test]
    fn path_by_id_resolves_only_a_valid_direct_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let one_path = store.dir().join("one.jsonl");
        let mut session = Session::create(&one_path).unwrap();
        session
            .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
                content: vec![UserPart::Text("one".into())],
            })))
            .unwrap();
        std::fs::write(store.dir().join("one.txt"), b"").unwrap();
        std::fs::write(
            store.dir().join("unrelated.jsonl"),
            b"not-json\nstill-not-json\n",
        )
        .unwrap();
        std::fs::create_dir(store.dir().join("directory.jsonl")).unwrap();

        assert_eq!(store.path_by_id("one").unwrap(), one_path);
        assert!(store.session_file_exists("one").unwrap());
        assert!(!store.session_file_exists("missing").unwrap());
        for invalid in ["", ".", "..", "../one", "one/two", "one\n"] {
            assert!(store.path_by_id(invalid).is_err(), "accepted {invalid:?}");
        }
        assert!(store.path_by_id("directory").is_err());
        assert!(store.session_file_exists("directory").is_err());
        let mut session_file_ids = store.session_file_ids();
        session_file_ids.sort();
        assert_eq!(session_file_ids, vec!["one", "unrelated"]);

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&one_path, store.dir().join("linked.jsonl")).unwrap();
            assert!(store.path_by_id("linked").is_err());
            assert!(store.session_file_exists("linked").is_err());
            assert!(!store.session_file_ids().iter().any(|id| id == "linked"));
            assert!(!store.list().iter().any(|session| {
                session.path.file_stem().and_then(|stem| stem.to_str()) == Some("linked")
            }));
        }
    }

    /// A credential-shaped string a hostile roster could carry as free text; it
    /// must never reach a handle or a refusal reason.
    const HANDLE_TEST_SECRET: &str = "sk-handle-secret-9f2b7c41d6ea";

    /// One owner-only directory, the mode the host's private `team-*` directory
    /// uses.
    fn private_directory(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    /// The durable delegation roster exactly as the host writes it: one
    /// owner-only `fleet.json` in the session store's private delegation
    /// directory, whose record carries `session_path` in `status`.
    fn write_roster(
        delegation: &Path,
        session_path: &Path,
        status: serde_json::Value,
        detached: bool,
    ) {
        let record = serde_json::json!({
            "agent_id": "agent-1",
            "agent_path": "/root/worker",
            "parent_id": "agent-0",
            "depth": 1,
            "task_name": "worker task",
            "display_task_name": "worker task",
            "session_path": session_path,
            "status": status,
            "detached": detached,
            "created_at_ms": 1,
            "started_at_ms": 2,
            "completed_at_ms": null,
            "turn_count": 0,
            "tool_call_count": 0,
            "usage": {
                "input_tokens": 0,
                "cache_read_tokens": 0,
                "cache_write_tokens": 0,
                "cache_write_1h_tokens": 0,
                "output_tokens": 0,
                "reasoning_tokens": 0,
                "total_tokens": 0,
            },
            "usage_uncertain": false,
            "cost": null,
            "cost_microdollars": null,
            "deadline_at_ms": null,
            "turn_limit": null,
            "extension_principal": null,
            "extension_profile": null,
            "extension_idempotency_key": null,
            "extension_fingerprint": null,
            "extension_policy": null,
            // Free text a forged or buggy roster could carry: the resolver's
            // durable diagnostic must never be relayed into a refusal.
            "durable_diagnostic": format!("credential {} must not leak", HANDLE_TEST_SECRET),
        });
        let fleet = serde_json::json!({
            "version": 1,
            "root_session": delegation.join("parent.jsonl"),
            "records": [record],
        });
        let path = delegation.join("fleet.json");
        std::fs::write(&path, fleet.to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    /// The typed verdict behind one refusal, which is what a frontend branches
    /// on instead of matching message text.
    fn refusal(error: &anyhow::Error) -> DelegatedHandleRefusal {
        *error
            .downcast_ref::<DelegatedHandleRefusal>()
            .unwrap_or_else(|| panic!("typed worker-handle refusal expected, got {error:#}"))
    }

    #[test]
    fn a_launchable_worker_handle_resolves_to_the_child_transcript() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let parent_path = store.dir().join("parent.jsonl");
        Session::create(&parent_path).unwrap();

        let delegation = store.dir().join(DELEGATION_DIRECTORY);
        let team = delegation.join("team-alpha");
        private_directory(&team);
        let child = team.join("0001-worker.jsonl");
        Session::create(&child).unwrap();
        write_roster(
            &delegation,
            &child,
            serde_json::json!({"state": "detached"}),
            true,
        );

        let handle = octet_agent::delegated_session_reference(&child).unwrap();
        assert_eq!(handle.len(), DELEGATED_SESSION_HANDLE_PREFIX.len() + 64);
        // Opaque, path-free, argv-safe, and credential-free.
        assert!(handle
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b':'));
        assert!(!handle.contains('/'));
        assert!(!handle.contains(HANDLE_TEST_SECRET));

        assert_eq!(store.path_by_id(&handle).unwrap(), child);
        assert_eq!(store.path_for_delegated_handle(&handle).unwrap(), child);
        // An ordinary session id keeps exactly its previous resolution.
        assert_eq!(store.path_by_id("parent").unwrap(), parent_path);
        // A settled worker is still launchable: a detached, completed, or
        // shutdown record is not a live writer. The store adds only the
        // roster-level liveness rule the durable record cannot carry.
        write_roster(
            &delegation,
            &child,
            serde_json::json!({"state": "completed", "output": "done"}),
            true,
        );
        assert_eq!(store.path_by_id(&handle).unwrap(), child);
    }

    #[test]
    fn every_unlaunchable_worker_handle_refuses_with_a_distinct_bounded_reason() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let delegation = store.dir().join(DELEGATION_DIRECTORY);
        let team = delegation.join("team-alpha");
        private_directory(&team);
        let child = team.join("0001-worker.jsonl");
        Session::create(&child).unwrap();
        let handle = octet_agent::delegated_session_reference(&child).unwrap();
        let unknown = format!("agent-session:{}", "0".repeat(64));
        let detached = || serde_json::json!({"state": "detached"});

        // Parked at the approval boundary: opening it elsewhere would be
        // unattended mutation.
        write_roster(
            &delegation,
            &child,
            serde_json::json!({"state": "awaiting_approval", "reason": "approval is unavailable"}),
            false,
        );
        assert_eq!(
            refusal(&store.path_by_id(&handle).unwrap_err()),
            DelegatedHandleRefusal::ParkedAtApprovalBoundary
        );

        // Live in the owning process: the durable roster cannot carry the
        // process-local liveness flag, so the live roster state is the
        // fail-closed signal, and one session has one writer.
        for state in ["pending", "running"] {
            write_roster(
                &delegation,
                &child,
                serde_json::json!({ "state": state }),
                false,
            );
            assert_eq!(
                refusal(&store.path_by_id(&handle).unwrap_err()),
                DelegatedHandleRefusal::LiveInOwningProcess { status: state }
            );
        }

        // Vanished transcript: nothing to open, so no fabricated launch.
        std::fs::remove_file(&child).unwrap();
        write_roster(&delegation, &child, detached(), true);
        assert_eq!(
            refusal(&store.path_by_id(&handle).unwrap_err()),
            DelegatedHandleRefusal::VanishedTranscript
        );

        // Unknown handle: the roster is readable and simply does not know it.
        assert_eq!(
            refusal(&store.path_by_id(&unknown).unwrap_err()),
            DelegatedHandleRefusal::UnknownWorker
        );

        // Missing roster: an explicit refusal, never an empty success.
        std::fs::remove_file(delegation.join("fleet.json")).unwrap();
        assert_eq!(
            refusal(&store.path_by_id(&handle).unwrap_err()),
            DelegatedHandleRefusal::RosterUnavailable
        );

        let verdicts = [
            DelegatedHandleRefusal::MalformedHandle,
            DelegatedHandleRefusal::RosterUnavailable,
            DelegatedHandleRefusal::UnknownWorker,
            DelegatedHandleRefusal::ParkedAtApprovalBoundary,
            DelegatedHandleRefusal::LiveInOwningProcess { status: "running" },
            DelegatedHandleRefusal::VanishedTranscript,
            DelegatedHandleRefusal::OutsideDelegationDirectory,
        ];
        let mut codes = HashSet::new();
        let mut reasons = HashSet::new();
        let store_directory = store.dir().to_string_lossy().into_owned();
        let child_path = child.to_string_lossy().into_owned();
        for verdict in verdicts {
            let reason = verdict.to_string();
            assert!(
                reason.len() <= 400,
                "every reason is bounded: {} bytes",
                reason.len()
            );
            assert!(!reason.chars().any(char::is_control), "{reason}");
            // No credential, no session secret, no transcript path, and no
            // roster path in any reason.
            assert!(!reason.contains(HANDLE_TEST_SECRET), "{reason}");
            assert!(!reason.contains(&store_directory), "{reason}");
            assert!(!reason.contains(&child_path), "{reason}");
            assert!(!reason.contains("fleet.json"), "{reason}");
            assert!(codes.insert(verdict.code()), "codes are distinct");
            assert!(reasons.insert(reason), "reasons are distinct");
        }
        assert_eq!(codes.len(), 7);
        // The stable codes are the machine-readable half of the same reason.
        assert_eq!(
            DelegatedHandleRefusal::MalformedHandle.code(),
            "malformed_worker_handle"
        );
        assert_eq!(
            DelegatedHandleRefusal::ParkedAtApprovalBoundary.code(),
            "worker_awaiting_approval"
        );
        assert_eq!(
            DelegatedHandleRefusal::LiveInOwningProcess { status: "pending" }.code(),
            "worker_live_in_owning_process"
        );
    }

    #[test]
    fn a_malformed_worker_handle_is_refused_before_any_filesystem_work() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        // Deliberately nothing on disk: no session directory, no delegation
        // directory, no roster. The shape check must not need any of them, so a
        // shell metacharacter, a control byte, or a path component can never
        // reach a path join.
        assert!(!store.dir().exists());
        let mut malformed = vec![
            "agent-session:".to_owned(),
            "agent-session:0".to_owned(),
            format!("agent-session:{}", "a".repeat(63)),
            format!("agent-session:{}", "A".repeat(64)),
            format!("agent-session:{}x", "a".repeat(64)),
            format!("agent-session:x{}", "a".repeat(64)),
            format!("agent-session:{}\n", "a".repeat(64)),
            format!("agent-session:{} ", "a".repeat(64)),
            "agent-session:../../etc/passwd".to_owned(),
            "agent-session:$(id)".to_owned(),
            "agent-session:a;rm -rf /.jsonl".to_owned(),
            "agent-session:/tmp/0001-worker.jsonl".to_owned(),
            "agent-session:team-alpha/0001-worker.jsonl".to_owned(),
            "agent-session:é".to_owned(),
        ];
        malformed.push(format!("agent-session:{}", "\u{0}".repeat(64)));
        for value in malformed {
            assert_eq!(
                refusal(&store.path_by_id(&value).unwrap_err()),
                DelegatedHandleRefusal::MalformedHandle,
                "accepted {value:?}"
            );
            assert_eq!(
                refusal(&store.path_for_delegated_handle(&value).unwrap_err()),
                DelegatedHandleRefusal::MalformedHandle
            );
        }
        assert!(
            !store.dir().exists(),
            "shape validation must not touch the filesystem"
        );
    }

    #[test]
    fn a_forged_roster_entry_cannot_escape_the_delegation_directory() {
        let root = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        let store = SessionStore::new(root.path(), workspace.path());
        std::fs::create_dir_all(store.dir()).unwrap();
        let delegation = store.dir().join(DELEGATION_DIRECTORY);
        private_directory(&delegation);
        let detached = || serde_json::json!({"state": "detached"});

        // The handle is derived only from the two trailing path components
        // (`octet_agent::delegated_session_reference`), so a copied or forged
        // roster entry can name the same pair *outside* the private delegation
        // directory and hash to the same handle.
        let escape_team = store.dir().join("team-escape");
        private_directory(&escape_team);
        let escaped = escape_team.join("0001-worker.jsonl");
        Session::create(&escaped).unwrap();
        let escaped_handle = octet_agent::delegated_session_reference(&escaped).unwrap();
        assert_eq!(
            octet_agent::delegated_session_reference(
                &delegation.join("team-escape").join("0001-worker.jsonl")
            )
            .unwrap(),
            escaped_handle
        );
        write_roster(&delegation, &escaped, detached(), true);
        assert_eq!(
            refusal(&store.path_by_id(&escaped_handle).unwrap_err()),
            DelegatedHandleRefusal::OutsideDelegationDirectory
        );

        // A traversal-bearing record path is not a two-component path inside the
        // delegation directory, so it is refused outright, even when it resolves
        // to a file that exists.
        let inside_escape_team = delegation.join("team-escape");
        private_directory(&inside_escape_team);
        let inside_escape = inside_escape_team.join("0001-worker.jsonl");
        Session::create(&inside_escape).unwrap();
        assert_eq!(
            octet_agent::delegated_session_reference(&inside_escape).unwrap(),
            escaped_handle,
            "the traversal form carries the same handle, or the test proves nothing"
        );
        let team = delegation.join("team-alpha");
        private_directory(&team);
        let child = team.join("0001-worker.jsonl");
        Session::create(&child).unwrap();
        let traversing = delegation
            .join("team-alpha")
            .join("..")
            .join("team-escape")
            .join("0001-worker.jsonl");
        assert!(traversing.exists(), "the traversal target really exists");
        write_roster(&delegation, &traversing, detached(), true);
        assert_eq!(
            refusal(&store.path_by_id(&escaped_handle).unwrap_err()),
            DelegatedHandleRefusal::OutsideDelegationDirectory
        );

        // A symlinked team directory is refused, never followed.
        #[cfg(unix)]
        {
            let linked_team = delegation.join("team-linked");
            std::os::unix::fs::symlink(&escape_team, &linked_team).unwrap();
            let linked = linked_team.join("0001-worker.jsonl");
            let linked_handle = octet_agent::delegated_session_reference(&linked).unwrap();
            write_roster(&delegation, &linked, detached(), true);
            assert_eq!(
                refusal(&store.path_by_id(&linked_handle).unwrap_err()),
                DelegatedHandleRefusal::OutsideDelegationDirectory
            );
        }

        // The legitimate child beside the forged entries still resolves: the
        // confinement refuses the escape, not delegation itself.
        let handle = octet_agent::delegated_session_reference(&child).unwrap();
        write_roster(&delegation, &child, detached(), true);
        assert_eq!(store.path_by_id(&handle).unwrap(), child);
    }
}
