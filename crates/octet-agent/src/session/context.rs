//! The model-visible projection: compaction boundaries, assistant and
//! Responses turns, active-skill resolution, and the coalescing rules
//! that turn entries back into provider messages.
//!
//! Separate from `entries` because what a provider may see is a strictly
//! smaller view of the log: config markers, extension entries and
//! everything before the newest compaction boundary are in the file but
//! not in the request. Keeping the projection here is what makes that
//! boundary auditable in one place.

use super::*;

impl Session {
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
        self.compact_with_checkpoint(summary.into(), first_kept, details, None)
    }

    /// Atomically append a validated vision checkpoint in place of a model summary.
    pub fn compact_snapcompact(
        &mut self,
        summary: String,
        first_kept: EntryId,
        details: crate::compaction::CompactionDetails,
        snapcompact: SnapcompactCheckpoint,
    ) -> Result<EntryId, SessionError> {
        self.compact_with_checkpoint(summary, first_kept, details, Some(snapcompact))
    }

    fn compact_with_checkpoint(
        &mut self,
        summary: String,
        first_kept: EntryId,
        details: crate::compaction::CompactionDetails,
        snapcompact: Option<SnapcompactCheckpoint>,
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
            summary,
            snapcompact,
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
    /// Retains extension metadata and the host-captured per-call replay fence;
    /// prompt, tool-result, and run presentation fields are cleared before persistence.
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
                && !output.items().iter().any(|item| {
                    item.as_json()
                        .get("type")
                        .and_then(serde_json::Value::as_str)
                        == Some("configuration_update")
                })
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
        self.write_head_record(&mut buffer, &assistant_id, &self.total_cost_microdollars)?;
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

        self.invocation_entries
            .record(&assistant_entry, &self.index, &[]);
        self.entries.push(assistant_entry);
        self.index
            .insert(assistant_id.clone(), self.entries.len().saturating_sub(1));
        if let Some(sidecar_entry) = sidecar_entry {
            self.invocation_entries
                .record(&sidecar_entry, &self.index, &[]);
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
}

impl Session {
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
        let mut summary: Option<(String, Option<SnapcompactCheckpoint>)> = None;
        let mut boundary: Option<EntryId> = None;

        let mut cursor = self.head.as_ref();
        while let Some(id) = cursor {
            let entry = self
                .entry(id)
                .ok_or_else(|| SessionError::UnknownEntry(id.clone()))?;
            match &entry.value {
                EntryValue::Message(m) => newest_first.push(m.clone()),
                EntryValue::ResponsesReasoning { .. }
                | EntryValue::ResponsesSteering { .. }
                | EntryValue::Config { .. }
                | EntryValue::PromptTemplateSelected { .. }
                | EntryValue::ResponsesTurn { .. }
                | EntryValue::ResponsesCompaction { .. } => {}
                EntryValue::Compaction {
                    summary: compaction_summary,
                    snapcompact,
                    first_kept,
                    ..
                } => {
                    // A compaction summary represents everything it replaces,
                    // including any older summary in that range. Therefore only
                    // the marker nearest the head is model-visible; injecting
                    // older summaries again duplicates overlapping history.
                    if boundary.is_none() {
                        summary = Some((compaction_summary.clone(), snapcompact.clone()));
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
            .map(|(summary, snapcompact)| {
                Message::User(UserMessage {
                    content: compaction_parts(&summary, snapcompact.as_ref()),
                })
            })
            .collect();
        messages.extend(newest_first.into_iter().rev());
        Ok(coalesce_tool_results(messages))
    }

    /// Preview the proposed checkpoint without touching the durable branch.
    /// Used to refuse a bitmap checkpoint that cannot fit the active model.
    pub fn preview_compaction_context(
        &self,
        first_kept: &EntryId,
        summary: &str,
        checkpoint: &SnapcompactCheckpoint,
    ) -> Result<Vec<Message>, SessionError> {
        let branch = self.active_branch_entries()?;
        let start = branch
            .iter()
            .position(|entry| &entry.id == first_kept)
            .ok_or_else(|| SessionError::UnknownEntry(first_kept.clone()))?;
        let mut messages = vec![Message::User(UserMessage {
            content: compaction_parts(summary, Some(checkpoint)),
        })];
        messages.extend(
            branch[start..]
                .iter()
                .filter_map(|entry| match &entry.value {
                    EntryValue::Message(message) => Some(message.clone()),
                    _ => None,
                }),
        );
        Ok(coalesce_tool_results(messages))
    }

    /// The active checkpoint is bitmap-only and cannot be replayed on a text model.
    pub fn has_snapcompact_context(&self) -> Result<bool, SessionError> {
        Ok(self
            .active_branch_entries()?
            .iter()
            .rev()
            .find_map(|entry| match &entry.value {
                EntryValue::Compaction { snapcompact, .. } => Some(snapcompact.is_some()),
                _ => None,
            })
            .unwrap_or(false))
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
                EntryValue::Compaction {
                    summary,
                    snapcompact,
                    ..
                } => {
                    messages.clear();
                    let text = snapcompact
                        .as_ref()
                        .map_or(summary.as_str(), |image| image.source_text.as_str());
                    messages.push(Message::User(UserMessage {
                        content: vec![UserPart::Text(format!(
                            "[summary of earlier conversation]\n{text}"
                        ))],
                    }));
                }
                EntryValue::ResponsesReasoning { .. }
                | EntryValue::ResponsesSteering { .. }
                | EntryValue::Config { .. }
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
    pub(super) fn is_ancestor_of_head(&self, id: &EntryId) -> bool {
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
pub(super) fn append_context_message(messages: &mut Vec<Message>, message: &Message) {
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
pub(super) fn compaction_parts(
    summary: &str,
    checkpoint: Option<&SnapcompactCheckpoint>,
) -> Vec<UserPart> {
    let mut parts = vec![UserPart::Text(format!(
        "[summary of earlier conversation]\n{summary}"
    ))];
    if let Some(checkpoint) = checkpoint {
        parts.extend(checkpoint.frames.iter().cloned().map(UserPart::Media));
    }
    parts
}

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
