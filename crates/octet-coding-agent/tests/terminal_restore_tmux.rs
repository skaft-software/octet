//! Terminal restoration under a real terminal multiplexer.
//!
//! The kernel clears `PENDIN` when anything checks a cooked tty for readable
//! input. A raw PTY harness exits before the runtime's own tty watcher turns
//! again; tmux, like an interactive terminal, does not. Restoration must run
//! after the runtime's I/O driver has stopped, including after extension cleanup.
//! Requires `tmux`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const DEADLINE: Duration = Duration::from_secs(30);
const EXITS: usize = 3;

struct Tmux {
    socket: String,
}

impl Tmux {
    fn run(&self, args: &[&str]) -> Output {
        Command::new("tmux")
            .args(["-L", &self.socket, "-f", "/dev/null"])
            .args(args)
            .output()
            .expect("run tmux")
    }

    fn screen(&self, session: &str) -> String {
        String::from_utf8_lossy(&self.run(&["capture-pane", "-p", "-t", session]).stdout)
            .into_owned()
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

fn write_private(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("fixture permissions");
}

fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .count()
}

fn install_extension(run: &Path) -> String {
    let extensions = run.join("extensions");
    let extension = extensions.join("restore-probe");
    std::fs::create_dir_all(&extension).expect("extension directory");
    write_private(
        &extension.join("extension.toml"),
        r#"name = "restore-probe"
version = "0.1.0"
api_version = "0.1"
[entrypoint]
command = "probe.sh"
"#,
    );
    let script = extension.join("probe.sh");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
while IFS= read -r request; do
  id=$(printf '%s\n' "$request" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$request" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{"api_version":"0.1","tools":[],"commands":[]}}}}\n' "$id"
      echo started >> '{run}/started'
      ;;
    *'"method":"shutdown"'*)
      echo graceful >> '{run}/shutdown'
      printf '{{"jsonrpc":"2.0","id":%s,"result":{{}}}}\n' "$id"
      exit 0
      ;;
  esac
done
"#,
            run = run.display(),
        ),
    )
    .expect("extension script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .expect("extension script permissions");
    format!(
        "--extension-dir '{}' --enable-extension restore-probe --trust-extension restore-probe",
        extensions.display()
    )
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool, tmux: &Tmux, session: &str) {
    let started = Instant::now();
    while !ready() {
        assert!(
            started.elapsed() < DEADLINE,
            "timed out waiting for {what}; screen:\n{}",
            tmux.screen(session)
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn ctrl_d_under_tmux_keeps_the_starting_pendin_flag() {
    if Command::new("tmux").arg("-V").output().is_err() {
        eprintln!("tmux is not installed; terminal_restore_tmux did not run");
        return;
    }
    let directory = tempfile::tempdir().expect("tempdir");
    let root = directory.path();
    let home = root.join("home");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(home.join(".octet/credentials")).expect("credential directory");
    std::fs::create_dir_all(&workspace).expect("workspace");
    write_private(
        &home.join(".octet/credentials/custom.json"),
        r#"{"base_url":"http://127.0.0.1:9/v1/","api_key":"","api_name":"probe","headers":[],"models":[{"api_name": "probe", "context_window": 32768}],"auto_discover":false}"#,
    );
    let tmux = Tmux {
        socket: format!("octet-restore-{}", std::process::id()),
    };
    for exit in 0..EXITS {
        let run = root.join(format!("run-{exit}"));
        std::fs::create_dir_all(&run).expect("run directory");
        let extension_args = if exit > 0 {
            install_extension(&run)
        } else {
            String::new()
        };
        let script = run.join("launch.sh");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 stty pendin\n\
                 stty -a | tail -n +2 > '{run}/before'\n\
                 cd '{workspace}'\n\
                 env -i HOME='{home}' PATH=/usr/bin:/bin TERM=xterm-256color OCTET_THEME=auto OCTET_STARTUP_TRACE=1 \
                 '{octet}' --offline --no-context-files --model custom/probe {extension_args} 2>'{run}/stderr'\n\
                 echo $? > '{run}/status'\n\
                 stty -a | tail -n +2 > '{run}/after'\n\
                 touch '{run}/done'\n\
                 sleep 60\n",
                run = run.display(),
                workspace = workspace.display(),
                home = home.display(),
                octet = env!("CARGO_BIN_EXE_octet"),
            ),
        )
        .expect("launch script");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("launch script permissions");
        let session = format!("exit-{exit}");
        let started = tmux.run(&[
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "100",
            "-y",
            "30",
            script.to_str().expect("utf-8 script path"),
        ]);
        assert!(started.status.success(), "tmux new-session: {started:?}");
        wait_for(
            "the first frame",
            || tmux.screen(&session).contains("Ctrl+D to exit"),
            &tmux,
            &session,
        );
        if exit > 0 {
            wait_for(
                "extension startup to settle",
                || {
                    line_count(&run.join("started")) == 1
                        && std::fs::read_to_string(run.join("stderr"))
                            .unwrap_or_default()
                            .contains("extensions.settled")
                },
                &tmux,
                &session,
            );
        }
        if exit == 2 {
            assert!(tmux
                .run(&["send-keys", "-t", &session, "/reload", "Enter"])
                .status
                .success());
            wait_for(
                "extension reload",
                || line_count(&run.join("started")) == 2,
                &tmux,
                &session,
            );
            assert!(tmux
                .run(&["resize-window", "-t", &session, "-x", "80", "-y", "24"])
                .status
                .success());
        }
        assert!(tmux
            .run(&["send-keys", "-t", &session, "C-d"])
            .status
            .success());
        wait_for("exit", || run.join("done").exists(), &tmux, &session);
        let _ = tmux.run(&["kill-session", "-t", &session]);
        let status = std::fs::read_to_string(run.join("status")).expect("exit status");
        assert_eq!(status.trim(), "0", "octet exit status");
        if exit > 0 {
            assert_eq!(
                line_count(&run.join("shutdown")),
                exit,
                "graceful shutdowns"
            );
        }
        let before = std::fs::read_to_string(run.join("before")).expect("starting attributes");
        let after = std::fs::read_to_string(run.join("after")).expect("final attributes");
        assert!(
            before.contains(" pendin "),
            "starting PENDIN was not applied"
        );
        assert_eq!(after, before, "exit {exit} changed the terminal attributes");
    }
}
