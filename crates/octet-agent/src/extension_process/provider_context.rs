//! Process-backed canonical projection with one authoritative Session consumer.
use super::session_leaf::SessionLeafProcessLease;
use super::*;
use crate::extension::{
    ProviderContextHook, ProviderContextProjection, ProviderContextProjectionContext,
    ProviderContextSessionWait,
};
use crate::session::Session;
use crate::session_leaf::{SessionLeafBinding, SessionLeafConsumer};

static NEXT_CONTEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);

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
                dyn std::future::Future<Output = Result<Option<ProviderContextProjection>, String>>
                    + Send
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
        let future = lease.run_hook(ExtensionHook::ProviderContext, payload, execution);
        Some(Box::pin(async move {
            let output = future
                .await
                .map_err(|_| "process context hook failed".to_owned())?;
            if output.disposition != ExtensionHookDisposition::Continue {
                return Err("process context hook refused preparation".into());
            }
            output
                .provider_context
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| "invalid canonical process context projection".into())
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
        match self.consumer.consume_next(session, &self.binding) {
            Ok(true) => Ok(()),
            Ok(false) => Err("process context session lane closed".into()),
            Err(_) => Err("process context session append refused".into()),
        }
    }
}

#[async_trait::async_trait]
impl ProviderContextHook for ExtensionProcess {
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
            return Err("process context preparation authority unavailable".into());
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
            .map_err(|_| "process context session consumer refused".to_owned())?;
        let lease = self
            .bind_session_leaf(producer, consumer.revoker(), grant)
            .map_err(|_| "process context session binding refused".to_owned())?;
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
