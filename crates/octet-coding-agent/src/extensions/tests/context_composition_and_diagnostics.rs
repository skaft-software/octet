//! What text goes into and out of the model's context, and the diagnostic ring.
//!
//! Covers typed, deterministic, bounded context composition, refusal of
//! oversized context before prompting, assistant-message text extraction dropping
//! reasoning and tool calls, the byte- and count-bounded diagnostics history, a
//! process event flood that must not grow either unbounded, invalid and
//! overflowing context items being dropped rather than bricking the prompt path,
//! and pending context committing only after the prompt append succeeds.

use super::*;

#[test]
fn context_composition_is_typed_deterministic_and_bounded() {
    let contributions = vec![
        ContextContribution {
            label: "rules".into(),
            content: "stay local".into(),
            placement: ContextPlacement::SystemPrefix,
        },
        ContextContribution {
            label: "repo".into(),
            content: "workspace facts".into(),
            placement: ContextPlacement::PromptSuffix,
        },
    ];
    let (system, prompt) = compose_context("system", "prompt".into(), contributions).unwrap();
    assert!(system.starts_with("<octet-extension-context label=\"rules\">"));
    assert!(system.ends_with("system"));
    assert!(prompt.starts_with("prompt"));
    assert!(prompt.ends_with("</octet-extension-context>"));
}

#[test]
fn oversized_context_is_rejected_before_prompting() {
    let result = compose_context(
        "",
        String::new(),
        vec![ContextContribution {
            label: "large".into(),
            content: "x".repeat(MAX_CONTEXT_CONTRIBUTION_BYTES + 1),
            placement: ContextPlacement::PromptSuffix,
        }],
    );
    assert!(result.is_err());
}

#[test]
fn diagnostics_history_is_byte_and_count_bounded() {
    let mut diagnostics = BoundedDiagnostics::default();
    for index in 0..(MAX_DIAGNOSTIC_ENTRIES * 2) {
        diagnostics.push(format!(
            "diagnostic-{index}:{}",
            "x".repeat(MAX_DIAGNOSTIC_ENTRY_BYTES * 2)
        ));
    }

    assert!(diagnostics.len() <= MAX_DIAGNOSTIC_ENTRIES);
    assert!(diagnostics.retained_bytes() <= MAX_DIAGNOSTIC_BYTES);
    assert!(diagnostics
        .iter()
        .all(|message| message.len() <= MAX_DIAGNOSTIC_ENTRY_BYTES));
    assert!(diagnostics
        .iter()
        .all(|message| message.ends_with("[… diagnostic truncated …]")));
    assert!(diagnostics.dropped > 0);
}

#[cfg(unix)]
#[tokio::test]
async fn process_event_flood_keeps_context_and_diagnostics_bounded() {
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("bounded-events-fixture.sh");
    std::fs::write(
        &fixture,
        r#"#!/bin/sh
IFS= read -r initialize
id=$(printf '%s\n' "$initialize" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"api_version":"0.1","tools":[],"commands":[]}}\n' "$id"
oversized=$(printf '%*s' 65537 '' | tr ' ' x)
chunk=$(printf '%*s' 8192 '' | tr ' ' y)
long_method=$(printf '%*s' 20000 '' | tr ' ' z)
round=0
while IFS= read -r request; do
  case "$request" in
*'"method":"context/collect"'*)
  id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{"jsonrpc":"2.0","method":"context/contribution","params":{"label":"oversized","content":"%s","placement":"prompt_suffix"}}\n' "$oversized"
  if [ "$round" -eq 0 ]; then
    printf '{"jsonrpc":"2.0","method":"%s","params":{}}\n' "$long_method"
  fi
  index=0
  while [ "$index" -lt 40 ]; do
    printf '{"jsonrpc":"2.0","method":"context/contribution","params":{"label":"context-%s-%s","content":"%s","placement":"prompt_suffix"}}\n' "$round" "$index" "$chunk"
    printf '%s\n' '{"jsonrpc":"2.0","method":"flood/unknown","params":{}}'
    index=$((index + 1))
  done
  printf '{"jsonrpc":"2.0","id":%s,"result":[{"label":"collected-oversized","content":"%s","placement":"prompt_suffix"}]}\n' "$id" "$oversized"
  round=$((round + 1))
  ;;
*'"method":"shutdown"'*)
  id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  printf '{"jsonrpc":"2.0","id":%s,"result":{}}\n' "$id"
  exit 0
  ;;
  esac
done
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fixture).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&fixture, permissions).unwrap();

    let manifest = ExtensionManifest::parse(
        r#"
name = "bounded-events-fixture"
version = "0.1.0"
api_version = "0.1"

[entrypoint]
command = "bounded-events-fixture.sh"

[contributes]
context = true
"#,
    )
    .unwrap();
    let mut runtime = ExtensionRuntimeConfig::new(temp.path());
    runtime.request_timeout = Duration::from_secs(5);
    let process = ExtensionProcess::start(
        DiscoveredExtension {
            manifest,
            manifest_path: temp.path().join("extension.toml"),
            source: ExtensionSource::Explicit,
            activation: octet_agent::extension_process::ExtensionActivation {
                enabled: true,
                trust: ExtensionTrust::Trusted,
            },
        },
        runtime,
    )
    .await
    .unwrap();

    let mut extensions = ExecutableExtensions::default();
    extensions.receivers.push(process.subscribe());
    extensions.processes.push(process.clone());
    for _ in 0..10 {
        tokio::time::timeout(
            Duration::from_secs(5),
            process.collect_context(None, process.current_context()),
        )
        .await
        .expect("fixture context flood timed out")
        .expect("fixture context request failed");
        extensions.drain_events();
    }
    // Continue at the same bounded per-frame rate after the producer has
    // stopped so the diagnostic-ring eviction path is exercised too.
    for _ in 0..128 {
        extensions.drain_events();
    }

    assert!(extensions.pending_context.len() <= MAX_PENDING_CONTEXT_ITEMS);
    assert!(extensions.pending_context.retained_bytes() <= MAX_EXTENSION_CONTEXT_BYTES);
    assert!(extensions.diagnostics.len() <= MAX_DIAGNOSTIC_ENTRIES);
    assert!(extensions.diagnostics.retained_bytes() <= MAX_DIAGNOSTIC_BYTES);
    assert!(extensions
        .diagnostics
        .iter()
        .all(|message| message.len() <= MAX_DIAGNOSTIC_ENTRY_BYTES));
    assert!(extensions
        .compose_prompt("system", "still usable".into())
        .await
        .is_ok());
    assert!(process.shutdown().await);
}

