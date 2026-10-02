//! Request-scoped tool composition, sharing direct-call admission and hooks.

use super::*;
use crate::tool_composition::{
    callable_catalog, restore_store, validate_store_writes, ToolCompositionRecord,
    ToolCompositionService, COMPOSITION_TIMEOUT_MS, MAX_COMPOSITION_CALLS,
    MAX_COMPOSITION_STORE_BYTES,
};
use serde_json::{Map, Value};
use std::sync::atomic::AtomicUsize;
use std::sync::OnceLock;
use tokio::sync::{RwLock, Semaphore};

static NEXT_COMPOSITION_SCOPE: AtomicU64 = AtomicU64::new(1);

/// Capability lifetime is strictly shorter than the outer model tool call.
/// Dropping the outer operation cancels executing/queued nested calls too.
pub(super) struct CompositionScope(pub(super) Arc<CompositionDispatcher>);

impl Drop for CompositionScope {
    fn drop(&mut self) {
        self.0.stop.cancel();
    }
}

pub(super) struct CompositionDispatcher {
    parent_id: octet_ai::ToolCallId,
    nested_prefix: String,
    tool_name: String,
    tools: Vec<Arc<dyn Tool>>,
    definitions: Vec<ToolDef>,
    sandbox: SandboxConfig,
    tool_scope: String,
    resource_owner: String,
    run_id: String,
    generation: u64,
    active_skills: Vec<crate::session::SkillActivatedSnapshot>,
    registered_tools: Vec<String>,
    hooks: Vec<Arc<dyn ToolCallHook>>,
    broker: EffectBroker,
    progress: ToolProgressSink,
    cancellation: CancellationToken,
    pub(super) stop: CancellationToken,
    deadline: OnceLock<tokio::time::Instant>,
    calls: AtomicUsize,
    slots: Semaphore,
    effects: RwLock<()>,
    store: tokio::sync::Mutex<Map<String, Value>>,
    store_committed: AtomicBool,
    usage: Mutex<Usage>,
    remaining_tokens: Option<u64>,
    remaining_cost: Option<u64>,
}

impl CompositionDispatcher {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn scope(
        parent_id: octet_ai::ToolCallId,
        tool_name: String,
        tools: Vec<Arc<dyn Tool>>,
        sandbox: SandboxConfig,
        tool_scope: String,
        resource_owner: String,
        run_id: String,
        generation: u64,
        active_skills: Vec<crate::session::SkillActivatedSnapshot>,
        hooks: Vec<Arc<dyn ToolCallHook>>,
        broker: EffectBroker,
        progress: ToolProgressSink,
        cancellation: CancellationToken,
        session: &Session,
        _model: Model,
        max_session_tokens: Option<u64>,
        max_session_cost: Option<u64>,
    ) -> CompositionScope {
        let nested_prefix = format!(
            "nested:{}:{}",
            content_hash(format!("{resource_owner}\0{run_id}\0{}", parent_id.0).as_bytes()),
            NEXT_COMPOSITION_SCOPE.fetch_add(1, Ordering::Relaxed)
        );
        let store = restore_store(session, &tool_name);
        let tools = tools
            .into_iter()
            .filter(|tool| tool.composition_config().is_none())
            .collect::<Vec<_>>();
        let definitions = tools
            .iter()
            .map(|tool| tool.definition())
            .collect::<Vec<_>>();
        let registered_tools = definitions.iter().map(|tool| tool.name.clone()).collect();
        CompositionScope(Arc::new(Self {
            parent_id,
            nested_prefix,
            tool_name,
            tools,
            definitions,
            sandbox,
            tool_scope,
            resource_owner,
            run_id,
            generation,
            active_skills,
            registered_tools,
            hooks,
            broker,
            progress,
            cancellation,
            stop: CancellationToken::default(),
            deadline: OnceLock::new(),
            calls: AtomicUsize::new(0),
            slots: Semaphore::new(4),
            effects: RwLock::new(()),
            store: tokio::sync::Mutex::new(store),
            store_committed: AtomicBool::new(false),
            usage: Mutex::new(Usage::default()),
            remaining_tokens: max_session_tokens
                .map(|limit| limit.saturating_sub(session_total_tokens_for_own_context(session))),
            remaining_cost: max_session_cost
                .map(|limit| limit.saturating_sub(session.total_cost_microdollars())),
        }))
    }

