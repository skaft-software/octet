#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use octet_agent::{AgentEvent, OutputChannel, ToolOutput};
use octet_ai::{
    AssistantMessage, AssistantPart, Media, ModelId, Protocol, StopReason, ToolCall, ToolCallId,
    Usage,
};
use serde_json::json;

use super::{
    content_hash, derive_model_display_name, format_duration, model_display_name_variants,
    project_changed_files, provider_status_name, reported_output_hash, resolve_model_display_name,
    summarize_tool, summarize_tool_with_workspace, trusted_output_hash,
    validate_changed_file_evidence, RunId, RunOutcome, RunPhase, RunSummary, RunTracker,
    WorkspaceSnapshot,
};

fn instant_after(origin: Instant, milliseconds: u64) -> Instant {
    origin
        .checked_add(Duration::from_millis(milliseconds))
        .expect("fixture timestamp overflow")
}

fn usage(output_tokens: u64) -> Usage {
    Usage {
        output_tokens,
        total_tokens: output_tokens,
        ..Usage::default()
    }
}

fn assistant_message(protocol: Protocol, content: Vec<AssistantPart>) -> AssistantMessage {
    AssistantMessage {
        content,
        model: ModelId("fixture-model".to_owned()),
        protocol,
    }
}

fn turn_finished(
    protocol: Protocol,
    content: Vec<AssistantPart>,
    output_tokens: u64,
) -> AgentEvent {
    let turn_usage = usage(output_tokens);
    AgentEvent::TurnFinished {
        message: assistant_message(protocol, content),
        stop_reason: StopReason::EndTurn,
        turn_usage,
        turn_cost: None,
        usage: turn_usage,
        session_cost_microdollars: None,
        run_cost_microdollars: 0,
    }
}

fn text_turn(protocol: Protocol, output_tokens: u64) -> AgentEvent {
    turn_finished(
        protocol,
        vec![AssistantPart::Text("fixture answer".to_owned())],
        output_tokens,
    )
}

fn start_tracker(provider: &str, model: &str) -> (RunTracker, RunId, Instant) {
    let origin = Instant::now();
    let mut tracker = RunTracker::default();
    let id = tracker
        .begin_for_model_at(provider, model, origin)
        .expect("fixture run starts");
    (tracker, id, origin)
}

fn record_request(
    tracker: &mut RunTracker,
    id: RunId,
    origin: Instant,
    protocol: Protocol,
    output_tokens: u64,
) {
    assert!(tracker.request_submitted_at(id, instant_after(origin, 1)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 2))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "first".to_owned(),
                },
                instant_after(origin, 3),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "last".to_owned(),
                },
                instant_after(origin, 13),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(protocol, output_tokens),
                instant_after(origin, 20),
            )
            .accepted
    );
}

#[test]
fn openai_chat_usage_only_done_uses_provider_output_and_generation_bounds() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    record_request(&mut tracker, id, origin, Protocol::OpenAiChat, 321);

    let run = tracker.current().expect("current run");
    let throughput = run.request_throughput().expect("completed request rate");
    assert_eq!(throughput.output_tokens(), 321);
    assert_eq!(throughput.generation_elapsed(), Duration::from_millis(10));
    assert_eq!(throughput.timing().submitted_at(), instant_after(origin, 1));
    assert_eq!(
        throughput.timing().stream_opened_at(),
        Some(instant_after(origin, 2))
    );
    assert_eq!(
        throughput.timing().first_provider_event_at(),
        Some(instant_after(origin, 3))
    );
    assert_eq!(
        throughput.timing().provider_finished_at(),
        Some(instant_after(origin, 20))
    );
    assert_eq!(
        throughput.timing().committed_at(),
        Some(instant_after(origin, 20))
    );
}

#[test]
fn openai_responses_completed_is_a_provider_finish_boundary() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4.1");
    record_request(&mut tracker, id, origin, Protocol::OpenAiResponses, 88);

    let timing = tracker
        .current()
        .expect("current run")
        .request_timing()
        .expect("timing");
    assert_eq!(
        timing.provider_finished_at(),
        Some(instant_after(origin, 20))
    );
    assert_eq!(timing.committed_at(), Some(instant_after(origin, 20)));
    assert_eq!(
        tracker
            .current()
            .unwrap()
            .request_throughput()
            .unwrap()
            .output_tokens(),
        88
    );
}

