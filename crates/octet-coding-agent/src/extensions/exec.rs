//! Policy-bound Pi direct process execution, using the native effect broker and
//! process-group lifetime guard. No shell interpolation or Node execution.
use super::*;
use octet_agent::extension_process::{
    sanitized_subprocess_environment, ExtensionExecRequest, ProcessGroupGuard,
};
use tokio::io::AsyncReadExt;

impl ExecutableExtensions {
    pub(super) fn start_exec_request(
        &mut self,
        process: ExtensionProcess,
        request_id: ExtensionRequestId,
        generation: u64,
        owner: ExtensionResourceOwner,
        request: ExtensionExecRequest,
    ) {
        self.renderer_tasks.retain(|task| !task.is_finished());
        let (progress, output_limit) =
            process.exec_request_frontend_context(&request_id, generation);
        let config = self.rescan_config.clone();
        self.renderer_tasks.push(tokio::spawn(async move {
            let result = match config {
                Some(config) => {
                    execute_with_frontend(
                        &config,
                        &owner.extension_instance_id,
                        generation,
                        &request,
                        progress,
                        output_limit,
                        || process.exec_request_is_cancelled(&request_id, generation),
                    )
                    .await
                }
                None => Err(anyhow::anyhow!(
                    "host process execution configuration is unavailable"
                )),
            };
            let outcome = match result {
                Ok(result) => {
                    let wire =
                        serde_json::json!({"jsonrpc":"2.0", "id":request_id, "result":result});
                    if serde_json::to_vec(&wire)
                        .expect("JSON wire serializes")
                        .len()
                        <= output_limit
                    {
                        ExtensionRequestOutcome::Ok(result)
                    } else {
                        ExtensionRequestOutcome::Failed(
                            ExtensionRequestFailure::BoundsExceeded,
                            "full exec result exceeds the negotiated protocol frame capacity"
                                .into(),
                        )
                    }
                }
                Err(error) => ExtensionRequestOutcome::Failed(
                    ExtensionRequestFailure::InvalidRequest,
                    error.to_string(),
                ),
            };
            let _ = process
                .respond_to_extension_request(request_id, generation, outcome)
                .await;
        }));
    }
}

