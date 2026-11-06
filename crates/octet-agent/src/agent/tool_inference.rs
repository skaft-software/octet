use super::*;
use crate::tool_inference::{
    ToolInferenceModelResolver, ToolInferenceRequest, ToolInferenceService,
};
use std::future::Future;
use tokio::sync::oneshot;
use tokio::time::Instant;

const DEADLINE: Duration = Duration::from_secs(30);
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_EVENTS: usize = 4096;

impl Agent {
    #[allow(missing_docs)]
    pub fn set_tool_inference_resolver(
        &mut self,
        resolver: Option<Arc<dyn ToolInferenceModelResolver>>,
    ) {
        self.tool_inference_resolver = resolver;
    }
}

type InferenceFuture = Pin<Box<dyn Future<Output = Result<octet_ai::Response, ()>> + Send>>;
type PreparedInference = (
    Model,
    Request,
    Option<Arc<dyn octet_ai::HostStreamTransport>>,
);

pub(super) struct InferenceScope {
    pub(super) service: Arc<dyn ToolInferenceService>,
    stop: CancellationToken,
}

impl Drop for InferenceScope {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct InferenceHandle {
    sender: mpsc::Sender<InferenceJob>,
    used: AtomicBool,
    stop: CancellationToken,
    parent: CancellationToken,
}

struct InferenceJob {
    request: ToolInferenceRequest,
    cancellation: CancellationToken,
    caller: CancellationToken,
    deadline: Instant,
    updates: Option<mpsc::Sender<StreamEvent>>,
    reply: oneshot::Sender<Result<octet_ai::Response, ToolError>>,
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[async_trait::async_trait]
impl ToolInferenceService for InferenceHandle {
    async fn stream(
        &self,
        request: ToolInferenceRequest,
        cancellation: CancellationToken,
        updates: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<octet_ai::Response, ToolError> {
        if self.stop.is_cancelled() || self.parent.is_cancelled() || cancellation.is_cancelled() {
            return Err(refused("tool inference cancelled"));
        }
        let caller = CancellationToken::default();
        let _cancel = CancelOnDrop(caller.clone());
        if self.used.swap(true, Ordering::AcqRel) {
            return Err(refused("only one inference is allowed per tool execution"));
        }
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(InferenceJob {
                request,
                cancellation,
                caller,
                deadline: Instant::now() + DEADLINE,
                updates,
                reply,
            })
            .map_err(|_| refused("tool inference owner unavailable"))?;
        result
            .await
            .map_err(|_| refused("tool inference owner unavailable"))?
    }
}

struct InferenceFlight {
    attempt: u64,
    model: Model,
    client: AiClient,
    future: InferenceFuture,
    cancellation: CancellationToken,
    caller: CancellationToken,
    deadline: Instant,
    reply: oneshot::Sender<Result<octet_ai::Response, ToolError>>,
}

pub(super) struct InferenceDriver {
    owner: String,
    parent_id: String,
    resolver: Arc<dyn ToolInferenceModelResolver>,
    client: AiClient,
    receiver: mpsc::Receiver<InferenceJob>,
    active: Option<InferenceFlight>,
    stop: CancellationToken,
    parent: CancellationToken,
    limits: InferenceLimits,
    session_id: String,
}

#[derive(Clone, Copy, Default)]
pub(super) struct InferenceLimits {
    pub(super) tokens: Option<u64>,
    pub(super) cost: Option<u64>,
    pub(super) max_output_tokens: u64,
    pub(super) cache_retention: CacheRetention,
}

pub(super) struct InferenceStep(InferenceStepKind);

enum InferenceStepKind {
    Admit(Box<InferenceJob>),
    Finished(Result<Box<octet_ai::Response>, ()>),
    Cancelled,
    TimedOut,
}

pub(super) struct InferenceSettlement {
    pub(super) usage: Usage,
    pub(super) cost: Option<Cost>,
    pub(super) uncertain: bool,
    pub(super) reported: bool,
}

impl InferenceDriver {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        owner: String,
        parent_id: String,
        session_id: String,
        resolver: Arc<dyn ToolInferenceModelResolver>,
        client: AiClient,
        parent: CancellationToken,
        limits: InferenceLimits,
    ) -> (Self, InferenceScope) {
        let (sender, receiver) = mpsc::channel(1);
        let stop = CancellationToken::default();
        let scope = InferenceScope {
            service: Arc::new(InferenceHandle {
                sender,
                used: AtomicBool::new(false),
                stop: stop.clone(),
                parent: parent.clone(),
            }),
            stop: stop.clone(),
        };
        (
            Self {
                owner,
                parent_id,
                resolver,
                client,
                receiver,
                active: None,
                stop,
                parent,
                limits,
                session_id,
            },
            scope,
        )
    }

