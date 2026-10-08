//! Notification layout at the real shell and redirected native-host boundaries.
use super::*;
use octet_agent::extension_process::{ExtensionNotification, ExtensionNotificationLevel};

fn issue(message: &str) -> ExtensionNotification {
    ExtensionNotification {
        level: ExtensionNotificationLevel::Warning,
        title: Some("[Extension issues]".into()),
        message: message.into(),
        source: None,
    }
}

#[test]
fn notifications_keep_one_header_and_indented_literal_lines() {
    let formatted = format_notification(
        "octet-pi-compat",
        &issue("  /reviewed/first.ts\n    Callback failed.\n    Next: Update the extension.\n  /reviewed/second.ts\n    File missing.\n    Next: Restore the file."),
    );
    assert_eq!(
        formatted,
        "[octet-pi-compat Warning] [Extension issues]:\n    /reviewed/first.ts\n      Callback failed.\n      Next: Update the extension.\n    /reviewed/second.ts\n      File missing.\n      Next: Restore the file."
    );
    assert_eq!(formatted.matches("[Extension issues]").count(), 1);
    assert_eq!(
        format_notification("ordinary", &issue("one line")),
        "[ordinary Warning] [Extension issues]: one line"
    );
}

#[test]
fn pi_notification_sources_replace_bridge_name_and_only_show_meaningful_severity() {
    let mut notification = issue("Inserted drawing into editor.");
    notification.source = Some("Termdraw".into());
    notification.level = ExtensionNotificationLevel::Info;
    assert_eq!(
        format_notification("octet-pi-compat", &notification),
        "[Termdraw] Inserted drawing into editor."
    );
    notification.level = ExtensionNotificationLevel::Error;
    assert_eq!(
        format_notification("octet-pi-compat", &notification),
        "[Termdraw Error] Inserted drawing into editor."
    );
    notification.source = Some("Termdraw\nforged".into());
    assert!(format_notification("octet-pi-compat", &notification).contains("<U+000A>"));
    assert!(format_notification("another-extension", &notification)
        .starts_with("[another-extension Error]"));
}

#[test]
fn notification_headers_and_each_body_line_are_control_safe_and_bounded() {
    let mut notification = issue("  /reviewed/a\rforged\n    **literal**\t\x1b[31mred\x07\u{009b}2J\u{202e}reverse\n    Next: Update.");
    notification.title = Some("[Extension issues]\nforged\x1b]0;title\x07".into());
    let formatted = format_notification("extension\nforged", &notification);
    assert_eq!(formatted.lines().count(), 4);
    assert!(formatted.contains("**literal**"));
    assert!(formatted.contains("<U+000A>"));
    assert!(formatted.contains("<U+000D>"));
    assert!(formatted.contains("<U+0009>"));
    assert!(formatted.contains("<U+202E>"));
    assert!(formatted.chars().all(|ch| ch == '\n' || !ch.is_control()));
    assert!(formatted.lines().skip(1).all(|line| line.starts_with("  ")));

    for body in ["界\x1b".repeat(20_000), "\n".repeat(20_000)] {
        let formatted = format_notification("bounded", &issue(&body));
        assert!(formatted.len() <= MAX_DIAGNOSTIC_ENTRY_BYTES);
        assert!(formatted.lines().count() <= 258);
        assert!(formatted.contains("truncated") || formatted.contains("omitted"));
        assert!(formatted.chars().all(|ch| ch == '\n' || !ch.is_control()));
    }
}

