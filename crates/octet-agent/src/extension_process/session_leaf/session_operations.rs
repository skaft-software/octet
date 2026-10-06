//! Session-operation callbacks on a pinned process with a live private append lane.
use super::*;
use crate::compaction::{
    SessionOperation, SessionOperationDecision, SessionOperationFuture, SessionOperationHook,
    SessionOperationInvocation,
};
use crate::session::Session;
use crate::session_leaf::SessionLeafConsumer;

static NEXT_SESSION_OPERATION: AtomicU64 = AtomicU64::new(1);

struct ProcessSessionOperation {
    process: ExtensionProcess,
    consumer: SessionLeafConsumer,
    binding: SessionLeafBinding,
    hook: ExtensionHook,
    payload: serde_json::Value,
    lease: Option<SessionLeafProcessLease>,
}

fn validate_session_operation_output(output: &ExtensionHookOutput) -> Result<(), String> {
    if !output.custom_messages.is_empty()
        || output.system_prompt.is_some()
        || output.provider_context.is_some()
        || output.provider_retry.is_some()
        || output.cache_warming_decision.is_some()
        || output.compaction_frames.is_some()
        || output.persistence_metadata.is_some()
        || output.post_mutation.is_some()
        || !output.context.is_empty()
    {
        return Err("unrelated session callback effect".into());
    }
    Ok(())
}

impl SessionOperationInvocation for ProcessSessionOperation {
    fn take_future(&mut self) -> SessionOperationFuture {
        let lease = self
            .lease
            .take()
            .expect("one callback per session activation");
        let process = self.process.clone();
        let mut context = process.execution_context();
        context.resource_owner = Some(self.binding.owner.clone());
        let call = lease.run_hook_value(self.hook, self.payload.clone(), context);
        Box::pin(async move {
            let mut value = call
                .await
                .map_err(|_| "session callback failed".to_owned())?;
            let object = value
                .as_object_mut()
                .ok_or("invalid session callback response")?;
            let decision = object
                .remove("session_operation")
                .map(serde_json::from_value::<SessionOperationDecision>)
                .transpose()
                .map_err(|_| "invalid session decision")?
                .unwrap_or_default();
            // Validate the ordinary envelope too: no unknown fields or ignored
            // replacements from some unrelated hook contract.
            let output: ExtensionHookOutput =
                serde_json::from_value(value).map_err(|_| "invalid session callback envelope")?;
            validate_session_operation_output(&output)?;
            process.publish_hook_output(&output);
            match output.disposition {
                ExtensionHookDisposition::Continue => Ok(decision),
                ExtensionHookDisposition::Deny { .. } => Ok(SessionOperationDecision::Cancel),
            }
        })
    }

    fn ready(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'static>> {
        Box::pin(self.consumer.ready())
    }

    fn consume_next(&mut self, session: &mut Session) -> Result<(), String> {
        self.validate_current(session)?;
        match self.consumer.consume_next(session, &self.binding) {
            Ok(true) => Ok(()),
            _ => Err("session operation append refused".into()),
        }
    }

    fn validate_current(&self, session: &Session) -> Result<(), String> {
        let current = self
            .process
            .current_context_for_resource_owner(session.resource_owner_key());
        let connection = read_std_lock(&self.process.inner.connection);
        if current.resource_owner.as_ref() != Some(&self.binding.owner)
            || connection.closed.load(Ordering::Acquire)
            || connection.draining.load(Ordering::Acquire)
            || connection.generation != self.binding.owner.process_generation
        {
            self.consumer.revoker().revoke();
            return Err("session operation authority changed".into());
        }
        Ok(())
    }
}

