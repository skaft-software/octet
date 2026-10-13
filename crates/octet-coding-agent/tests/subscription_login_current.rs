#![allow(missing_docs)]

//! End-to-end coverage for the shared subscription-login framework, driven
//! through the real `octet` binary against an isolated `HOME`.
//!
//! Every assertion here is about what a *user* can observe: which credentials
//! exist afterwards, which files are touched, and what is printed. The wire
//! behaviour of each grant is covered by the unit tests beside the flows.

use std::path::{Path, PathBuf};
use std::time::Duration;

use octet_sdk::provider::{builtin_provider_definitions, ProviderAccess};

/// A stand-in for a real token. Nothing here may ever print or persist it in a
/// way a test could mistake for a working credential.
const SENTINEL: &str = "subscription-PRIVATE-SENTINEL";

/// Run the real binary with a fully isolated environment.
///
/// `env_clear` plus an explicit `HOME` is what makes these tests safe to run in
/// parallel with a developer's real `~/.octet`: no provider credential file,
/// cache, or config outside the temporary home can be read or written.
async fn octet(home: &Path, args: &[&str]) -> std::process::Output {
    let tmp = home.join("tmp");
    let cache = home.join("cache");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::create_dir_all(&cache).unwrap();
    tokio::time::timeout(
        Duration::from_secs(20),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_octet"))
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("TMPDIR", &tmp)
            .env("XDG_CACHE_HOME", &cache)
            .current_dir(home)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("the auth command must settle rather than hang")
    .unwrap()
}

fn credentials(home: &Path) -> PathBuf {
    home.join(".octet/credentials")
}

/// Every provider `--login`/`--logout` accepts, with its canonical selector.
fn advertised_logins() -> Vec<&'static str> {
    octet_sdk::supported_subscription_providers()
}

#[test]
fn every_advertised_login_maps_to_a_public_subscription_definition() {
    let advertised = advertised_logins();
    for selector in &advertised {
        assert!(!selector.is_empty());
    }
    // The list is what a user is told in the error message, so it must not
    // silently drift from the declarations that actually implement it.
    let definitions = builtin_provider_definitions();
    for selector in &advertised {
        let declaration = definitions
            .iter()
            .find(|definition| {
                matches!(
                    definition.authentication(),
                    ProviderAccess::Subscription { login } if login == selector
                )
            })
            .unwrap_or_else(|| {
                panic!("advertised login {selector:?} has no subscription declaration")
            });
        assert!(
            declaration.label().len() > 3,
            "{selector} needs a human label"
        );
    }
    // Codex owns its own module, so it is not part of the shared registry.
    assert!(!advertised.contains(&"codex"), "{advertised:?}");
    assert!(advertised.contains(&"grok"), "{advertised:?}");
}

#[tokio::test]
async fn an_unknown_provider_lists_the_supported_ones() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let output = octet(&home, &["--login", "not-a-provider"]).await;
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown provider"), "{stderr}");
    assert!(stderr.contains("codex"), "{stderr}");
    assert!(stderr.contains("copilot"), "{stderr}");
    for selector in advertised_logins() {
        assert!(
            stderr.contains(selector),
            "{selector} missing from: {stderr}"
        );
    }
    // Nothing was created for a provider that does not exist.
    assert!(!credentials(&home).exists(), "{:?}", credentials(&home));
}

#[cfg(unix)]
#[tokio::test]
async fn openrouter_always_prints_recovery_url_without_a_browser_or_pasted_code() {
    for headless in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let empty_path = home.join("no-browser-executables");
        std::fs::create_dir(&empty_path).unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_octet"));
        command.args(["--login", "openrouter"]);
        if headless {
            command.arg("--headless");
        }
        // There is no provider I/O before a pasted code. Empty stdin stops the
        // flow; an empty PATH prevents any real browser/desktop action.
        let output = tokio::time::timeout(
            Duration::from_secs(20),
            command
                .env_clear()
                .env("HOME", &home)
                .env("PATH", &empty_path)
                .current_dir(&home)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Open this URL to authorize:"), "{stdout}");
        assert!(stdout.contains("https://openrouter.ai/auth?"), "{stdout}");
        assert!(stdout.contains("code_challenge_method=S256"), "{stdout}");
        assert!(
            stdout.find("https://openrouter.ai/auth?").unwrap()
                < stdout.find("Paste redirect URL:").unwrap()
        );
        assert!(!home.join(".octet/credentials/openrouter.json").exists());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("no authorization redirect was pasted"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn logout_removes_only_the_selected_credential() {
    for selector in advertised_logins() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let directory = credentials(&home);
        std::fs::create_dir_all(&directory).unwrap();
        let selected = directory.join(format!("{selector}.json"));
        let sibling = directory.join("codex.json");
        let body =
            format!(r#"{{"version":1,"access_token":"{SENTINEL}","expires_at":4102444800}}"#);
        for path in [&selected, &sibling] {
            octet_agent::secure_fs::write_private_atomic(path, body.as_bytes(), 16384).unwrap();
        }
        let before = std::fs::read(&sibling).unwrap();

        let output = octet(&home, &["--logout", selector]).await;
        assert!(
            output.status.success(),
            "{}: {}",
            selector,
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(stdout.contains("Signed out of"), "{selector}: {stdout}");
        assert!(!stdout.contains(SENTINEL), "{selector}: {stdout}");
        // Signing out of one provider must never touch another's credential.
        assert!(!selected.exists(), "{selector} was not removed");
        assert_eq!(std::fs::read(&sibling).unwrap(), before);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn logout_is_idempotent_and_never_needs_the_network() {
    for selector in advertised_logins() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        for _ in 0..2 {
            let output = octet(&home, &["--logout", selector]).await;
            assert!(
                output.status.success(),
                "{selector}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