    pub(super) async fn next_step(&mut self) -> InferenceStep {
        let Some(flight) = self.active.as_mut() else {
            return match self.receiver.recv().await {
                Some(job) => InferenceStep(InferenceStepKind::Admit(Box::new(job))),
                None => std::future::pending().await,
            };
        };
        if self.parent.is_cancelled()
            || self.stop.is_cancelled()
            || flight.cancellation.is_cancelled()
            || flight.caller.is_cancelled()
            || flight.reply.is_closed()
        {
            return InferenceStep(InferenceStepKind::Cancelled);
        }
        if Instant::now() >= flight.deadline {
            return InferenceStep(InferenceStepKind::TimedOut);
        }
        tokio::select! {
            biased;
            result = &mut flight.future => InferenceStep(InferenceStepKind::Finished(result.map(Box::new))),
            _ = self.parent.cancelled() => InferenceStep(InferenceStepKind::Cancelled),
            _ = self.stop.cancelled() => InferenceStep(InferenceStepKind::Cancelled),
            _ = flight.cancellation.cancelled() => InferenceStep(InferenceStepKind::Cancelled),
            _ = flight.caller.cancelled() => InferenceStep(InferenceStepKind::Cancelled),
            _ = flight.reply.closed() => InferenceStep(InferenceStepKind::Cancelled),
            _ = tokio::time::sleep_until(flight.deadline) => InferenceStep(InferenceStepKind::TimedOut),
        }
    }

    pub(super) fn advance(
        &mut self,
        step: InferenceStep,
        session: &mut Session,
    ) -> Result<Option<InferenceSettlement>, SessionError> {
        if session.resource_owner_key() != self.owner {
            return Err(SessionError::Limit("tool inference owner changed".into()));
        }
        match step.0 {
            InferenceStepKind::Admit(job) => {
                self.admit(*job, session)?;
                Ok(None)
            }
            InferenceStepKind::Finished(Ok(response)) => {
                self.finish_known(*response, session).map(Some)
            }
            InferenceStepKind::Finished(Err(())) => self
                .finish_unknown(session, "tool inference failed")
                .map(Some),
            InferenceStepKind::Cancelled => self
                .finish_unknown(session, "tool inference cancelled")
                .map(Some),
            InferenceStepKind::TimedOut => self
                .finish_unknown(
                    session,
                    "tool inference exceeded the 30 second host deadline",
                )
                .map(Some),
        }
    }