    fn deadline(&self) -> tokio::time::Instant {
        *self.deadline.get_or_init(|| {
            tokio::time::Instant::now() + std::time::Duration::from_millis(COMPOSITION_TIMEOUT_MS)
        })
    }

    fn ensure_live(&self) -> Result<(), ToolError> {
        if self.cancellation.is_cancelled() || self.stop.is_cancelled() {
            return Err(cancelled_tool_error());
        }
        if tokio::time::Instant::now() >= self.deadline() {
            return Err(ToolError::new(
                "composition exceeded the 30 second host deadline",
            ));
        }
        let usage = self
            .usage
            .lock()
            .expect("composition usage is not poisoned");
        if self
            .remaining_tokens
            .is_some_and(|limit| usage_total_tokens(&usage) >= limit)
        {
            return Err(ToolError::new(
                "composition exhausted the session token budget",
            ));
        }
        if self
            .remaining_cost
            .is_some_and(|limit| limit == 0 || usage_total_tokens(&usage) > 0)
        {
            return Err(ToolError::new(
                "composition cannot admit further calls with exhausted or unpriced session cost exposure",
            ));
        }
        Ok(())
    }

    pub(super) fn collect_usage(
        &self,
        result: Result<ToolOutput, ToolError>,
    ) -> Result<ToolOutput, ToolError> {
        let mut usage = *self
            .usage
            .lock()
            .expect("composition usage is not poisoned");
        if usage_total_tokens(&usage) == 0 {
            return result;
        }
        match result {
            Ok(output) => {
                if let Some(own) = output.usage() {
                    add_usage(&mut usage, own);
                }
                Ok(output.with_usage(usage))
            }
            // Usage is still billable when a script throws or times out.
            Err(error) => Ok(ToolOutput::new(error.message)
                .with_is_error(true)
                .with_usage(usage)),
        }
    }

    async fn record(&self, record: ToolCompositionRecord) -> Result<(), ToolError> {
        if !record.valid() {
            return Err(ToolError::new("invalid composition journal record"));
        }
        self.progress
            .append_metadata(EntryMetadata {
                tool_composition: Some(record),
                ..EntryMetadata::default()
            })
            .await
            .map(|_| ())
    }

    async fn relay(&self, progress: ToolProgress) {
        match progress {
            // Raw nested outputs stay in guest memory unless text()/image()
            // publishes them. In particular Bash streams must not leak through
            // the outer live-output panel or its durable checkpoint.
            ToolProgress::Output { .. }
            | ToolProgress::Decoration(_)
            | ToolProgress::Dropped { .. } => {}
            ToolProgress::Status(status) => self.progress.status(status),
            progress => {
                self.progress.forward_semantic(progress).await;
            }
        }
    }

    async fn dispatch(
        &self,
        tool: &dyn Tool,
        name: &str,
        arguments: Value,
        id: &octet_ai::ToolCallId,
        context: &ToolContext<'_>,
    ) -> (Result<ToolOutput, ToolError>, Option<ToolPolicyDecision>) {
        let admission = reserve_tool_effect(
            &self.broker,
            tool,
            name,
            &arguments,
            context,
            &self.resource_owner,
            &self.run_id,
            self.generation,
            id,
            true,
        )
        .await;
        let ToolEffectAdmission {
            intent,
            reservation,
            effect,
        } = match admission {
            Ok(admission) => admission,
            Err(ToolEffectAdmissionError { error, decision }) => {
                return (Err(error), Some(decision));
            }
        };
        for hook in &self.hooks {
            if hook
                .before_tool_call(name, &arguments, context)
                .await
                .is_err()
            {
                let (error, decision) =
                    secondary_hook_denial(&self.sandbox, &self.broker, Some(effect));
                return (Err(error), Some(decision));
            }
        }
        if context.cancellation.is_cancelled() {
            return (Err(cancelled_tool_error()), None);
        }
        let receipt = match reservation.commit(&intent) {
            Ok(receipt) => receipt,
            Err(error) => {
                let (error, decision) =
                    effect_reservation_commit_denial(&self.sandbox, &self.broker, effect, &error);
                return (Err(error), Some(decision));
            }
        };
        let mut decision = Some(policy_decision(
            &self.sandbox,
            &self.broker,
            Some(effect),
            Some(receipt.authorization()),
            None,
        ));
        let result = tool.execute(arguments.clone(), context).await;
        if let Ok(output) = &result {
            if let Some(usage) = output.usage() {
                add_usage(
                    &mut self
                        .usage
                        .lock()
                        .expect("composition usage is not poisoned"),
                    usage,
                );
            }
        }
        let (text, is_error) = match &result {
            Ok(output) => (output.text.as_str(), output.is_error()),
            Err(error) => (error.message.as_str(), true),
        };
        for hook in &self.hooks {
            hook.after_tool_call(name, &arguments, text, is_error, context)
                .await;
        }
        apply_execution_policy_denial(&mut decision, &result);
        (result, decision)
    }

