//! The usage ledger: per-operation provider usage records, the picodollar
//! carry between operations, session totals, delegated runs, cache-warm
//! lifecycle and the uncertain-usage ledger.
//!
//! Separate from `entries` because cost accounting has its own durability
//! rule: an entry may be written while the usage record for it is still
//! uncertain, and the ledger is session-global across branches while the
//! entry tree is not.

use super::*;

impl Session {
    /// Provider usage records in append order, across all preserved branches.
    ///
    /// Assistant-turn records point at their exact durable assistant entry,
    /// unlike checkpoint usage which is aggregated for a whole user prompt.
    pub fn usage_records(&self) -> &[UsageRecord] {
        &self.usage_records
    }

    /// The one `UsageRecord` shape every `record_*_usage` entry point builds:
    /// a provider call charged to this session, attributed to one endpoint and
    /// model, stamped at completion, carrying the exact microdollar total
    /// derived from its own picodollar cost. Only `kind` — and `stop_reason`,
    /// which only assistant turns can supply — tells an assistant turn from a
    /// cache warm, a compaction, a rejected Responses turn, a terminal gate or
    /// a delegated child. The two session-total fields stay `None` because
    /// `record_usage` is the single owner of the picodollar carry.
    fn provider_usage_record(
        kind: UsageRecordKind,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
        stop_reason: Option<StopReason>,
    ) -> UsageRecord {
        UsageRecord {
            kind,
            usage,
            stop_reason,
            endpoint: Some(endpoint),
            model: Some(model),
            completed_at_unix_ms: Some(now_unix_millis()),
            cost,
            cost_microdollars: cost.map(|cost| cost.total),
            session_cost_microdollars: None,
            session_cost_picodollars_remainder: None,
        }
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
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::AssistantTurn { assistant },
            endpoint,
            model,
            usage,
            cost,
            stop_reason,
        ))
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
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::DelegatedAgent {
                agent_id,
                turn_count,
                tool_call_count,
            },
            endpoint,
            model,
            usage,
            cost,
            None,
        ))
    }

    /// Persist provider-reported usage for one completed cache-warm call.
    /// It is charged to the session but never enters the assistant-turn cache
    /// hit-rate denominator or the model-visible conversation.
    pub(crate) fn record_cache_warm_usage(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::CacheWarm,
            endpoint,
            model,
            usage,
            cost,
            None,
        ))
    }

    /// Append a sanitized cache-warm lifecycle transition. A started attempt
    /// without a terminal transition is conservatively usage-uncertain on resume.
    pub(crate) fn record_cache_warm_status(
        &mut self,
        record: CacheWarmRecord,
    ) -> Result<(), SessionError> {
        let identifiers = UsageUncertaintyRecord {
            endpoint: record.endpoint.clone(),
            model: record.model.clone(),
            operation: "cache_warm".into(),
        };
        identifiers.validate()?;
        let valid = record.attempt > 0
            && match record.state {
                CacheWarmState::Started => {
                    self.cache_warm_records
                        .last()
                        .map_or(record.attempt == 1, |last| {
                            last.attempt.checked_add(1) == Some(record.attempt)
                                && last.state != CacheWarmState::Started
                        })
                }
                _ => self.cache_warm_records.last().is_some_and(|last| {
                    last.attempt == record.attempt
                        && last.state == CacheWarmState::Started
                        && last.endpoint == record.endpoint
                        && last.model == record.model
                        && last.anchor == record.anchor
                        && last.extension_override == record.extension_override
                }),
            };
        if !valid {
            return Err(SessionError::Limit("invalid cache-warm lifecycle".into()));
        }
        let mut buffer = Vec::with_capacity(180);
        write_json_line(
            &mut buffer,
            &SessionRecordRef::CacheWarm { record: &record },
        )?;
        self.persist(&buffer)?;
        self.cache_warm_records.push(record);
        Ok(())
    }

    /// Session-global cache-warm statuses, including abandoned branches.
    pub fn cache_warm_records(&self) -> &[CacheWarmRecord] {
        &self.cache_warm_records
    }

    /// Persist usage for a context-compaction provider call.
    pub fn record_compaction_usage(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        usage: Usage,
        cost: Option<Cost>,
    ) -> Result<(), SessionError> {
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::Compaction,
            endpoint,
            model,
            usage,
            cost,
            None,
        ))
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
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::RejectedResponsesTurn,
            endpoint,
            model,
            usage,
            cost,
            None,
        ))
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
        self.record_usage(Self::provider_usage_record(
            UsageRecordKind::TerminalGate { returned },
            endpoint,
            model,
            usage,
            cost,
            None,
        ))
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
        self.record_usage_uncertainty_with_bound(endpoint, model, operation, None)
    }

    /// Persist an accepted attempt's unknown usage with its conservative
    /// admission exposure. An absent bound continues to fail hard ceilings closed.
    pub fn record_usage_uncertainty_with_bound(
        &mut self,
        endpoint: EndpointId,
        model: ModelId,
        operation: impl Into<String>,
        bound: Option<UsageUncertaintyBound>,
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
            &SessionRecordRef::UsageUncertainty {
                record: &record,
                bound,
            },
        )?;
        self.persist(&buffer)?;
        self.usage_uncertainty_records.push(record);
        self.usage_uncertainty_bounds.push(bound);
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
        bound: Option<UsageUncertaintyBound>,
    ) -> Result<(), SessionError> {
        let result = self.record_usage_uncertainty_with_bound(
            endpoint.clone(),
            model.clone(),
            operation,
            bound,
        );
        if result.is_err() {
            self.usage_uncertainty_records.push(UsageUncertaintyRecord {
                endpoint,
                model,
                operation: operation.to_owned(),
            });
            self.usage_uncertainty_bounds.push(bound);
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

    /// Conservative total admission exposure for all uncertain attempts.
    /// None means a bound is missing, native steering is unsettled, or a cache
    /// warm attempt is still in flight. Cost stays unavailable if any route was
    /// unpriced, even if its token bound is known.
    pub fn usage_uncertainty_exposure(&self) -> Option<UsageUncertaintyBound> {
        if self.has_unsettled_native_steering()
            || self
                .cache_warm_records
                .last()
                .is_some_and(|record| record.state == CacheWarmState::Started)
        {
            return None;
        }
        self.usage_uncertainty_bounds.iter().try_fold(
            UsageUncertaintyBound {
                tokens: 0,
                cost_microdollars: Some(0),
            },
            |total, bound| {
                let bound = bound.as_ref()?;
                Some(UsageUncertaintyBound {
                    tokens: total.tokens.saturating_add(bound.tokens),
                    cost_microdollars: total
                        .cost_microdollars
                        .zip(bound.cost_microdollars)
                        .map(|(left, right)| left.saturating_add(right)),
                })
            },
        )
    }

    /// Whether any durable accepted-attempt usage is unknown, on any branch.
    /// Known usage/cost totals are only subtotals while this is true. Hard
    /// cumulative ceilings fail closed only when admission exposure lacks a bound.
    pub fn has_uncertain_usage(&self) -> bool {
        !self.usage_uncertainty_records.is_empty()
            || self.has_unsettled_native_steering()
            || self
                .cache_warm_records
                .last()
                .is_some_and(|record| record.state == CacheWarmState::Started)
    }

    /// A durable native intent without a completed, accounted successor cannot
    /// be blindly resumed after a process interruption. Preparation may precede
    /// actual dispatch; uncertainty deliberately fails closed at that gap.
    pub fn has_unsettled_native_steering(&self) -> bool {
        let mut pending = std::collections::HashSet::new();
        for entry in &self.entries {
            if let EntryValue::ResponsesSteering {
                operation,
                local_id,
                input,
                completed,
                ..
            } = &entry.value
            {
                let key = (operation.as_str(), *local_id);
                if input.is_some() {
                    pending.insert(key);
                }
                if completed.is_some() {
                    pending.remove(&key);
                }
            }
        }
        !pending.is_empty()
    }

    /// Admission bounds alongside uncertainty evidence, in append order.
    pub(crate) fn usage_uncertainty_bounds(&self) -> &[Option<UsageUncertaintyBound>] {
        &self.usage_uncertainty_bounds
    }

    /// Unknown-usage evidence in append order, independent of the active head.
    /// These records are not usage totals.
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
        self.write_head_record(
            &mut buf,
            self.head.as_ref().expect("head exists after first append"),
            &new_total,
        )?;
        self.persist(&buf)?;
        self.total_cost_microdollars = new_total;
        Ok(())
    }
}
