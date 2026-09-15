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
use std::time::Duration;

use octet_agent::effect::ToolEffect;
use octet_agent::sandbox::SandboxConfig;
use octet_agent::tool::{Tool, ToolContext, ToolProgressSink};
use octet_agent::tools::{
    BashTool, EditTool, FindTool, GrepTool, LsTool, PowerShellTool, ReadTool, SearchTool,
    ShellSessionEnvironment, WriteTool,
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

fn binary_available(name: &str) -> bool {
    std::process::Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

/// Runs `body` with `PATH` restricted to `path` and restores it afterwards.
/// The caller must hold [`serial`].
async fn run_with_path<T>(path: &std::path::Path, body: impl std::future::Future<Output = T>) -> T {
    let previous = std::env::var_os("PATH");
    // SAFETY: the caller holds the process-wide `serial` lock.
    unsafe { std::env::set_var("PATH", path) };
    let result = body.await;
    match previous {
        Some(value) => unsafe { std::env::set_var("PATH", value) },
        None => unsafe { std::env::remove_var("PATH") },
    }
    result
}

// ── 4.1 `ls`: directories, dotfiles, limit ────────────────────────────────

#[cfg(unix)]
#[tokio::test]
async fn ls_lists_directories_and_dotfiles_without_recursing() {
    let f = fixture();
    f.write("B.txt", "b");
    f.write("a.txt", "a");
    f.write(".dotfile", "d");
    f.write("sub/inner.txt", "i");

    let output = LsTool
        .execute(json!({"path": "."}), &f.ctx())
        .await
        .unwrap();

    // Case-insensitive ordering: `.dotfile`, `a.txt`, `B.txt`, `sub/`.
    let lines: Vec<&str> = output.text.lines().collect();
    assert_eq!(
        lines,
        vec![".dotfile", "a.txt", "B.txt", "sub/"],
        "{}",
        output.text
    );
    // Directories carry a trailing slash and the listing is not recursive.
    assert!(!output.text.contains("inner.txt"), "{}", output.text);

    // A subdirectory lists its own children when requested explicitly.
    let nested = LsTool
        .execute(json!({"path": "sub"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(nested.text, "inner.txt");

    // Effect classification stays a workspace read; `ls` needs no process
    // authority even though it enumerates through descriptor-relative syscalls.
    assert_eq!(
        LsTool.effect(&json!({"path": "."}), &f.ctx()).unwrap(),
        ToolEffect::WorkspaceRead
    );
}

#[cfg(unix)]
#[tokio::test]
async fn ls_enforces_limit_and_rejects_zero() {
    let f = fixture();
    for name in ["a", "b", "c"] {
        f.write(name, "x");
    }

    let limited = LsTool
        .execute(json!({"path": ".", "limit": 2}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(
        limited.text,
        "a\nb\n[entries or byte limit reached; limit=2; truncated=true]"
    );

    let error = LsTool
        .execute(json!({"path": ".", "limit": 0}), &f.ctx())
        .await
        .unwrap_err();
    assert!(error.message.contains("limit must be positive"), "{error}");
}

#[cfg(unix)]
#[tokio::test]
async fn ls_reports_an_empty_directory_explicitly() {
    let f = fixture();
    std::fs::create_dir_all(f.workspace.join("empty")).unwrap();
    let output = LsTool
        .execute(json!({"path": "empty"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(output.text, "(empty directory)");
}

// ── 4.2 `find`: glob, gitignore, limit ───────────────────────────────────

#[tokio::test]
async fn find_glob_includes_hidden_paths_and_respects_gitignore_with_limit() {
    let _serial = serial().await;
    if !binary_available("fd") {
        eprintln!("skipping: fd not on PATH");
        return;
    }
    let f = fixture();
    // A `.git` directory makes the workspace a repository, so `.gitignore` is
    // authoritative for the run.
    std::fs::create_dir_all(f.workspace.join(".git")).unwrap();
    f.write(".gitignore", "ignored.rs\n");
    f.write("src/main.rs", "fn main() {}");
    f.write("src/notes.txt", "notes");
    f.write("ignored.rs", "fn ignored() {}");
    f.write(".hidden/keep.rs", "fn keep() {}");

    let output = FindTool
        .execute(json!({"pattern": "*.rs"}), &f.ctx())
        .await
        .unwrap();
    assert!(output.text.contains("src/main.rs"), "{}", output.text);
    // `--hidden`: dot-directories are searched...
    assert!(output.text.contains(".hidden/keep.rs"), "{}", output.text);
    // ...while `.gitignore` rules still exclude ignored paths.
    assert!(!output.text.contains("ignored.rs"), "{}", output.text);
    assert!(!output.text.contains("notes.txt"), "{}", output.text);
    // Untruncated output is a bare, newline-separated path list.
    let mut listed: Vec<&str> = output.text.lines().collect();
    listed.sort_unstable();
    assert_eq!(
        listed,
        vec![".hidden/keep.rs", "src/main.rs"],
        "{}",
        output.text
    );
    assert_eq!(output.text.matches('\n').count(), 1, "{:?}", output.text);

    let limited = FindTool
        .execute(json!({"pattern": "*.rs", "limit": 1}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(
        limited.text.lines().last().unwrap(),
        "[results or byte limit reached; limit=1; truncated=true]"
    );

    let none = FindTool
        .execute(json!({"pattern": "*.nomatch"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(none.text, "No files found matching pattern");
}

#[tokio::test]
async fn find_reports_a_missing_fd_as_an_error_without_downloading() {
    let _serial = serial().await;
    let f = fixture();
    let empty = f.workspace.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    let error = run_with_path(&empty, async {
        FindTool
            .execute(json!({"pattern": "*.rs", "path": "."}), &f.ctx())
            .await
    })
    .await
    .unwrap_err();
    // No download: the failure is a clear tool error naming the missing
    // primitive, not a silent network fetch or a bash fallback.
    assert!(
        error.message.contains("find requires installed fd"),
        "{error}"
    );
}

// ── 4.3 default `grep`: ignoreCase, context, limit, hidden ───────────────

#[tokio::test]
async fn grep_defaults_include_hidden_files_and_honor_ignore_case_and_context() {
    let _serial = serial().await;
    if !binary_available("rg") {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    f.write(".hidden/secrets.txt", "line one\nNEEDLE\nline three\n");
    f.write("plain.txt", "nothing here\n");

    // Hidden files are searched by default.
    let hidden = GrepTool
        .execute(json!({"pattern": "NEEDLE"}), &f.ctx())
        .await
        .unwrap();
    assert!(
        hidden.text.contains(".hidden/secrets.txt:2"),
        "{}",
        hidden.text
    );
    assert!(hidden.text.starts_with("1 match\n"), "{}", hidden.text);
    assert!(hidden.text.ends_with("truncated=false"), "{}", hidden.text);

    // ignoreCase is off by default and turns the same call into a match.
    let case_sensitive = GrepTool
        .execute(json!({"pattern": "needle"}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(case_sensitive.text, "no matches");
    let ignore_case = GrepTool
        .execute(json!({"pattern": "needle", "ignoreCase": true}), &f.ctx())
        .await
        .unwrap();
    assert!(
        ignore_case.text.contains(".hidden/secrets.txt:2"),
        "{}",
        ignore_case.text
    );

    // Context lines use a `-` separator and do not consume the match limit.
    let context = GrepTool
        .execute(json!({"pattern": "NEEDLE", "context": 1}), &f.ctx())
        .await
        .unwrap();
    assert!(
        context.text.contains(".hidden/secrets.txt-1  line one"),
        "{}",
        context.text
    );
    assert!(
        context.text.contains(".hidden/secrets.txt-3  line three"),
        "{}",
        context.text
    );
    assert!(context.text.starts_with("1 match\n"), "{}", context.text);

    // `hidden: false` excludes dot-directories again.
    let visible_only = GrepTool
        .execute(json!({"pattern": "NEEDLE", "hidden": false}), &f.ctx())
        .await
        .unwrap();
    assert_eq!(visible_only.text, "no matches");
}

#[tokio::test]
async fn grep_limit_defaults_to_one_hundred_and_rejects_bad_arguments() {
    let _serial = serial().await;
    if !binary_available("rg") {
        eprintln!("skipping: rg not on PATH");
        return;
    }
    let f = fixture();
    let body: String = (1..=5).map(|n| format!("needle {n}\n")).collect();
    f.write("many.txt", &body);

    let limited = GrepTool
        .execute(json!({"pattern": "needle", "limit": 3}), &f.ctx())
        .await
        .unwrap();
    assert!(limited.text.starts_with("3+ matches\n"), "{}", limited.text);
    assert!(limited.text.ends_with("truncated=true"), "{}", limited.text);
    assert_eq!(
        limited.text.matches("many.txt").count(),
        3,
        "{}",
        limited.text
    );

    // Default limit is 100: five matches are all returned, untruncated.
    let defaulted = GrepTool
        .execute(json!({"pattern": "needle"}), &f.ctx())
        .await
        .unwrap();
    assert!(
        defaulted.text.starts_with("5 matches\n"),
        "{}",
        defaulted.text
    );
    assert_eq!(defaulted.text.matches("many.txt").count(), 5);

    for (arguments, expected) in [
        (
            json!({"pattern": "x", "unknown": true}),
            "unknown grep property",
        ),
        (json!({"pattern": "x", "limit": 0}), "positive integer"),
        (
            json!({"pattern": "x", "max_results": 1}),
            "unknown grep property",
        ),
        (
            json!({"pattern": "x", "ignoreCase": "yes"}),
            "must be boolean",
        ),
        (
            json!({"pattern": "x", "literal": "yes"}),
            "literal must be boolean",
        ),
        (
            json!({"pattern": "x", "mode": "regex"}),
            "unknown grep property",
        ),
    ] {
        let error = GrepTool
            .execute(arguments.clone(), &f.ctx())
            .await
            .unwrap_err();
        assert!(error.message.contains(expected), "{arguments}: {error}");
    }

    // `literal` bypasses regex interpretation; regex-free patterns behave the
    // same either way because literal is the default mode.
    f.write("regex.txt", "a.c\n");
    let literal = GrepTool
        .execute(json!({"pattern": "a.c", "literal": true}), &f.ctx())
        .await
        .unwrap();
    assert!(
        literal.text.contains("regex.txt:1  a.c"),
        "{}",
        literal.text
    );
    let defaulted_mode = GrepTool
        .execute(json!({"pattern": "a.c"}), &f.ctx())
        .await
        .unwrap();
    assert!(
        defaulted_mode.text.contains("regex.txt:1  a.c"),
        "{}",
        defaulted_mode.text
    );
    assert!(
        defaulted_mode.text.contains("many.txt") == false,
        "a literal `a.c` must not match `needle 1`: {}",
        defaulted_mode.text
    );

    // The definition advertises the Pi-shaped schema.
    let definition = GrepTool.definition();
    assert_eq!(definition.name, "grep");
    let properties = definition.parameters["properties"].as_object().unwrap();
    for key in [
        "pattern",
        "path",
        "glob",
        "ignoreCase",
        "literal",
        "context",
        "limit",
        "hidden",
    ] {
        assert!(properties.contains_key(key), "missing grep property {key}");
    }
    assert_eq!(definition.parameters["required"], json!(["pattern"]));
    // The search-only vocabulary is not advertised through `grep`.
    for key in ["query", "mode", "max_results"] {
        assert!(!properties.contains_key(key), "grep must not expose {key}");
    }
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

    let tools: Vec<&dyn Tool> = vec![
        &BashTool,
        &ReadTool,
        &EditTool,
        &WriteTool,
        &SearchTool,
        &LsTool,
        &FindTool,
        &GrepTool,
    ];
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
        vec!["bash", "read", "edit", "write", "ls", "find", "grep"]
    );
    assert_eq!(
        by_name("bash").snippet,
        "Execute bash commands (ls, grep, find, etc.)"
    );
    assert_eq!(by_name("read").snippet, "Read file contents");
    assert_eq!(by_name("write").snippet, "Create or overwrite files");
    assert_eq!(by_name("ls").snippet, "List directory contents");
    assert_eq!(
        by_name("find").snippet,
        "Find files by glob pattern (respects .gitignore)"
    );
    assert_eq!(
        by_name("grep").snippet,
        "Search file contents for patterns (respects .gitignore)"
    );
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
    assert!(by_name("ls").guidelines.is_empty());
    assert!(by_name("grep").guidelines.is_empty());

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
        None,
        "search stays unadvertised: the coding product disables it by default"
    );
}
