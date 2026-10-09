//! The Responses API replay projection: the per-route sidecar cache, its
//! branch/route invalidation, and the reasoning-state rebuild.
//!
//! Separate from `context` because the Responses wire format carries
//! opaque provider items that cannot be regenerated from the canonical
//! history, so this is a *second* projection with its own cache key and
//! its own fallback to canonical history.

use super::context::compaction_parts;
use super::*;

impl Session {
    pub(super) fn active_branch_entries(&self) -> Result<Vec<&Entry>, SessionError> {
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
    /// fallback when any assistant lacks one or belongs to another route.
    /// Opaque output is never replayed on a different endpoint/model.
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
                    EntryValue::Compaction { .. }
                        | EntryValue::ResponsesCompaction { .. }
                        | EntryValue::ResponsesReasoning { .. }
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

    pub(super) fn rebuild_responses_replay(
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
                        snapcompact,
                        first_kept,
                        ..
                    } => Some((index, summary, snapcompact, first_kept)),
                    _ => None,
                });
        let local_marker_index = local_compaction.map(|(index, _, _, _)| index);
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
        } else if let Some((_marker_index, summary, snapcompact, first_kept)) = local_compaction {
            let first_kept_index = branch
                .iter()
                .position(|entry| &entry.id == first_kept)
                .ok_or_else(|| SessionError::UnknownEntry(first_kept.clone()))?;
            replay.push(octet_ai::responses::ResponsesReplayItem::User(
                UserMessage {
                    content: compaction_parts(summary, snapcompact.as_ref()),
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
                EntryValue::ResponsesReasoning {
                    endpoint: recorded_endpoint,
                    model: recorded_model,
                    update,
                    ..
                } => {
                    if recorded_endpoint != endpoint || recorded_model != model {
                        return Ok(false);
                    }
                    if update.is_none() {
                        // A host baseline reset supersedes prior effort updates,
                        // while retaining every conversation/opaque output item.
                        replay.retain(|item| {
                            !matches!(item, octet_ai::ResponsesReplayItem::ConfigurationUpdate(_))
                        });
                    }
                    if let Some(update) = update {
                        // Only an undispatched tail can be adjacent: a completed
                        // response inserts its opaque output between updates.
                        if matches!(
                            replay.last(),
                            Some(octet_ai::ResponsesReplayItem::ConfigurationUpdate(_))
                        ) {
                            replay.pop();
                        }
                        replay.push(octet_ai::ResponsesReplayItem::ConfigurationUpdate(
                            update.clone(),
                        ));
                    }
                }
                EntryValue::BranchSummary { summary, .. } => {
                    let Message::User(user) = super::context::branch_summary_message(summary)
                    else {
                        unreachable!("branch summary projects as a user message")
                    };
                    replay.push(octet_ai::responses::ResponsesReplayItem::User(user));
                }
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
                        return Ok(false);
                    }
                    if output.items().iter().any(|item| {
                        item.as_json()
                            .get("type")
                            .and_then(serde_json::Value::as_str)
                            == Some("configuration_update")
                    }) {
                        return Err(SessionError::InvalidResponsesSidecar(
                            "provider output cannot authorize configuration updates".into(),
                        ));
                    }
                    replay.push(octet_ai::responses::ResponsesReplayItem::Output(
                        output.clone(),
                    ));
                }
                EntryValue::Compaction { .. }
                | EntryValue::ResponsesTurn { .. }
                | EntryValue::ResponsesCompaction { .. }
                | EntryValue::ResponsesSteering { .. }
                | EntryValue::Config { .. }
                | EntryValue::PromptTemplateSelected { .. }
                | EntryValue::SkillActivated { .. }
                | EntryValue::SkillResourceRead { .. }
                | EntryValue::SkillDeactivated { .. } => {}
            }
        }
        Ok(true)
    }

    /// Pinned and effective reasoning for the current route-affine replay window.
    /// Successful local compaction rebases the removed prefix, while retained
    /// updates keep their original positions. A different route ends the prior
    /// reasoning segment; fork/checkout use the selected branch.
    pub fn responses_reasoning(
        &self,
        endpoint: &EndpointId,
        model: &ModelId,
    ) -> Result<Option<(octet_ai::ReasoningConfig, octet_ai::ReasoningConfig)>, SessionError> {
        let branch = self.active_branch_entries()?;
        let kept = branch.iter().rev().find_map(|entry| match &entry.value {
            EntryValue::Compaction { first_kept, .. } => Some(first_kept),
            _ => None,
        });
        let start = kept
            .and_then(|id| branch.iter().position(|entry| &entry.id == id))
            .unwrap_or(0);
        let mut state = None;
        for (index, entry) in branch.iter().enumerate() {
            if let EntryValue::Config {
                model: Some(selected),
                ..
            } = &entry.value
            {
                if selected != &model.0 {
                    state = None;
                }
            }
            if let EntryValue::ResponsesReasoning {
                endpoint: recorded_endpoint,
                model: recorded_model,
                baseline,
                update,
            } = &entry.value
            {
                if recorded_endpoint != endpoint || recorded_model != model {
                    state = None;
                    continue;
                }
                if update.is_none() {
                    state = Some((baseline.clone(), baseline.clone()));
                }
                let (pin, effective) =
                    state.get_or_insert_with(|| (baseline.clone(), baseline.clone()));
                if let Some(update) = update {
                    *effective = update.reasoning.clone();
                }
                if index < start {
                    *pin = effective.clone();
                }
            }
        }
        Ok(state)
    }
}
