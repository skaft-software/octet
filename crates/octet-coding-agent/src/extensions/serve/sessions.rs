//! Durable session seeds, model selection, and catalog projections.
//!
//! Session state is persisted by the host, while this module owns the
//! conversion between that durable state and Serve's public session shapes.

use super::*;

pub(super) struct SessionSeedOptions<'a> {
    pub(super) workspace: &'a Path,
    pub(super) project_id: Option<ProjectId>,
    pub(super) model: ModelSelection,
    pub(super) authority: AuthorityProfile,
    pub(super) generation: u64,
    pub(super) meta: Option<SessionMeta>,
    pub(super) attachment_store: Option<&'a AttachmentStore>,
    pub(super) resource_store: Option<&'a octet_serve_backend::ResourceStore>,
}

pub(super) fn seed_from_session(
    session: &Session,
    session_id: SessionId,
    options: SessionSeedOptions<'_>,
) -> Result<SessionSeed, ServiceError> {
    let SessionSeedOptions {
        workspace,
        project_id,
        model,
        authority,
        generation,
        meta,
        attachment_store,
        resource_store,
    } = options;
    let mut chain = Vec::new();
    let mut cursor = session.head_ref();
    while let Some(id) = cursor {
        let entry = session.entry(id).ok_or(ServiceError::InvalidSeed)?;
        chain.push(entry);
        cursor = entry.parent.as_ref();
    }
    chain.reverse();
    let active_entry_ids = chain
        .iter()
        .map(|entry| entry.id.0.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let mut items = Vec::new();
    let mut sources = Vec::new();
    let mut artifacts = Vec::new();
    let mut tool_items = HashMap::new();
    let mut tool_calls = HashMap::new();
    let mut attributions = HashMap::<String, Vec<StoredRunItemAttribution>>::new();
    let mut run_ids_by_entry = HashMap::<String, RunId>::new();
    let mut reviews_by_outcome = HashMap::<String, CompletionReview>::new();
    if let Some(resources) = resource_store {
        for entry in &chain {
            if entry
                .metadata
                .as_ref()
                .is_none_or(|metadata| metadata.run_outcome.is_none())
            {
                continue;
            }
            let Ok(outcome_entry_id) = DurableEntryId::new(entry.id.0.clone()) else {
                continue;
            };
            let Some(record) = load_stored_run_record(resources, &session_id, &outcome_entry_id)
            else {
                continue;
            };
            let Ok(run_id) = RunId::new(record.run_id.clone()) else {
                continue;
            };
            reviews_by_outcome.insert(entry.id.0.clone(), record.review.clone());
            for item in record.items {
                if !active_entry_ids.contains(item.durable_entry_id.as_str()) {
                    continue;
                }
                run_ids_by_entry.insert(item.durable_entry_id.clone(), run_id.clone());
                attributions
                    .entry(item.durable_entry_id.clone())
                    .or_default()
                    .push(item);
            }
            for tool in record.tools {
                let Ok(item_id) = ItemId::new(tool.item_id.clone()) else {
                    continue;
                };
                let Ok(turn_id) = TurnId::new(tool.turn_id.clone()) else {
                    continue;
                };
                tool_items.insert(tool.tool_call_id.clone(), item_id);
                tool_calls.insert(
                    tool.tool_call_id,
                    ProjectedToolCall {
                        name: tool.activity.raw_tool_name.clone(),
                        arguments: serde_json::Value::Null,
                        activity: tool.activity,
                        result: tool.result,
                        turn_id,
                    },
                );
            }
        }
    }
    for entries in attributions.values_mut() {
        entries.sort_by_key(|item| item.ordinal);
    }
    let mut pending_attachments = VecDeque::new();
    for entry in chain {
        if is_local_synthetic_assistant(entry) {
            continue;
        }
        let attachments = attachment_refs_for_entry(
            entry,
            attachment_store,
            &session_id,
            &mut pending_attachments,
        )?;
        let run_id = run_ids_by_entry.get(&entry.id.0).cloned();
        let review = reviews_by_outcome.get(&entry.id.0);
        let mut projected = project_entry(
            entry,
            workspace,
            run_id.clone(),
            None,
            None,
            None,
            &mut tool_items,
            &mut tool_calls,
            review,
            attachments,
        )?;
        if let Some(stored) = attributions.get(&entry.id.0) {
            for (item, attribution) in projected.iter_mut().zip(stored) {
                item.id = ItemId::new(attribution.item_id.clone())
                    .map_err(|_| ServiceError::InvalidSeed)?;
                item.turn_id = Some(
                    TurnId::new(attribution.turn_id.clone())
                        .map_err(|_| ServiceError::InvalidSeed)?,
                );
                item.run_id = run_id.clone();
                if let ItemPayload::UserMessage {
                    delivery,
                    documents,
                    project_files,
                    branch_provenance,
                    ..
                } = &mut item.payload
                {
                    *delivery = attribution.user_delivery;
                    *documents = attribution.documents.clone();
                    *project_files = attribution.project_files.clone();
                    *branch_provenance = attribution.branch_provenance.clone();
                }
            }
        }
        items.extend(projected);
        if let Some(projection) = resource_store.and_then(|store| {
            rehydrate_stored_evidence(
                store,
                session,
                &session_id,
                entry,
                &active_entry_ids,
                &tool_items,
            )
        }) {
            items.extend(projection.items);
            sources.extend(projection.sources);
            artifacts.extend(projection.artifacts);
        }
    }
    // A legacy session may predate semantic run sidecars. Its result entry is
    // encountered after the corresponding call entry, so apply the safe
    // terminal fallback back onto the already-projected call. New sessions
    // take the same path with the exact persisted activity.
    let projected_tools = tool_items
        .iter()
        .filter_map(|(tool_call_id, item_id)| {
            tool_calls
                .get(tool_call_id)
                .map(|tool| (item_id.clone(), tool.activity.clone()))
        })
        .collect::<HashMap<_, _>>();
    for item in &mut items {
        if let ItemPayload::ToolCall(activity) = &mut item.payload {
            if let Some(projected) = projected_tools.get(&item.id) {
                *activity = projected.clone();
            }
        }
    }
    if items.len() > MAX_PROJECTED_SESSION_ITEMS {
        items = items.split_off(items.len() - MAX_PROJECTED_SESSION_ITEMS);
    }
    let modified_at_ms = meta
        .as_ref()
        .map(|meta| system_time_ms(meta.modified))
        .unwrap_or_else(now_ms);
    let title = meta
        .as_ref()
        .map(|meta| bounded_text(&meta.title, 512))
        .unwrap_or_else(|| "Session".into());
    let pinned = meta.as_ref().is_some_and(|meta| meta.pinned);
    let archived = meta.as_ref().is_some_and(|meta| meta.archived);
    let (lifecycle, retention, forked_from) = meta
        .as_ref()
        .map(|meta| session_catalog_metadata(meta, &session_id))
        .transpose()?
        .unwrap_or((SessionCatalogState::Active, None, None));
    let summary = SessionSummary {
        id: session_id.clone(),
        project_id,
        title,
        tags: meta.map(|meta| meta.tags).unwrap_or_default(),
        created_at_ms: modified_at_ms,
        modified_at_ms,
        pinned,
        archived,
        lifecycle,
        retention,
        forked_from,
        provisional: false,
        live_state: SessionLiveState::Idle,
        attention: AttentionState::None,
        pull_request: None,
        owner: ActorOwnerState::Hosted,
        model: model.clone(),
    };
    let branches = branch_graph(session)?;
    let snapshot = SessionSnapshot {
        session_id,
        delegated_parent_session_id: None,
        actor_generation: generation,
        cursor: SessionCursor::zero(generation),
        durable_head: branches.head.clone(),
        branches,
        live_state: SessionLiveState::Idle,
        active_run_id: None,
        model,
        authority,
        context: ContextUsage {
            usage_uncertain: session.has_uncertain_usage(),
            ..ContextUsage::default()
        },
        items,
        extension_presentations: Vec::new(),
        pending_requests: Vec::new(),
        sources,
        artifacts,
    };
    let seed = SessionSeed { summary, snapshot };
    seed.validate()?;
    Ok(seed)
}

pub(super) fn empty_seed(
    session_id: SessionId,
    project_id: Option<ProjectId>,
    model: ModelSelection,
    authority: AuthorityProfile,
    generation: u64,
) -> SessionSeed {
    let timestamp = now_ms();
    SessionSeed {
        summary: SessionSummary {
            id: session_id.clone(),
            project_id,
            title: "New session".into(),
            tags: Vec::new(),
            created_at_ms: timestamp,
            modified_at_ms: timestamp,
            pinned: false,
            archived: false,
            lifecycle: SessionCatalogState::Active,
            retention: None,
            forked_from: None,
            provisional: true,
            live_state: SessionLiveState::Idle,
            attention: AttentionState::None,
            pull_request: None,
            owner: ActorOwnerState::Hosted,
            model: model.clone(),
        },
        snapshot: SessionSnapshot {
            session_id,
            delegated_parent_session_id: None,
            actor_generation: generation,
            cursor: SessionCursor::zero(generation),
            durable_head: None,
            branches: SessionBranchGraph::default(),
            live_state: SessionLiveState::Idle,
            active_run_id: None,
            model,
            authority,
            context: ContextUsage::default(),
            items: Vec::new(),
            extension_presentations: Vec::new(),
            pending_requests: Vec::new(),
            sources: Vec::new(),
            artifacts: Vec::new(),
        },
    }
}
