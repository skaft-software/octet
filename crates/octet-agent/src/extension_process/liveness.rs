//! Dispatcher-owned callback progress across the process boundary. The pipe
//! worker being responsive is not proof that a callback is making progress.
use super::*;

pub(super) const FEATURE: &str = "dispatcher_liveness_v1";
const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);
const SETTLEMENT_GRACE: Duration = Duration::from_millis(500);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Snapshot {
    sequence: u64,
    last_dispatched_at_ms: Option<u64>,
    last_completed_at_ms: Option<u64>,
    callbacks: Vec<Callback>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Callback {
    id: u64,
    #[serde(rename = "request_id")]
    _request_id: ExtensionRequestId,
    kind: String,
    extension: String,
    interrupted: bool,
    remaining_ms: Option<u64>,
}

#[derive(Default)]
pub(super) struct Progress {
    sequence: u64,
    callbacks: Vec<(Callback, Option<Instant>)>,
}

impl Progress {
    pub(super) fn install(&mut self, value: serde_json::Value) -> Result<(), String> {
        let snapshot: Snapshot = serde_json::from_value(value)
            .map_err(|_| "invalid dispatcher liveness snapshot".to_owned())?;
        if snapshot.callbacks.len() > 128 {
            return Err("dispatcher callback bound exceeded".into());
        }
        let mut ids = HashSet::new();
        for callback in &snapshot.callbacks {
            if !ids.insert(callback.id)
                || callback.kind.is_empty()
                || callback.kind.len() > 128
                || !callback
                    .kind
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_/:.-".contains(&b))
                || callback.extension.len() > 4096
                || callback.extension.chars().any(char::is_control)
                || callback.remaining_ms.is_some_and(|ms| ms > 600_000)
            {
                return Err("invalid dispatcher callback".into());
            }
        }
        // The timestamps are diagnostic facts from the dispatcher, not host
        // clocks. Deadlines use a bounded remaining duration and our Instant.
        let _ = (
            snapshot.last_dispatched_at_ms,
            snapshot.last_completed_at_ms,
        );
        if snapshot.sequence <= self.sequence {
            return Ok(());
        }
        self.sequence = snapshot.sequence;
        let now = Instant::now();
        self.callbacks = snapshot
            .callbacks
            .into_iter()
            .map(|callback| {
                let deadline = (!callback.interrupted)
                    .then_some(callback.remaining_ms)
                    .flatten()
                    .map(|ms| now + Duration::from_millis(ms) + SETTLEMENT_GRACE);
                (callback, deadline)
            })
            .collect();
        Ok(())
    }

    fn overdue(&self) -> Option<String> {
        let now = Instant::now();
        self.callbacks.iter().find_map(|(callback, deadline)| {
            deadline.filter(|deadline| *deadline <= now).map(|_| format!(
                "extension {:?} stage {} callback {} interrupted after its deadline; skipped without replay",
                callback.extension, callback.kind, callback.id,
            ))
        })
    }

    pub(super) fn interrupted(&self) -> usize {
        self.callbacks
            .iter()
            .filter(|(callback, _)| !callback.interrupted)
            .count()
    }
}

impl ProcessConnection {
    pub(super) fn monitor_dispatcher(
        self: &Arc<Self>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ExtensionRuntimeError> + Send + '_>>
    {
        Box::pin(async move {
            let mut unanswered_at = None;
            loop {
                tokio::time::sleep(PROBE_INTERVAL).await;
                let (sequence, overdue) = {
                    let health = read_std_lock(&self.health);
                    (health.dispatcher.sequence, health.dispatcher.overdue())
                };
                if let Some(message) = overdue {
                    let _ = self.events.send(ExtensionEvent::Diagnostic {
                        message: message.clone(),
                    });
                    update_health(
                        &self.health,
                        ExtensionHealthState::Crashed,
                        Some(message.clone()),
                    );
                    // Synchronously blocked JS cannot be detached inside its own
                    // dispatcher. Terminate this generation, never replay its work.
                    self.kill_process_group();
                    return ExtensionRuntimeError::Closed(message);
                }
                // Do not fill the bounded pipe queue while an unbounded input
                // callback blocks JS. After a missed probe, wait for dispatcher
                // progress before sending another; cached bounded deadlines still
                // expire normally. A missed probe is never a callback replay.
                if unanswered_at == Some(sequence) {
                    continue;
                }
                // Control bypasses ordinary callback slots, including routed session
                // slots: a legitimate input/user wait may own every ordinary slot.
                let query: std::pin::Pin<
                    Box<
                        dyn std::future::Future<
                                Output = Result<serde_json::Value, ExtensionRuntimeError>,
                            > + Send
                            + '_,
                    >,
                > = Box::pin(self.request_inner(
                    methods::STATUS_COLLECT,
                    serde_json::json!({"liveness":true}),
                    PROBE_TIMEOUT,
                    false,
                    false,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                ));
                match query.await {
                    Ok(reply) => {
                        unanswered_at = None;
                        if let Some(snapshot) = reply.get("liveness") {
                            if let Err(message) = write_std_lock(&self.health)
                                .dispatcher
                                .install(snapshot.clone())
                            {
                                return ExtensionRuntimeError::Protocol(message);
                            }
                        }
                    }
                    Err(ExtensionRuntimeError::Closed(message)) => {
                        return ExtensionRuntimeError::Closed(message)
                    }
                    // Missing responses alone are not evidence of a stuck callback.
                    // Only an overdue bounded dispatcher record can establish that.
                    Err(_) => {
                        unanswered_at = Some(read_std_lock(&self.health).dispatcher.sequence);
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::trusted_descriptor;
    use super::*;

    async fn adapter(
        source: &str,
        budget: u64,
        timeout: Duration,
    ) -> (tempfile::TempDir, ExtensionProcess) {
        let temp = tempfile::tempdir().unwrap();
        let factory = temp.path().join("factory.mjs");
        std::fs::write(&factory, source).unwrap();
        let path = super::super::pi_fixture::capture(temp.path(), &[factory]).await;
        let mut manifest = ExtensionManifest::load(&path).unwrap();
        manifest
            .entrypoint
            .env
            .insert("OCTET_PI_HANDLER_BUDGET_MS".into(), budget.to_string());
        let mut config = ExtensionRuntimeConfig::new(temp.path());
        config.supervise = false;
        config.request_timeout = timeout;
        let mut descriptor = trusted_descriptor(temp.path(), manifest);
        descriptor.manifest_path = path;
        let process = ExtensionProcess::start(descriptor, config).await.unwrap();
        (temp, process)
    }

    fn context(process: &ExtensionProcess) -> ExtensionExecutionContext {
        let mut context = process.execution_context();
        context.resource_owner = Some(ExtensionResourceOwner {
            session_id: "liveness-owner".into(),
            extension_instance_id: process.inner.instance_id.clone(),
            process_generation: process.health_snapshot().generation,
        });
        context
    }

    #[tokio::test]
    async fn dispatcher_queries_preserve_legitimate_input_waits() {
        let (_temp, process) = adapter("export default pi => pi.on('input', async () => { await new Promise(r=>setTimeout(r,650)); return {action:'transform',text:'accepted'}; });", 40, Duration::from_secs(5)).await;
        assert!(process.supports_feature(FEATURE));
        let result = process
            .run_hook(
                ExtensionHook::BeforePrompt,
                serde_json::json!({"phase":"input","text":"original","source":"interactive"}),
                context(&process),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["input_event"]["text"],
            "accepted"
        );
        assert!(process.is_running());
        process.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_missed_probes_do_not_kill_unbounded_input_callbacks() {
        let (_temp, process) = adapter("export default pi => pi.on('input', () => { const until=Date.now()+2800; while(Date.now()<until) {} return {action:'transform',text:'accepted'}; });", 40, Duration::from_secs(5)).await;
        let connection = read_std_lock(&process.inner.connection).clone();
        let issued = connection.next_id.load(Ordering::Acquire);
        let result = process
            .run_hook(
                ExtensionHook::BeforePrompt,
                serde_json::json!({"phase":"input","text":"original","source":"interactive"}),
                context(&process),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(result).unwrap()["input_event"]["text"],
            "accepted"
        );
        assert!(process.is_running());
        assert!(
            connection.next_id.load(Ordering::Acquire) <= issued + 3,
            "unanswered probes must not fill the pipe queue"
        );
        process.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_shutdown_control_keeps_its_cleanup_deadline() {
        let (_temp, process) = adapter("export default pi => pi.on('input', () => { const until=Date.now()+2800; while(Date.now()<until) {} });", 40, Duration::from_secs(5)).await;
        let connection = read_std_lock(&process.inner.connection).clone();
        let pending = {
            let process = process.clone();
            tokio::spawn(async move {
                process.run_hook(ExtensionHook::BeforePrompt,
                serde_json::json!({"phase":"input","text":"original","source":"interactive"}), context(&process)).await
            })
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            while read_std_lock(&connection.health).dispatcher.interrupted() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            connection.request_during_shutdown(
                methods::SHUTDOWN,
                serde_json::json!({}),
                Duration::from_millis(50),
            ),
        )
        .await
        .unwrap();
        assert!(
            matches!(result, Err(ExtensionRuntimeError::Timeout { .. })),
            "{result:?}"
        );
        pending.abort();
        process.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_bounded_sync_hang_is_interrupted_without_replay() {
        let (_temp, process) = adapter(
            "export default pi => pi.on('tool_call', () => { while (true) {} });",
            40,
            Duration::from_secs(5),
        )
        .await;
        let mut events = process.subscribe();
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            process.run_hook(
                ExtensionHook::BeforeToolCall,
                serde_json::json!({"name":"read","arguments":{},"tool_call_id":"call"}),
                context(&process),
            ),
        )
        .await
        .expect("bounded dispatcher callback must not hold a turn");
        assert!(
            matches!(result, Err(ExtensionRuntimeError::Closed(ref message)) if message.contains("tool_call") && message.contains("interrupted")),
            "{result:?}"
        );
        let mut warned = false;
        while let Ok(event) = events.try_recv() {
            if let ExtensionEvent::Diagnostic { message } = event {
                warned |= message.contains("tool_call") && message.contains("without replay");
            }
        }
        assert!(warned);
        process.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_interrupted_guard_leaves_native_tool_admission_open() {
        let (temp, process) = adapter(
            "export default pi => pi.on('tool_call', () => { while (true) {} });",
            40,
            Duration::from_secs(5),
        )
        .await;
        let sandbox = crate::SandboxConfig::new(temp.path());
        let context = ToolContext {
            workspace: temp.path(),
            sandbox: &sandbox,
            execution_scope: "liveness-test",
            resource_owner: "liveness-owner",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: crate::CancellationToken::default(),
        };
        let result = tokio::time::timeout(
            Duration::from_secs(4),
            process.transform_tool_call("read", &serde_json::json!({}), &context),
        )
        .await
        .unwrap();
        assert!(matches!(result, Ok(None)), "{result:?}");
        // A stopped generation cannot fail every subsequent tool either.
        assert!(matches!(
            process
                .transform_tool_call("read", &serde_json::json!({}), &context)
                .await,
            Ok(None)
        ));
        process.shutdown().await;
    }

    #[tokio::test]
    async fn dispatcher_disconnect_reports_exit_status_not_a_hang() {
        let (_temp, process) = adapter(
            "export default pi => pi.on('input', () => process.exit(23));",
            40,
            Duration::from_secs(5),
        )
        .await;
        let result = process
            .run_hook(
                ExtensionHook::BeforePrompt,
                serde_json::json!({"phase":"input","text":"original","source":"interactive"}),
                context(&process),
            )
            .await;
        assert!(
            matches!(result, Err(ExtensionRuntimeError::Closed(ref message)) if message.contains("disconnected") && message.contains("23") && !message.contains("deadline")),
            "{result:?}"
        );
        process.shutdown().await;
    }

    #[test]
    fn dispatcher_progress_validates_bounds_and_excludes_interrupted_and_unbounded_callbacks() {
        let mut progress = Progress::default();
        let callback = |id, remaining_ms, interrupted| serde_json::json!({"id":id,"request_id":1,"kind":"input","extension":"test.mjs","remaining_ms":remaining_ms,"interrupted":interrupted});
        progress.install(serde_json::json!({"sequence":1,"last_dispatched_at_ms":1,"last_completed_at_ms":null,"callbacks":[callback(1, None::<u64>, false), callback(2, Some(0), true)]})).unwrap();
        assert!(progress.overdue().is_none());
        assert_eq!(progress.interrupted(), 1);
        assert!(progress
            .install(
                serde_json::json!({"sequence":2,"callbacks":[callback(1, Some(600_001), false)]})
            )
            .is_err());
    }
}
