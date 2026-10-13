//! Fail-closed transcript import. Setup migration is deliberately unrelated.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use octet_agent::{EntryValue, Session, SessionRecord};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde::Deserialize;
use serde_json::Value;

use super::{pi, strict_json, SessionStore, SessionUserMetadata, MAX_SESSION_FILE_BYTES};

#[derive(Debug)]
pub(crate) struct SessionImportReport {
    pub destination: PathBuf,
    pub id: String,
    pub source_format: String,
    pub warnings: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    format: String,
    version: u32,
    exported_at_unix_seconds: u64,
    source_id: String,
    source_title: String,
    metadata: SessionUserMetadata,
    redacted: bool,
    redaction_count: usize,
    records: Vec<Value>,
}

pub(crate) fn import_session(
    store: &SessionStore,
    source: &Path,
    cwd: &Path,
) -> anyhow::Result<SessionImportReport> {
    let source = if source.is_absolute() {
        source.to_owned()
    } else {
        cwd.join(source)
    };
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(&source, MAX_SESSION_FILE_BYTES)?;
    let (mut records, mut metadata, format, mut warnings) = decode(&bytes)?;
    validate_records(&records)?;
    // Imports have a new identity/workspace, never a foreign path or extension authority.
    records.retain(|r| r["type"] != "header");
    let id = new_id()?;
    metadata.trashed_at_ms = None;
    metadata.purge_after_ms = None;
    metadata.forked_from_session_id = None;
    metadata.forked_from_entry_id = None;
    metadata.archived = false;
    octet_agent::secure_fs::create_private_directory_all(store.dir())?;
    let stage = tempfile::Builder::new()
        .prefix(".session-import-")
        .tempdir_in(store.dir())?;
    let staged = stage.path().join(format!("{id}.jsonl"));
    // Use core's creation boundary: the filename supplies identity and core
    // stamps the real creation time/workspace, rather than trusting foreign metadata.
    let mut created = Session::create(&staged)?;
    created.initialize_header(store.workspace().unwrap_or(cwd), None)?;
    records.insert(
        0,
        serde_json::to_value(SessionRecord::Header {
            header: created.header().expect("initialized header").clone(),
        })?,
    );
    drop(created);
    let creation_bytes =
        octet_agent::secure_fs::read_private_file_bounded(&staged, MAX_SESSION_FILE_BYTES)?;
    let original_payload = encode_jsonl(&records)?;
    octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &staged,
        Some(&creation_bytes),
        &original_payload,
        MAX_SESSION_FILE_BYTES,
    )?;
    // Validate ALL source semantics before stripping authority. Otherwise a
    // corrupt private sidecar could be silently hidden by the security projection.
    validate_resumable(&staged)?;
    strip_import_authority(&mut records, &mut warnings);
    let payload = encode_jsonl(&records)?;
    octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &staged,
        Some(&original_payload),
        &payload,
        MAX_SESSION_FILE_BYTES,
    )?;
    validate_resumable(&staged)?;
    let staged_store = SessionStore::for_directory(stage.path(), store.root());
    staged_store.save_metadata(&id, &metadata)?;
    let metadata_bytes = octet_agent::secure_fs::read_private_file_bounded(
        &stage.path().join(".metadata").join(format!("{id}.json")),
        64 * 1024,
    )?;
    let destination = store.dir().join(format!("{id}.jsonl"));
    let metadata_path = store.dir().join(".metadata").join(format!("{id}.json"));
    // Metadata precedes the transcript's single no-replace publication point.
    octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &metadata_path,
        None,
        &metadata_bytes,
        64 * 1024,
    )?;
    if let Err(error) = octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &destination,
        None,
        &payload,
        MAX_SESSION_FILE_BYTES,
    ) {
        let _ = octet_agent::secure_fs::remove_private_file_if_unchanged(
            &metadata_path,
            &metadata_bytes,
            64 * 1024,
        );
        return Err(error.into());
    }
    Ok(SessionImportReport {
        destination,
        id,
        source_format: format,
        warnings,
    })
}

