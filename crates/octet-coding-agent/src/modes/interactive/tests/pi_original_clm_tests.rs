//! Opt-in pinned original acceptance through the real App, fleet and command pump.
//! No altered factory, handwritten protocol peer, or provider acceptance claim.
#![cfg(unix)]
use super::*;
use sha2::{Digest, Sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires reviewed pinned PI_CLM_PATH and existing adapter dependencies"]
async fn native_original_clm_restores_branch_settings_before_first_command() {
    let entry = std::path::PathBuf::from(
        std::env::var_os("PI_CLM_PATH")
            .expect("explicit reviewed PI_CLM_PATH is required; never install originals"),
    );
    let originals = [
        (
            entry.clone(),
            "d5e9d73034eb87e28dc9909a2d8ac28b85a1969fd234c7a8f3eddecb71e3fde6",
        ),
        (
            entry.parent().unwrap().join("src/index.ts"),
            "a6d1c464bebdcc927a0eb6d0bb1ff319c3e5cdf5bf844e03b599ec91169f7170",
        ),
    ];
    let verify_originals = || {
        for (path, expected) in &originals {
            assert_eq!(
                format!("{:x}", Sha256::digest(std::fs::read(path).unwrap())),
                *expected,
                "unchanged pinned CLM source {}",
                path.display()
            );
        }
    };
    verify_originals();
    let (directory, mut app) = crate::compaction::tests::app_for_estimate();
    let extension_root = directory.path().join("extensions");
    let adapter = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../extensions/octet-pi-compat")
        .canonicalize()
        .unwrap();
    let configured = std::process::Command::new("node")
        .arg(adapter.join("configure.mjs"))
        .arg("--reviewed")
        .arg("--output")
        .arg(extension_root.join("octet-pi-compat"))
        .arg(&entry)
        .current_dir(&app.config.workspace)
        .env("PI_OFFLINE", "1")
        .output()
        .expect("existing Node and local adapter dependencies are required");
    assert!(
        configured.status.success(),
        "{}",
        String::from_utf8_lossy(&configured.stderr)
    );
    app.config.extension_paths = vec![extension_root];
    app.config.enabled_extensions = vec!["octet-pi-compat".into()];
    app.config.invocation_trusted_extensions = vec!["octet-pi-compat".into()];
    app.config.workspace_trusted = true;
    app.config.mode = crate::config::Mode::Interactive;
    app.config.effect_policy = octet_agent::EffectPolicy::UnsafeHost;
    app.config.sandbox.allow_process = true;
    app.config.sandbox.allow_shell = true;
    let session = app.agent.session_mut();
    let root = session
        .append(EntryValue::Config {
            model: None,
            reasoning: None,
            reasoning_mode: None,
        })
        .unwrap();
    let selected = session
        .append_extension_entry(
            "octet-pi-compat",
            Some(1),
            "pi-clm-settings",
            serde_json::json!({"version":1,"overrides":{"budget":12000}}),
        )
        .unwrap();
    session.checkout(root).unwrap();
    session
        .append_extension_entry(
            "octet-pi-compat",
            Some(1),
            "pi-clm-settings",
            serde_json::json!({"version":1,"overrides":{"budget":24000}}),
        )
        .unwrap();
    session.checkout(selected).unwrap();
    session
        .append_extension_entry(
            "foreign-extension",
            Some(1),
            "pi-clm-settings",
            serde_json::json!({"version":1,"overrides":{"budget":36000}}),
        )
        .unwrap();
    let before = session.entries().len();
    let capability = crate::app::resource_paths::ResourceConsumerCapability::AppFrontend;
    let mut host = octet_agent::ExtensionHost::new();
    let mut extensions =
        crate::extensions::ExecutableExtensions::discover_and_start_with_provider_runtime(
            &app.config,
            app.agent.session(),
            &app.model,
            &app.reasoning,
            &app.sessions,
            &mut host,
            None,
            crate::extensions::ExtensionProviderRuntime::default(),
            capability,
            None,
        );
    assert!(
        extensions
            .summaries()
            .iter()
            .any(|s| s.name == "octet-pi-compat" && s.running),
        "{}",
        extensions.inspect_text()
    );
    app.resource_paths = crate::app::resource_paths::ResourcePathConsumer::new(
        &app.config,
        &app.skills,
        &app.prompts,
        &extensions,
        &mut host,
        capability,
    );
    app.executable_extensions = extensions;
    app.executable_extensions
        .activate_session_lifecycle_driver();
    let mut shell = InteractiveShell::test_shell();
    let mut input = futures_util::stream::pending::<std::io::Result<Event>>();
    resource_paths::refresh_resource_paths(&mut app, &mut shell, &mut input)
        .await
        .unwrap();
    let dialogs = app.executable_extensions.lifecycle_snapshot();
    let output = {
        let mut frontend = InteractiveExtensionConfirmations {
            shell: &mut shell,
            input: &mut input,
            dialogs: &dialogs,
        };
        tokio::time::timeout(
            Duration::from_secs(10),
            run_interactive_extension_command(
                &mut app,
                &mut frontend,
                Some("octet-pi-compat"),
                "clm",
                vec!["config".into(), "budget".into()],
                false,
                0,
            ),
        )
        .await
    };
    // Cleanup even on a command failure; receipt assertions follow real settlement.
    app.executable_extensions.shutdown().await;
    let output = output
        .expect("native CLM command/idle barrier timed out")
        .unwrap()
        .unwrap();
    let observed = format!("{}\n{}", output, shell.debug_snapshot());
    assert!(observed.contains("Budget: 12k —"), "{observed}");
    assert!(!observed.contains("Pi compatibility error"), "{observed}");
    assert!(
        !observed.contains("Budget: 24k") && !observed.contains("Budget: 36k"),
        "{observed}"
    );
    assert_eq!(
        app.agent.session().entries().len(),
        before,
        "read-only settings query mutated history"
    );
    let reopened = Session::open_read_only(app.agent.session().path()).unwrap();
    assert_eq!(reopened.entries().len(), before);
    verify_originals();
}
