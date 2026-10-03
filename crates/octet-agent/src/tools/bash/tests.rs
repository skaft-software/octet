//! Unix-side bash tool regressions: shell selection, capture budgeting,
//! spill behaviour and process-tree cleanup.
//!
//! Separate from `bash.rs` because the implementation is cfg-split by platform
//! and each platform's cases only ever exercise the half compiled on that
//! host; keeping them adjacent to the code made it impossible to see that
//! split without scrolling past hundreds of test lines.
use super::*;
use crate::sandbox::SandboxConfig;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

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
    sandbox.bash_timeout = Duration::from_secs(10);
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
            execution_scope: "bash-test",
            resource_owner: "bash-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        }
    }
}

fn process_is_alive(pid: i32) -> bool {
    crate::extension_process::process_is_live_for_test(pid)
}

async fn wait_for_process_exit(pid: i32, timeout: Duration) -> bool {
    let started = std::time::Instant::now();
    while process_is_alive(pid) && started.elapsed() < timeout {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    !process_is_alive(pid)
}

#[tokio::test]
async fn spill_quota_keeps_a_prefix_and_drains_the_rest() {
    use tokio::io::AsyncWriteExt;
    let bytes = b"first\nsecond\nthird\nfourth\nfifth\nlast\n";
    let (mut writer, reader) = tokio::io::duplex(4);
    let mut reader = Some(reader);
    let progress = ToolProgressSink::null();
    let drain = read_bounded_with_spill_limit(
        &mut reader,
        12,
        &progress,
        OutputStream::Stdout,
        None,
        9,
        ("quota-test", "quota-call"),
    );
    let write = async {
        writer.write_all(bytes).await.unwrap();
        writer.shutdown().await.unwrap();
    };
    let (mut capture, ()) =
        tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(drain, write) })
            .await
            .expect("quota must not stop pipe draining");
    assert_eq!(capture.total_bytes, bytes.len());
    assert_eq!(capture.spill_bytes, 9);
    assert!(capture.spill_truncated);
    assert!(!capture.spill_error);
    capture.fit_to_budget(12);
    let path = &capture.spill.as_ref().unwrap().path;
    assert_eq!(std::fs::read(path).unwrap(), bytes[..9]);
    let rendered = capture.render("stdout");
    assert!(rendered.contains("partial_output_path="), "{rendered}");
    assert!(rendered.contains("spill_truncated=true"), "{rendered}");
    assert!(!rendered.contains("full_output_path="), "{rendered}");
    assert!(!rendered.contains("spill_error=true"), "{rendered}");
    assert!(
        rendered.contains("last"),
        "tail after quota was lost: {rendered}"
    );
    std::fs::remove_file(path).unwrap();

    // Exactly reaching the cap still preserves a complete spill; only
    // seeing an omitted byte can change the path label to partial.
    let mut reader = Some(std::io::Cursor::new(bytes));
    let mut capture = read_bounded_with_spill_limit(
        &mut reader,
        12,
        &progress,
        OutputStream::Stdout,
        None,
        bytes.len(),
        ("quota-test", "quota-call-2"),
    )
    .await;
    capture.fit_to_budget(12);
    assert!(capture.render("stdout").contains("full_output_path="));
    assert!(!capture.spill_truncated);
    BashTool::release_owner("quota-test");
}

