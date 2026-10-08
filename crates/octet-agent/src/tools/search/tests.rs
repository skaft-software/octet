//! Unit tests for the ripgrep-backed `search` tool.
//!
//! Separate from `search.rs` so the query/mode dispatch stays readable
//! without the argument-parsing cases interleaved.
use super::*;
use crate::sandbox::SandboxConfig;
use crate::ToolProgressSink;
use serde_json::json;
use std::path::PathBuf;
// Only the Unix process-liveness helpers below use deadlines.
#[cfg(unix)]
use std::time::{Duration, Instant};

struct Fixture {
    _dir: tempfile::TempDir,
    workspace: PathBuf,
    sandbox: SandboxConfig,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().canonicalize().unwrap();
    std::fs::create_dir(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("src/api.rs"),
        "pub enum AudioPayload {\n    Inline,\n}\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("src/chat.rs"),
        "use AudioPayload;\nfn f() { let _ = AudioPayload::Inline; }\n",
    )
    .unwrap();
    let mut sandbox = SandboxConfig::new(&workspace);
    sandbox.allow_process = true;
    sandbox.allow_shell = true;
    Fixture {
        _dir: dir,
        workspace,
        sandbox,
    }
}

impl Fixture {
    fn ctx(&self) -> ToolContext<'_> {
        self.ctx_with(&self.sandbox)
    }

    fn ctx_with<'a>(&'a self, sandbox: &'a SandboxConfig) -> ToolContext<'a> {
        ToolContext {
            workspace: &self.workspace,
            sandbox,
            execution_scope: "search-test",
            resource_owner: "search-test",
            active_skills: &[],
            registered_tools: &[],
            progress: ToolProgressSink::null(),
            cancellation: Default::default(),
        }
    }
}