#[test]
fn vllm_compatible_stream_keeps_authoritative_usage_without_estimation() {
    let (mut tracker, id, origin) = start_tracker("vllm-compatible", "local-model");
    record_request(&mut tracker, id, origin, Protocol::OpenAiChat, 1_455);

    let throughput = tracker.current().unwrap().request_throughput().unwrap();
    assert_eq!(throughput.output_tokens(), 1_455);
    assert_eq!(throughput.generation_elapsed(), Duration::from_millis(10));
}

#[test]
fn missing_submission_origin_is_handed_off_instead_of_estimated_from_event_receipt() {
    let (mut tracker, id, origin) = start_tracker("mistral", "mistral-large");
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 5))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "provider output".to_owned(),
                },
                instant_after(origin, 6),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(Protocol::OpenAiChat, 40),
                instant_after(origin, 20),
            )
            .accepted
    );

    let run = tracker.current().unwrap();
    assert!(run.request_throughput().is_none());
    assert!(run.request_timing().is_none());
}

#[test]
fn retry_replaces_attempt_origin_and_does_not_mix_intervals() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    assert!(tracker.request_submitted_at(id, instant_after(origin, 1)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 2))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Reasoning,
                    text: "discarded".to_owned(),
                },
                instant_after(origin, 3),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::ProviderRetry {
                    attempt: 1,
                    max_attempts: 2,
                    delay: Duration::from_millis(5),
                    error: "fixture retry".to_owned(),
                },
                instant_after(origin, 4),
            )
            .accepted
    );

    assert!(tracker.request_submitted_at(id, instant_after(origin, 8)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 10))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "replacement first".to_owned(),
                },
                instant_after(origin, 11),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "replacement last".to_owned(),
                },
                instant_after(origin, 19),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(Protocol::OpenAiChat, 12),
                instant_after(origin, 20),
            )
            .accepted
    );

    let throughput = tracker.current().unwrap().request_throughput().unwrap();
    assert_eq!(throughput.output_tokens(), 12);
    assert_eq!(throughput.generation_elapsed(), Duration::from_millis(8));
    assert_eq!(throughput.timing().submitted_at(), instant_after(origin, 8));
    assert_eq!(
        throughput.timing().first_generated_at(),
        Some(instant_after(origin, 11))
    );
}

#[test]
fn one_chunk_and_zero_token_requests_do_not_create_unstable_or_stale_rates() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    assert!(tracker.request_submitted_at(id, instant_after(origin, 1)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 2))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "one chunk".to_owned(),
                },
                instant_after(origin, 5),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(Protocol::OpenAiChat, 50),
                instant_after(origin, 5),
            )
            .accepted
    );
    assert!(tracker.current().unwrap().request_throughput().is_none());
    assert_eq!(
        tracker
            .current()
            .unwrap()
            .request_timing()
            .unwrap()
            .generation_elapsed(),
        Some(Duration::ZERO)
    );

    let second_origin = instant_after(origin, 30);
    assert!(tracker.request_submitted_at(id, second_origin));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 31))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "zero-token request".to_owned(),
                },
                instant_after(origin, 32),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(Protocol::OpenAiChat, 0),
                instant_after(origin, 40),
            )
            .accepted
    );
    assert!(tracker.current().unwrap().request_throughput().is_none());
}

