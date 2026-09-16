//! Behavioral parity tests for the Pi tool rows recorded in `docs/parity/tools.md`.
//!
//! Every test in this file calls the real tool implementation through the public
//! [`Tool`] boundary and asserts on the observed output, error, or on-disk
//! effect. Nothing here is a type-only or load-only check: a row is qualified
//! only when the assertions below exercise the behavior it claims.
//!
//! Tests that resolve a child binary through `PATH` (`rg`, `fd`, the shell) take
//! a process-wide async lock, because one test temporarily restricts `PATH`.
//! `rg`/`fd` absence follows the existing repository convention: the test prints
//! why it skipped instead of silently passing.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use octet_agent::effect::ToolEffect;
use octet_agent::sandbox::SandboxConfig;
use octet_agent::tool::{
    PartialOutputCheckpointSink, Tool, ToolContext, ToolError, ToolProgressSink,
};
use octet_agent::tools::{
    deferred::{
        prepare_deferred_poll, suspend_deferred_response, DeferredHandle, DeferredHandleRejection,
        DeferredPhase, DeferredPollOutcome, DeferredPollPermit, DeferredPollPreparation,
        DeferredPollRefusalKind, DeferredResponseDeclaration, DeferredResume, DeferredStopReason,
        DeferredSuspendDecision, DeferredSuspendFailureKind, DeferredSuspended, ModelIdentity,
        INVALID_DEFERRED_HANDLE_DIAGNOSTIC,
    },
    durability::{
        DurableInvocationStore, InvocationError, InvocationHandle, InvocationOutcome,
        InvocationScope, InvocationState, MemoLookup, StoreLimits,
        INTERRUPTED_OUTCOME_UNKNOWN_MARKER,
    },
    summarization::{
        run_summarization_with_retry, CompactionFailureKind, CompactionStepOutcome,
        SummarizationAttempt, SummarizationDiagnostic, SummarizationFailure,
        SummarizationFailureKind, SummarizationOutcome, SummarizationRetryPolicy,
    },
    BashCheckpointPublisher, BashTool, CheckpointedBashTool, EditTool, PowerShellTool, ReadTool,
    SearchTool, ShellSessionEnvironment, WriteTool, BASH_CHECKPOINT_MAX_BYTES,
};
use serde_json::json;

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    sandbox: SandboxConfig,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    sandbox.allow_edit = true;
    sandbox.allow_write = true;
    sandbox.bash_timeout = Duration::from_secs(30);
    Fixture {
        _dir: dir,
        workspace,
        sandbox,
    }
}

impl Fixture {
    fn ctx(&self) -> ToolContext<'_> {
        ToolContext {
            workspace: &self.workspace,
            sandbox: &self.sandbox,
            execution_scope: "parity-tools",
            resource_owner: "parity-tools",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.workspace.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    fn read(&self, relative: &str) -> String {
        std::fs::read_to_string(self.workspace.join(relative)).unwrap()
    }
}

/// Serializes tests that spawn `PATH`-resolved children. `make_serial` in one
/// test mutates the process `PATH`, which every other such test must observe.
async fn serial() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    LOCK.lock().await
}

// ── 4.4 bash spilled output path ─────────────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn bash_truncated_output_spills_the_full_stream_to_a_readable_path() {
    let _serial = serial().await;
    let mut f = fixture();
    f.sandbox.max_output_bytes = 4096;

    let output = BashTool
        .execute(
            json!({"command": "i=0; while [ $i -lt 400 ]; do printf 'line-%04d-payload\\n' $i; i=$((i+1)); done"}),
            &f.ctx(),
        )
        .await
        .unwrap();

    assert!(output.text.contains("truncated_stdout="), "{}", output.text);
    let spill = output
        .text
        .lines()
        .find_map(|line| line.strip_prefix("full_output_path="))
        .unwrap_or_else(|| panic!("missing spilled output path in {}", output.text));
    let spill = PathBuf::from(spill);
    let contents = std::fs::read_to_string(&spill).unwrap();
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 400, "spill must retain the complete stream");
    assert_eq!(lines[0], "line-0000-payload");
    assert_eq!(lines[399], "line-0399-payload");
    assert!(!output.text.contains("spill_error=true"), "{}", output.text);
    // The advertised description names the path field the model can follow.
    assert!(
        BashTool
            .definition()
            .description
            .contains("full_output_path"),
        "{}",
        BashTool.definition().description
    );
    let _ = std::fs::remove_file(spill);
}

#[cfg(unix)]
#[tokio::test]
async fn bash_untruncated_output_leaks_no_spill_path() {
    let _serial = serial().await;
    let f = fixture();
    let output = BashTool
        .execute(json!({"command": "printf 'small\\n'"}), &f.ctx())
        .await
        .unwrap();
    assert!(
        output.text.contains("complete_stdout=true"),
        "{}",
        output.text
    );
    assert!(!output.text.contains("_output_path="), "{}", output.text);
    assert!(!output.text.contains("spill_error"), "{}", output.text);
}

// ── 4.5 bash session identity/provider/model/reasoning + commandPrefix ──

