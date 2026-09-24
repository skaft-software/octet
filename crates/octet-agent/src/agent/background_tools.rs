//! Bounded, run-owned post-persistence observations. Provider async metadata
//! never grants effects: the caller has already checked the immutable advertised
//! schema and host parallel-observation classification. Admission is repeated
//! by the ordinary effect broker and hooks inside the task.
use super::*;

struct BackgroundJob {
    call: ToolCall,
    task: Option<tokio::task::JoinHandle<Box<ParallelReadWaveExecution>>>,
    completed: Option<CompletedToolExecution>,
}

#[derive(Default)]
pub(super) struct BackgroundTools {
    jobs: VecDeque<BackgroundJob>,
}

impl Drop for BackgroundTools {
    fn drop(&mut self) {
        // Dropping Run must never detach effects. Durable unresolved async calls
        // are paired as indeterminate on restart, not redispatched.
        for job in &self.jobs {
            if let Some(task) = &job.task {
                task.abort();
            }
        }
    }
}

impl BackgroundTools {
    pub(super) fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &mut self,
        call: ToolCall,
        invocation: InvocationHandle,
        tool: Arc<dyn Tool>,
        hooks: Vec<Arc<dyn ToolCallHook>>,
        broker: EffectBroker,
        run_id: String,
        generation: u64,
        sandbox: SandboxConfig,
        tool_scope: String,
        resource_owner: String,
        active_skills: Vec<crate::session::SkillActivatedSnapshot>,
        registered_tools: Vec<String>,
        cancellation: CancellationToken,
    ) {
        assert!(self.jobs.len() < MAX_PARALLEL_READ_WAVE_WIDTH);
        let owned_call = call.clone();
        let task = tokio::spawn(async move {
            let prepared = prepare_parallel_read_call(
                invocation,
                tool,
                &hooks,
                &broker,
                &run_id,
                generation,
                &owned_call.id,
                &owned_call.name,
                owned_call
                    .arguments_value()
                    .expect("admitted complete arguments"),
                &sandbox,
                &tool_scope,
                &resource_owner,
                &active_skills,
                &registered_tools,
                cancellation.clone(),
            )
            .await;
            let mut completed = match prepared {
                ParallelReadPreparation::Completed(result) => result,
                ParallelReadPreparation::Admitted(admitted) => Box::new(
                    execute_admitted_parallel_read(
                        *admitted,
                        &sandbox,
                        &tool_scope,
                        &resource_owner,
                        &active_skills,
                        &registered_tools,
                        cancellation.clone(),
                    )
                    .await,
                ),
            };
            if let Some(after) = completed.after.take() {
                run_parallel_after_tool_hooks(
                    after,
                    &hooks,
                    &completed.execution.result,
                    &sandbox,
                    &tool_scope,
                    &resource_owner,
                    &active_skills,
                    &registered_tools,
                    cancellation,
                )
                .await;
            }
            completed
        });
        self.jobs.push_back(BackgroundJob {
            call,
            task: Some(task),
            completed: None,
        });
    }

    /// Settle in original call order after the concurrently generated assistant
    /// is durable. Results were not in that request's input, so they must not be
    /// projected ahead of its completed output.
    pub(super) async fn settle_one(
        &mut self,
        session: &mut Session,
        model: &Model,
        sandbox: &SandboxConfig,
        context: &ContextTracker,
        usage: &mut Usage,
        evidence: &mut Option<TerminalGateEvidence>,
    ) -> Result<Vec<AgentEvent>, AgentError> {
        let job = self.jobs.front_mut().expect("pending background job");
        if job.completed.is_none() {
            let completed = job.task.as_mut().expect("running background job").await;
            job.task = None;
            job.completed = Some(match completed {
                Ok(completed) => completed.execution,
                Err(_) => {
                    let (tx, rx) = mpsc::channel(1);
                    completed_parallel_read_execution(
                        Err(ToolError::new(
                            "background tool interrupted; not automatically replayed",
                        )),
                        None,
                        rx,
                        ToolProgressSink::live(tx),
                        std::time::Instant::now(),
                        true,
                    )
                    .execution
                }
            });
        }
        // Keep the completed output across a failed append. Failure cleanup may
        // retry settlement, but must never poll an already-consumed JoinHandle.
        let call = job.call.clone();
        let execution = job.completed.as_mut().expect("joined background result");
        let mut events = Vec::new();
        apply_execution_policy_denial(&mut execution.policy_decision, &execution.result);
        if let Some(decision) = execution.policy_decision.clone() {
            events.push(AgentEvent::ToolPolicyDecision {
                id: call.id.clone(),
                name: call.name.clone(),
                decision,
            });
        }
        let (message, accepted_media, text, is_error, details) = lower_tool_result(
            call.id.clone(),
            &execution.result,
            model,
            sandbox.max_output_bytes,
            Vec::new(),
        );
        session.append_with_metadata(
            EntryValue::Message(Message::User(message)),
            details.map(|tool_output| EntryMetadata {
                tool_output: Some(tool_output),
                tool_started_unix_ms: execution.started_unix_ms,
                tool_finished_unix_ms: execution.finished_unix_ms,
                ..EntryMetadata::default()
            }),
        )?;
        // Only remove after the paired result is durable. A failed append ends
        // the run and leaves restart recovery the original unresolved identity.
        let mut execution = self
            .jobs
            .pop_front()
            .expect("persisted background job")
            .completed
            .expect("joined background result");
        resolve_tool_delivery_after_persistence(&execution.result, sandbox.max_output_bytes);
        if let Some(evidence) = evidence {
            evidence.record_action(&call.name, &call.arguments_json, is_error, &text);
        }
        while let Ok(progress) = execution.progress_rx.try_recv() {
            if let ProgressSettlement::Emit(progress) =
                settle_tool_progress(progress, execution.cancellation_won, session)
            {
                events.push(AgentEvent::ToolProgress {
                    id: call.id.clone(),
                    progress,
                });
            }
        }
        let (bytes, dropped_events) = execution.progress_sink.take_dropped();
        if bytes > 0 || dropped_events > 0 {
            events.push(AgentEvent::ToolProgress {
                id: call.id.clone(),
                progress: ToolProgress::Dropped {
                    bytes,
                    events: dropped_events,
                },
            });
        }
        context.tool_finished();
        if let Ok(output) = &execution.result {
            if let Some(tool_usage) = output.usage() {
                add_usage(usage, tool_usage);
            }
        }
        let result = execution.result.map(|output| {
            output
                .without_media_payloads_for(accepted_media)
                .with_is_error(is_error)
        });
        events.push(AgentEvent::ToolFinished {
            id: call.id,
            result,
            duration: execution.duration,
        });
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn failed_append_retains_completed_job_without_rejoining_or_redispatch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        let model = octet_ai::ModelCatalog::builtin()
            .unwrap()
            .resolve(&octet_ai::ModelId("gpt-6-astra".into()))
            .unwrap();
        let call = ToolCall {
            id: octet_ai::ToolCallId("committed-original".into()),
            name: "read".into(),
            arguments_json: "{}".into(),
            argument_error: None,
            async_execution: true,
        };
        let mut writer = Session::create(&path).unwrap();
        writer
            .append(EntryValue::Message(Message::Assistant(AssistantMessage {
                model: model.spec.id.clone(),
                protocol: Protocol::OpenAiResponses,
                content: vec![AssistantPart::ToolCall(call.clone())],
            })))
            .unwrap();
        drop(writer);
        let mut session = Session::open_read_only(&path).unwrap();
        let (tx, rx) = mpsc::channel(1);
        let task = tokio::spawn(async move {
            completed_parallel_read_execution(
                Ok(ToolOutput::new("retained-completed-output")),
                None,
                rx,
                ToolProgressSink::live(tx),
                std::time::Instant::now(),
                false,
            )
        });
        let mut jobs = BackgroundTools {
            jobs: VecDeque::from([BackgroundJob {
                call,
                task: Some(task),
                completed: None,
            }]),
        };
        let sandbox = SandboxConfig::new(directory.path());
        let context = ContextTracker::default();
        let mut usage = Usage::default();
        for _ in 0..2 {
            assert!(jobs
                .settle_one(
                    &mut session,
                    &model,
                    &sandbox,
                    &context,
                    &mut usage,
                    &mut None
                )
                .await
                .is_err());
            let job = jobs.jobs.front().unwrap();
            assert!(job.task.is_none());
            assert_eq!(
                job.completed
                    .as_ref()
                    .unwrap()
                    .result
                    .as_ref()
                    .unwrap()
                    .text,
                "retained-completed-output"
            );
        }
        let (calls, results) = pending_tool_state(&session).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.0, "committed-original");
        assert!(results.is_empty());
        // A later authorized writer can persist the retained output once, with
        // no second task or effect execution.
        drop(session);
        let mut session = Session::open(&path).unwrap();
        jobs.settle_one(
            &mut session,
            &model,
            &sandbox,
            &context,
            &mut usage,
            &mut None,
        )
        .await
        .unwrap();
        assert!(jobs.is_empty());
        assert_eq!(pending_tool_state(&session).unwrap().1.len(), 1);
    }
}
