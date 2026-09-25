//! Serve process startup and shutdown.
//!
//! This module owns the process-level lock, loopback launch, browser hand-off,
//! signal-driven graceful shutdown, and launch-option normalization. Session
//! admission and domain services remain below the host/supervisor boundary.

use super::*;

#[cfg(test)]
mod tests;

const MAX_STARTUP_SESSION_NAME_CHARS: usize = 120;

pub(super) fn normalize_startup_session_name(
    name: Option<String>,
) -> anyhow::Result<Option<String>> {
    let Some(name) = name else {
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > MAX_STARTUP_SESSION_NAME_CHARS || name.chars().any(char::is_control) {
        anyhow::bail!(
            "session name must be at most {MAX_STARTUP_SESSION_NAME_CHARS} characters and contain no control characters"
        );
    }
    Ok(Some(name.to_owned()))
}

async fn wait_for_serve_shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            interrupted = tokio::signal::ctrl_c() => interrupted,
            _ = sigterm.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Starts the graphical Serve host and applies `session_name` to the first
/// provisional session created by the root-client bootstrap.
///
/// Keeping this option at the startup boundary means the name is persisted by
/// the host before the supervisor returns its bootstrap; reconnects and later
/// catalog reads therefore use the normal session metadata path.
pub async fn run_with_session_name(
    config: Config,
    port: u16,
    no_open: bool,
    web_root: Option<PathBuf>,
    session_name: Option<String>,
) -> anyhow::Result<()> {
    let _host_lock = ServeHostLock::acquire(&config)?;
    let terminal =
        config
            .sandbox
            .process_execution_allowed()
            .then(|| octet_serve_backend::TerminalConfig {
                cwd: config.workspace.clone(),
                shell: config.sandbox.shell_path.clone(),
            });
    let host = Arc::new(OctetHost::new_with_session_name(config, session_name)?);
    let goal_store_root = host.serve_state_dir.join("goals");
    let supervisor = Arc::new(SessionSupervisor::new(
        Arc::clone(&host),
        SupervisorConfig {
            fresh_session_authority: host.authority_ceiling(),
            ..SupervisorConfig::default()
        },
    ));
    let server = LoopbackServer::start(
        Arc::clone(&supervisor),
        LoopbackConfig {
            port,
            web_root: web_root.clone(),
            terminal,
            goal_store_root,
        },
    )
    .await?;
    let pull_request_refresh = tokio::spawn(run_pull_request_catalog_refresh(host, supervisor));
    let clean_url = server.url();
    if let Some(root) = web_root {
        crate::output::stdout_line(format!("Web app: {}", root.display()));
    } else {
        crate::output::stdout_line("Web app: embedded");
    }
    if no_open {
        // Explicit trusted terminal output: the launch capability is one-use,
        // process-local, and stripped from the browser address bar by an immediate
        // redirect. It is never persisted or included in server errors.
        crate::output::stdout_line(format!("Open octet once: {}", server.launch_url()));
    } else {
        if let Err(error) = open_browser(&server.launch_url()) {
            crate::output::stderr_line(format!(
                "warning: could not open the browser automatically: {error}"
            ));
        }
        crate::output::stdout_line(format!("octet graphical host: {clean_url}"));
    }
    let shutdown_requested = wait_for_serve_shutdown_signal().await;
    pull_request_refresh.abort();
    let _ = pull_request_refresh.await;
    // A signal-registration error must still quiesce owners before releasing
    // the process lock. Preserve the signal error after cleanup has finished.
    let shutdown_result = server.shutdown().await;
    shutdown_requested?;
    shutdown_result?;
    Ok(())
}

/// One graphical host per octet session root.
///
/// This intentionally does not claim that legacy TUI processes participate in
/// the same host lock. Individual session opens still surface the underlying
/// octet session lock/concurrent-modification failure rather than crossing into
/// `octet-agent` core to change legacy ownership semantics.
struct ServeHostLock {
    _file: std::fs::File,
}

impl ServeHostLock {
    fn acquire(config: &Config) -> anyhow::Result<Self> {
        Self::acquire_at(&config.session_dir)
    }

    fn acquire_at(session_dir: &Path) -> anyhow::Result<Self> {
        use fs2::FileExt as _;

        let state_dir = secure_serve_state_dir(session_dir)?;
        let path = state_dir.join("host.lock");
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        if !file.metadata()?.is_file() {
            anyhow::bail!("octet serve host lock must be a regular file");
        }
        file.try_lock_exclusive().map_err(|error| {
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::PermissionDenied
            ) {
                anyhow::anyhow!("another octet serve process already owns this session catalog")
            } else {
                anyhow::Error::from(error)
            }
        })?;
        Ok(Self { _file: file })
    }
}

fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}
