//! Process-backed canonical projection with one authoritative Session consumer.
use super::session_leaf::SessionLeafProcessLease;
use super::*;
use crate::extension::{
    ProviderContextHook, ProviderContextHookError, ProviderContextProjection,
    ProviderContextProjectionContext, ProviderContextSessionWait,
};
use crate::session::Session;
use crate::session_leaf::{SessionLeafBinding, SessionLeafConsumer};

static NEXT_CONTEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);

fn binding_activation_refusal(error: ExtensionRuntimeError) -> String {
    match error {
        ExtensionRuntimeError::Closed(_) => "session_append_process_retired",
        ExtensionRuntimeError::Protocol(ref message)
            if message == "private session leaf activation already bound" =>
        {
            "session_append_lease_busy"
        }
        _ => "session_append_authority_unavailable",
    }
    .to_owned()
}

fn snapshot_activation_refusal(error: ExtensionRuntimeError) -> String {
    match error {
        ExtensionRuntimeError::Closed(_) => "session_append_process_retired",
        ExtensionRuntimeError::MessageTooLarge { .. } => "session_snapshot_too_large",
        ExtensionRuntimeError::Protocol(ref message)
            if matches!(
                message.as_str(),
                "session snapshot exceeds wire bound; no entries truncated"
                    | "session snapshot exceeds entry bound; no entries truncated"
                    | "session metadata exceeds namespace limit"
                    | "session metadata exceeds durable bounds"
            ) =>
        {
            "session_snapshot_too_large"
        }
        ExtensionRuntimeError::Protocol(ref message)
            if message == "session snapshot owner changed" =>
        {
            "session_append_authority_unavailable"
        }
        _ => "session_snapshot_unavailable",
    }
    .to_owned()
}

/// Classify one failed process hook round trip.
///
/// Pi isolates a handler that fails on its own terms and keeps the request, so
/// those cases skip with a notice. Everything else is fail-closed: a host-owner
/// refusal, a session-lease check, and any error this function does not
/// explicitly recognize. An unrecognized native error must never become a
/// skippable fault, because the host cannot prove nothing authoritative was
/// refused first. Reasons stay finite and payload-free.
fn hook_failure(error: ExtensionRuntimeError) -> ProviderContextHookError {
    match error {
        ExtensionRuntimeError::Remote { code: -32800, .. }
        | ExtensionRuntimeError::Cancelled { .. } => ProviderContextHookError::Cancelled,
        // Host-owner refusal (`not_foreground_owner` and its siblings).
        ExtensionRuntimeError::Remote { code: -32002, .. } => ProviderContextHookError::Refused,
        // The handler answered for itself: method-not-found, invalid params, a
        // server error, or a transport timeout while it was running.
        ExtensionRuntimeError::Remote { .. } | ExtensionRuntimeError::Timeout { .. } => {
            ProviderContextHookError::Failed("process context hook failed".into())
        }
        ExtensionRuntimeError::Closed(_) => {
            ProviderContextHookError::Failed("process context hook closed".into())
        }
        // Native protocol/authority failures, including the session-lease
        // hook-authority check, are never a handler's own answer.
        _ => ProviderContextHookError::Refused,
    }
}

struct ProcessContextWait {
    process: ExtensionProcess,
    consumer: SessionLeafConsumer,
    binding: SessionLeafBinding,
    preparation: ProviderContextProjectionContext,
    lease: Option<SessionLeafProcessLease>,
}

