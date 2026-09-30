//! Durable tool evidence: workspace snapshots, diffs and stored records.

use super::*;

pub(super) struct WorkspaceFileSnapshot {
    pub(super) display_path: String,
    pub(super) display_name: String,
    pub(super) bytes: bytes::Bytes,
    pub(super) media_type: &'static str,
    pub(super) artifact_kind: ArtifactKind,
}

pub(super) const STORED_EVIDENCE_VERSION: u16 = 2;

pub(super) const STORED_RUN_RECORD_VERSION: u16 = 1;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredRunItemAttribution {
    pub(super) durable_entry_id: String,
    pub(super) ordinal: u32,
    pub(super) item_id: String,
    pub(super) turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) user_delivery: Option<UserMessageDelivery>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) documents: Vec<DocumentReference>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) project_files: Vec<TrustedFileEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) branch_provenance: Option<ConversationBranchProvenance>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredRunTool {
    pub(super) tool_call_id: String,
    pub(super) item_id: String,
    pub(super) turn_id: String,
    pub(super) activity: ToolActivity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) result: Option<ToolResultSummary>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredRunRecord {
    pub(super) version: u16,
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) outcome_entry_id: String,
    pub(super) started_at_ms: u64,
    pub(super) completed_at_ms: u64,
    pub(super) items: Vec<StoredRunItemAttribution>,
    pub(super) tools: Vec<StoredRunTool>,
    pub(super) review: CompletionReview,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StoredToolEvidence {
    pub(super) version: u16,
    pub(super) session_id: String,
    pub(super) tool_call_id: String,
    pub(super) call_entry_id: String,
    pub(super) result_entry_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) origin_item_id: Option<String>,
    pub(super) entries: Vec<StoredEvidenceEntry>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum StoredEvidenceEntry {
    Source {
        item_id: String,
        source_id: String,
        source_kind: SourceKind,
        title: String,
        handle: String,
        consulted_at_ms: u64,
    },
    FileChange {
        item_id: String,
        diff_handle: String,
        result_handle: String,
        display_path: String,
        additions: u32,
        deletions: u32,
    },
    Artifact {
        item_id: String,
        artifact_id: String,
        artifact_kind: ArtifactKind,
        name: String,
        media_type: String,
        handle: String,
        byte_len: u64,
        content_hash: String,
    },
}