    async fn execute_call(
        &self,
        name: String,
        arguments: Value,
        index: usize,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError> {
        self.ensure_live()?;
        let tool = self
            .tools
            .iter()
            .find(|tool| tool.definition().name == name)
            .ok_or_else(|| ToolError::new(format!("unknown or unavailable nested tool: {name}")))?;
        // The same validator used by provider normalization; no parse/repair
        // fallback, no hand-written schema subset, and no hook before validation.
        match octet_ai::validate_tool_arguments(&name, &arguments, &self.definitions)
            .map_err(|_| ToolError::new("nested tool argument validation failed"))?
        {
            octet_ai::ToolArgumentValidation::Valid => {}
            _ => return Err(ToolError::new(SCHEMA_MISMATCH_TOOL_ERROR)),
        }
        if (self.remaining_tokens.is_some() || self.remaining_cost.is_some())
            && !tool.composition_is_unmetered()
        {
            return Err(ToolError::new(
                "nested tool cannot be admitted under hard session ceilings without an authoritative pre-execution usage and pricing bound",
            ));
        }
        let _slot = self
            .slots
            .acquire()
            .await
            .expect("composition slots stay open");
        self.ensure_live()?;
        let (tx, mut rx) = mpsc::channel(PROGRESS_CHANNEL_CAPACITY);
        let progress = ToolProgressSink::live(tx).for_nested_call();
        let context = ToolContext {
            workspace: &self.sandbox.workspace,
            sandbox: &self.sandbox,
            execution_scope: &self.tool_scope,
            resource_owner: &self.resource_owner,
            active_skills: &self.active_skills,
            registered_tools: &self.registered_tools,
            progress,
            cancellation,
        };
        // Fair reader/writer admission: at most four declared safe reads;
        // every mutation/extension/unknown effect excludes all other calls.
        // Reclassification and broker reservation still happen at dispatch.
        let parallel = tool.concurrency() == ToolConcurrency::Parallel
            && tool
                .effect(&arguments, &context)
                .is_ok_and(effect_is_parallel_observation);
        let read_guard;
        let write_guard;
        if parallel {
            read_guard = Some(self.effects.read().await);
            write_guard = None;
        } else {
            write_guard = Some(self.effects.write().await);
            read_guard = None;
        }
        self.ensure_live()?;
        let id = octet_ai::ToolCallId(format!("{}:{index}", self.nested_prefix));
        self.record(ToolCompositionRecord::CallStarted {
            parent: self.parent_id.0.clone(),
            id: id.0.clone(),
            tool: name.clone(),
            arguments_hash: content_hash(
                &serde_json::to_vec(&arguments)
                    .map_err(|error| ToolError::new(error.to_string()))?,
            ),
        })
        .await?;
        let start = std::time::Instant::now();
        self.progress.status(format!(
            "Nested tool {index}/{MAX_COMPOSITION_CALLS}: {name}"
        ));
        let operation = self.dispatch(tool.as_ref(), &name, arguments, &id, &context);
        tokio::pin!(operation);
        let (result, decision) = loop {
            tokio::select! {
                outcome = &mut operation => break outcome,
                progress = rx.recv() => if let Some(progress) = progress { self.relay(progress).await; },
            }
        };
        while let Ok(progress) = rx.try_recv() {
            self.relay(progress).await;
        }
        let delivery_limit = self.sandbox.max_output_bytes.min(1024 * 1024);
        let oversized_delivery = result.as_ref().ok().is_some_and(|output| {
            output.has_delivery_commit() && output.text.len() > delivery_limit
        });
        self.record(ToolCompositionRecord::CallFinished {
            parent: self.parent_id.0.clone(),
            id: id.0.clone(),
            tool: name.clone(),
            ok: !tool_execution_failed(&result) && !oversized_delivery,
            duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            effect: decision.as_ref().and_then(|decision| decision.effect),
            allowed: decision.as_ref().is_some_and(|decision| decision.allowed),
            denial_code: decision.as_ref().and_then(|decision| decision.denial_code),
            // Only durability-acknowledged tools need their exact result stored.
            delivery_text: result
                .as_ref()
                .ok()
                .filter(|output| {
                    output.has_delivery_commit() && output.text.len() <= delivery_limit
                })
                .map(|output| output.text.clone()),
        })
        .await?;
        resolve_tool_delivery_after_persistence(&result, delivery_limit);
        drop((read_guard, write_guard));
        if oversized_delivery {
            return Err(ToolError::new(
                "nested durable-delivery result exceeds its private receipt limit",
            ));
        }
        let output = result?;
        if output.is_error() {
            return Err(ToolError::new(output.text));
        }
        let value = if tool.output_schema().is_some() {
            output
                .programmatic_content()
                .or_else(|| output.structured_content())
                .cloned()
        } else {
            None
        };
        // A declared schema may legitimately include a text/media-summary
        // alternative (the core read tool does). Never parse arbitrary text as
        // JSON and never publish nested media implicitly.
        Ok(value.unwrap_or(Value::String(output.text)))
    }
}

struct CallCancellation(CancellationToken);
impl Drop for CallCancellation {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[async_trait::async_trait]
impl ToolCompositionService for CompositionDispatcher {
    async fn context(&self) -> Result<Value, ToolError> {
        self.ensure_live()?;
        Ok(serde_json::json!({
            "tools":callable_catalog(&self.tools), "store":*self.store.lock().await,
            "limits":{"timeout_ms":COMPOSITION_TIMEOUT_MS,"max_calls":MAX_COMPOSITION_CALLS},
        }))
    }