async fn capture<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    limit: usize,
) -> anyhow::Result<String> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(
        bytes.len() <= limit,
        "full exec output exceeds the negotiated protocol capacity"
    );
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
async fn execute(
    config: &Config,
    principal: &str,
    generation: u64,
    request: &ExtensionExecRequest,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<serde_json::Value> {
    execute_with_frontend(
        config,
        principal,
        generation,
        request,
        None,
        octet_agent::extension_process::DEFAULT_EXTENSION_MESSAGE_BYTES,
        cancelled,
    )
    .await
}

async fn execute_with_frontend(
    config: &Config,
    principal: &str,
    generation: u64,
    request: &ExtensionExecRequest,
    progress: Option<ToolProgressSink>,
    output_limit: usize,
    cancelled: impl Fn() -> bool,
) -> anyhow::Result<serde_json::Value> {
    request.validate().map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        config.sandbox.allow_process,
        "process execution is disabled by host policy"
    );
    // Passing a shell executable must not bypass the independent shell gate.
    anyhow::ensure!(
        config.sandbox.allow_shell,
        "direct exec requires the host shell/process gate"
    );
    let sandbox = config.sandbox.to_sandbox_config(&config.workspace);
    let context = octet_agent::ToolContext {
        workspace: &config.workspace,
        sandbox: &sandbox,
        execution_scope: principal,
        resource_owner: principal,
        active_skills: &[],
        registered_tools: &[],
        progress: octet_agent::ToolProgressSink::null(),
        cancellation: octet_agent::CancellationToken::default(),
    };
    let cwd = context.resolve_existing(&request.cwd)?;
    anyhow::ensure!(cwd.is_dir(), "exec cwd is not a directory");
    // The native safety parser accepts only a plain word as command_name.
    // Leave shell-inert executable names unquoted; quote every argument and
    // any executable containing metacharacters so those remain fail-closed.
    let executable = if request
        .command
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/_-.".contains(&byte))
    {
        request.command.clone()
    } else {
        shell_quote(&request.command)
    };
    let safety_command = std::iter::once(executable)
        .chain(request.args.iter().map(|argument| shell_quote(argument)))
        .collect::<Vec<_>>()
        .join(" ");
    let intent = octet_agent::EffectIntent::new(
        principal,
        "pi.exec",
        generation,
        format!("{}", request.parent_request_id),
        "bash",
        octet_agent::ToolEffect::HostProcess,
        serde_json::json!({"command":safety_command, "exec":request}),
    )?;
    let broker = octet_agent::EffectBroker::new(config.effect_policy);
    // Same native bash safety analysis and one-shot approval path used by
    // approve_shell_escape. Only analysis sees quoting; execution keeps argv.
    let abandoned = async {
        while !cancelled() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    tokio::select! {
        result = broker.authorize(&intent, progress.as_ref()) => { result?; }
        _ = abandoned => return Ok(serde_json::json!({"stdout":"", "stderr":"", "code":0, "killed":true})),
    }
    if cancelled() {
        return Ok(serde_json::json!({"stdout":"", "stderr":"", "code":0, "killed":true}));
    }
    #[cfg(windows)]
    anyhow::bail!("direct exec process-group supervision is unavailable on Windows");
    #[cfg(not(windows))]
    {
        let mut command = tokio::process::Command::new(&request.command);
        command
            .args(&request.args)
            .current_dir(cwd)
            .env_clear()
            .envs(sanitized_subprocess_environment())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                return Ok(
                    serde_json::json!({"stdout":"", "stderr":"", "code":1, "killed":request.cancelled}),
                )
            }
        };
        let guard = ProcessGroupGuard::bash(child.id());
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let started = Instant::now();
        let mut killed = false;
        let stop = async {
            loop {
                if request.cancelled
                    || cancelled()
                    || request
                        .timeout_ms
                        .filter(|ms| *ms > 0)
                        .is_some_and(|ms| started.elapsed() >= Duration::from_millis(ms))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        };
        let result = async {
            let wait = async {
                tokio::select! {
                    result = child.wait() => { guard.terminate_now(); result },
                    _ = stop => {
                        killed = true; guard.signal_terminate();
                        let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                            Ok(result) => result,
                            Err(_) => { guard.terminate_now(); child.wait().await }
                        };
                        guard.terminate_now(); status
                    }
                }
            };
            let (status, stdout, stderr) = tokio::try_join!(wait, async { capture(stdout, output_limit).await.map_err(std::io::Error::other) },
                async { capture(stderr, output_limit).await.map_err(std::io::Error::other) })?;
            Ok::<_, anyhow::Error>(serde_json::json!({"stdout":stdout, "stderr":stderr, "code":status.code().unwrap_or(0), "killed":killed}))
        }.await;
        // Kill remaining descendants even after the leader exits; cancellation
        // or a pipe/read failure must never detach a process tree.
        guard.terminate_now();
        let _ = child.wait().await;
        guard.disarm();
        result
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn request(cwd: &std::path::Path, script: &str) -> ExtensionExecRequest {
        ExtensionExecRequest {
            parent_request_id: 1,
            resource_owner: None,
            command: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            cwd: cwd.canonicalize().unwrap().to_string_lossy().into(),
            timeout_ms: None,
            cancelled: false,
        }
    }
    #[tokio::test]
    async fn native_exec_denial_does_not_launch() {
        let dir = tempfile::tempdir().unwrap();
        let (_fixture, app) = crate::compaction::tests::app_for_estimate();
        let mut config = app.config.clone();
        config.workspace = dir.path().canonicalize().unwrap();
        config.sandbox.allow_process = true;
        config.sandbox.allow_shell = true;
        config.effect_policy = octet_agent::EffectPolicy::Controlled;
        assert!(execute(
            &config,
            "test",
            1,
            &request(dir.path(), "touch forbidden"),
            || false
        )
        .await
        .is_err());
        assert!(!dir.path().join("forbidden").exists());
    }
    #[tokio::test]
    async fn native_exec_preserves_output_status_cwd_and_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (_fixture, app) = crate::compaction::tests::app_for_estimate();
        let mut config = app.config.clone();
        config.workspace = dir.path().canonicalize().unwrap();
        config.sandbox.allow_process = true;
        config.sandbox.allow_shell = true;
        config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
        let result = execute(
            &config,
            "test",
            1,
            &request(
                dir.path(),
                "printf 'a b'; printf err >&2; pwd > marker; exit 7",
            ),
            || false,
        )
        .await
        .unwrap();
        assert_eq!(
            result,
            serde_json::json!({"stdout":"a b", "stderr":"err", "code":7, "killed":false})
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("marker"))
                .unwrap()
                .trim(),
            config.workspace.to_str().unwrap()
        );
        let mut slow = request(dir.path(), "printf partial; sleep 30");
        slow.timeout_ms = Some(50);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            execute(&config, "test", 1, &slow, || false),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["stdout"], "partial");
        assert_eq!(result["killed"], true);
    }
}
