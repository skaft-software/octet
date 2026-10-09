//! Sharing has a prepare/review/confirm/send boundary. Never probes gh auth.
use sha2::{Digest as _, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWriteExt as _};

use super::{SessionStore, MAX_SESSION_FILE_BYTES};

const WARNING: &str = "Publish this exact redacted snapshot as an UNLISTED GitHub gist? Anyone with the link can read, copy or redistribute it. It includes ALL durable branches, tool outputs, summaries and accounting, not just the visible branch. Automatic redaction cannot guarantee removal of private information; review the snapshot first. Radius organization sharing is not supported. Cancellation cannot undo an upload already accepted by GitHub.";

pub(crate) struct PreparedShare {
    _directory: tempfile::TempDir,
    path: PathBuf,
    hash: String,
    redactions: usize,
}
impl PreparedShare {
    pub(crate) fn package_path(&self) -> &Path {
        &self.path
    }
    pub(crate) fn sha256(&self) -> &str {
        &self.hash
    }
    pub(crate) fn warning(&self) -> &str {
        WARNING
    }
    pub(crate) fn redaction_count(&self) -> usize {
        self.redactions
    }
}

pub(crate) fn prepare_share(store: &SessionStore, id: &str) -> anyhow::Result<PreparedShare> {
    let (package, redactions, ignored_tail) = super::build_export_package(store, id, false)?;
    if ignored_tail {
        anyhow::bail!("repair the interrupted session tail before preparing a share");
    }
    let bytes = serde_json::to_vec_pretty(&package)?;
    if bytes.len() > MAX_SESSION_FILE_BYTES {
        anyhow::bail!("share snapshot exceeds byte limit");
    }
    let directory = tempfile::Builder::new().prefix("octet-share-").tempdir()?;
    // Canonicalize the host's temp-root alias, never the snapshot file.
    let path = directory
        .path()
        .canonicalize()?
        .join("session.octet-session.json");
    octet_agent::secure_fs::write_private_atomic_if_unchanged(
        &path,
        None,
        &bytes,
        MAX_SESSION_FILE_BYTES,
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    }
    Ok(PreparedShare {
        _directory: directory,
        path,
        hash: hash(&bytes),
        redactions,
    })
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(crate) async fn publish_share(
    prepared: PreparedShare,
    confirmed: bool,
    cancelled: &AtomicBool,
) -> anyhow::Result<String> {
    publish_with(prepared, confirmed, cancelled, std::ffi::OsStr::new("gh")).await
}

async fn publish_with(
    prepared: PreparedShare,
    confirmed: bool,
    cancelled: &AtomicBool,
    program: &std::ffi::OsStr,
) -> anyhow::Result<String> {
    if !confirmed {
        anyhow::bail!("share was not explicitly confirmed; nothing was uploaded");
    }
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("share cancelled; nothing was uploaded");
    }
    // The preview is deliberately read-only (0400); the credential-store
    // reader requires writable 0600 files. Keep bounded, component-wise
    // no-follow reads here and verify the reviewed bytes before any send.
    let bytes =
        octet_agent::secure_fs::read_regular_file_bounded(&prepared.path, MAX_SESSION_FILE_BYTES)?;
    if hash(&bytes) != prepared.hash {
        anyhow::bail!("share snapshot changed since review; prepare and confirm again");
    }
    let package = super::strict_json::parse(&bytes)?;
    if package["format"] != "octet-session-export"
        || package["version"] != 1
        || package["redacted"] != true
    {
        anyhow::bail!("share requires a redacted Octet snapshot");
    }
    // Stream the verified bytes, not a pathname that could change after review.
    // Explicit false matches Pi's unlisted fallback; no shell interpolation.
    if cancelled.load(Ordering::Acquire) {
        anyhow::bail!("share cancelled; nothing was uploaded");
    }
    let mut child = tokio::process::Command::new(program)
        .args([
            "gist",
            "create",
            "--public=false",
            "--filename",
            "session.octet-session.json",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| {
            anyhow::anyhow!(
                "cannot start GitHub CLI; install gh and configure it separately before sharing"
            )
        })?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let outcome = {
        let io = async {
            let send = async {
                stdin.write_all(&bytes).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<_, std::io::Error>(())
            };
            let wait = async { child.wait().await };
            tokio::try_join!(send, bounded_output(stdout), bounded_output(stderr), wait)
        };
        tokio::pin!(io);
        tokio::select! {
            result = &mut io => Some(result),
            _ = async { loop { if cancelled.load(Ordering::Acquire) { break; } tokio::time::sleep(Duration::from_millis(30)).await; } } => None,
            _ = tokio::time::sleep(Duration::from_secs(120)) => None,
        }
    };
    let (_, stdout, _stderr, status) = match outcome {
        Some(Ok(result)) => result,
        _ => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            anyhow::bail!("share cancelled, timed out or failed; local snapshot cleaned up (an upload already accepted by GitHub cannot be undone)");
        }
    };
    if !status.success() {
        anyhow::bail!("GitHub CLI gist creation failed; local snapshot cleaned up (publication may have succeeded; no credential-bearing stderr is echoed)");
    }
    let text = std::str::from_utf8(&stdout)
        .map_err(|_| {
            anyhow::anyhow!("GitHub CLI returned invalid output; publication may have succeeded")
        })?
        .trim();
    let url = url::Url::parse(text).map_err(|_| {
        anyhow::anyhow!("GitHub CLI returned an invalid gist URL; publication may have succeeded")
    })?;
    let segments: Vec<_> = url
        .path()
        .strip_prefix('/')
        .unwrap_or("")
        .split('/')
        .collect();
    let valid_path = segments.len() == 2
        && !segments[0].is_empty()
        && segments[0].len() <= 39
        && segments[0]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !segments[1].is_empty()
        && segments[1].len() <= 64
        && segments[1].bytes().all(|b| b.is_ascii_hexdigit());
    if url.scheme() != "https"
        || url.host_str() != Some("gist.github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !valid_path
        || url.as_str() != text
    {
        anyhow::bail!("GitHub CLI returned an unexpected gist URL; publication may have succeeded");
    }
    Ok(url.to_string())
}

async fn bounded_output(reader: impl AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(8193).read_to_end(&mut bytes).await?;
    if bytes.len() > 8192 {
        return Err(std::io::Error::other("GitHub CLI output exceeded limit"));
    }
    Ok(bytes)
}

pub(super) fn share_cli(store: &SessionStore, id: &str, yes: bool) -> anyhow::Result<()> {
    use std::io::{BufRead as _, IsTerminal as _, Read as _};
    let prepared: super::PreparedShare = super::prepare_share(store, id)?;
    crate::output::stdout_line(prepared.warning());
    crate::output::stdout_line(format!(
        "Review: {}\nSHA-256: {}\nRedacted values: {}",
        prepared.package_path().display(),
        prepared.sha256(),
        prepared.redaction_count()
    ));
    let confirmed = if yes {
        true
    } else {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            anyhow::bail!("share requires attended confirmation; review a redacted export, then pass --yes to approve unlisted publication");
        }
        crate::output::stdout_line("Publish this snapshot? [y/N]");
        let mut answer = String::new();
        std::io::stdin().lock().take(32).read_line(&mut answer)?;
        matches!(answer.trim(), "y" | "Y" | "yes" | "YES")
    };
    if !confirmed {
        crate::output::stdout_line("Share cancelled; nothing was uploaded.");
        return Ok(());
    }
    // Session CLI dispatch can run under an existing runtime; isolate blocking
    // dispatch rather than nesting Runtime::block_on inside it.
    let cancelled = std::sync::Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    let signal = signal_hook::flag::register(
        signal_hook::consts::SIGINT,
        std::sync::Arc::clone(&cancelled),
    )?;
    let result = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        runtime.block_on(super::publish_share(prepared, true, &cancelled))
    })
    .join()
    .map_err(|_| anyhow::anyhow!("share worker failed"));
    #[cfg(unix)]
    signal_hook::low_level::unregister(signal);
    let url = result??;
    crate::output::stdout_line(format!("Unlisted gist: {url}"));
    Ok(())
}

#[cfg(test)]
mod tests;