#[test]
fn effect_validates_capabilities_and_arguments_before_approval() {
    let f = fixture();
    assert_eq!(
        BashTool
            .effect(&json!({"command": "printf ok"}), &f.ctx())
            .unwrap(),
        ToolEffect::HostProcess
    );
    for arguments in [
        json!({"command": ""}),
        json!({"command": "echo ok", "cwd": "../outside"}),
        json!({"command": "echo ok", "cwd": "/tmp"}),
        json!({"command": "echo ok", "unknown": true}),
        json!({"command": "echo ok", "timeout_ms": 0}),
        json!({"command": "bad\0command"}),
    ] {
        assert!(
            BashTool.effect(&arguments, &f.ctx()).is_err(),
            "{arguments}"
        );
    }
    assert!(BashTool
        .effect(
            &json!({"command": "x".repeat(MAX_BASH_COMMAND_BYTES + 1)}),
            &f.ctx(),
        )
        .is_err());

    let mut disabled = fixture();
    disabled.sandbox.allow_shell = false;
    let error = BashTool
        .effect(&json!({"command": "printf ok"}), &disabled.ctx())
        .unwrap_err();
    assert!(error.message.contains("allow_shell=true"));
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::ShellDisabled)
    );

    disabled.sandbox.allow_shell = true;
    disabled.sandbox.allow_process = false;
    let error = BashTool
        .effect(&json!({"command": "printf ok"}), &disabled.ctx())
        .unwrap_err();
    assert!(error.message.contains("allow_process=true"));
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::ProcessDisabled)
    );
}

