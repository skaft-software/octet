use super::*;
use octet_agent::{EntryValue, Session};
use octet_ai::{Message, UserMessage, UserPart};

fn fixture(root: &tempfile::TempDir) -> SessionStore {
    let root = root.path().canonicalize().unwrap();
    let store = SessionStore::new(&root.join("sessions"), &root);
    std::fs::create_dir_all(store.dir()).unwrap();
    let mut session = Session::create(store.dir().join("synthetic.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("share sk-syntheticsecret123456".into())],
        })))
        .unwrap();
    drop(session);
    store
}

#[test]
fn snapshot_is_redacted_bounded_private_immutable_and_drop_cleans_it() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    let prepared = prepare_share(&store, "synthetic").unwrap();
    let path = prepared.package_path().to_owned();
    let bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("syntheticsecret"));
    assert!(String::from_utf8_lossy(&bytes).contains("[REDACTED]"));
    assert_eq!(hash(&bytes), prepared.sha256());
    assert!(prepared.warning().contains("UNLISTED"));
    assert!(prepared.warning().contains("Anyone with the link"));
    // The secret appears in both the source title and the conversation.
    assert_eq!(prepared.redaction_count(), 2);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o400
        );
    }
    drop(prepared);
    assert!(!path.exists());
}

#[tokio::test]
async fn no_confirmation_and_pre_cancel_never_start_subprocess_and_clean_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    for (confirmed, cancelled) in [(false, false), (true, true)] {
        let prepared = prepare_share(&store, "synthetic").unwrap();
        let path = prepared.package_path().to_owned();
        let error = publish_with(
            prepared,
            confirmed,
            &AtomicBool::new(cancelled),
            std::ffi::OsStr::new("no-executable-must-be-started"),
        )
        .await
        .unwrap_err();
        assert!(!error.to_string().contains("cannot start"));
        assert!(!path.exists());
    }
}

#[cfg(unix)]
fn mock(root: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.path().canonicalize().unwrap().join(name);
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test]
async fn mocked_gh_success_streams_only_verified_snapshot_and_uses_unlisted_argv() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    let program = mock(&root,"mock-success",concat!(
        "test \"$1\" = gist && test \"$2\" = create && test \"$3\" = --public=false && test \"$4\" = --filename && test \"$5\" = session.octet-session.json && test \"$6\" = - && test \"$#\" = 6 || exit 9\n",
        "cat > \"$0.received\"\n",
        "printf 'https://gist.github.com/synthetic/123456abcdef\\n'"));
    let prepared = prepare_share(&store, "synthetic").unwrap();
    let path = prepared.package_path().to_owned();
    let expected = std::fs::read(&path).unwrap();
    // Source changes after preparation must not change the confirmed snapshot.
    let mut session = Session::open(store.dir().join("synthetic.jsonl")).unwrap();
    session
        .append(EntryValue::Message(Message::User(UserMessage {
            content: vec![UserPart::Text("later not shared".into())],
        })))
        .unwrap();
    drop(session);
    let url = publish_with(prepared, true, &AtomicBool::new(false), program.as_os_str())
        .await
        .unwrap();
    assert_eq!(url, "https://gist.github.com/synthetic/123456abcdef");
    assert_eq!(
        std::fs::read(program.with_file_name("mock-success.received")).unwrap(),
        expected
    );
    assert!(!path.exists());
}

#[cfg(unix)]
#[tokio::test]
async fn mocked_gh_failure_and_cancellation_cleanup_without_echoing_credentials() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    let failure = mock(
        &root,
        "mock-failure",
        "cat >/dev/null\nprintf 'token=synthetic-credential' >&2\nexit 17",
    );
    let prepared = prepare_share(&store, "synthetic").unwrap();
    let path = prepared.package_path().to_owned();
    let error = publish_with(prepared, true, &AtomicBool::new(false), failure.as_os_str())
        .await
        .unwrap_err();
    assert!(!error.to_string().contains("synthetic-credential"));
    assert!(!path.exists());
    let hanging = mock(
        &root,
        "mock-cancel",
        "cat >/dev/null\nprintf '%s' \"$$\" > \"$0.called\"\nexec sleep 30",
    );
    let prepared = prepare_share(&store, "synthetic").unwrap();
    let path = prepared.package_path().to_owned();
    let cancelled = AtomicBool::new(false);
    let marker = hanging.with_file_name("mock-cancel.called");
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mock subprocess must actually start before cancellation");
        cancelled.store(true, Ordering::Release);
    };
    let (result, ()) = tokio::join!(
        publish_with(prepared, true, &cancelled, hanging.as_os_str()),
        cancel
    );
    let error = result.unwrap_err().to_string();
    assert!(error.contains("cancelled"));
    assert!(error.contains("cannot be undone"));
    assert!(!path.exists());
    let pid = std::fs::read_to_string(marker).unwrap();
    let exists = std::process::Command::new("kill")
        .args(["-0", pid.trim()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !exists.success(),
        "cancelled mock process must be killed AND reaped"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn changed_snapshot_or_symlink_never_starts_mocked_gh() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    let program = mock(
        &root,
        "mock-not-called",
        "printf called > \"$0.called\"\nexit 1",
    );
    for replace_symlink in [false, true] {
        let prepared = prepare_share(&store, "synthetic").unwrap();
        let path = prepared.package_path().to_owned();
        if replace_symlink {
            std::fs::remove_file(&path).unwrap();
            symlink(store.dir().join("synthetic.jsonl"), &path).unwrap();
        } else {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            std::fs::write(&path, b"changed").unwrap();
        }
        assert!(
            publish_with(prepared, true, &AtomicBool::new(false), program.as_os_str())
                .await
                .is_err()
        );
        assert!(!path.exists());
        assert!(!program.with_file_name("mock-not-called.called").exists());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn mocked_gh_invalid_output_is_rejected_without_claiming_rollback() {
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    for (index, output) in [
        "https://example.invalid/synthetic/abcdef",
        "https://gist.github.com/synthetic/not-a-gist",
        "https://gist.github.com/synthetic/abcdef?secret=synthetic",
        "https://gist.github.com/synthetic/abcdef/",
        "https://gist.github.com/synthetic/%61bcdef",
        "https://gist.github.com/synthetic/abcdef\nhttps://gist.github.com/synthetic/012345",
        "not a URL token=synthetic-credential",
    ]
    .iter()
    .enumerate()
    {
        let program = mock(
            &root,
            &format!("mock-invalid-{index}"),
            &format!("cat >/dev/null\nprintf '%s\\n' '{output}'"),
        );
        let prepared = prepare_share(&store, "synthetic").unwrap();
        let path = prepared.package_path().to_owned();
        let error = publish_with(prepared, true, &AtomicBool::new(false), program.as_os_str())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("publication may have succeeded"));
        assert!(!error.to_string().contains("synthetic-credential"));
        assert!(!path.exists());
    }
}

#[test]
fn share_rejects_torn_or_corrupt_source_without_rewriting_it() {
    use std::io::Write;
    let root = tempfile::tempdir().unwrap();
    let store = fixture(&root);
    let source = store.dir().join("synthetic.jsonl");
    let original = std::fs::read(&source).unwrap();
    for tail in [
        b"{\"type\":".as_slice(),
        b"{\"type\":\"head\",\"id\":\"missing\"}\n".as_slice(),
    ] {
        std::fs::write(&source, &original).unwrap();
        std::fs::OpenOptions::new()
            .append(true)
            .open(&source)
            .unwrap()
            .write_all(tail)
            .unwrap();
        let before = std::fs::read(&source).unwrap();
        assert!(prepare_share(&store, "synthetic").is_err());
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}