impl ProviderContextSessionWait for ProcessContextWait {
    fn projection_future(
        &mut self,
        request: &octet_ai::Request,
        context: &ProviderContextProjectionContext,
    ) -> Option<
        std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            Option<ProviderContextProjection>,
                            ProviderContextHookError,
                        >,
                    > + Send
                    + 'static,
            >,
        >,
    > {
        let lease = self.lease.take().expect("one future per preparation guard");
        let payload = serde_json::json!({
            "request": request,
            "preparation": {
                "resource_owner": context.resource_owner,
                "session_id": context.session_id,
                "head": context.head,
                "tool_generation": context.tool_generation,
            },
        });
        let mut execution = self.process.execution_context();
        execution.resource_owner = Some(self.binding.owner.clone());
        execution.host.session_id = Some(context.session_id.clone());
        let future = lease.run_hook(ExtensionHook::ProviderContext, payload, execution);
        Some(Box::pin(async move {
            let output = future.await.map_err(hook_failure)?;
            if output.disposition != ExtensionHookDisposition::Continue {
                return Err(ProviderContextHookError::Failed(
                    "process context hook declined".into(),
                ));
            }
            output
                .provider_context
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| {
                    ProviderContextHookError::Failed(
                        "process context hook returned an invalid projection".into(),
                    )
                })
        }))
    }

    fn ready(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let ready = self.consumer.ready();
        // Terminal readiness wakes the owner once; consume_next refuses it,
        // rather than spinning or starting another hook on a closed lane.
        Box::pin(async move {
            let _ = ready.await;
        })
    }

    fn consume_next(
        &mut self,
        session: &mut Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<(), String> {
        let current = self
            .process
            .current_context_for_resource_owner(session.resource_owner_key());
        if context != &self.preparation
            || current.resource_owner.as_ref() != Some(&self.binding.owner)
        {
            self.consumer.revoker().revoke();
            return Err("process context session authority changed".into());
        }
        match super::session_leaf::consume_process_append(
            &self.process,
            &mut self.consumer,
            session,
            &self.binding,
        ) {
            Ok(true) => Ok(()),
            Ok(false) => Err("process context session lane closed".into()),
            Err(_) => Err("process context session append refused".into()),
        }
    }
}

#[async_trait::async_trait]
impl ProviderContextHook for ExtensionProcess {
    fn continues_on_failure(&self) -> bool {
        true
    }

    fn failure_notice_id(&self) -> Option<&str> {
        Some(&self.inner.instance_id)
    }

    fn failure_notice_name(&self) -> Option<&str> {
        Some(&self.inner.descriptor.manifest.name)
    }

    fn begin_session_wait(
        &self,
        session: &Session,
        context: &ProviderContextProjectionContext,
    ) -> Result<Option<Box<dyn ProviderContextSessionWait>>, String> {
        if self.api_version() != EXTENSION_API_VERSION_0_4
            || !self.supports_feature(EXTENSION_FEATURE_SESSION_ENTRIES)
            || !self
                .inner
                .contributions
                .hooks
                .contains(&ExtensionHook::ProviderContext)
            || context.resource_owner != session.resource_owner_key()
        {
            return Err("session_append_authority_unavailable".into());
        }
        let execution = self.current_context_for_resource_owner(session.resource_owner_key());
        let owner = execution
            .resource_owner
            .expect("current process context has an owner");
        let epoch = NEXT_CONTEXT_ACTIVATION.fetch_add(1, Ordering::Relaxed);
        let binding = SessionLeafBinding {
            activation_epoch: epoch,
            owner,
            namespace: self.descriptor().manifest.name.clone(),
            operation_id: format!("provider-context:{epoch}"),
        };
        let (consumer, producer, grant) = SessionLeafConsumer::new(session, binding.clone())
            .map_err(|_| "session_append_authority_unavailable".to_owned())?;
        let lease = self
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .map_err(binding_activation_refusal)?
            .with_session_snapshot(session)
            .map_err(snapshot_activation_refusal)?;
        Ok(Some(Box::new(ProcessContextWait {
            process: self.clone(),
            consumer,
            binding,
            preparation: context.clone(),
            lease: Some(lease),
        })))
    }

    async fn project_context(
        &self,
        _: &octet_ai::Request,
        _: &ProviderContextProjectionContext,
    ) -> Result<Option<ProviderContextProjection>, String> {
        // Direct process projection has no authoritative consumer. Only the
        // owning driver's guard-bound, generation-pinned future may dispatch it.
        Err("process context hook requires the owning Session driver".into())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod large_history_tests;
#[cfg(test)]
mod recovery_tests;