    fn admit(&mut self, job: InferenceJob, session: &mut Session) -> Result<(), SessionError> {
        let admitted = self.prepare(&job, session);
        let (model, mut request, fetch) = match admitted {
            Ok(value) => value,
            Err(error) => {
                let _ = job.reply.send(Err(error));
                return Ok(());
            }
        };
        let attempt = match session.start_tool_inference(
            self.parent_id.clone(),
            model.endpoint.id.clone(),
            model.spec.id.clone(),
        ) {
            Ok(attempt) => attempt,
            Err(error) => {
                let _ = job
                    .reply
                    .send(Err(refused("tool inference persistence failed")));
                return Err(error);
            }
        };
        request.session_id = Some(format!("{}:tool-inference:{attempt}", self.session_id));
        let client = self.client.track_request_dispatch();
        let tracked = client.clone();
        let selected = model.clone();
        let updates = job.updates;
        let future = Box::pin(async move {
            let overrides = octet_ai::RequestOverrides {
                codex_transport: (selected.endpoint.transport
                    == octet_ai::EndpointTransport::WebSocketPreferred
                    && fetch
                        .as_ref()
                        .is_none_or(|transport| transport.native_model().is_some()))
                .then_some(octet_ai::declarations::codex::CodexTransport::Sse),
                ..Default::default()
            };
            let options = octet_ai::HostRequestOptions {
                fetch,
                ..Default::default()
            };
            let mut stream = tracked
                .stream_with_host_options(&selected, request, overrides, options)
                .await
                .map_err(|_| ())?;
            let mut output_bytes = 0usize;
            let mut events = 0usize;
            while let Some(event) = stream.next().await {
                let event = event.map_err(|_| ())?;
                if !matches!(&event, StreamEvent::Finished(_)) {
                    events += 1;
                    if events > MAX_EVENTS {
                        return Err(());
                    }
                    let bytes = match &event {
                        StreamEvent::Started { response_id } => {
                            response_id.as_ref().map_or(0, String::len)
                        }
                        StreamEvent::ProviderLifecycle(status) => {
                            status.detail.as_ref().map_or(0, String::len)
                        }
                        StreamEvent::TextDelta { delta, .. }
                        | StreamEvent::ReasoningDelta { delta, .. }
                        | StreamEvent::ToolCallArgsDelta { delta, .. } => delta.len(),
                        StreamEvent::ToolCallStart { id, name, .. } => {
                            id.0.len().saturating_add(name.len())
                        }
                        StreamEvent::MediaCompleted { media, .. } => {
                            let mut size = RequestSize(0);
                            serde_json::to_writer(&mut size, media).map_err(|_| ())?;
                            size.0
                        }
                        _ => 0,
                    };
                    output_bytes = output_bytes.checked_add(bytes).ok_or(())?;
                    if output_bytes > MAX_OUTPUT_BYTES {
                        return Err(());
                    }
                }
                match event {
                    StreamEvent::Finished(response) => return Ok(response),
                    event => {
                        if let Some(sender) = &updates {
                            sender.try_send(event).map_err(|_| ())?;
                        }
                    }
                }
            }
            Err(())
        });
        self.active = Some(InferenceFlight {
            attempt,
            model,
            client,
            future,
            cancellation: job.cancellation,
            caller: job.caller,
            deadline: job.deadline,
            reply: job.reply,
        });
        Ok(())
    }

    fn prepare(
        &self,
        job: &InferenceJob,
        session: &Session,
    ) -> Result<PreparedInference, ToolError> {
        if self.active.is_some()
            || self.stop.is_cancelled()
            || self.parent.is_cancelled()
            || job.cancellation.is_cancelled()
            || job.caller.is_cancelled()
            || job.reply.is_closed()
        {
            return Err(refused("tool inference cancelled"));
        }
        if Instant::now() >= job.deadline {
            return Err(refused(
                "tool inference exceeded the 30 second host deadline",
            ));
        }
        if job
            .updates
            .as_ref()
            .is_some_and(|sender| sender.max_capacity() > 64)
        {
            return Err(refused(
                "tool inference update channel exceeds the host limit",
            ));
        }
        identifier(&job.request.provider)?;
        identifier(&job.request.model)?;
        serde_json::to_writer(RequestSize(0), &job.request.request)
            .map_err(|_| refused("tool inference request exceeds the host byte limit"))?;
        if job.request.request.responses.is_some() || job.request.request.session_id.is_some() {
            return Err(refused("tool inference cannot import session replay state"));
        }
        let resolved = self
            .resolver
            .resolve(&job.request.provider, &job.request.model)
            .map_err(|_| refused("tool inference model is not configured or available"))?;
        let model = resolved.model;
        let mut request = job.request.request.clone();
        let input = provider_context::effective_request_estimate(&request);
        let requested = request
            .max_output_tokens
            .unwrap_or(self.limits.max_output_tokens)
            .min(self.limits.max_output_tokens)
            .min(model.spec.limits.max_output_tokens);
        let output =
            resolve_request_max_output_tokens(model.spec.limits.context_window, input, requested);
        if output == 0 {
            return Err(refused("tool inference has no output capacity"));
        }
        request.max_output_tokens = Some(output);
        request.cache_retention = self.limits.cache_retention;
        request.session_id = Some(self.session_id.clone());
        octet_ai::validate_provider_request(&model, &request)
            .map_err(|_| refused("tool inference request is unsupported by the selected model"))?;
        let output = reservation_output_tokens(
            session,
            &model,
            output,
            self.limits.tokens,
            self.limits.cost,
        )
        .map_err(|_| refused("tool inference cannot enforce the session budget"))?;
        reserve_request_tokens(session, input, output, self.limits.tokens)
            .map_err(|_| refused("tool inference session token budget exhausted"))?;
        reserve_request_cost(
            session,
            &model,
            input,
            output,
            self.limits.cost,
            request.cache_retention,
        )
        .map_err(|_| refused("tool inference session cost budget unavailable or exhausted"))?;
        Ok((model, request, resolved.fetch))
    }