impl ExtensionProcess {
    /// Await the requested completion/error callback after compaction and its
    /// after-hooks, with an invocation-local native append consumer.
    pub async fn run_compaction_callback(
        &self,
        session: &mut Session,
        owner: &ExtensionResourceOwner,
        parent_request_id: u64,
        result: &Result<ExtensionSessionCompactionResult, String>,
        cancellation: CancellationToken,
    ) -> Result<(), String> {
        if self
            .current_context_for_resource_owner(session.resource_owner_key())
            .resource_owner
            .as_ref()
            != Some(owner)
        {
            return Err("compaction callback owner retired".into());
        }
        let epoch = NEXT_SESSION_OPERATION.fetch_add(1, Ordering::Relaxed);
        let binding = SessionLeafBinding {
            activation_epoch: epoch,
            owner: owner.clone(),
            namespace: self.descriptor().manifest.name.clone(),
            operation_id: format!("compaction-callback:{epoch}"),
        };
        let (consumer, producer, grant) = SessionLeafConsumer::new(session, binding.clone())
            .map_err(|_| "compaction callback consumer refused")?;
        let lease = self
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .and_then(|lease| lease.with_session_snapshot(session))
            .map_err(|_| "compaction callback binding refused")?;
        let payload = match result {
            Ok(result) => {
                serde_json::json!({"kind":"compaction_callback", "parent_request_id":parent_request_id, "result":result})
            }
            Err(error) => {
                serde_json::json!({"kind":"compaction_callback", "parent_request_id":parent_request_id, "error":error})
            }
        };
        let mut invocation = ProcessSessionOperation {
            process: self.clone(),
            consumer,
            binding,
            hook: ExtensionHook::SessionCompact,
            payload,
            lease: Some(lease),
        };
        let mut future = invocation.take_future();
        loop {
            invocation.validate_current(session)?;
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err("compaction callback cancelled".into()),
                ready = invocation.ready() => {
                    if !ready { return Err("compaction callback consumer retired".into()); }
                    invocation.consume_next(session)?;
                }
                result = &mut future => {
                    let decision = result?;
                    if decision != SessionOperationDecision::Continue {
                        return Err("compaction callback cannot replace committed work".into());
                    }
                    return Ok(());
                }
            }
        }
    }
}

impl SessionOperationHook for ExtensionProcess {
    fn begin(
        &self,
        session: &Session,
        operation: &SessionOperation,
    ) -> Result<Option<Box<dyn SessionOperationInvocation>>, String> {
        let hook = match operation {
            SessionOperation::BeforeCompact { .. } => ExtensionHook::SessionBeforeCompact,
            SessionOperation::Compacted { .. } => ExtensionHook::SessionCompact,
            SessionOperation::BeforeTree { .. } => ExtensionHook::SessionBeforeTree,
            SessionOperation::Tree { .. } => ExtensionHook::SessionTree,
            SessionOperation::ModelTurnStart { .. } => ExtensionHook::ModelTurnStart,
            SessionOperation::ModelTurnEnd { .. } => ExtensionHook::ModelTurnEnd,
        };
        if !self.inner.contributions.hooks.contains(&hook) {
            return Ok(None);
        }
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self.supports_feature(EXTENSION_FEATURE_SESSION_ENTRIES)
        {
            return Err("session operation consumer unavailable".into());
        }
        let context = self.current_context_for_resource_owner(session.resource_owner_key());
        let owner = context
            .resource_owner
            .ok_or("session operation owner unavailable")?;
        let epoch = NEXT_SESSION_OPERATION.fetch_add(1, Ordering::Relaxed);
        let binding = SessionLeafBinding {
            activation_epoch: epoch,
            owner,
            namespace: self.descriptor().manifest.name.clone(),
            operation_id: format!("session-operation:{epoch}"),
        };
        let (consumer, producer, grant) = SessionLeafConsumer::new(session, binding.clone())
            .map_err(|_| "session operation consumer refused")?;
        let lease = self
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .and_then(|lease| lease.with_session_snapshot(session))
            .map_err(|_| "session operation binding refused")?;
        let mut operation = operation.clone();
        match &mut operation {
            SessionOperation::BeforeCompact { branch_entries, .. } => {
                for entry in branch_entries {
                    *entry = session_entry_for_namespace(entry, &binding.namespace)
                        .map_err(|_| "session operation metadata refused")?;
                }
            }
            SessionOperation::Compacted { entry, .. } => {
                *entry = session_entry_for_namespace(entry, &binding.namespace)
                    .map_err(|_| "session operation metadata refused")?;
            }
            SessionOperation::ModelTurnEnd {
                assistant_entry,
                tool_result_entries,
                ..
            } => {
                *assistant_entry = session_entry_for_namespace(assistant_entry, &binding.namespace)
                    .map_err(|_| "session operation metadata refused")?;
                for entry in tool_result_entries {
                    *entry = session_entry_for_namespace(entry, &binding.namespace)
                        .map_err(|_| "session operation metadata refused")?;
                }
            }
            _ => {}
        }
        Ok(Some(Box::new(ProcessSessionOperation {
            process: self.clone(),
            consumer,
            binding,
            hook,
            payload: serde_json::to_value(operation).map_err(|_| "session event unavailable")?,
            lease: Some(lease),
        })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_observations_reject_unapplied_system_prompt_replacement() {
        assert!(validate_session_operation_output(&ExtensionHookOutput::default()).is_ok());
        for prompt in ["", "unapplied replacement"] {
            let output: ExtensionHookOutput =
                serde_json::from_value(serde_json::json!({"system_prompt": prompt})).unwrap();
            assert_eq!(
                validate_session_operation_output(&output),
                Err("unrelated session callback effect".into())
            );
        }
    }
}