fn rg_available() -> bool {
    std::process::Command::new("rg")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

#[cfg(unix)]
fn executable_script(workspace: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = workspace.join(name);
    std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
fn process_is_alive(pid: i32) -> bool {
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(unix)]
async fn wait_for_process_exit(pid: i32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while process_is_alive(pid) && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    !process_is_alive(pid)
}

// These tests exercise process I/O and deadlines, not executable startup
// latency. Fresh temporary scripts can take over a second to enter /bin/sh
// on macOS. Keep paused Tokio time from auto-advancing while real I/O runs,
// but bound every wait in wall time without a detached keepalive task.
#[cfg(unix)]
async fn drive_without_advancing_time<F: std::future::Future>(future: F) -> F::Output {
    tokio::pin!(future);
    let started = Instant::now();
    loop {
        if let std::task::Poll::Ready(result) = futures_util::poll!(&mut future) {
            return result;
        }
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "search fixture made no progress within the wall-clock watchdog"
        );
        tokio::task::yield_now().await;
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn stderr_saturation_does_not_block_search() {
    let f = fixture();
    let program = executable_script(
        &f.workspace,
        "rg-stderr-saturating",
        r#"
            dd if=/dev/zero bs=1048576 count=1 >&2
            exit 2
            "#,
    );
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_secs(2);
    let ctx = f.ctx_with(&sandbox);
    let started = tokio::time::Instant::now();

    let error = drive_without_advancing_time(SearchTool.execute_with_program(
        json!({"query": "needle"}),
        &ctx,
        &program,
    ))
    .await
    .unwrap_err();

    assert!(started.elapsed() < sandbox.bash_timeout);
    assert!(
        error.message.contains("ripgrep reported an error"),
        "{error}"
    );
}

#[cfg(unix)]
#[tokio::test(start_paused = true)]
async fn stdout_eof_does_not_bypass_search_timeout_or_cleanup() {
    let f = fixture();
    let program = executable_script(
        &f.workspace,
        "rg-closes-stdout",
        r#"
            exec 1>&-
            printf '%s\n' "$$" > child.pid
            kill -STOP "$$"
            exit 2
            "#,
    );
    let mut sandbox = f.sandbox.clone();
    sandbox.bash_timeout = Duration::from_millis(150);
    let ctx = f.ctx_with(&sandbox);
    let started = tokio::time::Instant::now();
    let search = SearchTool.execute_with_program(json!({"query": "needle"}), &ctx, &program);
    tokio::pin!(search);

    // The PID is published only after stdout is closed. Do not expire the
    // search before the shell has entered the EOF-with-live-child state.
    let pid = drive_without_advancing_time(std::future::poll_fn(|cx| {
        assert!(
            std::future::Future::poll(search.as_mut(), cx).is_pending(),
            "search completed before its deadline with a live child"
        );
        let pid = std::fs::read_to_string(f.workspace.join("child.pid"))
            .ok()
            .filter(|text| text.ends_with('\n'))
            .and_then(|text| text.trim().parse::<i32>().ok());
        match pid {
            Some(pid) => std::task::Poll::Ready(pid),
            None => std::task::Poll::Pending,
        }
    }))
    .await;
    assert!(process_is_alive(pid), "EOF fixture child exited early");
    assert_eq!(started.elapsed(), Duration::ZERO);

    tokio::time::advance(sandbox.bash_timeout).await;
    let error = drive_without_advancing_time(&mut search).await.unwrap_err();
    assert!(error.message.contains("execution limit"), "{error}");
    assert_eq!(started.elapsed(), sandbox.bash_timeout);
    // Reaping is an OS observation, so retain a real-time cleanup bound.
    tokio::time::resume();
    assert!(
        wait_for_process_exit(pid, Duration::from_secs(1)).await,
        "timed-out search child was not reaped"
    );
}

#[test]
fn effect_requires_process_authority_and_metadata_fails_closed() {
    let mut fixture = fixture();
    assert_eq!(
        SearchTool
            .effect(&json!({"query": "needle"}), &fixture.ctx())
            .unwrap(),
        ToolEffect::HostProcess
    );
    assert_eq!(SearchTool.replay_safety(), ReplaySafety::Unsafe);
    // One self-contained `rg` child per call: overlappable only while host
    // processes need no approval, never advertised as an async tool.
    assert_eq!(SearchTool.concurrency(), ToolConcurrency::ParallelProcess);

    fixture.sandbox.allow_process = false;
    let error = SearchTool
        .effect(&json!({"query": "needle"}), &fixture.ctx())
        .unwrap_err();
    assert!(error.to_string().contains("allow_process=true"));
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::ProcessDisabled)
    );

    fixture.sandbox.allow_process = true;
    fixture.sandbox.allow_shell = false;
    let error = SearchTool
        .effect(&json!({"query": "needle"}), &fixture.ctx())
        .unwrap_err();
    assert!(error.to_string().contains("allow_shell=true"));
    assert_eq!(
        error.policy_denial_code(),
        Some(ToolPolicyDenialCode::ShellDisabled)
    );
}

#[test]
fn effect_rejects_malicious_argument_shapes_before_spawning() {
    let f = fixture();
    for arguments in [
        json!({"query": "needle", "path": "../outside"}),
        json!({"query": "needle", "path": "/tmp/outside"}),
        json!({"query": "needle", "unexpected": true}),
        json!({"query": ""}),
        json!({"query": "needle", "max_results": 0}),
    ] {
        assert!(
            SearchTool.effect(&arguments, &f.ctx()).is_err(),
            "{arguments}"
        );
    }
    assert!(SearchTool
        .effect(
            &json!({"query": "x".repeat(MAX_SEARCH_PATTERN_BYTES + 1)}),
            &f.ctx(),
        )
        .is_err());
}

fn match_event(path: &str, line: u64, text: &str) -> String {
    serde_json::json!({
        "type": "match",
        "data": {
            "path": {"text": path},
            "line_number": line,
            "lines": {"text": text}
        }
    })
    .to_string()
}

#[tokio::test]
async fn bounded_rg_framing_handles_fragmentation_final_records_and_limits() {
    let first = match_event("a.rs", 1, "first\n");
    let second = match_event("b.rs", 2, "second\n");
    let input = format!("{first}\n{{not json}}\n{second}");
    let reader = tokio::io::BufReader::with_capacity(3, std::io::Cursor::new(input.into_bytes()));
    let (results, truncated, _) = collect_rg_stdout(reader, 10, 4 * 1024).await.unwrap();
    assert_eq!(
        results.iter().map(SearchLine::render).collect::<Vec<_>>(),
        vec!["a.rs:1  first", "b.rs:2  second"]
    );
    assert!(!truncated);

    let input = format!("{first}\n{second}\n");
    let (results, truncated, _) =
        collect_rg_stdout(std::io::Cursor::new(input.into_bytes()), 1, 4 * 1024)
            .await
            .unwrap();
    assert_eq!(
        results.iter().map(SearchLine::render).collect::<Vec<_>>(),
        vec!["a.rs:1  first"]
    );
    assert!(truncated);

    let input = format!("{first}\n{second}\n");
    let (results, truncated, _) = collect_rg_stdout(
        std::io::Cursor::new(input.into_bytes()),
        10,
        "a.rs:1  first".len(),
    )
    .await
    .unwrap();
    assert_eq!(
        results.iter().map(SearchLine::render).collect::<Vec<_>>(),
        vec!["a.rs:1  first"]
    );
    assert!(truncated);

    let oversized = vec![b'x'; MAX_RG_EVENT_BYTES + 1];
    let error = collect_rg_stdout(std::io::Cursor::new(oversized), 10, 4 * 1024)
        .await
        .unwrap_err();
    assert!(error.message.contains("record exceeded"), "{error}");
}

#[tokio::test]
async fn dense_submatches_do_not_consume_the_line_record_budget() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    // Before framing elision, this small line expands into several MiB of
    // unused JSON offsets/matched-text objects and fails the event cap.
    let dense = "a".repeat(64 * 1024);
    std::fs::write(f.workspace.join("dense.txt"), format!("{dense}\ncontext\n")).unwrap();
    for (query, mode) in [("a", "literal"), ("a|$", "regex")] {
        let out = SearchTool
            .execute(
                json!({
                    "query": query, "mode": mode, "path": "dense.txt", "context": 1
                }),
                &f.ctx(),
            )
            .await
            .unwrap();
        assert!(out.text.contains("dense.txt:1  aaa"), "{}", out.text);
        assert!(out.text.ends_with("truncated=false"), "{}", out.text);
        assert!(out.text.len() < 1024);
    }
    // File text may contain JSON-looking keys and escaped delimiters.
    let text = r#"\"submatches\":[{\"match\":\"]\"}]"#;
    let raw = match_event("quoted\npath", 1, text);
    let mut event = RgEventBuffer::default();
    for byte in raw.bytes() {
        event.push(byte).unwrap();
    }
    assert_eq!(event.bytes, raw.as_bytes());
}