    fn finish_known(
        &mut self,
        response: octet_ai::Response,
        session: &mut Session,
    ) -> Result<InferenceSettlement, SessionError> {
        let flight = self.active.as_ref().expect("terminal step owns a flight");
        session.record_tool_inference_usage(flight.attempt, response.usage, response.cost)?;
        let flight = self.active.take().expect("terminal step owns a flight");
        let settlement = InferenceSettlement {
            usage: response.usage,
            cost: response.cost,
            uncertain: false,
            reported: true,
        };
        let cancelled = self.parent.is_cancelled()
            || self.stop.is_cancelled()
            || flight.cancellation.is_cancelled()
            || flight.caller.is_cancelled();
        let output_bounded = serde_json::to_writer(
            RequestSize(0),
            &(
                &response.message,
                &response.response_id,
                &response.responses_output,
            ),
        )
        .is_ok();
        let result = if cancelled {
            Err(refused("tool inference cancelled"))
        } else if Instant::now() >= flight.deadline {
            Err(refused(
                "tool inference exceeded the 30 second host deadline",
            ))
        } else if !output_bounded {
            Err(refused("tool inference output exceeds the host byte limit"))
        } else if incomplete_responses_error(&flight.model, &response).is_some()
            || response.deferred.is_some()
        {
            Err(refused(
                "tool inference returned an unsupported terminal response",
            ))
        } else {
            Ok(response)
        };
        drop(flight.future);
        let _ = flight.reply.send(result);
        Ok(settlement)
    }

    fn finish_unknown(
        &mut self,
        session: &mut Session,
        message: &'static str,
    ) -> Result<InferenceSettlement, SessionError> {
        let flight = self.active.take().expect("terminal step owns a flight");
        drop(flight.future);
        let uncertain = flight.client.request_may_have_been_sent();
        if uncertain {
            session.record_usage_uncertainty_with_bound(
                flight.model.endpoint.id.clone(),
                flight.model.spec.id.clone(),
                format!("tool_inference:{}", flight.attempt),
                None,
            )?;
        }
        session.settle_tool_inference(flight.attempt)?;
        let _ = flight.reply.send(Err(refused(message)));
        Ok(InferenceSettlement {
            usage: Usage::default(),
            cost: None,
            uncertain,
            reported: false,
        })
    }

    pub(super) fn cancel(
        &mut self,
        session: &mut Session,
    ) -> Result<Option<InferenceSettlement>, SessionError> {
        self.stop.cancel();
        self.receiver.close();
        while let Ok(job) = self.receiver.try_recv() {
            let _ = job.reply.send(Err(refused("tool inference cancelled")));
        }
        if self.active.is_some() {
            self.finish_unknown(session, "tool inference cancelled")
                .map(Some)
        } else {
            Ok(None)
        }
    }
}

fn refused(message: &'static str) -> ToolError {
    ToolError::new(message)
}

fn identifier(value: &str) -> Result<(), ToolError> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:/-".contains(&byte))
    {
        return Err(refused(
            "tool inference requires a configured model identifier",
        ));
    }
    Ok(())
}

struct RequestSize(usize);

impl Write for RequestSize {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_REQUEST_BYTES)
            .ok_or_else(|| io::Error::other("tool inference byte limit"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
