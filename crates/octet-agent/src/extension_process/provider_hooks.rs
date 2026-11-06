use super::*;

pub(super) const FEATURE: &str = "provider_request_hooks_v1";

pub(super) struct ProviderHooksJob {
    pub(super) hooks: octet_ai::HostProviderRequestHooks,
    pub(super) connection: Weak<ProcessConnection>,
    pub(super) route: crate::ExtensionProviderRoute,
    pub(super) stream_id: String,
    pub(super) claim: Arc<AtomicUsize>,
    bridge_allowed: bool,
    pub(super) cancelled: CancellationToken,
    calls: AtomicUsize,
    phases: AtomicU8,
    failed: AtomicBool,
    serial: Mutex<()>,
}

impl ProviderHooksJob {
    pub(super) fn new(
        hooks: octet_ai::HostProviderRequestHooks,
        connection: &Arc<ProcessConnection>,
        route: crate::ExtensionProviderRoute,
        stream_id: String,
        claim: Arc<AtomicUsize>,
        bridge_allowed: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            hooks,
            connection: Arc::downgrade(connection),
            route,
            stream_id,
            claim,
            bridge_allowed,
            cancelled: CancellationToken::default(),
            calls: AtomicUsize::new(0),
            phases: AtomicU8::new(0),
            failed: AtomicBool::new(false),
            serial: Mutex::new(()),
        })
    }

    fn live(&self) -> Result<Arc<ProcessConnection>, AiError> {
        let connection = self
            .connection
            .upgrade()
            .ok_or_else(provider_unavailable_error)?;
        if self.failed.load(Ordering::Acquire)
            || self.cancelled.is_cancelled()
            || !connection_is_usable(&connection)
            || connection.draining.load(Ordering::Acquire)
            || connection.provider_owner != self.route.owner
            || !connection
                .provider_registry
                .as_ref()
                .is_some_and(|registry| registry.route_is_active(&self.route))
            || !lock_std_mutex(&connection.provider_streams)
                .get(&self.stream_id)
                .is_some_and(|stream| {
                    !stream.terminal
                        && stream
                            .request_hooks
                            .as_ref()
                            .is_some_and(|job| std::ptr::eq(job.as_ref(), self))
                })
        {
            return Err(provider_unavailable_error());
        }
        Ok(connection)
    }

    pub(super) fn completed(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
            && (self.claim.load(Ordering::Acquire) == 1
                || self.bridge_allowed && self.phases.load(Ordering::Acquire) == 7)
    }

    async fn run(&self, phase: &str, request: HookRequest) -> Result<serde_json::Value, AiError> {
        let _serial = self.serial.lock().await;
        self.live()?;
        match self
            .claim
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) | Err(2) => {}
            Err(_) => return Err(provider_unavailable_error()),
        }
        if self.calls.fetch_add(1, Ordering::AcqRel) >= 64 {
            return Err(provider_unavailable_error());
        }
        let result = match phase {
            "provider/hooks/payload" => {
                if request.headers.is_some() || request.status.is_some() {
                    return Err(provider_unavailable_error());
                }
                let payload = request.payload.ok_or_else(provider_unavailable_error)?;
                if !payload.is_object() && !payload.is_array() {
                    return Err(provider_unavailable_error());
                }
                let payload = self.hooks.payload(payload).await?;
                if serde_json::to_vec(&payload)
                    .map_err(|_| provider_unavailable_error())?
                    .len()
                    > 786432
                {
                    return Err(provider_unavailable_error());
                }
                serde_json::json!({"payload": payload})
            }
            "provider/hooks/headers" | "provider/hooks/response" => {
                if request.payload.is_some() {
                    return Err(provider_unavailable_error());
                }
                let mut headers = reqwest::header::HeaderMap::new();
                provider_context::apply_pipeline_headers(
                    &mut headers,
                    request.headers.ok_or_else(provider_unavailable_error)?,
                )?;
                if phase == "provider/hooks/headers" {
                    if request.status.is_some() {
                        return Err(provider_unavailable_error());
                    }
                    self.hooks.headers(&mut headers).await?;
                    serde_json::json!({"headers": provider_context::pipeline_header_projection(&headers)?})
                } else {
                    let status = request
                        .status
                        .filter(|status| (100..=599).contains(status))
                        .ok_or_else(provider_unavailable_error)?;
                    self.hooks
                        .response(
                            reqwest::StatusCode::from_u16(status)
                                .map_err(|_| provider_unavailable_error())?,
                            &headers,
                        )
                        .await?;
                    serde_json::json!({"accepted": true})
                }
            }
            _ => return Err(provider_unavailable_error()),
        };
        self.live()?;
        self.phases.fetch_or(
            match phase {
                "provider/hooks/payload" => 1,
                "provider/hooks/headers" => 2,
                "provider/hooks/response" => 4,
                _ => unreachable!(),
            },
            Ordering::AcqRel,
        );
        Ok(result)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HookRequest {
    stream_id: String,
    #[serde(default)]
    payload: Option<serde_json::Value>,
    #[serde(default)]
    headers: Option<BTreeMap<String, Option<provider_context::PipelineHeaderValue>>>,
    #[serde(default)]
    status: Option<u16>,
}

