//! The boundary between a session id and the file it is allowed to name:
//! `path_by_id` resolves only a valid direct regular file inside the store, a
//! delegated worker handle resolves only inside the delegation directory, a
//! forged roster entry cannot escape it, and every unlaunchable handle shape is
//! refused with its own bounded reason before any filesystem work happens.
//!
//! Separate from the workspace tests because this is the only group where a
//! session name arrives from outside the process, so confinement rather than
//! bookkeeping is the property under test.

use super::*;

#[test]
fn path_by_id_resolves_only_a_valid_direct_regular_file() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let one_path = store.dir().join("one.jsonl");
    let mut session = Session::create(&one_path).unwrap();
    session
        .append(EntryValue::Message(Message::User(octet_ai::UserMessage {
            content: vec![UserPart::Text("one".into())],
        })))
        .unwrap();
    std::fs::write(store.dir().join("one.txt"), b"").unwrap();
    std::fs::write(
        store.dir().join("unrelated.jsonl"),
        b"not-json\nstill-not-json\n",
    )
    .unwrap();
    std::fs::create_dir(store.dir().join("directory.jsonl")).unwrap();

    assert_eq!(store.path_by_id("one").unwrap(), one_path);
    assert!(store.session_file_exists("one").unwrap());
    assert!(!store.session_file_exists("missing").unwrap());
    for invalid in ["", ".", "..", "../one", "one/two", "one\n"] {
        assert!(store.path_by_id(invalid).is_err(), "accepted {invalid:?}");
    }
    assert!(store.path_by_id("directory").is_err());
    assert!(store.session_file_exists("directory").is_err());
    let mut session_file_ids = store.session_file_ids();
    session_file_ids.sort();
    assert_eq!(session_file_ids, vec!["one", "unrelated"]);

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&one_path, store.dir().join("linked.jsonl")).unwrap();
        assert!(store.path_by_id("linked").is_err());
        assert!(store.session_file_exists("linked").is_err());
        assert!(!store.session_file_ids().iter().any(|id| id == "linked"));
        assert!(!store.list().iter().any(|session| {
            session.path.file_stem().and_then(|stem| stem.to_str()) == Some("linked")
        }));
    }
}

/// A credential-shaped string a hostile roster could carry as free text; it
/// must never reach a handle or a refusal reason.
const HANDLE_TEST_SECRET: &str = "sk-handle-secret-9f2b7c41d6ea";

