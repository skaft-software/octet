//! The host owns the raw terminal and the authoritative draft; a Pi adapter
//! cannot perform this native terminal handoff.
use std::io::Write as _;
use std::process::Stdio;

use crate::config::Config;
use crate::tui::view::InteractiveShell;

const MAX_DRAFT_BYTES: usize = 8 * 1024 * 1024;

pub(super) async fn edit(shell: &mut InteractiveShell, config: &Config) -> anyhow::Result<()> {
    if !config.sandbox.process_execution_allowed() {
        shell.error("External editor unavailable: child processes are disabled.".into());
        shell.render();
        return Ok(());
    }
    let draft = shell.extension_editor_snapshot();
    if !draft.focused || shell.extension_editor_owns_composer() {
        return Ok(());
    }
    let (file, mut editor) = match prepare(&draft.text) {
        Ok(prepared) => prepared,
        Err(error) => {
            shell.error(format!(
                "Could not prepare external editor draft; draft unchanged: {error}"
            ));
            shell.render();
            return Ok(());
        }
    };

    // Pi 1.5.2 dist/modes/interactive/interactive-mode.js:3901-3921
    // stops its UI for the foreground editor and applies only a successful edit.
    // The input source is parked as well as the renderer while the child owns it.
    shell.cede_terminal_input();
    shell.suspend();
    let result = tokio::select! {
        biased;
        _ = crate::tui::terminal::wait_for_shutdown_signal() => None,
        result = editor.current_dir(&config.invocation_cwd).status() => Some(result),
    };
    shell.release_terminal_input();
    shell.resume()?;

    match result {
        None => shell.request_close(),
        Some(Ok(status)) if status.success() => match load(&file.path().join("draft.md")) {
            Ok(text) => shell.apply_external_editor_draft(text),
            Err(error) => shell.error(format!(
                "Could not read external editor draft; draft unchanged: {error}"
            )),
        },
        Some(Ok(status)) => shell.error(format!(
            "External editor exited with {status}; draft unchanged."
        )),
        Some(Err(error)) => shell.error(format!(
            "Could not launch external editor; draft unchanged: {error}"
        )),
    }
    shell.render();
    Ok(())
}

fn prepare(draft: &str) -> anyhow::Result<(tempfile::TempDir, tokio::process::Command)> {
    anyhow::ensure!(draft.len() <= MAX_DRAFT_BYTES, "draft is larger than 8 MiB");
    // Store the text in a private directory: common editors save by renaming a
    // replacement file, and its inherited mode need not be private.
    let dir = tempfile::Builder::new().prefix("octet-draft-").tempdir()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700))?;
    }
    let path = dir.path().join("draft.md");
    std::fs::File::create_new(&path)?.write_all(draft.as_bytes())?;
    let command = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.trim().is_empty())
        .unwrap_or_else(|| if cfg!(windows) { "notepad" } else { "nano" }.into());
    // The exact existing path wins, including spaces; otherwise interpret
    // whitespace-separated arguments as literal argv, not shell expressions.
    let mut editor = if std::path::Path::new(&command).is_absolute()
        && std::path::Path::new(&command).is_file()
    {
        tokio::process::Command::new(command)
    } else {
        let mut words = command.split_whitespace();
        let mut editor = tokio::process::Command::new(words.next().expect("nonempty editor"));
        editor.args(words);
        editor
    };
    editor
        .arg(path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    Ok((dir, editor))
}

fn load(path: &std::path::Path) -> anyhow::Result<String> {
    let bytes = octet_agent::secure_fs::read_regular_file_bounded(path, MAX_DRAFT_BYTES)?;
    let text = String::from_utf8(bytes)?;
    let text = text
        .strip_prefix('\u{feff}')
        .unwrap_or(&text)
        .replace("\r\n", "\n");
    Ok(text.strip_suffix('\n').unwrap_or(&text).to_owned())
}
