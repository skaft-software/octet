//! Parallel waves of read-only tool calls.

use super::*;

pub(super) struct DeferredParallelAfterToolCall {
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
    pub(super) progress_sink: ToolProgressSink,
}

pub(super) struct ParallelReadWaveExecution {
    pub(super) execution: CompletedToolExecution,
    pub(super) after: Option<DeferredParallelAfterToolCall>,
}

impl ParallelReadWaveExecution {
    fn with_after(mut self: Box<Self>, name: &str, arguments: &serde_json::Value) -> Box<Self> {
        self.after = Some(DeferredParallelAfterToolCall {
            name: name.to_owned(),
            arguments: arguments.clone(),
            progress_sink: self.execution.progress_sink.clone(),
        });
        self
    }
}

pub(super) struct AdmittedParallelReadCall {
    pub(super) tool: Arc<dyn Tool>,
    pub(super) name: String,
    pub(super) arguments: serde_json::Value,
    pub(super) execute_arguments: serde_json::Value,
    pub(super) progress_rx: mpsc::Receiver<ToolProgress>,
    pub(super) progress_sink: ToolProgressSink,
    pub(super) policy_decision: ToolPolicyDecision,
    pub(super) start: std::time::Instant,
    pub(super) started_unix_ms: u64,
}

pub(super) enum ParallelReadPreparation {
    Admitted(Box<AdmittedParallelReadCall>),
    Completed(Box<ParallelReadWaveExecution>),
}

pub(super) fn advertised_tool_surface(tools: &[Arc<dyn Tool>], model: &Model) -> Vec<ToolDef> {
    let mut definitions = crate::tool_composition::advertised_surface(tools);
    if model.responses_features().async_tools {
        for definition in &mut definitions {
            if tools.iter().any(|tool| {
                tool.definition().name == definition.name
                    && tool.concurrency() == ToolConcurrency::Parallel
            }) {
                definition.async_execution = true;
            }
        }
    }
    definitions
}

pub(super) fn advertised_tool_definition(tool: &dyn Tool, model: &Model) -> ToolDef {
    let mut definition = tool.definition();
    // Static parallel capability permits scheduling hints, never effects.
    // Exact argument classification and broker admission still gate dispatch.
    if model.responses_features().async_tools && tool.concurrency() == ToolConcurrency::Parallel {
        definition.async_execution = true;
    }
    definition
}

pub(super) fn parallel_read_candidate(
    call: &ToolCall,
    call_index: usize,
    answer_only: bool,
    output_truncated: bool,
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    context: &ToolContext<'_>,
) -> bool {
    call_index < MAX_TOOL_CALLS_PER_TURN
        && !answer_only
        && !output_truncated
        && call.argument_error.is_none()
        && call.arguments_value().is_ok_and(|arguments| {
            tool_map.get(&call.name).is_some_and(|tool| {
                tool.concurrency() == ToolConcurrency::Parallel
                    && tool
                        .effect(&arguments, context)
                        .is_ok_and(effect_is_parallel_observation)
            })
        })
}

