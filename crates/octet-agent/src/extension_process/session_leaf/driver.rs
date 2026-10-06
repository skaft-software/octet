//! Host-created causal route to the sole actively-polled Session writer.
//!
//! This is native execution-local data, never a JSON authority or a frontend
//! dispatcher. Only the owning tool pump constructs it; composition propagates
//! that exact route to nested hooks without extending composition eligibility.
use super::*;
use crate::session_leaf::SessionLeafConsumer;
use std::future::Future;
use std::pin::Pin;

#[derive(Clone)]
pub(crate) struct SessionDriverRoute {
    owner: String,
    sender: mpsc::Sender<DriverRequest>,
}

pub(crate) struct DriverRequest {
    process: ExtensionProcess,
    reply: oneshot::Sender<Result<LinkedSessionInvocation, String>>,
}

pub(crate) struct SessionDriver {
    owner: String,
    receiver: mpsc::Receiver<DriverRequest>,
    activations: Vec<DriverActivation>,
}

struct DriverActivation {
    consumer: SessionLeafConsumer,
    binding: SessionLeafBinding,
    lease: SessionLeafProcessLease,
}

pub(crate) enum DriverEvent {
    Prepare(DriverRequest),
    Append(usize),
}

pub(crate) struct LinkedSessionInvocation {
    process: ExtensionProcess,
    connection: Arc<ProcessConnection>,
    bound: Arc<BoundLeaf>,
    snapshot: InvocationSnapshot,
}

impl SessionDriverRoute {
    pub(crate) async fn prepare(
        &self,
        process: &ExtensionProcess,
        owner: &str,
    ) -> Result<LinkedSessionInvocation, String> {
        if owner != self.owner {
            return Err("session driver owner changed".into());
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(DriverRequest {
                process: process.clone(),
                reply,
            })
            .map_err(|_| "session driver unavailable or saturated".to_owned())?;
        result
            .await
            .map_err(|_| "session driver unavailable".to_owned())?
    }
}

impl SessionDriver {
    pub(crate) fn new(owner: String) -> (Self, SessionDriverRoute) {
        let (sender, receiver) = mpsc::channel(64);
        (
            Self {
                owner: owner.clone(),
                receiver,
                activations: Vec::new(),
            },
            SessionDriverRoute { owner, sender },
        )
    }

    pub(crate) async fn next(&mut self) -> DriverEvent {
        let readiness: Vec<Pin<Box<dyn Future<Output = usize> + Send>>> = self
            .activations
            .iter()
            .enumerate()
            .map(|(index, active)| {
                let ready = active.consumer.ready();
                Box::pin(async move {
                    let _ = ready.await;
                    index
                }) as _
            })
            .collect();
        tokio::select! {
            request = self.receiver.recv() => match request {
                Some(request) => DriverEvent::Prepare(request),
                None => std::future::pending().await,
            },
            index = async {
                if readiness.is_empty() { std::future::pending().await }
                else { futures_util::future::select_all(readiness).await.0 }
            } => DriverEvent::Append(index),
        }
    }

    pub(crate) fn service(
        &mut self,
        event: DriverEvent,
        session: &mut crate::Session,
    ) -> Result<(), String> {
        if session.resource_owner_key() != self.owner {
            return Err("session driver owner changed".into());
        }
        match event {
            DriverEvent::Prepare(request) => {
                if request.reply.is_closed() {
                    return Ok(());
                }
                let result = self.prepare(&request.process, session);
                // A cancelled request cannot retain authority: its returned
                // invocation is dropped, and the driver still owns the consumer.
                let _ = request.reply.send(result);
                Ok(())
            }
            DriverEvent::Append(index) => {
                let active = &mut self.activations[index];
                let current = active
                    .lease
                    .process
                    .current_context_for_resource_owner(self.owner.clone());
                if current.resource_owner.as_ref() != Some(&active.binding.owner) {
                    active.consumer.revoker().revoke();
                    return Err("session driver process authority changed".into());
                }
                match active.consumer.consume_next(session, &active.binding) {
                    Ok(true) => Ok(()),
                    _ => Err("session driver append unavailable".into()),
                }
            }
        }
    }