#[cfg(unix)]
#[tokio::test]
async fn session_shell_exposes_live_identity_metadata_and_host_command_prefix() {
    let _serial = serial().await;
    let f = fixture();
    let tool = BashTool::with_session_environment(|| ShellSessionEnvironment {
        session_id: Some("session-123".to_string()),
        session_file: Some("/tmp/session-123.jsonl".to_string()),
        provider: Some("anthropic".to_string()),
        model: Some("claude-sonnet-4".to_string()),
        reasoning_level: Some("high".to_string()),
    })
    .with_command_prefix("OCTET_PREFIX_MARKER=active");

    let output = tool
        .execute(
            json!({"command": "printf '%s|%s|%s|%s|%s|%s' \"$PI_SESSION_ID\" \"$PI_SESSION_FILE\" \"$PI_PROVIDER\" \"$PI_MODEL\" \"$PI_REASONING_LEVEL\" \"$OCTET_PREFIX_MARKER\""}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(
        output
            .text
            .contains("session-123|/tmp/session-123.jsonl|anthropic|claude-sonnet-4|high|active"),
        "{}",
        output.text
    );

    // The prefix is host state, not a model argument: the schema is unchanged.
    assert_eq!(tool.definition().name, "bash");
    let definition = tool.definition();
    let properties = definition.parameters["properties"].as_object().unwrap();
    assert!(!properties.contains_key("commandPrefix"));
}

#[cfg(unix)]
#[tokio::test]
async fn session_shell_clears_inherited_metadata_and_rereads_the_resolver() {
    let _serial = serial().await;
    let f = fixture();

    // `ShellSessionEnvironment::default()` is the documented opt-out.
    let previous = std::env::var_os("PI_PROVIDER");
    // SAFETY: this test holds the process-wide `serial` lock.
    unsafe { std::env::set_var("PI_PROVIDER", "inherited-provider") };
    let opted_out = BashTool::with_session_environment(ShellSessionEnvironment::default)
        .execute(
            json!({"command": "printf '[%s]' \"${PI_PROVIDER-unset}\""}),
            &f.ctx(),
        )
        .await
        .unwrap();
    match previous {
        Some(value) => unsafe { std::env::set_var("PI_PROVIDER", value) },
        None => unsafe { std::env::remove_var("PI_PROVIDER") },
    }
    assert!(opted_out.text.contains("[unset]"), "{}", opted_out.text);
    assert!(
        !opted_out.text.contains("inherited-provider"),
        "{}",
        opted_out.text
    );

    // The resolver is re-read on every call rather than cached at build time.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let tool = BashTool::with_session_environment(move || {
        let call = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ShellSessionEnvironment {
            session_id: Some(format!("session-{call}")),
            ..ShellSessionEnvironment::default()
        }
    });
    for expected in ["session-0", "session-1"] {
        let output = tool
            .execute(
                json!({"command": "printf '%s' \"$PI_SESSION_ID\""}),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert!(output.text.contains(expected), "{}", output.text);
    }

    // Ephemeral sessions simply omit the file variable.
    let ephemeral = BashTool::with_session_environment(|| ShellSessionEnvironment {
        session_id: Some("ephemeral".to_string()),
        ..ShellSessionEnvironment::default()
    });
    let output = ephemeral
        .execute(
            json!({"command": "printf '[%s][%s]' \"$PI_SESSION_ID\" \"${PI_SESSION_FILE-unset}\""}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(
        output.text.contains("[ephemeral][unset]"),
        "{}",
        output.text
    );

    // NUL-bearing metadata is rejected before the child starts.
    let invalid = BashTool::with_session_environment(|| ShellSessionEnvironment {
        session_id: Some("bad\0id".to_string()),
        ..ShellSessionEnvironment::default()
    });
    let error = invalid
        .execute(json!({"command": "printf nope"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(
        error.message.contains("invalid shell session metadata"),
        "{error}"
    );
}

// ── 4.6 opt-in PowerShell ───────────────────────────────────────────────

#[tokio::test]
async fn powershell_is_opt_in_and_never_a_bash_fallback() {
    // Addressable by name for an explicit product allowlist, and shaped like
    // the shell schema it shares bounded capture with.
    assert_eq!(PowerShellTool.definition().name, "powershell");
    assert_eq!(
        PowerShellTool.definition().parameters,
        BashTool.definition().parameters
    );

    let f = fixture();

    #[cfg(not(windows))]
    {
        let error = PowerShellTool
            .execute(json!({"command": "Write-Output hi"}), &f.ctx())
            .await
            .unwrap_err();
        assert!(
            error.message.contains("available only on Windows"),
            "{error}"
        );
        // Classification is shared with bash, so the broker never mistakes it
        // for a narrower capability.
        assert_eq!(
            PowerShellTool
                .effect(&json!({"command": "Write-Output hi"}), &f.ctx())
                .unwrap(),
            ToolEffect::HostProcess
        );
    }

    #[cfg(windows)]
    {
        let output = PowerShellTool
            .execute(json!({"command": "Write-Output pwsh-ok"}), &f.ctx())
            .await
            .unwrap();
        assert!(output.text.contains("pwsh-ok"), "{}", output.text);
    }
}

// ── 4.9 original-file nonoverlapping multi-edit + legacy normalization ──

#[cfg(unix)]
#[tokio::test]
async fn edit_applies_multiple_edits_against_the_original_file() {
    let f = fixture();
    f.write("multi.txt", "alpha\nbeta\ngamma\n");

    let output = EditTool
        .execute(
            json!({"path": "multi.txt", "edits": [
                {"oldText": "alpha", "newText": "ALPHA"},
                {"oldText": "gamma", "newText": "GAMMA"}
            ]}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(
        output.text.starts_with("ok modified=1\n"),
        "{}",
        output.text
    );
    assert_eq!(f.read("multi.txt"), "ALPHA\nbeta\nGAMMA\n");

    // Every `old` matches the *original* file, so an edit that only becomes
    // present after an earlier replacement is a no-match error and the whole
    // batch is rejected without touching the file.
    f.write("original.txt", "alpha\n");
    let error = EditTool
        .execute(
            json!({"path": "original.txt", "edits": [
                {"oldText": "alpha", "newText": "beta"},
                {"oldText": "beta", "newText": "gamma"}
            ]}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("error no_match"), "{error}");
    assert_eq!(f.read("original.txt"), "alpha\n");

    // Ambiguous and overlapping batches are rejected deterministically.
    f.write("dup.txt", "same\nsame\n");
    let ambiguous = EditTool
        .execute(
            json!({"path": "dup.txt", "edits": [{"oldText": "same", "newText": "x"}]}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(ambiguous.message.contains("ambiguous"), "{ambiguous}");
    assert_eq!(f.read("dup.txt"), "same\nsame\n");

    f.write("over.txt", "abcdef\n");
    let overlap = EditTool
        .execute(
            json!({"path": "over.txt", "edits": [
                {"oldText": "abcd", "newText": "1"},
                {"oldText": "cdef", "newText": "2"}
            ]}),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(overlap.message.contains("overlapping_edits"), "{overlap}");
    assert_eq!(f.read("over.txt"), "abcdef\n");
}

#[cfg(unix)]
#[tokio::test]
async fn edit_legacy_shapes_are_normalized_into_one_batch() {
    let f = fixture();

    // Legacy `old`/`new` pair.
    f.write("legacy.txt", "hello world\n");
    let legacy = EditTool
        .execute(
            json!({"path": "legacy.txt", "old": "hello", "new": "goodbye"}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(legacy.text.contains("ok modified=1"), "{}", legacy.text);
    assert_eq!(f.read("legacy.txt"), "goodbye world\n");

    // JSON-encoded `edits` string plus `oldText`/`newText` combine into one batch.
    f.write("stringly.txt", "a\nb\n");
    let stringly = EditTool
        .execute(
            json!({
                "path": "stringly.txt",
                "edits": "[{\"oldText\": \"a\", \"newText\": \"A\"}]",
                "oldText": "b",
                "newText": "B"
            }),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(stringly.text.contains("ok modified=1"), "{}", stringly.text);
    assert_eq!(f.read("stringly.txt"), "A\nB\n");

    // A single replacement object is accepted in place of the array.
    f.write("single.txt", "one\n");
    let single = EditTool
        .execute(
            json!({"path": "single.txt", "edits": {"old": "one", "new": "two"}}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(single.text.contains("ok modified=1"), "{}", single.text);
    assert_eq!(f.read("single.txt"), "two\n");

    // Invalid batches fail before any mutation.
    for (arguments, expected) in [
        (
            json!({"path": "single.txt", "edits": []}),
            "1 to 1000 replacements",
        ),
        (
            json!({"path": "single.txt", "edits": [{"old": "", "new": "x"}]}),
            "must be non-empty",
        ),
        (json!({"path": "single.txt"}), "1 to 1000 replacements"),
        (
            json!({"path": "single.txt", "edits": [{"old": "two"}]}),
            "missing field",
        ),
    ] {
        let error = EditTool
            .execute(arguments.clone(), &f.ctx())
            .await
            .unwrap_err();
        assert!(error.message.contains(expected), "{arguments}: {error}");
    }
    assert_eq!(f.read("single.txt"), "two\n");

    // expected_hash still fences stale reads for a whole batch.
    let stale = EditTool
        .execute(
            json!({
                "path": "single.txt",
                "edits": [{"oldText": "two", "newText": "three"}],
                "expected_hash": "0".repeat(64)
            }),
            &f.ctx(),
        )
        .await
        .unwrap_err();
    assert!(stale.message.contains("stale_file"), "{stale}");
    assert_eq!(f.read("single.txt"), "two\n");
}

// ── 4.8 adaptive preview coalescing ─────────────────────────────────────

#[test]
fn preview_coalescer_paces_both_interval_and_encoded_bytes() {
    use octet_agent::tool::{AdaptivePreviewCoalescer, PreviewPublication};
    use std::time::{Duration, Instant};

    let coalescer = AdaptivePreviewCoalescer::new();
    // Pi's harness policy: 100 ms floor, 100 KB/s sustained.
    assert_eq!(
        coalescer.delay_for(1),
        Duration::from_millis(100),
        "the interval floor dominates small trickles"
    );
    assert_eq!(coalescer.delay_for(50_000), Duration::from_millis(500));
    assert_eq!(coalescer.delay_for(1_000_000), Duration::from_secs(10));
    assert_eq!(
        AdaptivePreviewCoalescer::with_policy(Duration::from_millis(10), 0).delay_for(500_000),
        Duration::from_millis(10),
        "a zero rate target leaves only the interval floor"
    );

    // 1. First dirty state after idle publishes immediately.
    let start = Instant::now();
    let mut coalescer = AdaptivePreviewCoalescer::new();
    assert_eq!(
        coalescer.record(5_000, start),
        PreviewPublication::Immediate
    );
    assert_eq!(coalescer.pending_bytes(), None);
    assert_eq!(
        coalescer.deadline_in(start),
        Some(Duration::from_millis(100))
    );

    // 2. Writes before the deadline collapse into the latest state only.
    let mut publishes = 1;
    let mut collapsed = 0;
    for step in 1..=9u64 {
        let now = start + Duration::from_millis(step * 10);
        let expected = Duration::from_millis(100 - step * 10);
        assert_eq!(
            coalescer.record(5_000 + step as usize, now),
            PreviewPublication::Scheduled(expected),
            "step {step}"
        );
        collapsed += 1;
    }
    // Nothing was published during the collapsed window, and the retained state
    // is exactly one snapshot: the latest write.
    assert_eq!(publishes, 1);
    assert_eq!(coalescer.pending_bytes(), Some(5_009));

    // 3. One trailing timer publishes the held state once the deadline passes.
    let due = start + Duration::from_millis(100);
    assert_eq!(coalescer.take_due(start + Duration::from_millis(99)), None);
    assert_eq!(coalescer.take_due(due), Some(5_009));
    publishes += 1;
    assert_eq!(coalescer.pending_bytes(), None);
    assert_eq!(coalescer.deadline_in(due), Some(Duration::from_millis(100)));
    assert_eq!(publishes, 2);
    assert_eq!(collapsed, 9, "nine writes produced no extra publication");

    // Sustained cadence: a 5 KB snapshot every 10 ms publishes at the 100 ms
    // floor, so the number of publications over 1 s stays bounded by the floor.
    let mut now = due;
    let mut sustained = 0;
    for _ in 0..100 {
        now += Duration::from_millis(10);
        match coalescer.record(5_000, now) {
            PreviewPublication::Immediate => sustained += 1,
            PreviewPublication::Scheduled(_) => {}
        }
        if coalescer.take_due(now).is_some() {
            sustained += 1;
        }
    }
    assert!(
        (9..=11).contains(&sustained),
        "expected ~10 publications per 100 writes, got {sustained}"
    );

    // 4. Forced publication cancels the trailing timer: no late update can fire
    // for a settled invocation.
    let mut coalescer = AdaptivePreviewCoalescer::new();
    let start = Instant::now() + Duration::from_secs(60);
    assert_eq!(coalescer.record(100, start), PreviewPublication::Immediate);
    assert_eq!(
        coalescer.record(100, start + Duration::from_millis(10)),
        PreviewPublication::Scheduled(Duration::from_millis(90))
    );
    coalescer.force(start + Duration::from_millis(10), 250);
    assert_eq!(coalescer.pending_bytes(), None);
    assert_eq!(
        coalescer.take_due(start + Duration::from_secs(1)),
        None,
        "a forced terminal flush must leave nothing to publish later"
    );
    assert_eq!(
        AdaptivePreviewCoalescer::default().delay_for(1),
        Duration::from_millis(100),
        "the default policy is the documented Pi policy"
    );
}

// ── 4.10 unanimous finalized-result batch termination ───────────────────

#[test]
fn batch_termination_requires_unanimous_finalized_results() {
    use octet_agent::tool::{batch_requests_termination, ToolOutput};

    // Default: a finalized result never requests termination.
    let plain = ToolOutput::new("done");
    assert!(!plain.terminates_run());
    let requesting = ToolOutput::new("done").requesting_termination();
    assert!(requesting.terminates_run());
    // Presentation copies keep the request; they are the same result.
    assert!(requesting.without_media_payloads().terminates_run());

    // Unanimous: every result of the batch agrees.
    let finalized = [
        ToolOutput::new("a").requesting_termination(),
        ToolOutput::new("b").requesting_termination(),
    ];
    assert!(batch_requests_termination(
        finalized.iter().map(ToolOutput::terminates_run)
    ));

    // One dissenting (or merely ordinary) result keeps the run alive, so a
    // sibling's result is never discarded.
    let mixed = [
        ToolOutput::new("a").requesting_termination(),
        ToolOutput::new("b"),
        ToolOutput::new("c").requesting_termination(),
    ];
    assert!(!batch_requests_termination(
        mixed.iter().map(ToolOutput::terminates_run)
    ));

    // Order does not matter and the rule is unanimous, not last-wins.
    for rotate in 0..mixed.len() {
        let rotated: Vec<bool> = (0..mixed.len())
            .map(|index| mixed[(index + rotate) % mixed.len()].terminates_run())
            .collect();
        assert!(!batch_requests_termination(rotated));
    }

    // An empty batch never terminates.
    assert!(!batch_requests_termination(Vec::new().into_iter()));
    assert!(batch_requests_termination(vec![true]));
    assert!(!batch_requests_termination(vec![true, false]));
}

// ── 4.13 tool promptSnippet/promptGuidelines ────────────────────────────

#[test]
fn tool_prompt_contributions_match_pi_snippets_and_guidelines() {
    use octet_agent::tool::collect_tool_prompt_contributions;

    let tools: Vec<&dyn Tool> = vec![&BashTool, &ReadTool, &EditTool, &WriteTool, &SearchTool];
    let contributions = collect_tool_prompt_contributions(tools);
    let by_name = |name: &str| {
        contributions
            .iter()
            .find(|contribution| contribution.name == name)
            .unwrap_or_else(|| panic!("missing contribution for {name}"))
    };

    // Presentation order is the registration order the host passed in.
    assert_eq!(
        contributions
            .iter()
            .map(|contribution| contribution.name.as_str())
            .collect::<Vec<_>>(),
        vec!["bash", "read", "edit", "write", "search"]
    );
    assert_eq!(
        by_name("bash").snippet,
        "Execute bash commands (prefer rg/ripgrep for file and content search)"
    );
    assert_eq!(by_name("read").snippet, "Read file contents");
    assert_eq!(by_name("write").snippet, "Create or overwrite files");
    assert!(by_name("edit")
        .snippet
        .starts_with("Make precise file edits"));
    assert_eq!(
        by_name("edit").guidelines.len(),
        4,
        "edit carries the four Pi guidelines"
    );
    assert_eq!(
        by_name("read").guidelines,
        vec!["Use read to examine files instead of cat or sed."]
    );
    assert_eq!(
        by_name("write").guidelines,
        vec!["Use write only for new files or complete rewrites."]
    );
    // Tools without guidelines contribute an empty list, not a placeholder.
    assert!(by_name("bash").guidelines.is_empty());

    // The PI_* guideline is gated on the variant that really injects the
    // metadata: the plain `bash` tool clears it, the session shell does not.
    let session_shell = BashTool::with_session_environment(ShellSessionEnvironment::default);
    let gated: Vec<&dyn Tool> = vec![&session_shell];
    let gated = collect_tool_prompt_contributions(gated);
    assert_eq!(gated.len(), 1);
    assert_eq!(gated[0].name, "bash");
    assert_eq!(
        gated[0].guidelines,
        vec!["You can inspect PI_* environment variables for current model and session details."]
    );

    // A tool with no snippet is absent from the contribution list: this list is
    // presentation intent, never a tool inventory, so it cannot widen an
    // allowlist.
    assert!(collect_tool_prompt_contributions(Vec::<&dyn Tool>::new()).is_empty());
    assert_eq!(
        SearchTool.prompt_snippet(),
        Some("Search file contents with ripgrep (rg)"),
        "only a host that enables search passes it to prompt assembly"
    );
}

// ── 4.7 interval durable partial bash output checkpoints ─────────────────

/// Counts and retains every checkpoint a bash run asks the host to persist.
struct RecordingCheckpointSink {
    snapshots: Mutex<Vec<String>>,
}

impl RecordingCheckpointSink {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            snapshots: Mutex::new(Vec::new()),
        })
    }

    fn snapshots(&self) -> Vec<String> {
        self.snapshots.lock().unwrap().clone()
    }
}

impl PartialOutputCheckpointSink for RecordingCheckpointSink {
    fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
        assert!(
            snapshot.len() <= BASH_CHECKPOINT_MAX_BYTES,
            "checkpoint snapshot is {} bytes (cap {BASH_CHECKPOINT_MAX_BYTES})",
            snapshot.len()
        );
        self.snapshots.lock().unwrap().push(snapshot.to_owned());
        Ok(())
    }
}

/// The bounded `stdout: N bytes seen` marker a checkpoint snapshot carries.
fn checkpoint_seen_bytes(snapshot: &str) -> usize {
    snapshot
        .lines()
        .find_map(|line| line.strip_prefix("stdout: "))
        .and_then(|rest| rest.split(' ').next())
        .expect("checkpoint snapshot carries a stdout byte marker")
        .parse()
        .expect("stdout byte marker is a number")
}

#[test]
fn bash_checkpoint_publisher_is_interval_bounded_and_dedupes_identical_snapshots() {
    use std::time::Instant;

    // The host may pace checkpoints faster than Pi's two seconds, but never
    // below the interval floor, and output volume never accelerates them.
    let mut publisher = BashCheckpointPublisher::new(Duration::from_millis(30));
    assert_eq!(publisher.interval(), Duration::from_millis(30));
    assert_eq!(
        BashCheckpointPublisher::new(Duration::ZERO).interval(),
        Duration::from_millis(10),
        "an interval floor bounds storage write rate independently of output volume"
    );

    let start = Instant::now();
    // First observation after idle is due immediately.
    assert!(publisher
        .observe("stdout: 5 bytes seen\nhello", start)
        .is_some());
    // Observations before the interval boundary never reach the sink.
    for offset in [1_u64, 10, 29] {
        assert!(publisher
            .observe(
                "stdout: 6 bytes seen\nhello!",
                start + Duration::from_millis(offset)
            )
            .is_none());
    }
    // At the boundary a changed snapshot is published...
    assert!(publisher
        .observe(
            "stdout: 6 bytes seen\nhello!",
            start + Duration::from_millis(30)
        )
        .is_some());
    // ...and a repeated snapshot is suppressed even though the interval elapsed.
    assert!(publisher
        .observe(
            "stdout: 6 bytes seen\nhello!",
            start + Duration::from_millis(60)
        )
        .is_none());
    // A fresh interval with new bytes publishes again.
    assert!(publisher
        .observe(
            "stdout: 7 bytes seen\nhello!!",
            start + Duration::from_millis(90)
        )
        .is_some());

    let stats = publisher.stats();
    assert_eq!(stats.requested, 3, "{stats:?}");
    assert_eq!(stats.suppressed, 1, "{stats:?}");
    assert_eq!(stats.before_interval, 3, "{stats:?}");

    // A snapshot too large for the durable value keeps its most recent bytes on
    // a code-point boundary and says so.
    let oversized = format!("{}tail", "x".repeat(BASH_CHECKPOINT_MAX_BYTES));
    let bounded = BashCheckpointPublisher::bound_snapshot(&oversized);
    assert!(
        bounded.len() <= BASH_CHECKPOINT_MAX_BYTES,
        "{} bytes",
        bounded.len()
    );
    assert!(bounded.ends_with("tail"), "{bounded}");
    assert!(bounded.starts_with("[earlier output elided]"), "{bounded}");
}

#[cfg(unix)]
#[tokio::test]
async fn bash_checkpoints_land_at_interval_boundaries_and_final_output_is_complete() {
    let _serial = serial().await;
    let f = fixture();

    // The durable side is the invocation store: the sink is the real capability
    // a host would hand a tool, so this test proves the whole path.
    let store = Arc::new(DurableInvocationStore::new());
    let scope = InvocationScope::new("op-4-7", "inv-4-7").unwrap();
    let handle = store.open(scope.clone()).unwrap();
    let recorder = RecordingCheckpointSink::new();
    let sink: Arc<dyn PartialOutputCheckpointSink> = Arc::new(DurableCheckpointSink {
        handle: handle.clone(),
        recorder: Arc::clone(&recorder),
    });

    let interval = Duration::from_millis(30);
    let tool = CheckpointedBashTool::with_checkpoints(sink, interval);
    let started = std::time::Instant::now();
    let output = tool
        .execute(
            json!({"command": "i=1; while [ $i -le 6 ]; do printf 'line-%02d\\n' $i; i=$((i+1)); sleep 0.03; done"}),
            &f.ctx(),
        )
        .await
        .unwrap();
    let elapsed = started.elapsed();

    // The final result is complete and ordered: every line, ascending, with the
    // completion marker and no truncation.
    for index in 1..=6 {
        assert!(
            output.text.contains(&format!("line-{index:02}")),
            "final output lost line-{index:02}: {}",
            output.text
        );
    }
    let final_line_order: Vec<usize> = output
        .text
        .lines()
        .filter_map(|line| line.strip_prefix("line-"))
        .filter_map(|line| line.parse::<usize>().ok())
        .collect();
    assert_eq!(final_line_order, vec![1, 2, 3, 4, 5, 6], "{}", output.text);
    assert!(
        output.text.contains("complete_stdout=true"),
        "{}",
        output.text
    );

    let snapshots = recorder.snapshots();
    eprintln!(
        "observed {} durable checkpoints at a {} ms cadence; last snapshot {} bytes",
        snapshots.len(),
        tool.interval().as_millis(),
        snapshots.last().map(String::len).unwrap_or(0)
    );
    assert!(
        snapshots.len() >= 2,
        "a ~0.2 s command at a 30 ms cadence must checkpoint at interval boundaries: {}",
        snapshots.len()
    );
    // Cadence is bounded by the interval, not by output volume: however long the
    // command took under load, it cannot have requested more checkpoints than
    // intervals elapsed.
    let interval_budget = elapsed.as_millis() / interval.as_millis() + 2;
    assert!(
        snapshots.len() as u128 <= interval_budget,
        "{} checkpoints in {elapsed:?} exceeded the interval budget {interval_budget}",
        snapshots.len()
    );

    let mut previous = 0;
    for snapshot in &snapshots {
        // A checkpoint never claims completion: it shares no wording with a
        // final result, so recovery cannot read it as proof the command ended.
        assert!(
            !snapshot.contains("complete_") && !snapshot.contains("truncated_"),
            "checkpoint text implies terminal state: {snapshot}"
        );
        let seen = checkpoint_seen_bytes(snapshot);
        assert!(
            seen >= previous,
            "checkpoints must describe progress in order: {previous} -> {seen}"
        );
        assert!(
            seen <= output.text.len() + 32,
            "a checkpoint cannot report more bytes than the command produced"
        );
        previous = seen;
        // Every line present in a checkpoint appears in the final output, in the
        // same ascending order.
        for line in snapshot.lines().filter(|line| line.starts_with("line-")) {
            assert!(
                output.text.contains(line),
                "checkpoint line {line} is missing from the final output"
            );
        }
    }

    // The durable value is the latest checkpoint, and it is a single replaced
    // value rather than an append log.
    let durable = handle
        .partial_output()
        .unwrap()
        .expect("a durable checkpoint");
    assert_eq!(durable, snapshots.last().unwrap().clone());
    assert_eq!(store.stored_values(&scope).len(), 1, "one replaced value");
    assert_eq!(tool.checkpoint_stats().failures, 0);

    // Settlement deletes it and fences late writes: a checkpoint can never
    // outlive the outcome it belongs to.
    let settlement = handle.settle().unwrap();
    assert!(!settlement.deleted_values.is_empty());
    assert!(handle.partial_output().is_err());
    assert!(handle.checkpoint_partial_output("late").is_err());
    assert_eq!(store.state(&scope), Some(InvocationState::OutcomeReady));
    assert!(store.stored_values(&scope).is_empty());
}

/// The invocation store's implementation of the checkpoint sink.
struct DurableCheckpointSink {
    handle: InvocationHandle,
    recorder: Arc<RecordingCheckpointSink>,
}

impl PartialOutputCheckpointSink for DurableCheckpointSink {
    fn checkpoint_partial_output(&self, snapshot: &str) -> Result<(), ToolError> {
        self.recorder.checkpoint_partial_output(snapshot)?;
        self.handle
            .replace_partial_output(snapshot)
            .map_err(ToolError::from)
    }
}

// ── 4.11 durable invocation memos through replay until the outcome is known ─

#[test]
fn invocation_memos_survive_replay_until_the_outcome_is_known() {
    let store = Arc::new(DurableInvocationStore::new());
    let scope = InvocationScope::new("op-4-11", "inv-4-11").unwrap();
    let step_a = AtomicUsize::new(0);
    let step_b = AtomicUsize::new(0);

    // First pass: both steps run and are recorded.
    let handle = store.open(scope.clone()).unwrap();
    assert_eq!(
        handle
            .replay_step("step/read", || {
                step_a.fetch_add(1, Ordering::SeqCst);
                json!({"file": "a.txt", "bytes": 12})
            })
            .unwrap(),
        json!({"file": "a.txt", "bytes": 12})
    );
    assert_eq!(
        handle.replay_lookup("step/summarize").unwrap(),
        MemoLookup::NotYetRecorded
    );
    handle
        .set_memo("step/summarize", json!("done"))
        .expect("a live capability records memos");

    // Replay before the outcome is known: a fresh capability for the same
    // invocation returns the recorded step and only runs the missing one.
    let replayed = store.open(scope.clone()).unwrap();
    assert_eq!(
        replayed.replay_lookup("step/read").unwrap(),
        MemoLookup::Memoized(json!({"file": "a.txt", "bytes": 12}))
    );
    assert_eq!(
        replayed
            .replay_step("step/read", || {
                step_a.fetch_add(1, Ordering::SeqCst);
                json!({"file": "a.txt", "bytes": 12})
            })
            .unwrap(),
        json!({"file": "a.txt", "bytes": 12})
    );
    assert_eq!(
        step_a.load(Ordering::SeqCst),
        1,
        "a memoized step never re-runs"
    );
    replayed
        .replay_step("step/write", || {
            step_b.fetch_add(1, Ordering::SeqCst);
            json!({"written": true})
        })
        .unwrap();
    assert_eq!(step_b.load(Ordering::SeqCst), 1);

    // The capability is invocation-scoped: another invocation sees nothing.
    let other_scope = InvocationScope::new("op-4-11", "inv-4-11-b").unwrap();
    let other = store.open(other_scope).unwrap();
    assert_eq!(
        other.replay_lookup("step/read").unwrap(),
        MemoLookup::NotYetRecorded
    );

    // Also bounded: an oversized memo fails closed instead of being truncated.
    let tiny = Arc::new(DurableInvocationStore::with_limits(StoreLimits {
        max_value_bytes: 64,
        ..StoreLimits::default()
    }));
    let tiny_handle = tiny
        .open(InvocationScope::new("op-tiny", "inv-tiny").unwrap())
        .unwrap();
    let oversized = tiny_handle.set_memo("step/big", json!("x".repeat(4096)));
    assert!(
        matches!(oversized, Err(InvocationError::BoundExceeded(_))),
        "{oversized:?}"
    );

    // Outcome known: memos are gone and the capability is expired.
    let settlement = handle.settle().unwrap();
    assert_eq!(settlement.scope, scope);
    assert!(settlement.generation > 0);
    assert!(store.stored_values(&scope).is_empty(), "memos are deleted");
    assert_eq!(store.state(&scope), Some(InvocationState::OutcomeReady));

    // Replay after the outcome is known fails closed. It is never "memoized"
    // (the value is gone) and never "not yet recorded" (which would re-run the
    // effect after the outcome was already delivered).
    let expired = store.open(scope.clone());
    assert!(
        matches!(expired, Err(InvocationError::OutcomeKnown(_))),
        "{expired:?}"
    );
    let replayed_after = handle.replay_lookup("step/read");
    assert!(
        matches!(replayed_after, Err(InvocationError::OutcomeKnown(_))),
        "a settled invocation must not look like an unrecorded memo: {replayed_after:?}"
    );
    let ran_after_settlement = handle.replay_step("step/read", || {
        step_a.fetch_add(1, Ordering::SeqCst);
        json!("must not run")
    });
    assert!(ran_after_settlement.is_err(), "{ran_after_settlement:?}");
    assert_eq!(step_a.load(Ordering::SeqCst), 1, "the effect never re-ran");

    // An unknown outcome is never reported as success or failure: recovery
    // preserves the bounded snapshot and states that the outcome is unknown.
    let orphan_scope = InvocationScope::new("op-4-11", "inv-orphan").unwrap();
    let orphan = store.open(orphan_scope.clone()).unwrap();
    orphan
        .replace_partial_output("stdout: 8 bytes seen\npartial")
        .unwrap();
    orphan
        .set_memo("step/read", json!({"file": "a.txt"}))
        .unwrap();
    let recovery = store.recover_unsafe_orphan(orphan_scope.clone()).unwrap();
    assert_eq!(recovery.invocation.outcome, InvocationOutcome::Unknown);
    assert!(recovery.invocation.is_error);
    assert!(recovery
        .invocation
        .text
        .contains(INTERRUPTED_OUTCOME_UNKNOWN_MARKER));
    assert!(recovery.invocation.text.starts_with("stdout: 8 bytes seen"));
    assert_eq!(
        recovery.invocation.partial_output.as_deref(),
        Some("stdout: 8 bytes seen\npartial")
    );
    assert!(!recovery.settlement.deleted_values.is_empty());
    assert!(store.stored_values(&orphan_scope).is_empty());
    // A second recovery must refuse: the outcome is now known.
    let second = store.recover_unsafe_orphan(orphan_scope);
    assert!(second.is_err(), "{second:?}");
}

// ── 4.12 deferred provider suspend/resume/handles/poll permits ───────────

fn deferred_identity() -> ModelIdentity {
    ModelIdentity::new("anthropic", "claude-sonnet-4")
}

fn deferred_handle(id: &str) -> DeferredHandle {
    DeferredHandle::new("anthropic", "claude-sonnet-4", "anthropic-messages", id)
}

fn suspend_with(declaration: DeferredResponseDeclaration) -> DeferredSuspendDecision {
    suspend_deferred_response(&deferred_identity(), "run-4-12", "entry-1", declaration)
}

fn deferred_declaration(handle: Option<DeferredHandle>) -> DeferredResponseDeclaration {
    DeferredResponseDeclaration {
        stop_reason: DeferredStopReason::Deferred,
        api: "anthropic-messages".to_string(),
        handle,
    }
}

fn valid_suspension() -> DeferredSuspended {
    match suspend_with(deferred_declaration(Some(deferred_handle("resp-1")))) {
        DeferredSuspendDecision::Suspended(suspended) => *suspended,
        other => panic!("a valid handle must suspend, got {other:?}"),
    }
}

#[test]
fn deferred_suspension_requires_a_valid_handle_and_rejects_every_mismatch() {
    let suspended = valid_suspension();
    assert_eq!(suspended.poll, 0);
    assert_eq!(suspended.phase, DeferredPhase::Suspended);
    assert_eq!(suspended.generation, 0);
    assert_eq!(suspended.source_entry_id, "entry-1");
    assert_eq!(suspended.observation().poll, 0);

    for (handle, expected) in [
        (None, DeferredHandleRejection::Absent),
        (Some(deferred_handle("")), DeferredHandleRejection::EmptyId),
        (
            Some(DeferredHandle::new(
                "openai",
                "gpt-5",
                "anthropic-messages",
                "resp-foreign",
            )),
            DeferredHandleRejection::ForeignProvider {
                configured: deferred_identity(),
                handle: ModelIdentity::new("openai", "gpt-5"),
            },
        ),
        (
            Some(DeferredHandle::new(
                "anthropic",
                "claude-sonnet-4",
                "openai-responses",
                "resp-wrong-api",
            )),
            DeferredHandleRejection::ForeignApi {
                configured: "anthropic-messages".to_string(),
                handle: "openai-responses".to_string(),
            },
        ),
    ] {
        match suspend_with(deferred_declaration(handle)) {
            DeferredSuspendDecision::Failed(failure) => {
                assert_eq!(
                    failure.kind,
                    DeferredSuspendFailureKind::MalformedHandle(expected.clone())
                );
                assert!(
                    failure
                        .diagnostic
                        .starts_with(INVALID_DEFERRED_HANDLE_DIAGNOSTIC),
                    "{}",
                    failure.diagnostic
                );
            }
            other => panic!("{expected:?} must fail terminally, got {other:?}"),
        }
    }

    // Non-deferred stop reasons never suspend; aborts and provider errors are
    // terminal decisions, not suspensions.
    assert_eq!(
        suspend_with(DeferredResponseDeclaration {
            stop_reason: DeferredStopReason::Settled,
            api: "anthropic-messages".to_string(),
            handle: None,
        }),
        DeferredSuspendDecision::Settled
    );
    for (stop_reason, kind) in [
        (
            DeferredStopReason::Aborted,
            DeferredSuspendFailureKind::Aborted,
        ),
        (
            DeferredStopReason::Failed,
            DeferredSuspendFailureKind::ProviderRejected,
        ),
    ] {
        match suspend_with(DeferredResponseDeclaration {
            stop_reason,
            api: "anthropic-messages".to_string(),
            handle: None,
        }) {
            DeferredSuspendDecision::Failed(failure) => assert_eq!(failure.kind, kind),
            other => panic!("{stop_reason:?} must fail terminally, got {other:?}"),
        }
    }
}

#[test]
fn deferred_polls_need_one_permit_per_pass_and_fail_closed_on_stale_duplicate_or_foreign_handles() {
    let suspended = valid_suspension();
    let mut counter = 0usize;
    let mut fresh_ids = move || {
        counter += 1;
        format!("id-{counter}")
    };

    // No permit: the run stays durably suspended, allocates nothing, and starts
    // no provider work.
    let mut no_permit = DeferredPollPermit::none("pass-quiet", suspended.generation);
    let waiting = prepare_deferred_poll(&suspended, &mut no_permit, 0, &mut fresh_ids);
    let observed_waiting = match &waiting {
        DeferredPollPreparation::Waiting(observation) => {
            assert_eq!(observation.poll, 0);
            assert_eq!(observation.handle.id, "resp-1");
            format!("waiting(handle={})", observation.handle.id)
        }
        other => panic!("a pass without a permit must stay suspended, got {other:?}"),
    };

    // A stale permit (minted for another generation) is refused, not treated as
    // waiting: the run must not poll with a permit it was not granted.
    let mut stale = DeferredPollPermit::one("pass-stale", suspended.generation + 7);
    match prepare_deferred_poll(&suspended, &mut stale, 0, &mut fresh_ids) {
        DeferredPollPreparation::Refused(refusal) => assert_eq!(
            refusal.kind,
            DeferredPollRefusalKind::StalePermit {
                permit: suspended.generation + 7,
                leaf: suspended.generation,
            }
        ),
        other => panic!("a stale permit must be refused, got {other:?}"),
    }

    // A foreign handle is refused before any provider work.
    let mut foreign_leaf = suspended.clone();
    foreign_leaf.handle = DeferredHandle::new("openai", "gpt-5", "anthropic-messages", "resp-x");
    let mut permit = DeferredPollPermit::one("pass-foreign", foreign_leaf.generation);
    match prepare_deferred_poll(&foreign_leaf, &mut permit, 0, &mut fresh_ids) {
        DeferredPollPreparation::Refused(refusal) => assert!(
            matches!(
                refusal.kind,
                DeferredPollRefusalKind::ForeignHandle(
                    DeferredHandleRejection::ForeignProvider { .. }
                )
            ),
            "{refusal:?}"
        ),
        other => panic!("a foreign handle must be refused, got {other:?}"),
    }

    // An expired handle is refused; expiry is provider-supplied and absolute.
    let mut expired_leaf = suspended.clone();
    expired_leaf.handle.expires_at_ms = Some(1_000);
    let mut permit = DeferredPollPermit::one("pass-expired", expired_leaf.generation);
    match prepare_deferred_poll(&expired_leaf, &mut permit, 2_000, &mut fresh_ids) {
        DeferredPollPreparation::Refused(refusal) => assert_eq!(
            refusal.kind,
            DeferredPollRefusalKind::ExpiredHandle {
                expires_at_ms: 1_000
            }
        ),
        other => panic!("an expired handle must be refused, got {other:?}"),
    }

    eprintln!(
        "observed {observed_waiting} for a permit-less pass against generation {}",
        suspended.generation
    );

    // The granted permit admits exactly one poll, increments the poll number,
    // reserves fresh ids, and emits `run_resume`.
    let mut permit = DeferredPollPermit::one("pass-fresh", suspended.generation);
    let admitted = match prepare_deferred_poll(&suspended, &mut permit, 0, &mut fresh_ids) {
        DeferredPollPreparation::Admitted(intent) => *intent,
        other => panic!("the permit must admit one poll, got {other:?}"),
    };
    assert_eq!(admitted.poll, 1, "a permitted poll starts poll 1");
    assert!(admitted.resume_event, "a permitted poll resumes the run");
    assert!(admitted.discard_unknown_poll.is_none());
    assert_eq!(
        admitted.phase,
        DeferredPhase::EffectPending {
            response_id: "id-1".to_string(),
            usage_id: "id-2".to_string(),
        }
    );
    assert!(permit.is_consumed());
    assert_eq!(permit.remaining(), 0);

    // A duplicate poll under the same permit is refused: the provider must not
    // be polled twice for one permit.
    match prepare_deferred_poll(&suspended, &mut permit, 0, &mut fresh_ids) {
        DeferredPollPreparation::Refused(refusal) => assert_eq!(
            refusal.kind,
            DeferredPollRefusalKind::AlreadyConsumed,
            "{refusal:?}"
        ),
        other => panic!("a duplicate poll must be refused, got {other:?}"),
    }

    // An unknown-outcome poll is replaced under fresh ids at the SAME poll
    // number, and its abandoned frame list is deleted.
    let mut unknown = valid_suspension();
    unknown.poll = 3;
    unknown.generation = 1;
    unknown.phase = DeferredPhase::EffectPending {
        response_id: "resp-unknown".to_string(),
        usage_id: "usage-unknown".to_string(),
    };
    let mut permit = DeferredPollPermit::one("pass-recovery", unknown.generation);
    let admitted = match prepare_deferred_poll(&unknown, &mut permit, 0, &mut fresh_ids) {
        DeferredPollPreparation::Admitted(intent) => *intent,
        other => panic!("recovery must admit one poll, got {other:?}"),
    };
    assert_eq!(admitted.poll, 3, "replacement keeps the poll number");
    let replacement = admitted
        .discard_unknown_poll
        .clone()
        .expect("the abandoned unknown poll is discarded");
    assert_eq!(replacement.abandoned_response_id, "resp-unknown");
    assert_eq!(replacement.abandoned_usage_id, "usage-unknown");
    assert_ne!(replacement.replacement_response_id, "resp-unknown");
    assert_ne!(replacement.replacement_usage_id, "usage-unknown");
    assert_eq!(
        admitted.phase,
        DeferredPhase::EffectPending {
            response_id: replacement.replacement_response_id.clone(),
            usage_id: replacement.replacement_usage_id.clone(),
        }
    );
    assert_ne!(
        replacement.replacement_response_id,
        replacement.replacement_usage_id
    );

    // Still deferred: back to `deferred.suspended` at the same poll number, with
    // a bumped generation, so the consumed permit cannot be reused.
    let mut polled_handle = deferred_handle("resp-2");
    polled_handle.poll_after_ms = Some(1_000);
    let resumed = unknown.resume_after_poll(
        &admitted,
        DeferredPollOutcome::StillDeferred(polled_handle.clone()),
    );
    let _ = &admitted;
    let resumed = match resumed {
        DeferredResume::Suspended(leaf) => *leaf,
        other => panic!("a still-deferred poll re-suspends, got {other:?}"),
    };
    assert_eq!(resumed.poll, 3);
    assert_eq!(resumed.phase, DeferredPhase::Suspended);
    assert_eq!(resumed.generation, unknown.generation + 1);
    assert_eq!(
        resumed.source_entry_id, replacement.replacement_response_id,
        "the newest response entry becomes the suspension source"
    );
    assert_eq!(resumed.handle, polled_handle);
    // The old permit is now stale and a new pass polls the new generation.
    let mut stale_again = DeferredPollPermit::one("pass-old", unknown.generation);
    assert!(matches!(
        prepare_deferred_poll(&resumed, &mut stale_again, 0, &mut fresh_ids),
        DeferredPollPreparation::Refused(_)
    ));
    let mut next_pass = DeferredPollPermit::one("pass-next", resumed.generation);
    match prepare_deferred_poll(&resumed, &mut next_pass, 0, &mut fresh_ids) {
        DeferredPollPreparation::Admitted(intent) => assert_eq!(intent.poll, 4),
        other => panic!("the next permitted pass polls again, got {other:?}"),
    }

    // A poll that returns another invalid handle fails closed instead of
    // parking the run on a handle nobody can poll.
    let bad = unknown.resume_after_poll(
        &admitted,
        DeferredPollOutcome::StillDeferred(DeferredHandle::new(
            "openai",
            "gpt-5",
            "anthropic-messages",
            "resp-bad",
        )),
    );
    match bad {
        DeferredResume::Failed(failure) => assert!(matches!(
            failure.kind,
            DeferredSuspendFailureKind::MalformedHandle(_)
        )),
        other => panic!("an invalid re-poll handle must fail, got {other:?}"),
    }

    // Settled and failed polls end the suspension instead of re-parking it.
    assert_eq!(
        unknown.resume_after_poll(&admitted, DeferredPollOutcome::Settled),
        DeferredResume::Settled
    );
    match unknown.resume_after_poll(
        &admitted,
        DeferredPollOutcome::Failed {
            message: "provider error".to_string(),
        },
    ) {
        DeferredResume::Failed(failure) => {
            assert_eq!(failure.kind, DeferredSuspendFailureKind::ProviderRejected)
        }
        other => panic!("a failed poll is terminal, got {other:?}"),
    }
}

// ── 4.14 summarization retry distinct from compaction failure ────────────

#[tokio::test]
async fn summarization_retries_are_distinct_from_compaction_failures_without_duplicate_durable_state(
) {
    // Deterministic capped exponential backoff, mirroring Pi's `retryDelayMs`.
    let default = SummarizationRetryPolicy::default();
    assert_eq!(default.attempts(), 3);
    assert_eq!(default.backoff_for_retry(1), Duration::from_millis(500));
    assert_eq!(default.backoff_for_retry(2), Duration::from_secs(1));
    assert_eq!(default.backoff_for_retry(3), Duration::from_secs(2));
    assert_eq!(
        SummarizationRetryPolicy {
            max_attempts: 0,
            ..default
        }
        .attempts(),
        1,
        "a zero attempt budget is one attempt"
    );
    assert_eq!(
        SummarizationRetryPolicy {
            max_attempts: usize::MAX,
            ..default
        }
        .attempts(),
        8,
        "attempts are clamped at the hard cap"
    );

    let policy = SummarizationRetryPolicy {
        max_attempts: 3,
        initial_backoff: Duration::ZERO,
        max_backoff: Duration::ZERO,
    };

    // Transient failures then success: the durable summary is committed exactly
    // once, and every scheduled retry is reported as a retry, not a failure.
    let attempts = std::cell::Cell::new(0usize);
    let commits = std::cell::Cell::new(0usize);
    let run = run_summarization_with_retry(
        &policy,
        |attempt| {
            attempts.set(attempts.get() + 1);
            async move {
                match attempt {
                    1 | 2 => SummarizationAttempt::RetryableFailure {
                        message: format!("stream dropped on attempt {attempt}"),
                    },
                    3 => SummarizationAttempt::Succeeded { summary_bytes: 128 },
                    _ => unreachable!("policy allows three attempts"),
                }
            }
        },
        |_attempt| {
            commits.set(commits.get() + 1);
            Ok(())
        },
    )
    .await;

    assert_eq!(
        run.outcome,
        SummarizationOutcome::Succeeded {
            attempts: 3,
            summary_bytes: 128
        }
    );
    assert_eq!(run.durable_summary_records(), 1);
    assert_eq!(commits.get(), 1, "a retry never re-commits durable state");
    assert_eq!(attempts.get(), 3);
    assert_eq!(
        run.diagnostics,
        vec![
            SummarizationDiagnostic::RetryScheduled {
                attempt: 1,
                max_attempts: 3,
                delay_ms: 0,
                error: "stream dropped on attempt 1".to_string(),
            },
            SummarizationDiagnostic::AttemptStart { attempt: 2 },
            SummarizationDiagnostic::RetryScheduled {
                attempt: 2,
                max_attempts: 3,
                delay_ms: 0,
                error: "stream dropped on attempt 2".to_string(),
            },
            SummarizationDiagnostic::AttemptStart { attempt: 3 },
            SummarizationDiagnostic::Finished {
                succeeded: true,
                attempts: 3,
                error: None,
            },
        ]
    );
    let retry_diagnostics: Vec<String> = run
        .diagnostics
        .iter()
        .filter_map(CompactionStepOutcome::from_diagnostic)
        .map(|outcome| match &outcome {
            CompactionStepOutcome::Retrying(retry) => {
                assert!(!outcome.is_compaction_failure());
                retry.diagnostic()
            }
            other => panic!("a scheduled retry is not a boundary outcome: {other:?}"),
        })
        .collect();
    assert_eq!(retry_diagnostics.len(), 2);
    for diagnostic in &retry_diagnostics {
        assert!(
            diagnostic.starts_with("summarization retry scheduled"),
            "{diagnostic}"
        );
    }
    assert_eq!(
        run.compaction_outcome(),
        CompactionStepOutcome::Committed {
            attempts: 3,
            durable_summary_records: 1
        }
    );

    // Exhausted retries: now the boundary fails, with the summarization cause
    // preserved and wording that cannot be confused with a retry.
    let run = run_summarization_with_retry(
        &policy,
        |_attempt| async {
            SummarizationAttempt::RetryableFailure {
                message: "socket closed".to_string(),
            }
        },
        |_attempt| panic!("a failed summarization must not commit"),
    )
    .await;
    let failed = match &run.outcome {
        SummarizationOutcome::Failed(failure) => failure.clone(),
        other => panic!("two retryable failures exhaust a three-attempt budget: {other:?}"),
    };
    assert_eq!(failed.kind, SummarizationFailureKind::RetriesExhausted);
    assert_eq!(failed.attempts, 3);
    assert_eq!(run.durable_summary_records(), 0);
    assert_eq!(
        run.diagnostics
            .iter()
            .filter(|diagnostic| matches!(
                diagnostic,
                SummarizationDiagnostic::RetryScheduled { .. }
            ))
            .count(),
        2
    );
    let boundary = run.compaction_outcome();
    let boundary_failure = match &boundary {
        CompactionStepOutcome::Failed(failure) => failure.clone(),
        other => panic!("exhausted retries fail the boundary: {other:?}"),
    };
    assert!(boundary.is_compaction_failure());
    assert_eq!(
        boundary_failure.kind,
        CompactionFailureKind::Summarization(SummarizationFailureKind::RetriesExhausted)
    );
    assert!(
        boundary_failure
            .diagnostic
            .starts_with("compaction boundary failed"),
        "{}",
        boundary_failure.diagnostic
    );
    assert!(
        !boundary_failure.diagnostic.contains("retry scheduled"),
        "a failure must not borrow the retry wording: {}",
        boundary_failure.diagnostic
    );
    for diagnostic in &retry_diagnostics {
        assert_ne!(diagnostic, &boundary_failure.diagnostic);
        assert_ne!(diagnostic, &failed.diagnostic());
    }

    // A deterministic failure is terminal on the first attempt.
    let run = run_summarization_with_retry(
        &policy,
        |_attempt| async {
            SummarizationAttempt::NonRetryableFailure {
                message: "summarization attempted to call a tool".to_string(),
            }
        },
        |_attempt| panic!("a failed summarization must not commit"),
    )
    .await;
    let failure = match &run.outcome {
        SummarizationOutcome::Failed(failure) => failure.clone(),
        other => panic!("a deterministic failure is terminal: {other:?}"),
    };
    assert_eq!(failure.kind, SummarizationFailureKind::NonRetryable);
    assert_eq!(failure.attempts, 1);
    assert!(run
        .diagnostics
        .iter()
        .all(|diagnostic| !matches!(diagnostic, SummarizationDiagnostic::RetryScheduled { .. })));
    assert_eq!(
        failure.diagnostic(),
        "summarization attempt 1 failed deterministically: summarization attempted to call a tool"
    );

    // An abort is terminal and never retried.
    let run = run_summarization_with_retry(
        &policy,
        |_attempt| async { SummarizationAttempt::Aborted },
        |_attempt| panic!("an aborted summarization must not commit"),
    )
    .await;
    match &run.outcome {
        SummarizationOutcome::Failed(failure) => {
            assert_eq!(failure.kind, SummarizationFailureKind::Aborted)
        }
        other => panic!("an abort is terminal: {other:?}"),
    }

    eprintln!(
        "observed retry diagnostic {:?} and boundary diagnostic {:?}",
        retry_diagnostics.first(),
        boundary_failure.diagnostic
    );

    // A summary that cannot be recorded is a durable-write failure of the
    // boundary, not a summarization retry: the two are reported distinctly.
    let run = run_summarization_with_retry(
        &policy,
        |_attempt| async { SummarizationAttempt::Succeeded { summary_bytes: 64 } },
        |_attempt| Err("session log is read-only".to_string()),
    )
    .await;
    assert_eq!(run.durable_summary_records(), 0);
    let boundary = run.compaction_outcome();
    match boundary {
        CompactionStepOutcome::Failed(failure) => {
            assert_eq!(failure.kind, CompactionFailureKind::DurableWrite);
            assert!(
                failure.diagnostic.contains("durable summary write failed"),
                "{}",
                failure.diagnostic
            );
        }
        other => panic!("a failed commit fails the boundary: {other:?}"),
    }

    // The policy is shared by compaction and branch summarization, so both
    // sources reach the same retry path.
    assert_eq!(
        SummarizationFailure {
            kind: SummarizationFailureKind::RetriesExhausted,
            attempts: 2,
            message: "socket closed".to_string(),
        }
        .diagnostic(),
        "summarization retries exhausted after 2 attempts: socket closed"
    );
}

// ── registered built-in surface (maintainer decision) ────────────────────

/// The model-visible built-in surface is exactly `read`/`write`/`edit`/`bash`
/// plus the ripgrep-backed `search` (and the Windows-only opt-in `powershell`).
///
/// This is the regression guard that stops a Pi-parity pass from re-adding a
/// dedicated `ls`/`find`/`grep` tool: filename discovery and content search are
/// served by `rg`, through `search` or `bash`, matching the v0.7.6 release
/// surface. `search` stays registered for embedders and explicit allowlists even
/// though the coding product leaves it out of its default allowlist.
#[test]
fn core_tools_register_exactly_the_narrow_maintainer_surface() {
    use octet_agent::extension::ExtensionHost;

    let mut host = ExtensionHost::new();
    host.load(&octet_agent::CoreTools);
    let mut names: Vec<String> = host
        .tool_definitions()
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    names.sort();

    #[cfg(windows)]
    let expected = vec!["bash", "edit", "powershell", "read", "search", "write"];
    #[cfg(not(windows))]
    let expected = vec!["bash", "edit", "read", "search", "write"];

    assert_eq!(
        names, expected,
        "the registered built-in tool surface changed"
    );
    for withdrawn in ["ls", "find", "grep"] {
        assert!(
            !names.iter().any(|name| name == withdrawn),
            "`{withdrawn}` must not be offered to the model"
        );
    }
}
