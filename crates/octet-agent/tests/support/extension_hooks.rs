use super::*;
use octet_agent::{
    AssistantPersistenceContext, PersistenceMetadataHook, PersistenceMetadataProposal,
};

#[derive(Clone)]
struct Annotation {
    value: serde_json::Value,
    public: bool,
    delay: Duration,
    contexts: Arc<std::sync::Mutex<Vec<AssistantPersistenceContext>>>,
}

#[async_trait::async_trait]
impl PersistenceMetadataHook for Annotation {
    async fn before_assistant_persist(
        &self,
        context: &AssistantPersistenceContext,
    ) -> Option<PersistenceMetadataProposal> {
        self.contexts.lock().unwrap().push(context.clone());
        tokio::time::sleep(self.delay).await;
        Some(PersistenceMetadataProposal::new(
            self.public,
            self.value.clone(),
        ))
    }
}

#[tokio::test]
async fn before_persistence_metadata_is_namespaced_durable_and_never_model_context() {
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut extensions = ExtensionHost::new();
    for (namespace, public, value) in [
        (
            "private.notes",
            false,
            serde_json::json!({"note":"private-sentinel"}),
        ),
        (
            "public.notes",
            true,
            serde_json::json!({"note":"public-sentinel"}),
        ),
        (
            "invalid.notes",
            true,
            serde_json::json!({"note":"\u{001b}[31m"}),
        ),
    ] {
        extensions.persistence_metadata_hook(
            namespace,
            Annotation {
                value,
                public,
                delay: Duration::ZERO,
                contexts: contexts.clone(),
            },
        );
    }
    let (mut agent, transport, _workspace) = operation_recovery_agent(
        vec![
            RecoveryStep::Reply("canonical-answer", Duration::ZERO),
            RecoveryStep::Reply("second-answer", Duration::ZERO),
        ],
        extensions,
    );
    agent.complete("first-prompt").await.unwrap();
    // Roadmap #265: the namespaced value is fixed *before* its own durable
    // append, and it is never rewritten afterwards. Each turn therefore
    // contributes exactly one occurrence of each value to the append-only log.
    let first_turn = agent
        .session()
        .entries()
        .iter()
        .find(|entry| {
            entry
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.extension_metadata.len() == 2)
        })
        .cloned()
        .expect("the first assistant turn carries both namespaces");
    let first_metadata = first_turn.metadata.clone().unwrap();
    let needles = first_metadata
        .extension_metadata
        .iter()
        .map(|(namespace, value)| {
            (
                namespace.clone(),
                serde_json::to_string(&value.value).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let durable_once = std::fs::read_to_string(agent.session().path()).unwrap();
    for (namespace, needle) in &needles {
        assert_eq!(
            durable_once.matches(needle.as_str()).count(),
            1,
            "{namespace} must be written with its own turn, exactly once"
        );
    }
    let entries = agent.session().entries();
    let metadata = entries
        .iter()
        .find_map(|entry| {
            entry
                .metadata
                .as_ref()
                .filter(|metadata| !metadata.extension_metadata.is_empty())
        })
        .unwrap();
    assert_eq!(metadata.extension_metadata.len(), 2);
    assert_eq!(
        metadata
            .public_extension_metadata()
            .keys()
            .collect::<Vec<_>>(),
        ["public.notes"]
    );
    for (namespace, value) in &metadata.extension_metadata {
        assert_eq!(&value.provenance.extension, namespace);
        assert_eq!(value.provenance.process_generation, None);
    }
    assert!(metadata.display_text.is_none());
    let observed = contexts.lock().unwrap();
    assert_eq!(observed.len(), 3);
    assert_eq!(observed[0].text_bytes, "canonical-answer".len());
    assert_eq!(observed[0].tool_call_count, 0);
    drop(observed);
    agent.complete("second-prompt").await.unwrap();
    let requests = format!("{:?}", transport.requests.lock().unwrap());
    assert!(!requests.contains("private-sentinel"));
    assert!(!requests.contains("public-sentinel"));
    assert!(requests.contains("canonical-answer"));
    let session_path = agent.session().path().to_owned();
    let durable_after = std::fs::read_to_string(&session_path).unwrap();
    drop(agent);
    let reopened = Session::open(session_path).unwrap();
    assert_eq!(
        reopened
            .entries()
            .iter()
            .filter(|entry| entry
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata.extension_metadata.len() == 2))
            .count(),
        2
    );
    // Roadmap #265, second half: never rewritten after persistence. Two
    // persisted turns leave exactly two copies of each value, and the first
    // turn's stored envelope is byte-identical after the second turn.
    for (namespace, needle) in &needles {
        assert_eq!(
            durable_after.matches(needle.as_str()).count(),
            2,
            "{namespace} must be appended once per turn and never rewritten"
        );
    }
    let rewritten = reopened
        .entries()
        .iter()
        .find(|entry| entry.id == first_turn.id)
        .expect("the first turn survives reopen");
    assert_eq!(
        rewritten.metadata.as_ref(),
        Some(&first_metadata),
        "metadata set before persistence must not change after later turns"
    );
    assert!(!format!("{:?}", reopened.context().unwrap()).contains("sentinel"));
}

#[tokio::test(start_paused = true)]
async fn before_persistence_metadata_timeout_is_non_veto_and_uses_one_aggregate_budget() {
    let contexts = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut extensions = ExtensionHost::new();
    for namespace in ["slow.first", "slow.second"] {
        extensions.persistence_metadata_hook(
            namespace,
            Annotation {
                value: serde_json::json!("must-not-persist"),
                public: true,
                delay: Duration::from_secs(30),
                contexts: contexts.clone(),
            },
        );
    }
    let (mut agent, _, _workspace) = operation_recovery_agent(
        vec![RecoveryStep::Reply("canonical", Duration::ZERO)],
        extensions,
    );
    let started = tokio::time::Instant::now();
    agent.complete("prompt").await.unwrap();
    assert_eq!(started.elapsed(), Duration::from_millis(200));
    assert_eq!(contexts.lock().unwrap().len(), 1);
    assert!(agent.session().entries().iter().all(|entry| entry
        .metadata
        .as_ref()
        .is_none_or(|metadata| metadata.extension_metadata.is_empty())));
    assert!(!std::fs::read_to_string(agent.session().path())
        .unwrap()
        .contains("must-not-persist"));
}

#[cfg(unix)]
#[tokio::test]
async fn sdk_progress_decoration_and_before_persistence_reach_native_agent_without_changing_results(
) {
    use octet_agent::extension_process::{
        DiscoveredExtension, ExtensionActivation, ExtensionManifest, ExtensionProcess,
        ExtensionRuntimeConfig, ExtensionSource, ExtensionTrust,
    };
    let root = tempfile::tempdir().unwrap();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extension_hooks.py");
    let log = root.path().join("hooks.jsonl");
    let manifest = ExtensionManifest::parse(&format!(
        r#"name = "hook-fixture"
version = "0.1.0"
api_version = "0.2"
[entrypoint]
command = "python3"
args = [{script:?}, {log:?}, "valid"]
[contributes]
tools = ["decorate"]
hooks = ["post_mutation", "before_persistence"]
"#
    ))
    .unwrap();
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: root.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        ExtensionRuntimeConfig::new(root.path()),
    )
    .await
    .unwrap();
    let mut extensions = ExtensionHost::new();
    extensions.load(&process);
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("messages"))
        .respond_with(Script {
            bodies: vec![
                scripted_tool_turn("decorate"),
                text_turn("canonical-answer"),
            ],
            next: AtomicUsize::new(0),
        })
        .mount(&server)
        .await;
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model: scripted_model(&server.uri()),
        session: Session::create(root.path().join("session.jsonl")).unwrap(),
        system: "system".into(),
        sandbox: SandboxConfig::new(root.path()),
        effect_broker: EffectBroker::new(EffectPolicy::UnsafeHost),
        extensions,
        max_turns: Some(4),
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: octet_ai::CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut run = agent.prompt("decorate").await.unwrap();
    let events = collect(&mut run).await;
    drop(run);
    assert!(matches!(
        assert_single_run_finished(&events),
        FinishReason::Completed
    ));
    let decorations = events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolProgress {
                progress: octet_agent::ToolProgress::Decoration(decoration),
                ..
            } => Some(decoration),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decorations.len(), 2);
    assert_eq!(decorations[0].label(), "ephemeral-label");
    assert_eq!(decorations[1].label().len(), 256);
    assert_eq!(decorations[1].detail().unwrap().len(), 4096);
    let durable = std::fs::read_to_string(agent.session().path()).unwrap();
    assert!(durable.contains("immutable-final-output"));
    assert!(durable.contains("private-marker"));
    assert!(!durable.contains("ephemeral-label"));
    assert!(!durable.contains("ephemeral-detail"));
    for entry in agent.session().entries() {
        if let Some(metadata) = &entry.metadata {
            for value in metadata.extension_metadata.values() {
                assert_eq!(value.provenance.extension, "hook-fixture");
                assert_eq!(value.provenance.process_generation, Some(1));
                assert!(!value.public);
            }
        }
    }
    let requests = format!("{:?}", server.received_requests().await.unwrap());
    assert!(!requests.contains("private-marker"));
    assert!(!requests.contains("ephemeral-label"));
    let records = std::fs::read_to_string(log).unwrap();
    assert!(
        !records.contains("canonical-answer"),
        "metadata hooks get counts, not content"
    );
    process.shutdown().await;
}
