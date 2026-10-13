//! Probes for the native clipboard helper ladder: the declared helper order on
//! every platform, the deadline and byte cap that bound a blocked helper, and the
//! test-only text and helper overrides. Separate from the read implementation
//! because the ladder is a fail-closed contract with its own ordering rules and
//! its own timeout budget.

use super::*;

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn linux_helper_order_follows_the_declared_environment_gates() {
    let all = helpers(
        Platform::Linux,
        env(&[
            ("TERMUX_VERSION", "1"),
            ("WAYLAND_DISPLAY", "wayland-0"),
            ("DISPLAY", ":0"),
        ]),
    );
    let programs: Vec<&str> = all.iter().map(|helper| helper.program.as_str()).collect();
    assert_eq!(
        programs,
        ["termux-clipboard-get", "wl-paste", "xclip", "xsel"]
    );
    assert_eq!(
        all[1].args,
        [
            "--no-newline".to_owned(),
            "--type".to_owned(),
            "text".to_owned()
        ]
    );
    assert_eq!(
        all[2].args,
        ["-selection", "clipboard", "-out"].map(str::to_owned)
    );
}

#[test]
fn a_session_with_no_declared_display_yields_no_helper() {
    assert!(helpers(Platform::Linux, env(&[])).is_empty());
    assert_eq!(helpers(Platform::Linux, env(&[("DISPLAY", ":0")])).len(), 2);
    assert_eq!(
        helpers(Platform::Linux, env(&[("WAYLAND_DISPLAY", "wayland-1")])).len(),
        1
    );
}

#[test]
fn macos_and_windows_read_through_one_declared_helper() {
    assert_eq!(helpers(Platform::MacOs, env(&[]))[0].program, "pbpaste");
    let windows = helpers(Platform::Windows, env(&[]));
    assert_eq!(windows[0].program, "powershell");
    assert!(windows[0]
        .args
        .iter()
        .any(|arg| arg.contains("Get-Clipboard")));
    assert!(!windows[0].args.iter().any(|arg| arg.contains("clip\"")));
}

#[test]
fn helper_failure_tries_the_next_transport_and_empty_success_settles() {
    assert_eq!(settle(Outcome::Failed), Err(()));
    assert_eq!(settle(Outcome::Empty), Ok(None));
    assert_eq!(
        settle(Outcome::Text(b"from clipboard".to_vec())),
        Ok(Some("from clipboard".to_owned()))
    );
}

#[test]
fn oversized_payloads_fail_closed_instead_of_pasting_a_prefix() {
    assert_eq!(classify(vec![b'x'; MAX_TEXT_BYTES + 1]), Outcome::Failed);
    assert_eq!(classify(Vec::new()), Outcome::Empty);
    assert_eq!(decode(&[]), None);
    assert_eq!(decode(&vec![b'x'; MAX_TEXT_BYTES + 1]), None);
    // The reference reader replaces invalid UTF-8 instead of dropping
    // the whole clipboard.
    assert_eq!(decode(&[0xff, 0xfe]), Some("\u{fffd}\u{fffd}".to_owned()));
}

#[tokio::test]
async fn a_missing_helper_fails_closed_without_panicking() {
    let missing = Helper {
        program: "octet-no-such-clipboard-helper".to_owned(),
        args: Vec::new(),
    };
    assert_eq!(run(&missing).await, Outcome::Failed);
}

/// Exercise the real spawn/decode/reap path against throwaway helper
/// programs. The developer's own clipboard is never read or written.
#[cfg(unix)]
#[tokio::test]
async fn a_real_helper_is_read_bounded_and_its_exit_status_is_honoured() {
    let text = Helper {
        program: "/bin/echo".to_owned(),
        args: vec!["clipboard text".to_owned()],
    };
    assert_eq!(
        run(&text).await,
        Outcome::Text(b"clipboard text\n".to_vec())
    );
    assert_eq!(
        settle(Outcome::Text(b"clipboard text\n".to_vec())),
        Ok(Some("clipboard text\n".to_owned()))
    );

    let empty = Helper {
        // /bin/true is absent on macOS; use the portable shell builtin.
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 0".to_owned()],
    };
    assert_eq!(run(&empty).await, Outcome::Empty);

    let failing = Helper {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exit 3".to_owned()],
    };
    assert_eq!(run(&failing).await, Outcome::Failed);

    // A helper that never returns is killed at the deadline instead of
    // holding the interactive loop.
    let wedged = Helper {
        program: "/bin/sh".to_owned(),
        args: vec!["-c".to_owned(), "exec sleep 30".to_owned()],
    };
    let started = std::time::Instant::now();
    assert_eq!(run(&wedged).await, Outcome::Failed);
    assert!(
        started.elapsed() < READ_TIMEOUT * 6,
        "bounded helper read took {:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn the_test_override_replaces_the_platform_read() {
    set_test_text(Some("overridden".to_owned()));
    assert_eq!(read_text().await, Some("overridden".to_owned()));
    set_test_text(None);
    assert_eq!(read_text().await, None);
    clear_test_text();
}