#[tokio::test]
async fn every_command_uses_bash_semantics() {
    let f = fixture();
    let out = BashTool
        .execute(
            json!({"command": "printf '%s\\n' brace-{one,two} \"$BASH_VERSION\""}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("exit=0"), "{}", out.text);
    assert!(out.text.contains("brace-one"), "{}", out.text);
    assert!(out.text.contains("brace-two"), "{}", out.text);
    assert!(
        out.text
            .lines()
            .any(|line| line.chars().next().is_some_and(|ch| ch.is_ascii_digit())),
        "BASH_VERSION was empty: {}",
        out.text
    );
}

#[tokio::test]
async fn explicit_shell_path_takes_precedence() {
    use std::os::unix::fs::PermissionsExt;

    let mut f = fixture();
    let shell = f.workspace.join("custom-shell");
    std::fs::write(
        &shell,
        concat!(
            "#!/bin/sh\n",
            "printf 'custom-shell\\n'\n",
            "exec /bin/sh \"$@\"\n",
        ),
    )
    .unwrap();
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    f.sandbox.shell_path = Some(shell);
    let out = BashTool
        .execute(json!({"command": "printf 'command-output\\n'"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("custom-shell"), "{}", out.text);
    assert!(out.text.contains("command-output"), "{}", out.text);
}

#[tokio::test]
async fn successful_command_without_descendants_releases_its_registry_entry() {
    let f = fixture();
    BashTool
        .execute(json!({"command": "printf '%s' $$ > leader.pid"}), &f.ctx())
        .await
        .unwrap();
    let leader = std::fs::read_to_string(f.workspace.join("leader.pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();

    assert!(
        !crate::extension_process::process_group_registered_for_test(leader),
        "a completed process group without descendants remained registered"
    );
}

#[tokio::test]
async fn detached_setsid_descendant_remains_supervised_after_leader_exit() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_secs(2);
    let cancellation = crate::tool::CancellationToken::default();
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: cancellation.clone(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-detached-descendant-test",
        resource_owner: "bash-detached-descendant-test",
    };

    let output = BashTool
            .execute(
                json!({
                    "command": r#"printf '%s' $$ > leader.pid
mkfifo descendant.ready
python3 -c 'import os,sys,time; os.setsid(); open(sys.argv[1], "w").write(str(os.getpid())); os.write(os.open(sys.argv[2], os.O_WRONLY), b"ready\n"); time.sleep(30)' descendant.pid descendant.ready </dev/null >/dev/null 2>&1 &
IFS= read -r _ < descendant.ready
rm descendant.ready"#
                }),
                &ctx,
            )
            .await
            .unwrap();
    let leader = std::fs::read_to_string(f.workspace.join("leader.pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let descendant = std::fs::read_to_string(f.workspace.join("descendant.pid"))
        .unwrap()
        .parse::<i32>()
        .unwrap();
    let descendant_was_alive = process_is_alive(descendant);
    let group_was_registered = crate::extension_process::process_group_registered_for_test(leader);

    cancellation.cancel();
    let descendant_exited = wait_for_process_exit(descendant, Duration::from_secs(2)).await;
    if !descendant_exited {
        unsafe {
            let _ = libc::kill(descendant, libc::SIGKILL);
        }
    }

    assert!(output.text.starts_with("exit=0"), "{}", output.text);
    assert!(
        descendant_was_alive,
        "background descendant exited before supervision could be verified"
    );
    assert!(
        group_was_registered,
        "leader completion prematurely unregistered the descendant process group"
    );
    assert!(
        descendant_exited,
        "background descendant survived cancellation"
    );
    assert!(
        !crate::extension_process::process_group_registered_for_test(leader),
        "cancelled descendant process group remained registered"
    );
}

#[tokio::test]
async fn all_commands_are_rejected_when_unified_shell_authority_is_false() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_shell = false;
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-permission-test",
        resource_owner: "bash-permission-test",
    };
    for command in [
        "true",
        "true | false",
        "/bin/sh -c 'printf bypass'",
        "python3 -c 'print(1)'",
        "env /bin/sh -c true",
    ] {
        let err = BashTool
            .execute(json!({"command": command}), &ctx)
            .await
            .unwrap_err();
        assert!(err.message.contains("shell-equivalent"), "{command}: {err}");
    }
}

#[tokio::test]
async fn bash_rejected_when_allow_process_false() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_process = false;
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-permission-test",
        resource_owner: "bash-permission-test",
    };
    let err = BashTool
        .execute(json!({"command": "true"}), &ctx)
        .await
        .unwrap_err();
    assert!(err.message.contains("allow_process"), "{err}");
}

#[tokio::test]
async fn nonzero_exit_and_stderr_are_reported_as_an_error() {
    let f = fixture();
    let error = BashTool
        .execute(json!({"command": "echo oops >&2; exit 3"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(error.message.contains("error nonzero_exit"), "{error}");
    assert!(error.message.contains("exit=3"), "{error}");
    assert!(error.message.contains("stderr: 1 lines\noops"), "{error}");
    assert!(error.message.contains("complete_stderr=true"), "{error}");
    assert!(!error.message.contains("truncated_stderr=false"), "{error}");
}

#[tokio::test]
async fn stdout_uses_the_unused_stderr_capture_budget() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.max_output_bytes = 2048;
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-shared-budget-test",
        resource_owner: "bash-shared-budget-test",
    };
    let out = BashTool
            .execute(
                json!({"command": "i=0; while [ $i -lt 150 ]; do printf 'abcdefghij\\n'; i=$((i+1)); done"}),
                &ctx,
            )
            .await
            .unwrap();

    assert!(out.text.contains("stdout: 150 lines"), "{}", out.text);
    assert!(out.text.contains("complete_stdout=true"), "{}", out.text);
    assert!(!out.text.contains("truncated_stdout"), "{}", out.text);
}

#[tokio::test]
async fn programmatic_bash_preserves_large_raw_streams_without_expanding_model_output() {
    let mut f = fixture();
    f.sandbox.max_output_bytes = 2048;
    let stdout = format!("begin\r\n{}\r\nend\n\n", "raw stdout ".repeat(12_000));
    let stderr = format!("warning\n{}\n\n", "raw stderr ".repeat(12_000));
    assert!(stdout.len() > 64 * 1024);
    assert!(stderr.len() > 64 * 1024);
    std::fs::write(f.workspace.join("stdout.txt"), &stdout).unwrap();
    std::fs::write(f.workspace.join("stderr.txt"), &stderr).unwrap();
    let (progress, mut receiver) = ToolProgressSink::bounded_channel();
    let mut ctx = f.ctx();
    ctx.execution_scope = "bash-programmatic-raw";
    ctx.resource_owner = "bash-programmatic-raw";
    ctx.progress = progress.for_nested_call();
    let out = BashTool
        .execute(
            json!({"command": "cat stdout.txt; cat stderr.txt >&2"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.text.len() <= f.sandbox.max_output_bytes);
    assert!(out.text.contains("truncated_stdout=head:"));
    assert!(out.text.contains("truncated_stderr=head:"));
    assert!(
        receiver.try_recv().is_err(),
        "nested raw output leaked to live progress"
    );
    let value = out.programmatic_content().unwrap();
    assert_eq!(value["stdout"], stdout);
    assert_eq!(value["stderr"], stderr);
    assert_eq!(value["exit_code"], 0);
    assert_eq!(value["truncated_stdout"], false);
    assert_eq!(value["truncated_stderr"], false);
    assert_eq!(value["complete_stdout"], true);
    assert_eq!(value["complete_stderr"], true);
    assert_eq!(value["stdout_bytes"], stdout.len());
    assert_eq!(value["stderr_bytes"], stderr.len());
    let schema = BashTool.output_schema().unwrap();
    let required = schema["required"].as_array().unwrap();
    assert_eq!(required.len(), value.as_object().unwrap().len());
    for key in required {
        assert!(value.get(key.as_str().unwrap()).is_some());
    }
    // The raw projection is not a durable detail or a Debug field.
    assert!(out.structured_content().is_none());
    assert!(!format!("{out:?}").contains("programmatic_content"));
    BashTool::release_owner("bash-programmatic-raw");
}

#[tokio::test]
async fn programmatic_stream_cap_is_per_stream_and_truncation_is_honest() {
    let progress = ToolProgressSink::null().for_nested_call();
    let mut exact = Some(std::io::Cursor::new(vec![
        b'a';
        MAX_PROGRAMMATIC_STREAM_BYTES
    ]));
    let mut oversized = Some(std::io::Cursor::new(vec![
        b'b';
        MAX_PROGRAMMATIC_STREAM_BYTES + 17
    ]));
    let (out, err) = tokio::join!(
        read_bounded_with_spill_limit(
            &mut exact,
            MAX_PROGRAMMATIC_STREAM_BYTES + 17,
            &progress,
            OutputStream::Stdout,
            None,
            0,
            ("raw-cap", "out")
        ),
        read_bounded_with_spill_limit(
            &mut oversized,
            MAX_PROGRAMMATIC_STREAM_BYTES + 17,
            &progress,
            OutputStream::Stderr,
            None,
            0,
            ("raw-cap", "err")
        )
    );
    let output = successful_output("bounded text".into(), &out, &err, Some(0)).unwrap();
    let value = output.programmatic_content().unwrap();
    assert_eq!(
        value["stdout"].as_str().unwrap().len(),
        MAX_PROGRAMMATIC_STREAM_BYTES
    );
    assert_eq!(
        value["stderr"].as_str().unwrap().len(),
        MAX_PROGRAMMATIC_STREAM_BYTES
    );
    assert_eq!(value["truncated_stdout"], false);
    assert_eq!(value["complete_stdout"], true);
    assert_eq!(value["truncated_stderr"], true);
    assert_eq!(value["complete_stderr"], false);
    assert_eq!(value["stderr_bytes"], MAX_PROGRAMMATIC_STREAM_BYTES + 17);
    BashTool::release_owner("raw-cap");
}

#[tokio::test]
async fn programmatic_capture_caps_source_bytes_before_lossy_utf8_conversion() {
    let progress = ToolProgressSink::null().for_nested_call();
    let mut bytes = vec![b'a'; MAX_PROGRAMMATIC_STREAM_BYTES - 1];
    bytes.extend_from_slice("€".as_bytes());
    let mut reader = Some(std::io::Cursor::new(bytes.clone()));
    let out = read_bounded_with_spill_limit(
        &mut reader,
        bytes.len(),
        &progress,
        OutputStream::Stdout,
        None,
        0,
        ("raw-utf8", "out"),
    )
    .await;
    assert_eq!(
        out.programmatic.as_ref().unwrap().bytes.len(),
        MAX_PROGRAMMATIC_STREAM_BYTES
    );
    let mut empty = None::<std::io::Cursor<Vec<u8>>>;
    let err = read_bounded_with_spill_limit(
        &mut empty,
        4096,
        &progress,
        OutputStream::Stderr,
        None,
        0,
        ("raw-utf8", "err"),
    )
    .await;
    let output = successful_output("bounded text".into(), &out, &err, Some(0)).unwrap();
    let value = output.programmatic_content().unwrap();
    assert_eq!(
        value["stdout"].as_str().unwrap(),
        String::from_utf8_lossy(&bytes[..MAX_PROGRAMMATIC_STREAM_BYTES]).as_ref()
    );
    assert!(value["stdout"].as_str().unwrap().ends_with('\u{fffd}'));
    assert_eq!(value["truncated_stdout"], true);
    assert_eq!(value["complete_stdout"], false);
    assert_eq!(value["stderr"], "");
    assert_eq!(value["complete_stderr"], true);
    BashTool::release_owner("raw-utf8");
}

#[tokio::test]
async fn programmatic_json_escaping_is_bounded_and_marked_as_truncated() {
    let progress = ToolProgressSink::null().for_nested_call();
    let mut reader = Some(std::io::Cursor::new(vec![0; MAX_PROGRAMMATIC_STREAM_BYTES]));
    let capture = read_bounded_with_spill_limit(
        &mut reader,
        MAX_PROGRAMMATIC_STREAM_BYTES,
        &progress,
        OutputStream::Stdout,
        None,
        0,
        ("raw-json", "out"),
    )
    .await;
    let output = successful_output("bounded text".into(), &capture, &capture, Some(0)).unwrap();
    let value = output.programmatic_content().unwrap();
    assert!(serde_json::to_vec(value).unwrap().len() <= 8 * 1024 * 1024);
    assert!(value["stdout"].as_str().unwrap().len() < MAX_PROGRAMMATIC_STREAM_BYTES);
    assert_eq!(value["truncated_stdout"], true);
    assert_eq!(value["truncated_stderr"], true);
    assert_eq!(value["complete_stdout"], false);
    assert_eq!(value["complete_stderr"], false);
    BashTool::release_owner("raw-json");
}

#[tokio::test]
async fn programmatic_nonzero_exit_still_rejects_with_the_standard_tool_error() {
    let f = fixture();
    let mut ctx = f.ctx();
    ctx.progress = ctx.progress.for_nested_call();
    let error = BashTool
        .execute(json!({"command": "printf 'oops\\n' >&2; exit 7"}), &ctx)
        .await
        .unwrap_err();
    assert!(error.message.starts_with("error nonzero_exit\nexit=7"));
    assert!(error
        .message
        .contains("stderr: 1 lines\noops\ncomplete_stderr=true"));
    let direct = BashTool
        .execute(json!({"command": "printf 'ok\\n'"}), &f.ctx())
        .await
        .unwrap();
    assert!(direct.programmatic_content().is_none());
}

#[tokio::test]
async fn cwd_is_workspace_bounded() {
    let f = fixture();
    std::fs::create_dir(f.workspace.join("sub")).unwrap();
    let out = BashTool
        .execute(json!({"command": "pwd", "cwd": "sub"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("/sub"), "{}", out.text);

    let err = BashTool
        .execute(json!({"command": "pwd", "cwd": "../"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains(".."), "{err}");

    let err = BashTool
        .execute(json!({"command": "pwd", "cwd": "missing"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains("missing"), "{err}");
}

#[tokio::test]
async fn trusted_local_mode_accepts_an_absolute_cwd() {
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_external_paths = true;
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-external-path-test",
        resource_owner: "bash-external-path-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
    };

    let out = BashTool
        .execute(
            json!({"command": "pwd", "cwd": outside.path().to_string_lossy()}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.text
            .contains(&outside.path().canonicalize().unwrap().display().to_string()),
        "{}",
        out.text
    );
}

#[tokio::test]
async fn output_is_bounded_with_head_tail_truncation() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.max_output_bytes = 2048;
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-output-test",
        resource_owner: "bash-output-test",
    };
    let out = BashTool
        .execute(
            json!({"command": "i=0; while [ $i -lt 2000 ]; do echo \"line $i\"; i=$((i+1)); done"}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(
        out.text.len() <= sandbox.max_output_bytes,
        "result exceeded the configured cap: {} bytes",
        out.text.len()
    );
    assert!(out.text.len() < 8192, "output must stay bounded");
    assert!(out.text.contains("truncated_stdout=head:"), "{}", out.text);
    assert!(out.text.contains("omitted_bytes:"), "{}", out.text);
    assert!(out.text.contains("line 0"), "head preserved: {}", out.text);
    assert!(
        out.text.contains("line 1999"),
        "tail preserved: {}",
        out.text
    );
}

#[tokio::test]
async fn timeout_kills_the_child() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_millis(200);
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-timeout-test",
        resource_owner: "bash-timeout-test",
    };
    let started = std::time::Instant::now();
    let err = BashTool
        .execute(
            json!({"command": "printf 'partial-before-timeout\\n'; sleep 30"}),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(err.message.contains("timeout"), "{err}");
    assert!(
        err.message.contains("partial-before-timeout"),
        "timeout diagnostics must retain partial output: {err}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "timeout must not wait for the child's natural exit"
    );
}

#[tokio::test]
async fn per_call_timeout_overrides_sandbox() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_secs(30);
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-per-call-timeout-test",
        resource_owner: "bash-per-call-timeout-test",
    };
    let started = std::time::Instant::now();
    let err = BashTool
        .execute(json!({"command": "sleep 30", "timeout_ms": 200}), &ctx)
        .await
        .unwrap_err();
    assert!(err.message.contains("timeout"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "per-call timeout must fire before sandbox timeout"
    );
}

#[tokio::test]
async fn timeout_drain_is_bounded_when_escaped_descendant_holds_pipes() {
    let f = fixture();
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_millis(100);
    let ctx = ToolContext {
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "bash-escaped-pipe-test",
        resource_owner: "bash-escaped-pipe-test",
    };
    let started = std::time::Instant::now();
    let error = BashTool
            .execute(
                json!({"command": "python3 -c 'import os,time; os.setsid(); open(\"escaped.pid\", \"w\").write(str(os.getpid())); time.sleep(30)' & sleep 30"}),
                &ctx,
            )
            .await
            .unwrap_err();

    if let Ok(pid) = std::fs::read_to_string(f.workspace.join("escaped.pid")) {
        if let Ok(pid) = pid.parse::<i32>() {
            unsafe {
                let _ = libc::kill(pid, libc::SIGKILL);
            }
        }
    }
    // Either behaviour is acceptable: the escaped descendant may exit
    // quickly enough that the drain succeeds, or it may hold pipes open.
    assert!(
        error.message.contains("output drain abandoned") || error.message.contains("timeout"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "escaped pipe holder defeated the deadline"
    );
}

#[tokio::test]
async fn cancellation_kills_the_child_process_tree() {
    let f = fixture();
    let marker = format!("986{}", std::process::id());
    let args = json!({
        "command": format!("sleep {marker} & wait")
    });

    {
        let ctx = f.ctx();
        let tool = BashTool;
        let bash = tool.execute(args, &ctx);
        tokio::pin!(bash);
        let _ = tokio::time::timeout(Duration::from_millis(500), &mut bash).await;
    }

    tokio::time::sleep(Duration::from_millis(300)).await;
    let check = tokio::process::Command::new("pgrep")
        .args(["-f", &format!("sleep {marker}")])
        .output()
        .await
        .unwrap();
    assert!(
        check.stdout.is_empty(),
        "grandchild survived cancellation: {}",
        String::from_utf8_lossy(&check.stdout)
    );
}
