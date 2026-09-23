//! Production startup with synthetic private keys and an isolated HOME. Every
//! child uses --offline: no discovery, inference, or real credentials are used.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const KEY: &str = "synthetic-startup-key";

fn credential(home: &Path, provider: &str, bytes: &[u8]) {
    octet_agent::secure_fs::write_private_atomic(
        &home.join(format!(".octet/credentials/api-keys/{provider}.json")),
        bytes,
        8192,
    )
    .unwrap();
}

fn list_models(home: &Path, workspace: &Path, environment_key: Option<&str>) -> (String, String) {
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_octet"));
    command
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env(
            "PATH",
            if cfg!(windows) {
                r"C:\Windows\System32"
            } else {
                "/usr/bin:/bin"
            },
        )
        .env("TERM", "dumb")
        .env("LANG", "C.UTF-8")
        .current_dir(workspace)
        .args(["--offline", "--safe-mode", "--list-models"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().unwrap()))
        .stderr(Stdio::from(stderr.try_clone().unwrap()));
    if let Some(key) = environment_key {
        command.env("OPENAI_API_KEY", key);
    }
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("offline model enumeration did not finish within 15 seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut output = Vec::new();
    for file in [&mut stdout, &mut stderr] {
        assert!(file.metadata().unwrap().len() <= 128 * 1024);
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        assert!(!text.contains(KEY), "synthetic key must not be rendered");
        output.push(text);
    }
    assert!(status.success(), "offline startup failed: {}", output[1]);
    (output.remove(0), output.remove(0))
}

#[test]
fn stored_openai_key_matches_environment_offline_inventory_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(
        &home,
        "openai",
        br#"{"version":1,"api_key":"synthetic-startup-key"}"#,
    );

    let (stored, _) = list_models(&home, &workspace, None);
    assert!(stored.lines().any(|line| line.starts_with("openai\t")));
    assert_eq!(stored, list_models(&home, &workspace, None).0);
    fs::remove_file(home.join(".octet/credentials/api-keys/openai.json")).unwrap();
    let (environment, _) = list_models(&home, &workspace, Some(KEY));
    assert_eq!(
        stored, environment,
        "credential source must not change offline inventory"
    );
}

#[test]
fn corrupt_openai_store_does_not_block_valid_anthropic_offline_inventory() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let workspace = directory.path().join("workspace");
    fs::create_dir_all(&workspace).unwrap();
    credential(&home, "openai", b"synthetic-startup-key invalid JSON");
    credential(
        &home,
        "anthropic",
        br#"{"version":1,"api_key":"synthetic-startup-key"}"#,
    );

    let (models, diagnostic) = list_models(&home, &workspace, None);
    assert!(models.lines().any(|line| line.starts_with("anthropic\t")));
    assert!(!models.lines().any(|line| line.starts_with("openai\t")));
    assert!(diagnostic.contains("OpenAI unavailable: could not resolve provider credentials"));
}