pub(super) fn completed_parallel_read_execution(
    result: Result<ToolOutput, ToolError>,
    policy_decision: Option<ToolPolicyDecision>,
    progress_rx: mpsc::Receiver<ToolProgress>,
    progress_sink: ToolProgressSink,
    start: std::time::Instant,
    cancellation_won: bool,
) -> Box<ParallelReadWaveExecution> {
    Box::new(ParallelReadWaveExecution {
        execution: CompletedToolExecution {
            result,
            policy_decision,
            duration: start.elapsed(),
            started_unix_ms: None,
            finished_unix_ms: Some(crate::session::now_unix_millis()),
            progress_rx,
            progress_sink,
            cancellation_won,
        },
        after: None,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_parallel_read_call(
    invocation: InvocationHandle,
    tool: Arc<dyn Tool>,
    hooks: &[Arc<dyn ToolCallHook>],
    broker: &EffectBroker,
    run_id: &str,
    generation: u64,
    request_id: &octet_ai::ToolCallId,
    name: &str,
    arguments: serde_json::Value,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> ParallelReadPreparation {
    let start = std::time::Instant::now();
    let (progress_tx, progress_rx) = mpsc::channel::<ToolProgress>(PROGRESS_CHANNEL_CAPACITY);
    let progress_sink = ToolProgressSink::live(progress_tx)
        .with_invocation(invocation)
        .with_tool_call_identity(request_id.0.clone(), None);
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink.clone(),
        cancellation: cancellation.clone(),
    };

    let arguments =
        match transform_tool_arguments(hooks, tool.as_ref(), name, arguments.clone(), &tool_ctx)
            .await
        {
            Ok(arguments) => arguments,
            Err(error) => {
                let cancellation_won = cancellation.is_cancelled();
                let (result, decision) = if cancellation_won {
                    (Err(cancelled_tool_error()), None)
                } else {
                    (
                        Err(error),
                        Some(secondary_hook_denial(sandbox, broker, None).1),
                    )
                };
                return ParallelReadPreparation::Completed(
                    completed_parallel_read_execution(
                        result,
                        decision,
                        progress_rx,
                        progress_sink,
                        start,
                        cancellation_won,
                    )
                    .with_after(name, &arguments),
                );
            }
        };
    let admission = reserve_tool_effect(
        broker,
        tool.as_ref(),
        name,
        &arguments,
        &tool_ctx,
        resource_owner,
        run_id,
        generation,
        request_id,
        false,
    )
    .await;
    let ToolEffectAdmission {
        intent,
        reservation: effect_reservation,
        effect,
    } = match admission {
        Ok(admission) => admission,
        Err(ToolEffectAdmissionError { error, decision }) => {
            let cancellation_won = cancellation.is_cancelled();
            let result = if cancellation_won {
                Err(cancelled_tool_error())
            } else {
                Err(error)
            };
            return ParallelReadPreparation::Completed(
                completed_parallel_read_execution(
                    result,
                    Some(decision),
                    progress_rx,
                    progress_sink,
                    start,
                    cancellation_won,
                )
                .with_after(name, &arguments),
            );
        }
    };

    let mut hook_denial = None;
    let mut cancellation_won = false;
    for hook in hooks {
        let hook_result = tokio::select! {
            biased;
            _ = cancellation.cancelled() => None,
            result = hook.before_tool_call(name, &arguments, &tool_ctx) => Some(result),
        };
        let Some(hook_result) = hook_result else {
            cancellation_won = true;
            break;
        };
        // A hook can synchronously cause cancellation while returning. The
        // level-triggered check keeps cancellation ahead of a same-poll denial.
        if cancellation.is_cancelled() {
            cancellation_won = true;
            break;
        }
        if hook_result.is_err() {
            hook_denial = Some(());
            break;
        }
    }
    if cancellation_won || cancellation.is_cancelled() {
        return ParallelReadPreparation::Completed(completed_parallel_read_execution(
            Err(cancelled_tool_error()),
            None,
            progress_rx,
            progress_sink,
            start,
            true,
        ));
    }
    if hook_denial.is_some() {
        let (error, decision) = secondary_hook_denial(sandbox, broker, Some(effect));
        return ParallelReadPreparation::Completed(
            completed_parallel_read_execution(
                Err(error),
                Some(decision),
                progress_rx,
                progress_sink,
                start,
                false,
            )
            .with_after(name, &arguments),
        );
    }
    if cancellation.is_cancelled() {
        return ParallelReadPreparation::Completed(completed_parallel_read_execution(
            Err(cancelled_tool_error()),
            None,
            progress_rx,
            progress_sink,
            start,
            true,
        ));
    }

    // Preserve the original hook arguments while completing the potentially
    // large execution allocation before the reservation is consumed.
    let execute_arguments = arguments.clone();
    let receipt = match effect_reservation.commit(&intent) {
        Ok(receipt) => receipt,
        Err(error) => {
            let (error, decision) =
                effect_reservation_commit_denial(sandbox, broker, effect, &error);
            return ParallelReadPreparation::Completed(
                completed_parallel_read_execution(
                    Err(error),
                    Some(decision),
                    progress_rx,
                    progress_sink,
                    start,
                    false,
                )
                .with_after(name, &arguments),
            );
        }
    };
    let policy_decision = policy_decision(
        sandbox,
        broker,
        Some(effect),
        Some(receipt.authorization()),
        None,
    );
    let started_unix_ms = crate::session::now_unix_millis();
    ParallelReadPreparation::Admitted(Box::new(AdmittedParallelReadCall {
        tool,
        name: name.to_owned(),
        arguments,
        execute_arguments,
        progress_rx,
        progress_sink,
        policy_decision,
        start,
        started_unix_ms,
    }))
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_admitted_parallel_read(
    admitted: AdmittedParallelReadCall,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> ParallelReadWaveExecution {
    let AdmittedParallelReadCall {
        tool,
        name,
        arguments,
        execute_arguments,
        progress_rx,
        progress_sink,
        policy_decision,
        start,
        started_unix_ms,
    } = admitted;
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink.clone(),
        cancellation: cancellation.clone(),
    };
    let execute = tool.execute(execute_arguments, &tool_ctx);
    tokio::pin!(execute);
    let mut cancellation_won = false;
    let execution_result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            cancellation_won = true;
            Err(cancelled_tool_error())
        }
        result = &mut execute => result,
    };
    let result = if cancellation_won || cancellation.is_cancelled() {
        cancellation_won = true;
        Err(cancelled_tool_error())
    } else {
        execution_result
    };
    ParallelReadWaveExecution {
        execution: CompletedToolExecution {
            result,
            policy_decision: Some(policy_decision),
            duration: start.elapsed(),
            started_unix_ms: Some(started_unix_ms),
            finished_unix_ms: Some(crate::session::now_unix_millis()),
            progress_rx,
            progress_sink: progress_sink.clone(),
            cancellation_won,
        },
        after: Some(DeferredParallelAfterToolCall {
            name,
            arguments,
            progress_sink,
        }),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_parallel_read_wave(
    calls: &[ToolCall],
    invocations: &[InvocationHandle],
    tool_map: &HashMap<String, Arc<dyn Tool>>,
    hooks: &[Arc<dyn ToolCallHook>],
    broker: &EffectBroker,
    run_id: &str,
    generation: u64,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> Vec<ParallelReadWaveExecution> {
    let mut results: Vec<Option<ParallelReadWaveExecution>> =
        (0..calls.len()).map(|_| None).collect();
    let mut executions = futures_util::stream::FuturesUnordered::new();

    for (index, call) in calls.iter().enumerate() {
        let arguments = call
            .arguments_value()
            .expect("parallel read wave validates arguments before admission");
        let prepared = prepare_parallel_read_call(
            invocations[index].clone(),
            Arc::clone(
                tool_map
                    .get(&call.name)
                    .expect("parallel read wave validates registered tools"),
            ),
            hooks,
            broker,
            run_id,
            generation,
            &call.id,
            &call.name,
            arguments,
            sandbox,
            tool_scope,
            resource_owner,
            active_skills,
            registered_tools,
            cancellation.clone(),
        )
        .await;
        match prepared {
            ParallelReadPreparation::Completed(execution) => results[index] = Some(*execution),
            ParallelReadPreparation::Admitted(admitted) => {
                let execution_cancellation = cancellation.clone();
                executions.push(async move {
                    (
                        index,
                        execute_admitted_parallel_read(
                            *admitted,
                            sandbox,
                            tool_scope,
                            resource_owner,
                            active_skills,
                            registered_tools,
                            execution_cancellation,
                        )
                        .await,
                    )
                });
                // Poll once after each commit so dispatch is not deferred until
                // all reservations in this wave have been consumed. This is a
                // deterministic executor handoff, not a timing-based delay.
                let _ = futures_util::future::poll_fn(|cx| {
                    match Pin::new(&mut executions).poll_next(cx) {
                        std::task::Poll::Ready(Some((index, execution))) => {
                            results[index] = Some(execution);
                            std::task::Poll::Ready(())
                        }
                        _ => std::task::Poll::Ready(()),
                    }
                })
                .await;
            }
        }
    }
    while let Some((index, execution)) = executions.next().await {
        results[index] = Some(execution);
    }
    results
        .into_iter()
        .map(|execution| execution.expect("parallel read wave produces one result per call"))
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_parallel_after_tool_hooks(
    after: DeferredParallelAfterToolCall,
    hooks: &[Arc<dyn ToolCallHook>],
    result: Result<ToolOutput, ToolError>,
    sandbox: &SandboxConfig,
    tool_scope: &str,
    resource_owner: &str,
    active_skills: &[crate::session::SkillActivatedSnapshot],
    registered_tools: &[String],
    cancellation: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    let DeferredParallelAfterToolCall {
        name,
        arguments,
        progress_sink,
    } = after;
    let tool_ctx = ToolContext {
        workspace: &sandbox.workspace,
        sandbox,
        execution_scope: tool_scope,
        resource_owner,
        active_skills,
        registered_tools,
        progress: progress_sink,
        cancellation,
    };
    settle_tool_result_hooks(hooks, &name, &arguments, result, &tool_ctx).await
}