fn validate_resumable(path: &Path) -> anyhow::Result<()> {
    let session = Session::open_read_only(path)?;
    if !session
        .entries()
        .iter()
        .any(|e| matches!(e.value, EntryValue::Message(_)))
    {
        anyhow::bail!("import has no resumable conversation");
    }
    // Eagerly exercise provider replay and compaction ancestry, not just decoding.
    session.context()?;
    Ok(())
}

fn new_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| anyhow::anyhow!("cannot generate import identity"))?;
    Ok(format!(
        "import-{}",
        bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
    ))
}

type Decoded = (Vec<Value>, SessionUserMetadata, String, Vec<String>);
fn decode(bytes: &[u8]) -> anyhow::Result<Decoded> {
    if bytes.len() > MAX_SESSION_FILE_BYTES {
        anyhow::bail!("session import exceeds byte limit");
    }
    if let Ok(value) = strict_json::parse(bytes) {
        if value.get("format").is_some() {
            let encoded = serde_json::to_vec(&value)?;
            let mut ignored = false;
            let mut de = serde_json::Deserializer::from_slice(&encoded);
            let package: Package = serde_ignored::deserialize(&mut de, |_| ignored = true)?;
            if ignored {
                anyhow::bail!("unsupported Octet package/metadata fields; refusing lossy import");
            }
            if package.format != "octet-session-export" || package.version != 1 {
                anyhow::bail!(
                    "unsupported Octet export format/version (requires octet-session-export v1)"
                );
            }
            let _ = (
                package.exported_at_unix_seconds,
                package.source_id,
                package.source_title,
                package.redaction_count,
            );
            let warnings = if package.redacted {
                vec!["Imported a redacted snapshot; removed secrets/provider replay state cannot be recovered.".into()]
            } else {
                Vec::new()
            };
            return Ok((
                package.records,
                package.metadata,
                "octet-session-export v1".into(),
                warnings,
            ));
        }
    }
    let records = strict_json::jsonl(bytes)?;
    if records[0]["type"] == "session" {
        return pi::convert(records);
    }
    if matches!(records[0]["type"].as_str(), Some("header" | "entry")) {
        Ok((
            records,
            SessionUserMetadata::default(),
            "octet-jsonl".into(),
            vec!["Native Octet JSONL carries the durable graph, not sidecar names/tags or an export redaction declaration; use portable JSON when those metadata matter.".into()],
        ))
    } else {
        anyhow::bail!("unsupported session import format; expected Octet portable JSON/JSONL or Pi v3 JSONL (HTML is view-only)")
    }
}

pub(super) fn encode_jsonl(records: &[Value]) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    for record in records {
        serde_json::to_writer(&mut bytes, record)?;
        bytes.push(b'\n');
    }
    if bytes.len() > MAX_SESSION_FILE_BYTES {
        anyhow::bail!("converted session exceeds byte limit");
    }
    Ok(bytes)
}

