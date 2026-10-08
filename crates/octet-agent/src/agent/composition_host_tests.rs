//! Parent-owned regression coverage for the generic composition capability.

use super::*;
use crate::effect::EffectPolicy;
use crate::tool_composition::{
    ToolCompositionConfig, ToolCompositionMode, ToolCompositionRecord, ToolCompositionService,
};
use serde_json::{json, Value};
use std::future::Future;
use std::sync::atomic::AtomicUsize;

struct Probe {
    name: String,
    effect: ToolEffect,
    parallel: bool,
    process: bool,
    composition: Option<ToolCompositionConfig>,
    usage: Option<Usage>,
    executions: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    delay: std::time::Duration,
}

impl Probe {
    fn new(name: &str, effect: ToolEffect) -> Self {
        Self {
            name: name.into(),
            effect,
            parallel: true,
            process: false,
            composition: None,
            usage: None,
            executions: Arc::new(AtomicUsize::new(0)),
            active: Arc::new(AtomicUsize::new(0)),
            peak: Arc::new(AtomicUsize::new(0)),
            delay: std::time::Duration::ZERO,
        }
    }
}

struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[async_trait::async_trait]
impl Tool for Probe {
    fn definition(&self) -> ToolDef {
        ToolDef {
            name: self.name.clone(),
            description: "Probe tool".into(),
            async_execution: false,
            constrained_sampling: None,
            parameters: json!({"type":"object","properties":{"value":{"type":"integer"}},"required":["value"],"additionalProperties":false}),
        }
    }
    fn effect(&self, _: &Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(self.effect)
    }
    fn concurrency(&self) -> ToolConcurrency {
        if self.process {
            ToolConcurrency::ParallelProcess
        } else if self.parallel {
            ToolConcurrency::Parallel
        } else {
            ToolConcurrency::Sequential
        }
    }
    fn composition_config(&self) -> Option<ToolCompositionConfig> {
        self.composition.clone()
    }
    fn composition_is_unmetered(&self) -> bool {
        self.usage.is_none()
    }
    fn output_schema(&self) -> Option<Value> {
        Some(json!({"type":"integer"}))
    }
    async fn execute(&self, args: Value, ctx: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        self.executions.fetch_add(1, Ordering::AcqRel);
        let count = self.active.fetch_add(1, Ordering::AcqRel) + 1;
        let _active = Active(self.active.clone());
        self.peak.fetch_max(count, Ordering::AcqRel);
        assert!(ctx.progress.is_programmatic());
        assert!(
            ctx.progress.composition_service().is_none(),
            "nested calls cannot compose recursively"
        );
        tokio::time::sleep(self.delay).await;
        let output = ToolOutput::new("private nested output")
            .try_with_programmatic_content(args["value"].clone())
            .map_err(|error| ToolError::new(error.to_string()))?;
        Ok(match self.usage {
            Some(usage) => output.with_usage(usage),
            None => output,
        })
    }
}

fn fixture() -> (tempfile::TempDir, Session, SandboxConfig, Model) {
    let directory = tempfile::tempdir().unwrap();
    let session = Session::create(directory.path().join("session.jsonl")).unwrap();
    let sandbox = SandboxConfig::new(directory.path());
    let model = octet_ai::ModelCatalog::builtin()
        .unwrap()
        .resolve(&octet_ai::ModelId("gpt-4o-mini".into()))
        .unwrap();
    (directory, session, sandbox, model)
}

fn scope(
    tools: Vec<Arc<dyn Tool>>,
    session: &Session,
    sandbox: SandboxConfig,
    model: Model,
    hooks: Vec<Arc<dyn ToolCallHook>>,
    broker: EffectBroker,
) -> (CompositionScope, mpsc::Receiver<ToolProgress>) {
    let (progress, receiver) = ToolProgressSink::bounded_channel();
    (
        CompositionDispatcher::scope(
            octet_ai::ToolCallId("outer".into()),
            "codemode".into(),
            tools,
            sandbox,
            "composition-test".into(),
            "owner".into(),
            "run:test".into(),
            1,
            Vec::new(),
            hooks,
            broker,
            progress,
            CancellationToken::default(),
            session,
            model,
            None,
            None,
        ),
        receiver,
    )
}