    fn prepare(
        &mut self,
        process: &ExtensionProcess,
        session: &crate::Session,
    ) -> Result<LinkedSessionInvocation, String> {
        let connection = read_std_lock(&process.inner.connection).clone();
        if !owner_routes_enabled(&read_std_lock(&connection.protocol)) {
            return Err("session driver routed profile unavailable".into());
        }
        let index = self.activations.iter().position(|active| {
            Arc::ptr_eq(&active.lease.process.inner, &process.inner)
                && Arc::ptr_eq(&active.lease.connection, &connection)
        });
        let index = if let Some(index) = index {
            index
        } else {
            if self.activations.len() >= 64 {
                return Err("session driver activation quota exceeded".into());
            }
            static NEXT_ACTIVATION: AtomicU64 = AtomicU64::new(1);
            let epoch = NEXT_ACTIVATION.fetch_add(1, Ordering::Relaxed);
            let owner = ExtensionResourceOwner {
                session_id: self.owner.clone(),
                extension_instance_id: process.inner.instance_id.clone(),
                process_generation: connection.generation,
            };
            let binding = SessionLeafBinding {
                activation_epoch: epoch,
                owner,
                namespace: process.descriptor().manifest.name.clone(),
                operation_id: format!("tool-driver:{epoch}"),
            };
            let (consumer, producer, grant) = SessionLeafConsumer::new(session, binding.clone())
                .map_err(|_| "session driver authority unavailable")?;
            let lease = process
                .bind_session_leaf(producer, consumer.revoker(), grant)
                .map_err(|_| "session driver activation unavailable")?;
            self.activations.push(DriverActivation {
                consumer,
                binding,
                lease,
            });
            self.activations.len() - 1
        };
        let active = &mut self.activations[index];
        // Host composition journal receipts may advance the sole writer between
        // callbacks. Refresh from that exact writer, never caller JSON. Prior
        // head-bound grants remain unusable once its native head has advanced.
        if lock_std_mutex(&active.lease.bound.current_grant)
            .as_ref()
            .and_then(|grant| grant.expected_head.as_deref())
            != session.head_ref().map(|id| id.0.as_str())
        {
            let grant = active
                .consumer
                .issue_grant(session)
                .map_err(|_| "session driver successor unavailable")?;
            *lock_std_mutex(&active.lease.bound.current_grant) =
                Some(SessionLeafGrantSnapshot::from(&grant));
        }
        // Capture a fresh complete immutable invocation view from the sole
        // writer. Parent and nested callbacks share the committed grant chain,
        // not a head/count heuristic or a second writer.
        let current = lock_std_mutex(&active.lease.bound.current_grant)
            .clone()
            .ok_or("session driver successor unavailable")?;
        let snapshot =
            InvocationSnapshot::capture(&connection, session, &active.binding.namespace, &current)
                .map_err(|_| "session driver snapshot unavailable")?;
        if let InvocationSnapshot::Chunked(view) = &snapshot {
            active
                .lease
                .bound
                .view_revision
                .store(view.descriptor.view_revision, Ordering::Release);
        }
        Ok(LinkedSessionInvocation {
            process: process.clone(),
            connection,
            bound: Arc::clone(&active.lease.bound),
            snapshot,
        })
    }

    pub(crate) async fn drive<F: Future>(
        &mut self,
        future: F,
        session: &mut crate::Session,
        cancellation: &CancellationToken,
    ) -> Result<F::Output, String> {
        tokio::pin!(future);
        loop {
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err("session driver cancelled".into()),
                event = self.next() => self.service(event, session)?,
                result = &mut future => return Ok(result),
            }
        }
    }
}

impl LinkedSessionInvocation {
    pub(crate) async fn run_hook(
        self,
        hook: ExtensionHook,
        payload: serde_json::Value,
        context: ExtensionExecutionContext,
    ) -> Result<ExtensionHookOutput, ExtensionRuntimeError> {
        if context.resource_owner.as_ref() != Some(&self.bound.snapshot.owner)
            || !self.process.inner.contributions.hooks.contains(&hook)
            || !lock_std_mutex(&self.connection.session_leaf.bound)
                .get(&self.bound.snapshot.owner)
                .is_some_and(|bound| Arc::ptr_eq(bound, &self.bound))
        {
            return Err(ExtensionRuntimeError::Protocol(
                "linked session hook authority unavailable".into(),
            ));
        }
        let current = lock_std_mutex(&self.bound.current_grant)
            .clone()
            .ok_or_else(|| {
                ExtensionRuntimeError::Protocol("linked session grant unavailable".into())
            })?;
        let mut params = serde_json::to_value(HookRequest {
            hook,
            payload,
            context,
        })
        .map_err(|_| {
            ExtensionRuntimeError::Protocol("linked session payload unavailable".into())
        })?;
        params["session_leaf"] = serde_json::to_value(current).map_err(|_| {
            ExtensionRuntimeError::Protocol("linked session grant unavailable".into())
        })?;
        let _staged = self.snapshot.attach(&self.connection, &mut params)?;
        self.process
            .request_typed_on_connection(
                self.connection,
                methods::HOOK_RUN,
                &params,
                Some(self.bound.snapshot.owner.clone()),
            )
            .await
    }
}