/// One owner-only directory, the mode the host's private `team-*` directory
/// uses.
fn private_directory(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

/// The durable delegation roster exactly as the host writes it: one
/// owner-only `fleet.json` in the session store's private delegation
/// directory, whose record carries `session_path` in `status`.
fn write_roster(delegation: &Path, session_path: &Path, status: serde_json::Value, detached: bool) {
    let record = serde_json::json!({
        "agent_id": "agent-1",
        "agent_path": "/root/worker",
        "parent_id": "agent-0",
        "depth": 1,
        "task_name": "worker task",
        "display_task_name": "worker task",
        "session_path": session_path,
        "status": status,
        "detached": detached,
        "created_at_ms": 1,
        "started_at_ms": 2,
        "completed_at_ms": null,
        "turn_count": 0,
        "tool_call_count": 0,
        "usage": {
            "input_tokens": 0,
            "cache_read_tokens": 0,
            "cache_write_tokens": 0,
            "cache_write_1h_tokens": 0,
            "output_tokens": 0,
            "reasoning_tokens": 0,
            "total_tokens": 0,
        },
        "usage_uncertain": false,
        "cost": null,
        "cost_microdollars": null,
        "deadline_at_ms": null,
        "turn_limit": null,
        "extension_principal": null,
        "extension_profile": null,
        "extension_idempotency_key": null,
        "extension_fingerprint": null,
        "extension_policy": null,
        // Free text a forged or buggy roster could carry: the resolver's
        // durable diagnostic must never be relayed into a refusal.
        "durable_diagnostic": format!("credential {} must not leak", HANDLE_TEST_SECRET),
    });
    let fleet = serde_json::json!({
        "version": 1,
        "root_session": delegation.join("parent.jsonl"),
        "records": [record],
    });
    let path = delegation.join("fleet.json");
    // The host persists the roster owner-only
    // (`DelegationManager::persist_durable_fleet_locked`); a plain write
    // would fail the owner-only read on Windows and skew every refusal
    // to `UnknownWorker`.
    octet_agent::secure_fs::write_private_atomic(&path, fleet.to_string().as_bytes(), 1024 * 1024)
        .unwrap();
}

/// The typed verdict behind one refusal, which is what a frontend branches
/// on instead of matching message text.
fn refusal(error: &anyhow::Error) -> DelegatedHandleRefusal {
    *error
        .downcast_ref::<DelegatedHandleRefusal>()
        .unwrap_or_else(|| panic!("typed worker-handle refusal expected, got {error:#}"))
}

#[test]
fn a_launchable_worker_handle_resolves_to_the_child_transcript() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let parent_path = store.dir().join("parent.jsonl");
    Session::create(&parent_path).unwrap();

    let delegation = store.dir().join(DELEGATION_DIRECTORY);
    let team = delegation.join("team-alpha");
    private_directory(&team);
    let child = team.join("0001-worker.jsonl");
    Session::create(&child).unwrap();
    write_roster(
        &delegation,
        &child,
        serde_json::json!({"state": "detached"}),
        true,
    );

    let handle = octet_agent::delegated_session_reference(&child).unwrap();
    assert_eq!(handle.len(), DELEGATED_SESSION_HANDLE_PREFIX.len() + 64);
    // Opaque, path-free, argv-safe, and credential-free.
    assert!(handle
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b':'));
    assert!(!handle.contains('/'));
    assert!(!handle.contains(HANDLE_TEST_SECRET));

    assert_eq!(store.path_by_id(&handle).unwrap(), child);
    assert_eq!(store.path_for_delegated_handle(&handle).unwrap(), child);
    // An ordinary session id keeps exactly its previous resolution.
    assert_eq!(store.path_by_id("parent").unwrap(), parent_path);
    // A settled worker is still launchable: a detached, completed, or
    // shutdown record is not a live writer. The store adds only the
    // roster-level liveness rule the durable record cannot carry.
    write_roster(
        &delegation,
        &child,
        serde_json::json!({"state": "completed", "output": "done"}),
        true,
    );
    assert_eq!(store.path_by_id(&handle).unwrap(), child);
}