async fn journal<T>(
    future: impl Future<Output = T>,
    receiver: &mut mpsc::Receiver<ToolProgress>,
    session: &mut Session,
) -> T {
    tokio::pin!(future);
    let result = loop {
        tokio::select! {
            result = &mut future => break result,
            progress = receiver.recv() => if let Some(progress) = progress {
                if let ProgressSettlement::Emit(ToolProgress::Output { .. }) =
                    settle_tool_progress(progress, false, session)
                {
                    panic!("nested raw stdout leaked");
                }
            },
        }
    };
    while let Ok(progress) = receiver.try_recv() {
        settle_tool_progress(progress, false, session);
    }
    result
}

struct Veto(Arc<AtomicUsize>);
#[async_trait::async_trait]
impl ToolCallHook for Veto {
    async fn before_tool_call(
        &self,
        _: &str,
        _: &Value,
        _: &ToolContext<'_>,
    ) -> Result<(), ToolError> {
        self.0.fetch_add(1, Ordering::AcqRel);
        Err(ToolError::new("do not expose hook secrets"))
    }
    async fn after_tool_call(&self, _: &str, _: &Value, _: &str, _: bool, _: &ToolContext<'_>) {}
}

#[tokio::test]
async fn nested_schema_validation_precedes_hooks_and_effects() {
    let (_directory, mut session, sandbox, model) = fixture();
    let tool = Arc::new(Probe::new("probe", ToolEffect::Pure));
    let hooks = Arc::new(AtomicUsize::new(0));
    let (scope, mut receiver) = scope(
        vec![tool.clone()],
        &session,
        sandbox,
        model,
        vec![Arc::new(Veto(hooks.clone()))],
        EffectBroker::default(),
    );
    let error = journal(
        scope.0.call(
            "probe".into(),
            json!({"value":"wrong"}),
            CancellationToken::default(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap_err();
    assert_eq!(error.message, SCHEMA_MISMATCH_TOOL_ERROR);
    assert_eq!(hooks.load(Ordering::Acquire), 0);
    assert_eq!(tool.executions.load(Ordering::Acquire), 0);
    let error = journal(
        scope.0.call(
            "probe".into(),
            json!({"value":2}),
            CancellationToken::default(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap_err();
    assert_eq!(hooks.load(Ordering::Acquire), 1);
    assert_eq!(tool.executions.load(Ordering::Acquire), 0);
    assert!(!error.message.contains("secrets"));
    let receipt = session
        .entries()
        .last()
        .unwrap()
        .metadata
        .as_ref()
        .unwrap()
        .tool_composition
        .as_ref()
        .unwrap();
    assert!(matches!(
        receipt,
        ToolCompositionRecord::CallFinished {
            allowed: false,
            denial_code: Some(ToolPolicyDenialCode::SecondaryHookDenied),
            ..
        }
    ));
}

#[tokio::test]
async fn controlled_policy_and_frozen_catalog_cannot_be_bypassed() {
    let (_directory, mut session, sandbox, model) = fixture();
    let process = Arc::new(Probe::new("process", ToolEffect::HostProcess));
    let (scope, mut receiver) = scope(
        vec![process.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    assert_eq!(
        scope.0.context().await.unwrap()["tools"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(journal(
        scope.0.call(
            "excluded_write".into(),
            json!({"value":1}),
            CancellationToken::default()
        ),
        &mut receiver,
        &mut session
    )
    .await
    .is_err());
    assert!(journal(
        scope.0.call(
            "process".into(),
            json!({"value":1}),
            CancellationToken::default()
        ),
        &mut receiver,
        &mut session
    )
    .await
    .is_err());
    assert_eq!(process.executions.load(Ordering::Acquire), 0);
    assert!(
        session.context().unwrap().is_empty(),
        "private receipts are not model messages"
    );
}

#[tokio::test]
async fn safe_reads_overlap_at_most_four_and_mutations_are_exclusive() {
    let (_directory, mut session, sandbox, model) = fixture();
    let mut read = Probe::new("read_probe", ToolEffect::WorkspaceRead);
    read.delay = std::time::Duration::from_millis(20);
    let mut mutation = Probe::new("mutation_probe", ToolEffect::WorkspaceMutation);
    mutation.parallel = false;
    mutation.delay = read.delay;
    mutation.active = read.active.clone();
    mutation.peak = read.peak.clone();
    let read = Arc::new(read);
    let mutation = Arc::new(mutation);
    let (scope, mut receiver) = scope(
        vec![read.clone(), mutation.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::new(EffectPolicy::UnsafeHost),
    );
    let reads = (0..12).map(|value| {
        scope.0.call(
            "read_probe".into(),
            json!({"value":value}),
            CancellationToken::default(),
        )
    });
    let results = journal(
        futures_util::future::join_all(reads),
        &mut receiver,
        &mut session,
    )
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(read.peak.load(Ordering::Acquire), 4);
    read.peak.store(0, Ordering::Release);
    let writes = (0..3).map(|value| {
        scope.0.call(
            "mutation_probe".into(),
            json!({"value":value}),
            CancellationToken::default(),
        )
    });
    let results = journal(
        futures_util::future::join_all(writes),
        &mut receiver,
        &mut session,
    )
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(read.peak.load(Ordering::Acquire), 1);
    assert!(session.context().unwrap().is_empty());
}

#[tokio::test]
async fn independent_processes_overlap_like_reads_under_full_access() {
    let (_directory, mut session, sandbox, model) = fixture();
    let mut process = Probe::new("process_probe", ToolEffect::HostProcess);
    process.process = true;
    process.delay = std::time::Duration::from_millis(20);
    let process = Arc::new(process);
    let (scope, mut receiver) = scope(
        vec![process.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::new(EffectPolicy::UnsafeHost),
    );
    let calls = (0..8).map(|value| {
        scope.0.call(
            "process_probe".into(),
            json!({"value":value}),
            CancellationToken::default(),
        )
    });
    let results = journal(
        futures_util::future::join_all(calls),
        &mut receiver,
        &mut session,
    )
    .await;
    assert!(results.iter().all(Result::is_ok));
    assert_eq!(process.executions.load(Ordering::Acquire), 8);
    assert_eq!(process.peak.load(Ordering::Acquire), 4);
}

#[tokio::test]
async fn cancellation_and_parent_settlement_revoke_nested_execution() {
    let (_directory, mut session, sandbox, model) = fixture();
    let mut probe = Probe::new("probe", ToolEffect::Pure);
    probe.delay = std::time::Duration::from_secs(60);
    let probe = Arc::new(probe);
    let (scope, mut receiver) = scope(
        vec![probe.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    let cancellation = CancellationToken::default();
    let cancel = cancellation.clone();
    let calls = async {
        let result = scope
            .0
            .call("probe".into(), json!({"value":1}), cancellation);
        let trigger = async {
            while probe.active.load(Ordering::Acquire) == 0 {
                tokio::task::yield_now().await;
            }
            cancel.cancel();
        };
        let (result, ()) = tokio::join!(result, trigger);
        result
    };
    assert!(journal(calls, &mut receiver, &mut session).await.is_err());
    assert_eq!(probe.active.load(Ordering::Acquire), 0);
    let service = scope.0.clone();
    drop(scope);
    assert!(service.context().await.is_err());
    assert!(service
        .call(
            "probe".into(),
            json!({"value":2}),
            CancellationToken::default()
        )
        .await
        .is_err());
    assert_eq!(probe.executions.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn host_enforces_256_calls_even_when_the_extension_ignores_its_limit() {
    let (_directory, mut session, sandbox, model) = fixture();
    let probe = Arc::new(Probe::new("probe", ToolEffect::Pure));
    let (scope, mut receiver) = scope(
        vec![probe.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    for value in 0..256 {
        assert_eq!(
            journal(
                scope.0.call(
                    "probe".into(),
                    json!({"value":value}),
                    CancellationToken::default()
                ),
                &mut receiver,
                &mut session
            )
            .await
            .unwrap(),
            json!(value)
        );
    }
    assert!(scope
        .0
        .call(
            "probe".into(),
            json!({"value":256}),
            CancellationToken::default()
        )
        .await
        .is_err());
    assert_eq!(probe.executions.load(Ordering::Acquire), 256);
    let ids = session
        .entries()
        .iter()
        .filter_map(|entry| {
            match entry
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.tool_composition.as_ref())
            {
                Some(ToolCompositionRecord::CallStarted { id, .. }) => Some(id.clone()),
                _ => None,
            }
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 256);
}

#[tokio::test]
async fn private_store_survives_reopen_and_obeys_branch_lineage() {
    let (directory, mut session, sandbox, model) = fixture();
    let root = session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    let (first, mut receiver) = scope(
        Vec::new(),
        &session,
        sandbox.clone(),
        model.clone(),
        Vec::new(),
        EffectBroker::default(),
    );
    let set = json!({"answer":{"value":42},"multiline":"a\nb"})
        .as_object()
        .unwrap()
        .clone();
    journal(first.0.store(set, Vec::new()), &mut receiver, &mut session)
        .await
        .unwrap();
    let saved = session.head().unwrap();
    assert!(
        first.0.store(Default::default(), Vec::new()).await.is_err(),
        "store may commit once only"
    );
    drop(first);
    drop(session);
    let mut session = Session::open(directory.path().join("session.jsonl")).unwrap();
    let (restored, _) = scope(
        Vec::new(),
        &session,
        sandbox.clone(),
        model.clone(),
        Vec::new(),
        EffectBroker::default(),
    );
    assert_eq!(
        restored.0.context().await.unwrap()["store"]["answer"]["value"],
        42
    );
    drop(restored);
    let inherited = session
        .fork_to(directory.path().join("fork.jsonl"), Some(&saved))
        .unwrap();
    assert_eq!(
        crate::tool_composition::restore_store(&inherited, "codemode")["answer"]["value"],
        42
    );
    session.checkout(root).unwrap();
    let (sibling, _) = scope(
        Vec::new(),
        &session,
        sandbox.clone(),
        model.clone(),
        Vec::new(),
        EffectBroker::default(),
    );
    assert!(sibling.0.context().await.unwrap()["store"]
        .as_object()
        .unwrap()
        .is_empty());
    drop(sibling);
    session.checkout(saved).unwrap();
    assert_eq!(
        crate::tool_composition::restore_store(&session, "codemode")["multiline"],
        "a\nb"
    );
    assert!(session.context().unwrap().is_empty());
}

#[tokio::test]
async fn compacted_forks_inherit_store_without_copying_discarded_messages() {
    let (directory, mut session, sandbox, model) = fixture();
    let old = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("discarded source message".into())],
        })))
        .unwrap();
    let (scope, mut receiver) = scope(
        Vec::new(),
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    journal(
        scope.0.store(
            json!({"answer":42}).as_object().unwrap().clone(),
            Vec::new(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap();
    let kept = session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("kept message".into())],
        })))
        .unwrap();
    let compacted = session
        .append(EntryValue::Compaction {
            summary: "summary".into(),
            snapcompact: None,
            first_kept: kept,
            active_skills: Vec::new(),
            skill_resources: Vec::new(),
            details: Default::default(),
        })
        .unwrap();
    let path = directory.path().join("compacted-fork.jsonl");
    let fork = session.fork_to(&path, Some(&compacted)).unwrap();
    assert_eq!(
        crate::tool_composition::restore_store(&fork, "codemode")["answer"],
        42
    );
    assert!(fork.entry(&old).is_none());
    assert!(!std::fs::read_to_string(path)
        .unwrap()
        .contains("discarded source message"));
    assert_eq!(
        serde_json::to_value(fork.context().unwrap()).unwrap(),
        serde_json::to_value(session.context().unwrap()).unwrap()
    );
}

#[tokio::test(start_paused = true)]
async fn the_host_deadline_drops_infinite_nested_work_without_extension_cooperation() {
    let (_directory, mut session, sandbox, model) = fixture();
    let mut probe = Probe::new("probe", ToolEffect::Pure);
    probe.delay = std::time::Duration::from_secs(60);
    let probe = Arc::new(probe);
    let (scope, mut receiver) = scope(
        vec![probe.clone()],
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    let error = journal(
        scope.0.call(
            "probe".into(),
            json!({"value":1}),
            CancellationToken::default(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("30 second host deadline"));
    assert_eq!(probe.active.load(Ordering::Acquire), 0);
    assert_eq!(probe.executions.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn cancellation_winning_before_store_acceptance_discards_all_writes() {
    let (_directory, mut session, sandbox, model) = fixture();
    let (scope, mut receiver) = scope(
        Vec::new(),
        &session,
        sandbox,
        model,
        Vec::new(),
        EffectBroker::default(),
    );
    let store = scope.0.store(
        json!({"discard":true}).as_object().unwrap().clone(),
        Vec::new(),
    );
    tokio::pin!(store);
    futures_util::future::poll_fn(|cx| {
        assert!(store.as_mut().poll(cx).is_pending());
        std::task::Poll::Ready(())
    })
    .await;
    let progress = receiver.recv().await.unwrap();
    assert!(matches!(
        settle_tool_progress(progress, true, &mut session),
        ProgressSettlement::Cancelled
    ));
    assert!(store.await.is_err());
    assert!(session.entries().is_empty());
    assert!(scope.0.context().await.unwrap()["store"]
        .as_object()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn failed_script_usage_is_billed_durably_and_cannot_reset_the_next_script_budget() {
    let (directory, mut session, sandbox, model) = fixture();
    let usage = Usage {
        output_tokens: 107,
        ..Usage::default()
    };
    let mut probe = Probe::new("probe", ToolEffect::Pure);
    probe.usage = Some(usage);
    let probe = Arc::new(probe);
    let (scope, mut receiver) = scope(
        vec![probe.clone()],
        &session,
        sandbox.clone(),
        model.clone(),
        Vec::new(),
        EffectBroker::default(),
    );
    journal(
        scope.0.call(
            "probe".into(),
            json!({"value":1}),
            CancellationToken::default(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap();
    let output = scope
        .0
        .collect_usage(Err(ToolError::new("script threw after the billed call")))
        .unwrap();
    assert!(output.is_error());
    assert_eq!(output.usage().unwrap().output_tokens, 107);
    session
        .record_tool_composition_usage("outer".into(), *output.usage().unwrap())
        .unwrap();
    assert_eq!(session_total_tokens_for_own_context(&session), 107);
    assert!(session.context().unwrap().is_empty());
    assert_eq!(session.total_cost_microdollars(), 0);
    assert!(session.has_unpriced_usage());
    assert!(session.usage_records()[0].model.is_none());
    assert!(session.usage_records()[0].endpoint.is_none());
    let reopened = Session::open(directory.path().join("session.jsonl")).unwrap();
    assert_eq!(session_total_tokens_for_own_context(&reopened), 107);
    for (tokens, dollars) in [(Some(107), None), (None, Some(1000)), (Some(1000), None)] {
        let (progress, mut receiver) = ToolProgressSink::bounded_channel();
        let next = CompositionDispatcher::scope(
            octet_ai::ToolCallId("next".into()),
            "codemode".into(),
            vec![probe.clone()],
            sandbox.clone(),
            "test".into(),
            "owner".into(),
            "run:test".into(),
            1,
            Vec::new(),
            Vec::new(),
            EffectBroker::default(),
            progress,
            CancellationToken::default(),
            &session,
            model.clone(),
            tokens,
            dollars,
        );
        assert!(journal(
            next.0.call(
                "probe".into(),
                json!({"value":2}),
                CancellationToken::default()
            ),
            &mut receiver,
            &mut session
        )
        .await
        .unwrap_err()
        .message
        .contains("session"));
    }
    assert_eq!(probe.executions.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn core_tools_keep_programmatic_results_under_hard_session_ceilings() {
    use crate::tools::{
        BashTool, EditTool, PowerShellTool, ReadTool, SearchTool, ShellSessionEnvironment,
        WriteTool,
    };
    let shell = BashTool::with_session_environment(ShellSessionEnvironment::default);
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadTool),
        Arc::new(SearchTool),
        Arc::new(BashTool),
        Arc::new(EditTool),
        Arc::new(WriteTool),
        Arc::new(PowerShellTool::default()),
        Arc::new(shell),
    ];
    assert!(tools.iter().all(|tool| tool.composition_is_unmetered()));
    assert_eq!(
        tools.last().unwrap().output_schema(),
        BashTool.output_schema()
    );
    let (directory, mut session, mut sandbox, model) = fixture();
    sandbox.workspace = directory.path().canonicalize().unwrap();
    std::fs::write(directory.path().join("data.txt"), "budgeted read\n").unwrap();
    let (progress, mut receiver) = ToolProgressSink::bounded_channel();
    let scope = CompositionDispatcher::scope(
        octet_ai::ToolCallId("budgeted".into()),
        "codemode".into(),
        vec![tools[0].clone()],
        sandbox,
        "test".into(),
        "owner".into(),
        "run:test".into(),
        1,
        Vec::new(),
        Vec::new(),
        EffectBroker::default(),
        progress,
        CancellationToken::default(),
        &session,
        model,
        Some(1000),
        Some(1000),
    );
    let value = journal(
        scope.0.call(
            "read".into(),
            json!({"path":"data.txt"}),
            CancellationToken::default(),
        ),
        &mut receiver,
        &mut session,
    )
    .await
    .unwrap();
    assert_eq!(value["content"], "budgeted read\n");
    assert_eq!(session_total_tokens_for_own_context(&session), 0);
    assert!(session.context().unwrap().is_empty());
}

struct LeaseProbe {
    text: String,
    deliveries: Arc<AtomicUsize>,
    rollbacks: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Tool for LeaseProbe {
    fn definition(&self) -> ToolDef {
        Probe::new("lease", ToolEffect::Pure).definition()
    }
    fn effect(&self, _: &Value, _: &ToolContext<'_>) -> Result<ToolEffect, ToolError> {
        Ok(ToolEffect::Pure)
    }
    async fn execute(&self, _: Value, _: &ToolContext<'_>) -> Result<ToolOutput, ToolError> {
        let delivered = self.deliveries.clone();
        let rolled_back = self.rollbacks.clone();
        Ok(ToolOutput::new(self.text.clone()).with_delivery_commit(
            move || {
                delivered.fetch_add(1, Ordering::AcqRel);
            },
            move || {
                rolled_back.fetch_add(1, Ordering::AcqRel);
            },
        ))
    }
}

#[tokio::test]
async fn leased_nested_values_are_returned_only_after_an_exact_private_durable_receipt() {
    for bytes in [64, 256] {
        let (_directory, mut session, mut sandbox, model) = fixture();
        sandbox.max_output_bytes = 128;
        let tool = Arc::new(LeaseProbe {
            text: "x".repeat(bytes),
            deliveries: Arc::new(AtomicUsize::new(0)),
            rollbacks: Arc::new(AtomicUsize::new(0)),
        });
        let (scope, mut receiver) = scope(
            vec![tool.clone()],
            &session,
            sandbox,
            model,
            Vec::new(),
            EffectBroker::default(),
        );
        let result = journal(
            scope.0.call(
                "lease".into(),
                json!({"value":1}),
                CancellationToken::default(),
            ),
            &mut receiver,
            &mut session,
        )
        .await;
        let receipt = session
            .entries()
            .iter()
            .find_map(|entry| {
                entry
                    .metadata
                    .as_ref()?
                    .tool_composition
                    .as_ref()
                    .filter(|receipt| matches!(receipt, ToolCompositionRecord::CallFinished { .. }))
            })
            .unwrap();
        if bytes <= 128 {
            assert_eq!(result.unwrap(), Value::String(tool.text.clone()));
            assert_eq!(tool.deliveries.load(Ordering::Acquire), 1);
            assert_eq!(tool.rollbacks.load(Ordering::Acquire), 0);
            assert!(
                matches!(receipt, ToolCompositionRecord::CallFinished { delivery_text:Some(text), ok:true, .. } if text == &tool.text)
            );
        } else {
            assert!(result.unwrap_err().message.contains("receipt limit"));
            assert_eq!(tool.deliveries.load(Ordering::Acquire), 0);
            assert_eq!(tool.rollbacks.load(Ordering::Acquire), 1);
            assert!(matches!(
                receipt,
                ToolCompositionRecord::CallFinished {
                    delivery_text: None,
                    ok: false,
                    ..
                }
            ));
        }
        assert!(session.context().unwrap().is_empty());
    }
}

#[test]
fn only_mode_changes_advertising_not_the_authority_snapshot_and_aliases_match_pi() {
    let normal: Arc<dyn Tool> = Arc::new(Probe::new("my-tool", ToolEffect::Pure));
    let shadowed: Arc<dyn Tool> = Arc::new(Probe::new("my_tool", ToolEffect::Pure));
    let mut composing = Probe::new("codemode", ToolEffect::Pure);
    composing.composition = Some(ToolCompositionConfig {
        mode: ToolCompositionMode::Only,
        inline_budget: 3000,
    });
    let composing: Arc<dyn Tool> = Arc::new(composing);
    let tools = vec![normal, shadowed, composing];
    let definitions = advertised_tool_surface(&tools, &fixture().3);
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, "codemode");
    assert!(
        definitions[0].description.contains("1 of 2"),
        "shadowed aliases must not advertise the wrong target"
    );
    assert_eq!(crate::tool_composition::direct_surface(&tools).len(), 1);
    assert_eq!(crate::tool_composition::callable_catalog(&tools).len(), 2);
    assert_eq!(crate::tool_composition::identifier("9abc"), "_abc");
}

#[test]
fn transient_programmatic_content_never_reaches_presentation_or_debug() {
    let output = ToolOutput::new("summary")
        .try_with_programmatic_content(json!({"private":"not in presentation"}))
        .unwrap();
    assert!(output.programmatic_content().is_some());
    assert!(output
        .without_media_payloads()
        .programmatic_content()
        .is_none());
    assert!(!format!("{output:?}").contains("not in presentation"));
}