#[test]
fn media_only_request_is_timed_when_origin_is_supplied_but_rate_still_uses_usage() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-image");
    assert!(tracker.request_submitted_at(id, instant_after(origin, 1)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 2))
            .accepted
    );
    let media = Media::image_url(
        url::Url::parse("https://example.invalid/fixture.png").expect("fixture URL"),
        None,
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputMedia {
                    index: 0,
                    media: media.clone(),
                },
                instant_after(origin, 4),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputMedia { index: 1, media },
                instant_after(origin, 9),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &turn_finished(
                    Protocol::OpenAiChat,
                    vec![AssistantPart::Media(Media::image_url(
                        url::Url::parse("https://example.invalid/fixture.png")
                            .expect("fixture URL"),
                        None,
                    ))],
                    9,
                ),
                instant_after(origin, 14),
            )
            .accepted
    );

    let throughput = tracker.current().unwrap().request_throughput().unwrap();
    assert_eq!(throughput.output_tokens(), 9);
    assert_eq!(throughput.generation_elapsed(), Duration::from_millis(5));
    assert_eq!(
        throughput.timing().first_generated_at(),
        Some(instant_after(origin, 4))
    );
}

#[test]
fn tool_turn_displays_latest_request_and_excludes_tool_wall_time() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    assert!(tracker.request_submitted_at(id, instant_after(origin, 1)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 2))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "tool request".to_owned(),
                },
                instant_after(origin, 3),
            )
            .accepted
    );
    let first_call = ToolCall {
        id: ToolCallId("call-1".to_owned()),
        name: "write".to_owned(),
        arguments_json: json!({"path": "src/lib.rs"}).to_string(),
        argument_error: None,
    };
    assert!(
        tracker
            .apply_event_at(
                id,
                &turn_finished(
                    Protocol::OpenAiChat,
                    vec![AssistantPart::ToolCall(first_call)],
                    7,
                ),
                instant_after(origin, 10),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::ToolStarted {
                    id: ToolCallId("call-1".to_owned()),
                    name: "write".to_owned(),
                    args: json!({"path": "src/lib.rs"}),
                },
                instant_after(origin, 11),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::ToolFinished {
                    id: ToolCallId("call-1".to_owned()),
                    result: Ok(ToolOutput::new("ok")),
                    duration: Duration::from_millis(69),
                },
                instant_after(origin, 80),
            )
            .accepted
    );

    assert!(tracker.request_submitted_at(id, instant_after(origin, 90)));
    assert!(
        tracker
            .apply_event_at(id, &AgentEvent::TurnStarted, instant_after(origin, 91))
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "answer first".to_owned(),
                },
                instant_after(origin, 92),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::OutputDelta {
                    channel: OutputChannel::Text,
                    text: "answer last".to_owned(),
                },
                instant_after(origin, 100),
            )
            .accepted
    );
    assert!(
        tracker
            .apply_event_at(
                id,
                &text_turn(Protocol::OpenAiChat, 40),
                instant_after(origin, 110),
            )
            .accepted
    );

    let throughput = tracker.current().unwrap().request_throughput().unwrap();
    assert_eq!(throughput.output_tokens(), 40);
    assert_eq!(throughput.generation_elapsed(), Duration::from_millis(8));
    assert_eq!(
        throughput.timing().submitted_at(),
        instant_after(origin, 90)
    );
}

#[test]
fn terminal_gate_rejection_clears_the_completed_candidate_rate() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    record_request(&mut tracker, id, origin, Protocol::OpenAiChat, 30);
    assert!(tracker.current().unwrap().request_throughput().is_some());
    assert!(
        tracker
            .apply_event_at(
                id,
                &AgentEvent::CandidateRejected {
                    usage: usage(31),
                    run_cost_microdollars: 0,
                    session_cost_microdollars: None,
                },
                instant_after(origin, 30),
            )
            .accepted
    );
    assert!(tracker.current().unwrap().request_throughput().is_none());
}