pub(super) fn dispatch(
    state: &ProtocolReadState,
    object: &serde_json::Map<String, serde_json::Value>,
    method: &str,
    params: serde_json::Value,
) -> Result<(), String> {
    let id = parse_child_request_id(object, method)?;
    let registered = insert_child_request(state, id.clone(), None, None)?;
    let request: HookRequest = match serde_json::from_value(params) {
        Ok(request) => request,
        Err(_) => {
            return queue_provider_host_response(
                state,
                &id,
                Err(ProviderHostResponseError::Invalid),
            )
        }
    };
    let enabled = {
        let protocol = read_std_lock(&state.protocol);
        protocol.version == EXTENSION_API_VERSION_0_4
            && protocol.supports(FEATURE)
            && protocol.supports("provider_proxy_v1")
    };
    let job = lock_std_mutex(&state.provider_streams)
        .get(&request.stream_id)
        .and_then(|stream| stream.request_hooks.clone());
    let Some(job) = job.filter(|job| {
        enabled
            && job.bridge_allowed
            && state.declared.providers
            && job.route.owner == state.provider_owner
            && job.route.owner.generation == state.generation
            && job.route.owner.extension_instance_id == state.instance_id
    }) else {
        return queue_provider_host_response(
            state,
            &id,
            Err(ProviderHostResponseError::Unavailable),
        );
    };
    let Some(registry) = state.provider_registry.clone() else {
        return queue_provider_host_response(
            state,
            &id,
            Err(ProviderHostResponseError::Unavailable),
        );
    };
    let worker = match state.child_work_slots.clone().try_acquire_owned() {
        Ok(worker) => worker,
        Err(_) => {
            return queue_provider_host_response(
                state,
                &id,
                Err(ProviderHostResponseError::ResourceExhausted),
            )
        }
    };
    let cancellation = CancellationToken::default();
    *lock_std_mutex(&registered.response_state.composition_cancellation) =
        Some(cancellation.clone());
    let writer = state.writer.clone();
    let children = state.child_requests.clone();
    let response_state = registered.response_state;
    let limit = state.max_message_bytes();
    let method = method.to_owned();
    tokio::spawn(async move {
        let _worker = worker;
        let result = tokio::select! {
            biased;
            _ = job.cancelled.cancelled() => Err(provider_unavailable_error()),
            _ = registry.route_invalidated(&job.route) => Err(provider_unavailable_error()),
            _ = cancellation.cancelled() => Err(provider_unavailable_error()),
            _ = child_response_settled(response_state) => Err(provider_unavailable_error()),
            result = job.run(&method, request) => result,
        };
        if result.is_err() {
            job.failed.store(true, Ordering::Release);
        }
        let reply = match result.and_then(|result| job.live().map(|_| result)) {
            Ok(result) => serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result}),
            Err(_) => {
                serde_json::json!({"jsonrpc":"2.0", "id":id, "error":{"code":-32002,"message":"provider hook unavailable"}})
            }
        };
        if try_queue_child_response(&children, &id, &writer, limit, reply).is_err() {
            settle_child_request(&children, &id);
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests;