pub(super) struct EvidenceProjection {
    pub(super) items: Vec<SessionItem>,
    pub(super) sources: Vec<SourceRef>,
    pub(super) artifacts: Vec<ArtifactRef>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn project_tool_evidence(
    session: &Session,
    workspace: &Path,
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    turn_id: &TurnId,
    tool_call_id: &str,
    tool_item_id: &ItemId,
    tool: &ProjectedToolCall,
    output: &ToolOutput,
) -> Vec<EventPayload> {
    match project_tool_evidence_inner(
        session,
        workspace,
        resources,
        session_id,
        run_id,
        turn_id,
        tool_call_id,
        tool_item_id,
        tool,
        output,
    ) {
        Ok(events) => events,
        Err(_) => {
            let _ = resources.rollback_uncommitted_tool_resources(session_id, tool_call_id);
            Vec::new()
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn project_tool_evidence_inner(
    session: &Session,
    workspace: &Path,
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    run_id: &RunId,
    turn_id: &TurnId,
    tool_call_id: &str,
    tool_item_id: &ItemId,
    tool: &ProjectedToolCall,
    output: &ToolOutput,
) -> Result<Vec<EventPayload>, ServiceError> {
    let (call_entry_id, result_entry_id) =
        durable_tool_anchor(session, tool_call_id).ok_or(ServiceError::InvalidBoundary)?;
    let identity = stable_hash(
        format!(
            "{}\0{}\0{}\0{}",
            session_id.as_str(),
            call_entry_id.as_str(),
            result_entry_id.as_str(),
            tool_call_id
        )
        .as_bytes(),
    );
    let Some(short_identity) = identity.get(..24) else {
        return Err(ServiceError::Internal);
    };

    let entries = match tool.name.as_str() {
        "read" => {
            let Some(path) = tool
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(Vec::new());
            };
            let Some(snapshot) = snapshot_workspace_file(workspace, path) else {
                return Ok(Vec::new());
            };
            if trusted_output_hash(&output.text).as_deref()
                != Some(stable_hash(&snapshot.bytes).as_str())
            {
                return Ok(Vec::new());
            }
            let stored = resources
                .register(
                    session_id,
                    tool_call_id,
                    "source",
                    &snapshot.display_name,
                    snapshot.media_type,
                    snapshot.bytes,
                )
                .map_err(resource_store_service_error)?;
            vec![StoredEvidenceEntry::Source {
                item_id: format!("item-source-{short_identity}"),
                source_id: format!("source-{short_identity}"),
                source_kind: SourceKind::File,
                title: snapshot.display_path,
                handle: stored.handle,
                consulted_at_ms: now_ms(),
            }]
        }
        "read_skill_resource" => {
            let Some(title) = tool
                .arguments
                .get("resource_path")
                .and_then(serde_json::Value::as_str)
                .and_then(safe_relative_path)
            else {
                return Ok(Vec::new());
            };
            let stored = resources
                .register(
                    session_id,
                    tool_call_id,
                    "source",
                    &title,
                    "text/plain",
                    bytes::Bytes::copy_from_slice(output.text.as_bytes()),
                )
                .map_err(resource_store_service_error)?;
            vec![StoredEvidenceEntry::Source {
                item_id: format!("item-source-{short_identity}"),
                source_id: format!("source-{short_identity}"),
                source_kind: SourceKind::Resource,
                title: bounded_text(&title, 512),
                handle: stored.handle,
                consulted_at_ms: now_ms(),
            }]
        }
        "edit" | "write" => {
            let Some(path) = tool
                .arguments
                .get("path")
                .and_then(serde_json::Value::as_str)
            else {
                return Ok(Vec::new());
            };
            let Some(snapshot) = snapshot_workspace_file(workspace, path) else {
                return Ok(Vec::new());
            };
            let snapshot_hash = stable_hash(&snapshot.bytes);
            if trusted_output_hash(&output.text).as_deref() != Some(snapshot_hash.as_str()) {
                return Ok(Vec::new());
            }
            let write_created = tool.name == "write" && output_reports_created(&output.text);
            if tool.name == "write" && output.text.contains("\n(no change)") {
                return Ok(Vec::new());
            }
            let diff = if write_created {
                let Some(content) = tool
                    .arguments
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                else {
                    return Ok(Vec::new());
                };
                creation_diff(&snapshot.display_path, content)
            } else {
                let Some(detail) = output.text.splitn(3, '\n').nth(2) else {
                    return Ok(Vec::new());
                };
                if !detail.starts_with("--- ") {
                    return Ok(Vec::new());
                }
                detail.to_owned()
            };
            if diff.is_empty() || diff.len() > MAX_OPAQUE_RESOURCE_BYTES {
                return Ok(Vec::new());
            }
            let diff_name = format!("{}.diff", snapshot.display_name);
            let stored_diff = resources
                .register(
                    session_id,
                    tool_call_id,
                    "diff",
                    &diff_name,
                    "text/plain",
                    bytes::Bytes::from(diff.clone()),
                )
                .map_err(resource_store_service_error)?;
            let stored_result = resources
                .register(
                    session_id,
                    tool_call_id,
                    "result",
                    &snapshot.display_name,
                    snapshot.media_type,
                    snapshot.bytes,
                )
                .map_err(resource_store_service_error)?;
            let (additions, deletions) = if tool.name == "edit" {
                (
                    line_count(
                        tool.arguments
                            .get("new")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    ),
                    line_count(
                        tool.arguments
                            .get("old")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default(),
                    ),
                )
            } else {
                diff_line_counts(&diff)
            };
            let mut entries = vec![StoredEvidenceEntry::FileChange {
                item_id: format!("item-file-change-{short_identity}"),
                diff_handle: stored_diff.handle,
                result_handle: stored_result.handle.clone(),
                display_path: snapshot.display_path,
                additions,
                deletions,
            }];
            if write_created && is_deliverable_artifact(snapshot.artifact_kind) {
                entries.push(StoredEvidenceEntry::Artifact {
                    item_id: format!("item-artifact-{short_identity}"),
                    artifact_id: format!("artifact-{short_identity}"),
                    artifact_kind: snapshot.artifact_kind,
                    name: snapshot.display_name,
                    media_type: snapshot.media_type.to_owned(),
                    handle: stored_result.handle,
                    byte_len: stored_result.byte_len,
                    content_hash: stored_result.sha256,
                });
            }
            entries
        }
        _ => return Ok(Vec::new()),
    };

    let record = StoredToolEvidence {
        version: STORED_EVIDENCE_VERSION,
        session_id: session_id.as_str().to_owned(),
        tool_call_id: tool_call_id.to_owned(),
        call_entry_id: call_entry_id.as_str().to_owned(),
        result_entry_id: result_entry_id.as_str().to_owned(),
        run_id: Some(run_id.as_str().to_owned()),
        turn_id: Some(turn_id.as_str().to_owned()),
        origin_item_id: Some(tool_item_id.as_str().to_owned()),
        entries,
    };
    let record_bytes = serde_json::to_vec(&record).map_err(|_| ServiceError::Internal)?;
    resources
        .persist_record(session_id, &result_entry_id, tool_call_id, &record_bytes)
        .map_err(resource_store_service_error)?;
    let projection = project_stored_evidence(
        resources,
        session_id,
        &record,
        Some(run_id.clone()),
        Some(turn_id.clone()),
        Some(tool_item_id.clone()),
    )?;
    let mut events = Vec::new();
    for source in projection.sources {
        events.push(EventPayload::SourceUpserted { source });
    }
    for artifact in projection.artifacts {
        events.push(EventPayload::ArtifactUpserted { artifact });
    }
    for item in projection.items {
        events.push(EventPayload::ItemCommitted { item });
    }
    Ok(events)
}

pub(super) fn durable_tool_anchor(
    session: &Session,
    tool_call_id: &str,
) -> Option<(DurableEntryId, DurableEntryId)> {
    let mut cursor = session.head_ref();
    let mut result_entry_id = None;
    while let Some(entry_id) = cursor {
        let entry = session.entry(entry_id)?;
        match &entry.value {
            EntryValue::Message(Message::User(message))
                if result_entry_id.is_none()
                    && message.content.iter().any(|part| {
                        matches!(
                            part,
                            UserPart::ToolResult(result)
                                if result.tool_call_id.0 == tool_call_id && !result.is_error
                        )
                    }) =>
            {
                result_entry_id = DurableEntryId::new(entry.id.0.clone()).ok();
            }
            EntryValue::Message(Message::Assistant(message))
                if result_entry_id.is_some()
                    && message.content.iter().any(|part| {
                        matches!(
                            part,
                            AssistantPart::ToolCall(call) if call.id.0 == tool_call_id
                        )
                    }) =>
            {
                return Some((
                    DurableEntryId::new(entry.id.0.clone()).ok()?,
                    result_entry_id?,
                ));
            }
            _ => {}
        }
        cursor = entry.parent.as_ref();
    }
    None
}

pub(super) fn trusted_output_hash(text: &str) -> Option<String> {
    text.split_ascii_whitespace()
        .find_map(|token| token.strip_prefix("hash="))
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .map(str::to_owned)
}

pub(super) fn output_reports_created(text: &str) -> bool {
    text.lines()
        .nth(1)
        .is_some_and(|line| line.contains("  created hash="))
}

pub(super) fn creation_diff(path: &str, content: &str) -> String {
    let total = content.lines().count();
    let mut diff = format!("--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{total} @@\n");
    for line in content.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    diff
}

pub(super) fn diff_line_counts(diff: &str) -> (u32, u32) {
    let additions = diff
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count()
        .min(u32::MAX as usize) as u32;
    let deletions = diff
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count()
        .min(u32::MAX as usize) as u32;
    (additions, deletions)
}

pub(super) fn is_deliverable_artifact(kind: ArtifactKind) -> bool {
    matches!(
        kind,
        ArtifactKind::Site
            | ArtifactKind::Document
            | ArtifactKind::Spreadsheet
            | ArtifactKind::Presentation
    )
}

pub(super) fn project_stored_evidence(
    resources: &octet_serve_backend::ResourceStore,
    session_id: &SessionId,
    record: &StoredToolEvidence,
    run_id: Option<RunId>,
    turn_id: Option<TurnId>,
    origin_item_id: Option<ItemId>,
) -> Result<EvidenceProjection, ServiceError> {
    if !matches!(record.version, 1 | STORED_EVIDENCE_VERSION)
        || record.session_id != session_id.as_str()
        || record.entries.is_empty()
    {
        return Err(ServiceError::InvalidSeed);
    }
    let durable_entry_id = DurableEntryId::new(record.result_entry_id.clone())
        .map_err(|_| ServiceError::InvalidSeed)?;
    let run_id = record
        .run_id
        .clone()
        .and_then(|value| RunId::new(value).ok())
        .or(run_id);
    let turn_id = record
        .turn_id
        .clone()
        .and_then(|value| TurnId::new(value).ok())
        .or(turn_id);
    let origin_item_id = record
        .origin_item_id
        .clone()
        .and_then(|value| ItemId::new(value).ok())
        .or(origin_item_id);
    let mut projection = EvidenceProjection {
        items: Vec::new(),
        sources: Vec::new(),
        artifacts: Vec::new(),
    };
    for entry in &record.entries {
        let (item_id, payload) = match entry {
            StoredEvidenceEntry::Source {
                item_id,
                source_id,
                source_kind,
                title,
                handle,
                consulted_at_ms,
            } => {
                let source = SourceRef {
                    id: SourceId::new(source_id.clone()).map_err(|_| ServiceError::InvalidSeed)?,
                    kind: *source_kind,
                    title: bounded_text(title, 512),
                    handle: handle.clone(),
                    origin_item_id: origin_item_id.clone(),
                    consulted_at_ms: *consulted_at_ms,
                    cited: false,
                    available: resources.content(session_id, handle).is_ok(),
                };
                projection.sources.push(source.clone());
                (item_id, ItemPayload::Source(source))
            }
            StoredEvidenceEntry::FileChange {
                item_id,
                diff_handle,
                result_handle,
                display_path,
                additions,
                deletions,
            } => (
                item_id,
                ItemPayload::FileChange(FileChange {
                    handle: diff_handle.clone(),
                    result_handle: Some(result_handle.clone()),
                    display_path: bounded_text(display_path, 1024),
                    origin_item_id: origin_item_id.clone(),
                    additions: *additions,
                    deletions: *deletions,
                }),
            ),
            StoredEvidenceEntry::Artifact {
                item_id,
                artifact_id,
                artifact_kind,
                name,
                media_type,
                handle,
                byte_len,
                content_hash,
            } => {
                let artifact = ArtifactRef {
                    id: ArtifactId::new(artifact_id.clone())
                        .map_err(|_| ServiceError::InvalidSeed)?,
                    kind: *artifact_kind,
                    name: bounded_text(name, 512),
                    media_type: media_type.clone(),
                    handle: handle.clone(),
                    byte_len: *byte_len,
                    content_hash: Some(content_hash.clone()),
                    origin_item_id: origin_item_id.clone(),
                    available: resources.content(session_id, handle).is_ok(),
                };
                projection.artifacts.push(artifact.clone());
                (item_id, ItemPayload::Artifact(artifact))
            }
        };
        projection.items.push(SessionItem {
            id: ItemId::new(item_id.clone()).map_err(|_| ServiceError::InvalidSeed)?,
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
            provider_attempt: None,
            lifecycle: ItemLifecycle::Committed,
            durable_entry_id: Some(durable_entry_id.clone()),
            payload,
        });
    }
    Ok(projection)
}

pub(super) fn snapshot_workspace_file(
    workspace: &Path,
    requested: &str,
) -> Option<WorkspaceFileSnapshot> {
    let workspace = workspace.canonicalize().ok()?;
    let requested_path = if requested.contains("://") || requested.starts_with("file:") {
        let url = url::Url::parse(requested).ok()?;
        if url.scheme() != "file"
            || url.fragment().is_some()
            || (!url.username().is_empty() || url.password().is_some())
            || !matches!(url.host_str(), None | Some("") | Some("localhost"))
        {
            return None;
        }
        url.to_file_path().ok()?
    } else {
        PathBuf::from(requested)
    };
    let candidate = if requested_path.is_absolute() {
        requested_path
    } else {
        workspace.join(requested_path)
    };
    let link_metadata = candidate.symlink_metadata().ok()?;
    if link_metadata.file_type().is_symlink() {
        return None;
    }
    let canonical = candidate.canonicalize().ok()?;
    if canonical == workspace || !canonical.starts_with(&workspace) {
        return None;
    }

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&canonical).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_OPAQUE_RESOURCE_BYTES as u64 {
        return None;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(MAX_OPAQUE_RESOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > MAX_OPAQUE_RESOURCE_BYTES {
        return None;
    }
    let relative = canonical.strip_prefix(&workspace).ok()?;
    let display_path = bounded_text(&relative.to_string_lossy().replace('\\', "/"), 512);
    let display_name = canonical.file_name()?.to_str()?.to_owned();
    let extension = canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    Some(WorkspaceFileSnapshot {
        display_path,
        display_name,
        media_type: workspace_media_type(&extension, &bytes),
        artifact_kind: artifact_kind_for_extension(&extension),
        bytes: bytes::Bytes::from(bytes),
    })
}

pub(super) fn workspace_media_type(extension: &str, bytes: &[u8]) -> &'static str {
    if std::str::from_utf8(bytes).is_err() {
        return match extension {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "pdf" => "application/pdf",
            _ => "application/octet-stream",
        };
    }
    match extension {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "jsx" | "mjs" | "cjs" => "text/javascript",
        "json" => "application/json",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "svg" => "image/svg+xml",
        _ => "text/plain",
    }
}

pub(super) fn artifact_kind_for_extension(extension: &str) -> ArtifactKind {
    match extension {
        "png" | "jpg" | "jpeg" | "gif" | "webp" => ArtifactKind::Image,
        "pdf" | "doc" | "docx" | "md" | "txt" => ArtifactKind::Document,
        "csv" | "tsv" | "xls" | "xlsx" => ArtifactKind::Spreadsheet,
        "ppt" | "pptx" | "key" => ArtifactKind::Presentation,
        "html" | "htm" => ArtifactKind::Site,
        "rs" | "js" | "jsx" | "ts" | "tsx" | "css" | "json" | "toml" | "yaml" | "yml" | "py"
        | "go" | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "hpp" | "sh" => ArtifactKind::File,
        _ => ArtifactKind::Other,
    }
}

pub(super) fn line_count(value: &str) -> u32 {
    value.lines().count().min(u32::MAX as usize) as u32
}