#[test]
fn changed_file_projection_requires_real_workspace_difference_and_hash_evidence() {
    let directory = tempfile::tempdir().expect("temporary workspace");
    let root = directory.path();
    fs::create_dir_all(root.join("src")).expect("source directory");
    fs::write(root.join("src/lib.rs"), "before\n").expect("baseline file");
    let before = WorkspaceSnapshot::capture(root).expect("baseline snapshot");
    fs::write(root.join("src/lib.rs"), "after\n").expect("mutated file");
    let after = WorkspaceSnapshot::capture(root).expect("after snapshot");
    let hash = after.file("src/lib.rs").unwrap().content_hash.clone();

    assert_eq!(
        validate_changed_file_evidence(&before, &after, "src/lib.rs", Some(&hash)),
        Some("src/lib.rs".to_owned())
    );
    assert_eq!(
        project_changed_files(&before, &after, ["src/lib.rs"])
            .paths()
            .iter()
            .collect::<Vec<_>>(),
        vec![&"src/lib.rs".to_owned()]
    );
    assert!(
        validate_changed_file_evidence(&before, &after, "src/lib.rs", Some(&"a".repeat(64)))
            .is_none()
    );
    assert!(validate_changed_file_evidence(
        &before,
        &after,
        "src/lib.rs",
        Some(&hash.to_ascii_uppercase())
    )
    .is_none());
    assert!(validate_changed_file_evidence(&before, &after, "../outside", None).is_none());

    fs::write(root.join("src/lib.rs"), "before\n").expect("restore file");
    let unchanged = WorkspaceSnapshot::capture(root).expect("unchanged snapshot");
    assert!(validate_changed_file_evidence(&before, &unchanged, "src/lib.rs", None).is_none());
}

#[test]
fn hash_evidence_is_single_lowercase_sha256_token() {
    let hash = content_hash(b"fixture");
    assert_eq!(
        trusted_output_hash(&format!("wrote hash={hash}")),
        Some(hash.clone())
    );
    assert_eq!(
        reported_output_hash(&format!("hash={hash}")),
        Some(hash.clone())
    );
    assert!(trusted_output_hash(&format!("hash={hash} hash={hash}")).is_none());
    assert!(trusted_output_hash(&format!("hash={}", hash.to_ascii_uppercase())).is_none());
    assert!(trusted_output_hash("hash=short").is_none());
}

#[test]
fn model_tool_and_duration_presentation_are_stable_boundaries() {
    assert_eq!(provider_status_name(" openai "), "OpenAI");
    assert_eq!(
        derive_model_display_name("openai/gpt-4o-20240806"),
        "GPT-4o"
    );
    assert_eq!(
        resolve_model_display_name(Some("Configured"), "unknown", "unknown"),
        "Configured"
    );
    assert_eq!(model_display_name_variants("GPT-4o")[0], "GPT-4o");
    assert_eq!(format_duration(Duration::from_millis(1_250)), "1.3s");

    let summary = summarize_tool("write", &json!({"path": "src/lib.rs"}));
    assert_eq!(summary.label, "write");
    assert_eq!(summary.changed_path.as_deref(), Some("src/lib.rs"));
    let workspace = Path::new("/workspace");
    let rendered = summarize_tool_with_workspace(
        "read",
        &json!({"path": "/workspace/src/lib.rs"}),
        Some(workspace),
    );
    assert_eq!(rendered.success, "read src/lib.rs");
}

#[test]
fn lifecycle_transitions_freeze_terminal_elapsed_and_reject_late_updates() {
    let (mut tracker, id, origin) = start_tracker("openai", "gpt-4o");
    assert!(tracker.awaiting_provider_at(id, instant_after(origin, 1)));
    assert!(tracker.set_phase_at(id, RunPhase::Thinking, instant_after(origin, 2),));
    let outcome = tracker
        .fail_at(id, "fixture failure", instant_after(origin, 7))
        .expect("failure outcome");
    assert_eq!(
        outcome,
        RunOutcome::Failed {
            elapsed: Duration::from_millis(7),
            reason: "fixture failure".to_owned(),
        }
    );
    assert!(!tracker.awaiting_provider_at(id, instant_after(origin, 101)));
    let run = tracker.current().unwrap();
    assert!(!run.is_active());
    assert_eq!(
        run.elapsed_at(instant_after(origin, 100)),
        Duration::from_millis(7)
    );
    assert_eq!(run.phase(), &RunPhase::Finished(outcome));
}

#[test]
fn run_summary_is_value_stable_for_completed_projection() {
    let summary = RunSummary {
        files_changed: 2,
        tool_calls: 3,
        warnings: 1,
    };
    assert_eq!(summary.files_changed, 2);
    assert_eq!(summary.tool_calls, 3);
    assert_eq!(summary.warnings, 1);
}