#[tokio::test]
async fn literal_matches_are_formatted_and_sorted() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let out = SearchTool
        .execute(json!({"query": "AudioPayload"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.starts_with("3 matches\n"), "{}", out.text);
    assert!(
        out.text.contains("src/api.rs:1  pub enum AudioPayload {"),
        "{}",
        out.text
    );
    assert!(out.text.contains("src/chat.rs:1  use AudioPayload;"));
    assert!(out.text.ends_with("truncated=false"));
    // Deterministic path ordering: api.rs before chat.rs.
    let api = out.text.find("src/api.rs").unwrap();
    let chat = out.text.find("src/chat.rs").unwrap();
    assert!(api < chat);
}

#[tokio::test]
async fn no_matches_is_successful_output() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let out = SearchTool
        .execute(json!({"query": "NoSuchSymbolAnywhere"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(out.text, "no matches");
}

#[tokio::test]
async fn max_results_truncates_explicitly() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let out = SearchTool
        .execute(json!({"query": "AudioPayload", "max_results": 1}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.starts_with("1+ matches\n"), "{}", out.text);
    assert!(out.text.ends_with("truncated=true"), "{}", out.text);
}

#[tokio::test]
async fn regex_mode_and_glob_filter() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let out = SearchTool
        .execute(
            json!({"query": "enum \\w+Payload", "mode": "regex", "glob": "api.rs"}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("1 match\n"), "{}", out.text);
    assert!(out.text.contains("src/api.rs:1"), "{}", out.text);

    // The same pattern is inert in literal mode.
    let out = SearchTool
        .execute(json!({"query": "enum \\w+Payload"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(out.text, "no matches");
}

#[tokio::test]
async fn scoped_path_is_validated_and_used() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let out = SearchTool
        .execute(
            json!({"query": "AudioPayload", "path": "src/api.rs"}),
            &f.ctx(),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("1 match\n"), "{}", out.text);

    let err = SearchTool
        .execute(json!({"query": "x", "path": "../"}), &f.ctx())
        .await
        .unwrap_err();
    assert!(err.message.contains(".."), "{err}");
}

#[tokio::test]
async fn trusted_local_mode_searches_an_absolute_path() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("outside.txt");
    std::fs::write(&file, "needle outside workspace\n").unwrap();
    let mut sandbox = f.sandbox.clone();
    sandbox.allow_external_paths = true;
    let ctx = ToolContext {
        workspace: &f.workspace,
        sandbox: &sandbox,
        execution_scope: "search-test",
        resource_owner: "search-test",
        active_skills: &[],
        registered_tools: &[],
        progress: ToolProgressSink::null(),
        cancellation: Default::default(),
    };

    let out = SearchTool
        .execute(
            json!({"query": "needle", "path": outside.path().to_string_lossy()}),
            &ctx,
        )
        .await
        .unwrap();
    assert!(out.text.contains("outside.txt:1"), "{}", out.text);
}

#[tokio::test]
async fn programmatic_search_preserves_fields_context_and_direct_text() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let args = json!({"query": "AudioPayload", "path": "src/api.rs", "context": 1});
    let direct = SearchTool.execute(args.clone(), &f.ctx()).await.unwrap();
    assert!(direct.programmatic_content().is_none());
    let mut ctx = f.ctx();
    ctx.progress = ctx.progress.for_nested_call();
    let nested = SearchTool.execute(args, &ctx).await.unwrap();
    assert_eq!(nested.text, direct.text);
    assert_eq!(
        nested.programmatic_content().unwrap(),
        &json!({
            "matches": [
                {"path": "src/api.rs", "line": 1, "text": "pub enum AudioPayload {", "is_context": false, "text_truncated": false},
                {"path": "src/api.rs", "line": 2, "text": "    Inline,", "is_context": true, "text_truncated": false}
            ],
            "total": 1, "truncated": false
        })
    );
    let empty = SearchTool
        .execute(json!({"query": "NoSuchSymbolAnywhere"}), &ctx)
        .await
        .unwrap();
    assert_eq!(empty.text, "no matches");
    assert_eq!(
        empty.programmatic_content().unwrap(),
        &json!({"matches": [], "total": 0, "truncated": false})
    );
    let limited = SearchTool
        .execute(json!({"query": "AudioPayload", "max_results": 1}), &ctx)
        .await
        .unwrap();
    let value = limited.programmatic_content().unwrap();
    assert_eq!(value["total"], 1);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["matches"].as_array().unwrap().len(), 1);
    let schema = SearchTool.output_schema().unwrap();
    assert_eq!(
        schema["required"].as_array().unwrap().len(),
        value.as_object().unwrap().len()
    );
}

#[tokio::test]
async fn structured_search_fields_do_not_depend_on_rendered_separators() {
    let path = "odd:123-456\npath.rs";
    let text = "needle:5  value\nembedded";
    let input = format!("{}\n", match_event(path, 7, text));
    let (results, truncated, total) = collect_rg_stdout(std::io::Cursor::new(input), 10, 4096)
        .await
        .unwrap();
    assert!(!truncated);
    assert_eq!(total, 1);
    let result = &results[0];
    assert_eq!(result.path, path);
    assert_eq!(result.line, 7);
    assert_eq!(result.text, text);
    assert!(!result.is_context);
    assert!(!result.text_truncated);
    let (clipped, _) =
        render_match(&match_event(path, 8, &"a".repeat(MAX_LINE_CHARS + 1))).unwrap();
    assert!(clipped.text_truncated);
    assert!(clipped.text.len() < MAX_LINE_CHARS + 100);
}

#[tokio::test]
async fn dashed_query_is_not_treated_as_a_flag() {
    if !rg_available() {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    std::fs::write(f.workspace.join("notes.txt"), "--force is dangerous\n").unwrap();
    let out = SearchTool
        .execute(json!({"query": "--force"}), &f.ctx())
        .await
        .unwrap();
    assert!(out.text.contains("notes.txt:1"), "{}", out.text);
}