pub(super) fn validate_records(records: &[Value]) -> anyhow::Result<()> {
    if records.is_empty() || records.len() > 1_000_000 {
        anyhow::bail!("empty or oversized session record list");
    }
    let mut ids = HashMap::new();
    let mut children: Vec<Vec<usize>> = Vec::new();
    let mut roots = Vec::new();
    let mut compactions = Vec::new();
    let mut saw_head = false;
    let mut uncommitted_entry = false;
    let mut header = false;
    for (position, value) in records.iter().enumerate() {
        let encoded = serde_json::to_vec(value)?;
        let mut ignored = false;
        let mut de = serde_json::Deserializer::from_slice(&encoded);
        let record: SessionRecord = serde_ignored::deserialize(&mut de, |_| ignored = true)
            .map_err(|_| {
                anyhow::anyhow!(
                    "unsupported or malformed Octet record at position {}",
                    position + 1
                )
            })?;
        // Internally tagged enums buffer their payload before decoding it, so
        // serde_ignored cannot observe every nested ignored field. Check the
        // typed serialization too, without tightening core persistence serde.
        let decoded = serde_json::to_value(&record)?;
        if ignored || !record_fields_preserved(value, &decoded, &decoded, &mut Vec::new()) {
            anyhow::bail!(
                "unsupported Octet record fields at position {}",
                position + 1
            );
        }
        match record {
            SessionRecord::Header { .. } => {
                if header || position != 0 {
                    anyhow::bail!("duplicate or misplaced session header");
                }
                header = true;
            }
            SessionRecord::Entry(entry) => {
                uncommitted_entry = true;
                if entry.id.0.is_empty()
                    || entry.id.0.len() > 128
                    || entry.id.0.chars().any(char::is_control)
                {
                    anyhow::bail!("invalid entry id");
                }
                if let Some(parent) = &entry.parent {
                    if !ids.contains_key(parent) {
                        anyhow::bail!(
                            "dangling/forward parent or cycle at record {}",
                            position + 1
                        );
                    }
                }
                let node = children.len();
                if ids.insert(entry.id.clone(), node).is_some() {
                    anyhow::bail!("duplicate entry id at record {}", position + 1);
                }
                children.push(Vec::new());
                if let Some(parent) = &entry.parent {
                    children[ids[parent]].push(node);
                } else {
                    roots.push(node);
                }
                if let EntryValue::Compaction { first_kept, .. } = &entry.value {
                    compactions.push((node, first_kept.clone()));
                }
                if let Some(custom) = entry
                    .metadata
                    .as_ref()
                    .and_then(|m| m.custom_message.as_ref())
                {
                    custom.validate()?;
                }
            }
            SessionRecord::Head {
                id,
                total_cost_picodollars_remainder,
                ..
            } => {
                if total_cost_picodollars_remainder >= 1_000_000 {
                    anyhow::bail!("invalid head cost remainder");
                }
                if !ids.contains_key(&id) {
                    anyhow::bail!("dangling session head");
                }
                saw_head = true;
                uncommitted_entry = false;
            }
            SessionRecord::RootHead {
                total_cost_picodollars_remainder,
                ..
            } => {
                if total_cost_picodollars_remainder >= 1_000_000 {
                    anyhow::bail!("invalid root-head cost remainder");
                }
                saw_head = true;
                uncommitted_entry = false;
            }
            _ => {}
        }
    }
    if !ids.is_empty() && (!saw_head || uncommitted_entry) {
        anyhow::bail!("session lacks a final durable head record (possibly truncated)");
    }
    // Validate boundaries on EVERY branch, not just the active head. Iterative
    // ancestry intervals keep hostile deep/large graphs linear and stack-safe.
    let mut entered = vec![0; children.len()];
    let mut exited = vec![0; children.len()];
    let mut clock = 0;
    let mut stack: Vec<_> = roots.into_iter().map(|root| (root, false)).collect();
    while let Some((node, exit)) = stack.pop() {
        if exit {
            exited[node] = clock;
        } else {
            entered[node] = clock;
            clock += 1;
            stack.push((node, true));
            stack.extend(children[node].iter().rev().map(|child| (*child, false)));
        }
    }
    for (node, first_kept) in compactions {
        let kept = *ids
            .get(&first_kept)
            .ok_or_else(|| anyhow::anyhow!("dangling compaction retained boundary"))?;
        if kept == node || entered[kept] > entered[node] || exited[node] > exited[kept] {
            anyhow::bail!("compaction retained boundary is not an ancestor on its branch");
        }
    }
    Ok(())
}

/// Only source keys matter: typed decoding may add legacy defaults or normalize
/// scalar encodings (for example byte arrays to base64). Opaque JSON retains all
/// of its keys naturally; it does not need a field-name allowlist.
fn record_fields_preserved<'a>(
    source: &'a Value,
    decoded: &Value,
    record: &Value,
    path: &mut Vec<&'a str>,
) -> bool {
    match (source, decoded) {
        (Value::Object(source), Value::Object(decoded)) => source.iter().all(|(key, value)| {
            path.push(key);
            let preserved = match decoded.get(key) {
                Some(decoded) => record_fields_preserved(value, decoded, record, path),
                None => known_omitted_record_field(record, path, value),
            };
            path.pop();
            preserved
        }),
        (Value::Object(_), _) => false,
        (Value::Array(source), Value::Array(decoded)) => {
            path.push("*");
            let preserved = source.len() == decoded.len()
                && source.iter().zip(decoded).all(|(source, decoded)| {
                    record_fields_preserved(source, decoded, record, path)
                });
            path.pop();
            preserved
        }
        _ => true,
    }
}