#[tokio::test]
async fn invalid_and_overflow_context_are_dropped_without_bricking_prompting() {
    let mut extensions = ExecutableExtensions::default();
    assert!(!extensions.enqueue_context(
        "fixture",
        ContextContribution {
            label: "too-large".into(),
            content: "x".repeat(MAX_CONTEXT_CONTRIBUTION_BYTES + 1),
            placement: ContextPlacement::PromptSuffix,
        }
    ));
    for index in 0..(MAX_PENDING_CONTEXT_ITEMS * 2) {
        extensions.enqueue_context(
            "fixture",
            ContextContribution {
                label: format!("context-{index}"),
                content: "bounded".repeat(1024),
                placement: ContextPlacement::PromptSuffix,
            },
        );
    }

    assert!(extensions.pending_context.len() <= MAX_PENDING_CONTEXT_ITEMS);
    assert!(extensions.pending_context.retained_bytes() <= MAX_EXTENSION_CONTEXT_BYTES);
    assert!(extensions.diagnostics.len() <= MAX_DIAGNOSTIC_ENTRIES);
    assert!(extensions.diagnostics.retained_bytes() <= MAX_DIAGNOSTIC_BYTES);
    assert!(extensions
        .diagnostics
        .iter()
        .any(|message| message.contains("dropped extension context")));

    let composition = extensions
        .compose_prompt("system", "prompt".into())
        .await
        .expect("rejected asynchronous context must not poison later prompts");
    assert!(composition.prompt.starts_with("prompt"));
}

#[tokio::test]
async fn pending_context_is_committed_only_after_the_prompt_append_succeeds() {
    use std::io::Write as _;

    use octet_agent::{Agent, AgentConfig, EffectBroker, ExtensionHost, SandboxConfig};
    use octet_ai::{AiClient, CacheRetention, ModelCatalog, ModelId};

    let temp = tempfile::tempdir().unwrap();
    let session_path = temp.path().join("session.jsonl");
    let session = Session::create(&session_path).unwrap();
    let catalog = ModelCatalog::builtin().unwrap();
    let model = catalog
        .resolve(&ModelId("gpt-5.4-mini-responses".into()))
        .unwrap();
    let mut agent = Agent::new(AgentConfig {
        client: AiClient::new(),
        model,
        session,
        system: "base system".into(),
        sandbox: SandboxConfig::new(temp.path()),
        effect_broker: EffectBroker::default(),
        extensions: ExtensionHost::new(),
        max_turns: None,
        reasoning: ReasoningConfig::Off,
        reasoning_mode: octet_ai::ReasoningMode::Standard,
        cache_retention: CacheRetention::Short,
        session_id: None,
    })
    .unwrap();
    let mut extensions = ExecutableExtensions::default();
    assert!(extensions.enqueue_context(
        "fixture",
        ContextContribution {
            label: "one-shot".into(),
            content: "retained for retry".into(),
            placement: ContextPlacement::PromptSuffix,
        }
    ));
    let composition = extensions
        .compose_prompt("base system", "first attempt".into())
        .await
        .unwrap();

    // Make the Agent's open session handle stale so its durable append
    // fails before mutating the in-memory session.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&session_path)
        .unwrap()
        .write_all(b" ")
        .unwrap();
    agent.set_system_prompt(composition.system);
    let failed = agent.prompt(composition.prompt).await;
    assert!(failed.is_err());
    drop(failed);

    assert_eq!(extensions.pending_context.len(), 1);
    let retry = extensions
        .compose_prompt("base system", "second attempt".into())
        .await
        .unwrap();
    assert!(retry.prompt.contains("retained for retry"));
    extensions.commit_prompt_context(retry.pending_context_count);
    assert!(extensions.pending_context.is_empty());
}

#[test]
fn assistant_text_excludes_reasoning_and_tool_calls() {
    let message = AssistantMessage {
        content: vec![
            AssistantPart::Reasoning(octet_ai::ReasoningPart {
                text: Some("private reasoning".into()),
                state: None,
            }),
            AssistantPart::Text("final ".into()),
            AssistantPart::ToolCall(octet_ai::ToolCall {
                async_execution: false,
                id: ToolCallId("call-1".into()),
                name: "read".into(),
                arguments_json: "{}".into(),
                argument_error: None,
            }),
            AssistantPart::Text("answer".into()),
        ],
        model: octet_ai::ModelId("test".into()),
        protocol: octet_ai::Protocol::OpenAiResponses,
    };

    assert_eq!(assistant_text(&message), "final answer");
}
