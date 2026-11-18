//! Display-only Pi v3 import. Unlike a resumable conversion, this module
//! never constructs a Session, restores a route, or accepts replay authority.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{redact_value, strict_json, write_export};

// The HTML viewer formats each record and includes a literal copy. Use a
// smaller source bound than native JSONL import to bound output and memory.
const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;

pub(super) fn archive(
    source: &Path,
    output: &Path,
    cwd: &Path,
    theme: &str,
) -> anyhow::Result<PathBuf> {
    let input = cwd.join(source);
    let destination = cwd.join(output);
    anyhow::ensure!(
        destination
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("html")),
        "--transcript-only requires an .html destination"
    );
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(&input, MAX_SOURCE_BYTES)?;
    let source = strict_json::jsonl(&bytes)?;
    let mut package = project(&source)?;
    let mut redactions = 0;
    redact_value(&mut package, None, &mut redactions)?;
    package["redaction_count"] = json!(redactions);
    let bytes = crate::modes::export_html::render(&package, theme)?;
    write_export(&destination, &bytes, false)?;
    Ok(destination)
}

fn project(source: &[Value]) -> anyhow::Result<Value> {
    let header = &source[0];
    anyhow::ensure!(
        header["type"] == "session" && header["version"] == 3,
        "--transcript-only expects a Pi v3 JSONL session"
    );
    // Pi 1.5.2 dist/core/session-manager.js:283-290 builds context from the
    // selected ancestry. Preserve branch links even across omitted records.
    let mut parents = HashMap::new();
    let mut entries = Vec::new();
    for (index, record) in source.iter().enumerate().skip(1) {
        let id = record["id"]
            .as_str()
            .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
            .ok_or_else(|| anyhow::anyhow!("invalid Pi entry id at record {}", index + 1))?;
        anyhow::ensure!(
            !parents.contains_key(id),
            "duplicate Pi entry id at record {}",
            index + 1
        );
        let parent = match record.get("parentId") {
            Some(Value::Null) => None,
            Some(Value::String(parent)) => *parents.get(parent.as_str()).ok_or_else(|| {
                anyhow::anyhow!("unknown or forward Pi parent at record {}", index + 1)
            })?,
            _ => anyhow::bail!("invalid Pi parent at record {}", index + 1),
        };
        let value =
            match record["type"].as_str() {
                Some("message") => {
                    let message = &record["message"];
                    match message["role"].as_str() {
                        Some("user") => value_of_text("User", &message["content"])?,
                        Some("assistant") => value_of_text("Assistant", &message["content"])?,
                        Some("toolResult") => {
                            let content = text_parts(&message["content"])?;
                            (!content.is_empty()).then(|| json!({"type":"message", "User": {
                            "content": [{"ToolResult": {
                                "content": content, "tool_name": message["toolName"].as_str(),
                                "is_error": message["isError"] == true
                            }}]
                        }}))
                        }
                        _ => None,
                    }
                }
                Some("branch_summary" | "compaction") => record["summary"]
                    .as_str()
                    .map(|summary| json!({"type":record["type"],"summary":summary})),
                _ => None,
            };
        let kept = if let Some(value) = value {
            entries.push(json!({"type":"entry","id":id,"parent":parent,"value":value}));
            Some(id)
        } else {
            parent
        };
        parents.insert(id, kept);
    }
    anyhow::ensure!(
        !entries.is_empty(),
        "Pi session has no readable transcript text"
    );
    Ok(json!({
        "format": "pi-transcript-only",
        "source_title": "Pi transcript (not resumable)",
        "note": "Text only. Tool arguments, reasoning, media, system instructions, usage and replay state are omitted.",
        "redacted": true,
        "records": entries
    }))
}

fn value_of_text(role: &str, content: &Value) -> anyhow::Result<Option<Value>> {
    let content = text_parts(content)?;
    Ok((!content.is_empty()).then(|| json!({"type":"message", (role): {"content": content}})))
}

fn text_parts(content: &Value) -> anyhow::Result<Vec<Value>> {
    if let Some(text) = content.as_str() {
        return Ok((!text.is_empty())
            .then(|| json!({"Text":text}))
            .into_iter()
            .collect());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid Pi message content"))?;
    blocks
        .iter()
        .filter(|part| part["type"] == "text")
        .map(|part| {
            let text = part["text"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid Pi text block"))?;
            Ok(json!({"Text":text}))
        })
        .collect()
}