#[tokio::test]
async fn shell_renders_grouped_notifications_as_literal_rows_not_markdown_or_controls() {
    let mut shell = InteractiveShell::test_shell();
    shell.notice(format_notification(
        "octet-pi-compat",
        &issue("  /reviewed/first.ts\n    **literal reason**\x1b[31m\n    Next: `update` the extension."),
    ));
    shell.render();
    let frame = shell.dump_rendered_frame().await.unwrap();
    let rows = frame
        .iter()
        .map(|row| sexy_tui_rs::strip_terminal_sequences(row))
        .collect::<Vec<_>>();
    let header = rows
        .iter()
        .position(|row| row.contains("[Extension issues]"))
        .unwrap();
    let path = rows
        .iter()
        .position(|row| row.contains("/reviewed/first.ts"))
        .unwrap();
    let reason = rows
        .iter()
        .position(|row| row.contains("**literal reason**"))
        .unwrap();
    let next = rows
        .iter()
        .position(|row| row.contains("Next: `update`"))
        .unwrap();
    assert!(header < path && path < reason && reason < next, "{rows:#?}");
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("[Extension issues]"))
            .count(),
        1
    );
    assert!(rows[reason].contains("^[[31m"), "{rows:#?}");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn configured_pi_startup_issues_reach_real_headless_stderr_as_one_group() {
    use std::process::Command;
    const CHILD: &str = "OCTET_TEST_NATIVE_NOTIFICATION_OUTPUT";
    const PRIVATE: &str = "private-native-notification-error-must-not-leak";
    if std::env::var_os(CHILD).is_none() {
        // Capture the actual App's stderr, not a hand-constructed JSON frame or
        // a duplicate of its output formatter. Isolate global terminal routing.
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extensions::tests::notification_output::configured_pi_startup_issues_reach_real_headless_stderr_as_one_group",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{stderr}\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert_eq!(stderr.matches("[Extension issues]").count(), 1, "{stderr}");
        let rows = stderr.lines().collect::<Vec<_>>();
        let header = rows
            .iter()
            .position(|line| line.contains("[Extension issues]"))
            .unwrap();
        assert!(rows[header].ends_with("[Extension issues]:"), "{stderr}");
        for (offset, name) in [(1, "first.mjs"), (4, "second.mjs")] {
            assert!(rows[header + offset].starts_with("    "), "{stderr}");
            assert!(rows[header + offset].ends_with(name), "{stderr}");
            assert!(
                rows[header + offset + 1].contains("failed while loading"),
                "{stderr}"
            );
            assert!(
                rows[header + offset + 2].starts_with("      Next:"),
                "{stderr}"
            );
        }
        assert!(!stderr.contains("<U+000A>"), "{stderr}");
        assert!(!stderr.contains(PRIVATE), "{stderr}");
        assert!(!String::from_utf8_lossy(&output.stdout).contains(PRIVATE));
        return;
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    let adapter = std::env::var_os("OCTET_PI_COMPAT_TEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/octet-pi-compat")
        })
        .canonicalize()
        .expect("existing local Pi adapter and dependencies required; never install");
    let first = temp.path().join("first.mjs");
    let second = temp.path().join("second.mjs");
    for path in [&first, &second] {
        std::fs::write(path, format!(
            "export default () => {{ if (!process.argv.includes('--inspect')) throw new Error({PRIVATE:?}); }};"
        )).unwrap();
    }
    let live = temp.path().join("live.mjs");
    std::fs::write(&live, "export default pi => { pi.on('resources_discover', async () => { await new Promise(resolve => setTimeout(resolve, 150)); return {}; }); };").unwrap();
    let extensions = temp.path().join("extensions");
    let configured = Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(extensions.join("octet-pi-compat"))
        .args([&first, &second, &live])
        .current_dir(&workspace)
        .env("PI_OFFLINE", "1")
        .output()
        .expect("existing Node 22.19+ and local adapter dependencies required");
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    let config = executable_extension_config(&workspace, &extensions, "octet-pi-compat");
    let boot = crate::app::bootstrap::bootstrap(config).unwrap();
    let launch = crate::app::bootstrap::resolve_launch_print(&boot, "notification-output").unwrap();
    let mut app =
        crate::app::bootstrap::build_app_with_resource_consumer(boot, launch, "BASE".into())
            .unwrap();
    tokio::time::timeout(
        Duration::from_secs(10),
        app.refresh_resource_paths_headless(),
    )
    .await
    .unwrap()
    .unwrap();
    app.executable_extensions.shutdown().await;
}