/// Exact paths/types for fields core omits via skip_serializing_if. An unknown
/// null/false/empty field is still unknown; matching a default value alone must
/// never make a lossy import acceptable. Keep this transfer-local list in sync
/// when durable types gain additional skipped fields.
fn known_omitted_record_field(record: &Value, path: &[&str], value: &Value) -> bool {
    let is_none = value.is_null();
    match (record["type"].as_str(), path) {
        (Some("entry"), ["metadata" | "timestamp_unix_ms"])
        | (Some("usage"), ["record", "stop_reason"])
        | (Some("cache_warm"), ["record", "anchor"])
        | (Some("usage_uncertainty"), ["bound"])
        | (Some("usage_uncertainty"), ["bound", "cost_microdollars"]) => is_none,
        (Some("entry"), ["metadata", field]) => match *field {
            "custom_message"
            | "native_steering"
            | "prompt_model"
            | "prompt_model_source"
            | "prompt_color"
            | "display_text"
            | "run_outcome"
            | "tool_output"
            | "tool_composition"
            | "replay_safe_tool_calls"
            | "tool_started_unix_ms"
            | "tool_finished_unix_ms" => is_none,
            "local_synthetic_assistant" => value == &Value::Bool(false),
            "extension_metadata" => value.as_object().is_some_and(|map| map.is_empty()),
            _ => false,
        },
        (Some("entry"), ["metadata", "run_outcome", "message"])
        | (Some("entry"), ["metadata", "tool_output", "metadata"])
        | (
            Some("entry"),
            ["metadata", "extension_metadata", _, "provenance", "process_generation"],
        ) => is_none,
        (Some("entry"), ["metadata", "tool_composition", "delivery_text"]) => {
            record["metadata"]["tool_composition"]["kind"] == "call_finished" && is_none
        }
        (Some("entry"), ["value", field]) => {
            matches!(
                (record["value"]["type"].as_str(), *field),
                (Some("compaction"), "snapcompact")
                    | (Some("responses_steering"), "completed")
                    | (Some("config"), "reasoning_mode")
            ) && is_none
        }
        (Some("entry"), ["value", "Assistant", "content", "*", "ToolCall", field])
            if record["value"]["type"] == "message" =>
        {
            match *field {
                "async" => value == &Value::Bool(false),
                "argument_error" => is_none,
                _ => false,
            }
        }
        (Some("entry"), ["value", "User", "content", "*", "ToolResult", "added_tool_names"])
            if record["value"]["type"] == "message" =>
        {
            is_none
        }
        (Some("entry"), ["value", "input", "content", "*", "ToolResult", "added_tool_names"])
            if record["value"]["type"] == "responses_steering" =>
        {
            is_none
        }
        _ => false,
    }
}

fn strip_import_authority(records: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let before = records.len();
    records.retain(|r| !matches!(r["type"].as_str(), Some("tool_invocation" | "deferred_run")));
    let mut removed = records.len() != before;
    for record in records {
        if record["type"] != "entry" {
            continue;
        }
        if let Some(metadata) = record.get_mut("metadata").and_then(Value::as_object_mut) {
            for key in ["extension_metadata", "tool_composition", "native_steering"] {
                removed |= metadata.remove(key).is_some();
            }
            // Imported claims cannot grant permission to replay effects. Empty denies all.
            metadata.insert("replay_safe_tool_calls".into(), serde_json::json!([]));
        } else {
            record["metadata"] = serde_json::json!({"replay_safe_tool_calls": []});
        }
    }
    if removed {
        warnings.push("Private tool/deferred state and extension authority were excluded; imported tool calls are not authorized for effect replay.".into());
    }
}

#[cfg(test)]
mod tests;