#[test]
fn every_unlaunchable_worker_handle_refuses_with_a_distinct_bounded_reason() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let delegation = store.dir().join(DELEGATION_DIRECTORY);
    let team = delegation.join("team-alpha");
    private_directory(&team);
    let child = team.join("0001-worker.jsonl");
    Session::create(&child).unwrap();
    let handle = octet_agent::delegated_session_reference(&child).unwrap();
    let unknown = format!("agent-session:{}", "0".repeat(64));
    let detached = || serde_json::json!({"state": "detached"});

    // Parked at the approval boundary: opening it elsewhere would be
    // unattended mutation.
    write_roster(
        &delegation,
        &child,
        serde_json::json!({"state": "awaiting_approval", "reason": "approval is unavailable"}),
        false,
    );
    assert_eq!(
        refusal(&store.path_by_id(&handle).unwrap_err()),
        DelegatedHandleRefusal::ParkedAtApprovalBoundary
    );

    // Live in the owning process: the durable roster cannot carry the
    // process-local liveness flag, so the live roster state is the
    // fail-closed signal, and one session has one writer.
    for state in ["pending", "running"] {
        write_roster(
            &delegation,
            &child,
            serde_json::json!({ "state": state }),
            false,
        );
        assert_eq!(
            refusal(&store.path_by_id(&handle).unwrap_err()),
            DelegatedHandleRefusal::LiveInOwningProcess { status: state }
        );
    }

    // Vanished transcript: nothing to open, so no fabricated launch.
    std::fs::remove_file(&child).unwrap();
    write_roster(&delegation, &child, detached(), true);
    assert_eq!(
        refusal(&store.path_by_id(&handle).unwrap_err()),
        DelegatedHandleRefusal::VanishedTranscript
    );

    // Unknown handle: the roster is readable and simply does not know it.
    assert_eq!(
        refusal(&store.path_by_id(&unknown).unwrap_err()),
        DelegatedHandleRefusal::UnknownWorker
    );

    // Missing roster: an explicit refusal, never an empty success.
    std::fs::remove_file(delegation.join("fleet.json")).unwrap();
    assert_eq!(
        refusal(&store.path_by_id(&handle).unwrap_err()),
        DelegatedHandleRefusal::RosterUnavailable
    );

    let verdicts = [
        DelegatedHandleRefusal::MalformedHandle,
        DelegatedHandleRefusal::RosterUnavailable,
        DelegatedHandleRefusal::UnknownWorker,
        DelegatedHandleRefusal::ParkedAtApprovalBoundary,
        DelegatedHandleRefusal::LiveInOwningProcess { status: "running" },
        DelegatedHandleRefusal::VanishedTranscript,
        DelegatedHandleRefusal::OutsideDelegationDirectory,
    ];
    let mut codes = HashSet::new();
    let mut reasons = HashSet::new();
    let store_directory = store.dir().to_string_lossy().into_owned();
    let child_path = child.to_string_lossy().into_owned();
    for verdict in verdicts {
        let reason = verdict.to_string();
        assert!(
            reason.len() <= 400,
            "every reason is bounded: {} bytes",
            reason.len()
        );
        assert!(!reason.chars().any(char::is_control), "{reason}");
        // No credential, no session secret, no transcript path, and no
        // roster path in any reason.
        assert!(!reason.contains(HANDLE_TEST_SECRET), "{reason}");
        assert!(!reason.contains(&store_directory), "{reason}");
        assert!(!reason.contains(&child_path), "{reason}");
        assert!(!reason.contains("fleet.json"), "{reason}");
        assert!(codes.insert(verdict.code()), "codes are distinct");
        assert!(reasons.insert(reason), "reasons are distinct");
    }
    assert_eq!(codes.len(), 7);
    // The stable codes are the machine-readable half of the same reason.
    assert_eq!(
        DelegatedHandleRefusal::MalformedHandle.code(),
        "malformed_worker_handle"
    );
    assert_eq!(
        DelegatedHandleRefusal::ParkedAtApprovalBoundary.code(),
        "worker_awaiting_approval"
    );
    assert_eq!(
        DelegatedHandleRefusal::LiveInOwningProcess { status: "pending" }.code(),
        "worker_live_in_owning_process"
    );
}

#[test]
fn a_malformed_worker_handle_is_refused_before_any_filesystem_work() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    // Deliberately nothing on disk: no session directory, no delegation
    // directory, no roster. The shape check must not need any of them, so a
    // shell metacharacter, a control byte, or a path component can never
    // reach a path join.
    assert!(!store.dir().exists());
    let mut malformed = vec![
        "agent-session:".to_owned(),
        "agent-session:0".to_owned(),
        format!("agent-session:{}", "a".repeat(63)),
        format!("agent-session:{}", "A".repeat(64)),
        format!("agent-session:{}x", "a".repeat(64)),
        format!("agent-session:x{}", "a".repeat(64)),
        format!("agent-session:{}\n", "a".repeat(64)),
        format!("agent-session:{} ", "a".repeat(64)),
        "agent-session:../../etc/passwd".to_owned(),
        "agent-session:$(id)".to_owned(),
        "agent-session:a;rm -rf /.jsonl".to_owned(),
        "agent-session:/tmp/0001-worker.jsonl".to_owned(),
        "agent-session:team-alpha/0001-worker.jsonl".to_owned(),
        "agent-session:é".to_owned(),
    ];
    malformed.push(format!("agent-session:{}", "\u{0}".repeat(64)));
    for value in malformed {
        assert_eq!(
            refusal(&store.path_by_id(&value).unwrap_err()),
            DelegatedHandleRefusal::MalformedHandle,
            "accepted {value:?}"
        );
        assert_eq!(
            refusal(&store.path_for_delegated_handle(&value).unwrap_err()),
            DelegatedHandleRefusal::MalformedHandle
        );
    }
    assert!(
        !store.dir().exists(),
        "shape validation must not touch the filesystem"
    );
}