    async fn call(
        &self,
        name: String,
        arguments: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, ToolError> {
        let index = self.calls.fetch_add(1, Ordering::AcqRel) + 1;
        if index > MAX_COMPOSITION_CALLS {
            return Err(ToolError::new("composition exceeded 256 nested calls"));
        }
        let local = CallCancellation(CancellationToken::default());
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => Err(cancelled_tool_error()),
            _ = self.stop.cancelled() => Err(cancelled_tool_error()),
            _ = cancellation.cancelled() => Err(cancelled_tool_error()),
            _ = tokio::time::sleep_until(self.deadline()) => Err(ToolError::new("composition exceeded the 30 second host deadline")),
            result = self.execute_call(name, arguments, index, local.0.clone()) => result,
        }
    }

    async fn store(&self, set: Map<String, Value>, delete: Vec<String>) -> Result<(), ToolError> {
        self.ensure_live()?;
        validate_store_writes(&set, &delete)?;
        if self.store_committed.swap(true, Ordering::AcqRel) {
            return Err(ToolError::new(
                "composition store already committed for this request",
            ));
        }
        let mut current = self.store.lock().await;
        let mut next = current.clone();
        for key in &delete {
            next.remove(key);
        }
        next.extend(set.clone());
        crate::tool::validate_tool_detail(
            "composition store",
            &Value::Object(next.clone()),
            MAX_COMPOSITION_STORE_BYTES,
            false,
        )
        .map_err(|error| ToolError::new(error.to_string()))?;
        // Sidecar append is synced and acknowledged by the owning run. A
        // cancellation that wins before acceptance discards the writes.
        self.record(ToolCompositionRecord::Store {
            tool: self.tool_name.clone(),
            set,
            delete,
        })
        .await?;
        *current = next;
        Ok(())
    }
}