#[test]
fn a_forged_roster_entry_cannot_escape_the_delegation_directory() {
    let root = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let store = SessionStore::new(root.path(), workspace.path());
    std::fs::create_dir_all(store.dir()).unwrap();
    let delegation = store.dir().join(DELEGATION_DIRECTORY);
    private_directory(&delegation);
    let detached = || serde_json::json!({"state": "detached"});

    // The handle is derived only from the two trailing path components
    // (`octet_agent::delegated_session_reference`), so a copied or forged
    // roster entry can name the same pair *outside* the private delegation
    // directory and hash to the same handle.
    let escape_team = store.dir().join("team-escape");
    private_directory(&escape_team);
    let escaped = escape_team.join("0001-worker.jsonl");
    Session::create(&escaped).unwrap();
    let escaped_handle = octet_agent::delegated_session_reference(&escaped).unwrap();
    assert_eq!(
        octet_agent::delegated_session_reference(
            &delegation.join("team-escape").join("0001-worker.jsonl")
        )
        .unwrap(),
        escaped_handle
    );
    write_roster(&delegation, &escaped, detached(), true);
    assert_eq!(
        refusal(&store.path_by_id(&escaped_handle).unwrap_err()),
        DelegatedHandleRefusal::OutsideDelegationDirectory
    );

    // A traversal-bearing record path is not a two-component path inside the
    // delegation directory, so it is refused outright, even when it resolves
    // to a file that exists.
    let inside_escape_team = delegation.join("team-escape");
    private_directory(&inside_escape_team);
    let inside_escape = inside_escape_team.join("0001-worker.jsonl");
    Session::create(&inside_escape).unwrap();
    assert_eq!(
        octet_agent::delegated_session_reference(&inside_escape).unwrap(),
        escaped_handle,
        "the traversal form carries the same handle, or the test proves nothing"
    );
    let team = delegation.join("team-alpha");
    private_directory(&team);
    let child = team.join("0001-worker.jsonl");
    Session::create(&child).unwrap();
    let traversing = delegation
        .join("team-alpha")
        .join("..")
        .join("team-escape")
        .join("0001-worker.jsonl");
    assert!(traversing.exists(), "the traversal target really exists");
    write_roster(&delegation, &traversing, detached(), true);
    assert_eq!(
        refusal(&store.path_by_id(&escaped_handle).unwrap_err()),
        DelegatedHandleRefusal::OutsideDelegationDirectory
    );

    // A symlinked team directory is refused, never followed.
    #[cfg(unix)]
    {
        let linked_team = delegation.join("team-linked");
        std::os::unix::fs::symlink(&escape_team, &linked_team).unwrap();
        let linked = linked_team.join("0001-worker.jsonl");
        let linked_handle = octet_agent::delegated_session_reference(&linked).unwrap();
        write_roster(&delegation, &linked, detached(), true);
        assert_eq!(
            refusal(&store.path_by_id(&linked_handle).unwrap_err()),
            DelegatedHandleRefusal::OutsideDelegationDirectory
        );
    }

    // The legitimate child beside the forged entries still resolves: the
    // confinement refuses the escape, not delegation itself.
    let handle = octet_agent::delegated_session_reference(&child).unwrap();
    write_roster(&delegation, &child, detached(), true);
    assert_eq!(store.path_by_id(&handle).unwrap(), child);
}
