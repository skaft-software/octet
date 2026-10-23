use super::*;
use std::os::windows::fs::OpenOptionsExt;
use std::process::Command;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const ID: &str = "test-extension";
const CHILD_ROOT: &str = "OCTET_TEST_EXTENSION_UPDATE_ROOT";
const CHILD_PHASE: &str = "OCTET_TEST_EXTENSION_UPDATE_PHASE";

struct Fixture {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    destination: PathBuf,
    staging: PathBuf,
    transaction: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary
            .path()
            .join("Profile with spaces ü/.octet/extensions");
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let destination = root.join(ID);
        let staging = root.join(".octet-extension-install-fixture");
        write_package(&destination, b"old", "0.9.5");
        write_package(&staging, b"new", env!("CARGO_PKG_VERSION"));
        let transaction = root.join(format!("{UPDATE_PREFIX}{ID}"));
        Self {
            _temporary: temporary,
            root,
            destination,
            staging,
            transaction,
        }
    }

    fn previous(&self) -> PathBuf {
        self.transaction.join(PREVIOUS)
    }

    fn publish(&self) -> anyhow::Result<()> {
        super::super::publish_staging(&self.root, &self.staging, &self.destination, true, ID)
    }

    fn interrupt(&self, phase: &str) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "extension_package::windows::tests::interrupted_update_child",
                "--nocapture",
            ])
            .env(CHILD_ROOT, &self.root)
            .env(CHILD_PHASE, phase)
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(73),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn assert_payload(&self, expected: &[u8]) {
        assert_eq!(
            fs::read(self.destination.join("payload")).unwrap(),
            expected
        );
    }
}

fn write_package(path: &Path, payload: &[u8], host: &str) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("payload"), payload).unwrap();
    fs::write(path.join("extension.toml"), format!(
        "name = '{ID}'\nversion = '0.1.0'\napi_version = '0.4'\nrequires_octet = '={host}'\n[entrypoint]\ncommand = 'payload'\n"
    )).unwrap();
    fs::create_dir(path.join("skills")).unwrap();
    let record = serde_json::json!({
        "schema_version": 1,
        "id": ID,
        "version": "0.1.0",
        "api_version": "0.4",
        "requires_octet": format!("={host}"),
        "source_kind": "local",
        "source": "reviewed-local-archive.tar.gz",
        "archive_sha256": digest_hex(&Sha256::digest(payload)),
        "installed_by_octet": host,
    });
    fs::write(
        path.join("install.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
}

// Invoked only by the parent regression with an isolated root. process::exit
// deliberately skips all destructors: neither rollback nor TempDir cleanup can
// accidentally make recovery pass. Normal test discovery does nothing here.
#[test]
fn interrupted_update_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let phase = std::env::var(CHILD_PHASE).unwrap();
    let _lock = acquire_lock(&root).unwrap();
    let staging = root.join(".octet-extension-install-fixture");
    let destination = root.join(ID);
    publish_update_observed(&root, &staging, &destination, ID, |at| {
        if phase == "restored" && at == "displaced" {
            rename_no_replace(&staging, &root.join("held-candidate")).unwrap();
        }
        if at == phase {
            std::process::exit(73);
        }
    })
    .unwrap();
    panic!("did not reach interruption phase {phase}");
}

#[test]
fn windows_publication_updates_a_bundle_from_an_older_host_without_changing_external_state() {
    let fixture = Fixture::new();
    let external = fixture.root.parent().unwrap().join("config.json");
    fs::write(&external, b"keep configuration and trust").unwrap();
    let _lock = acquire_lock(&fixture.root).unwrap();
    fixture.publish().unwrap();
    fixture.assert_payload(b"new");
    assert!(!fixture.staging.exists());
    assert!(!fixture.transaction.exists());
    assert_eq!(fs::read(external).unwrap(), b"keep configuration and trust");
}

#[test]
fn windows_recovery_survives_process_exit_at_every_publication_boundary() {
    for phase in ["created", "prepared", "displaced", "published", "restored"] {
        let fixture = Fixture::new();
        fixture.interrupt(phase);
        let _lock = acquire_lock(&fixture.root).unwrap();
        fixture.assert_payload(if phase == "published" { b"new" } else { b"old" });
        assert!(!fixture.transaction.exists(), "{phase}");
        drop(_lock);
        let _lock = acquire_lock(&fixture.root).unwrap();
        fixture.assert_payload(if phase == "published" { b"new" } else { b"old" });
    }
}

#[test]
fn windows_recovery_handles_a_torn_intent_before_displacement() {
    let fixture = Fixture::new();
    fixture.interrupt("created");
    fs::write(fixture.transaction.join(JOURNAL), b"{\"schema_version\":").unwrap();
    let _lock = acquire_lock(&fixture.root).unwrap();
    fixture.assert_payload(b"old");
    assert!(!fixture.transaction.exists());
}

#[test]
fn windows_recovery_finishes_interrupted_previous_package_cleanup() {
    for remove_directory in [false, true] {
        let fixture = Fixture::new();
        fixture.interrupt("published");
        fs::remove_file(fixture.previous().join("install.json")).unwrap();
        if remove_directory {
            fs::remove_dir_all(fixture.previous()).unwrap();
        }
        let _lock = acquire_lock(&fixture.root).unwrap();
        fixture.assert_payload(b"new");
        assert!(!fixture.transaction.exists());
    }
}

#[test]
fn windows_publication_restores_previous_when_candidate_rename_fails() {
    let fixture = Fixture::new();
    let _lock = acquire_lock(&fixture.root).unwrap();
    let error = publish_update_observed(
        &fixture.root,
        &fixture.staging,
        &fixture.destination,
        ID,
        |phase| {
            if phase == "displaced" {
                rename_no_replace(&fixture.staging, &fixture.root.join("held-candidate")).unwrap();
            }
        },
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("previous install was restored"),
        "{error:#}"
    );
    fixture.assert_payload(b"old");
    assert!(!fixture.transaction.exists());
}

#[test]
fn windows_publication_preserves_previous_and_foreign_destination_when_rollback_is_blocked() {
    for foreign_is_directory in [false, true] {
        let fixture = Fixture::new();
        let lock = acquire_lock(&fixture.root).unwrap();
        let error = publish_update_observed(
            &fixture.root,
            &fixture.staging,
            &fixture.destination,
            ID,
            |phase| {
                if phase == "displaced" {
                    if foreign_is_directory {
                        fs::create_dir(&fixture.destination).unwrap();
                        fs::write(fixture.destination.join("foreign"), b"keep me").unwrap();
                    } else {
                        fs::write(&fixture.destination, b"keep me").unwrap();
                    }
                }
            },
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("previous bundle retained"),
            "{error:#}"
        );
        assert!(recover_updates(&fixture.root).is_err());
        assert_eq!(
            fs::read(fixture.previous().join("payload")).unwrap(),
            b"old"
        );
        let foreign = fixture.root.join("foreign-destination");
        rename_no_replace(&fixture.destination, &foreign).unwrap();
        assert_eq!(
            fs::read(if foreign_is_directory {
                foreign.join("foreign")
            } else {
                foreign
            })
            .unwrap(),
            b"keep me"
        );
        drop(lock);
        let _lock = acquire_lock(&fixture.root).unwrap();
        fixture.assert_payload(b"old");
        assert!(!fixture.transaction.exists());
    }
}

#[test]
fn windows_recovery_does_not_discard_backup_for_a_different_managed_destination() {
    let fixture = Fixture::new();
    fixture.interrupt("displaced");
    write_package(&fixture.destination, b"foreign", env!("CARGO_PKG_VERSION"));
    let error = acquire_lock(&fixture.root)
        .err()
        .expect("foreign destination must block recovery");
    assert!(
        format!("{error:#}").contains("destination changed"),
        "{error:#}"
    );
    fixture.assert_payload(b"foreign");
    assert_eq!(
        fs::read(fixture.previous().join("payload")).unwrap(),
        b"old"
    );
}

#[test]
fn windows_publication_reports_in_use_without_moving_previous() {
    let fixture = Fixture::new();
    let _lock = acquire_lock(&fixture.root).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(&fixture.destination)
        .unwrap();
    let error = fixture.publish().unwrap_err();
    assert!(
        format!("{error:#}").contains("close other octet instances"),
        "{error:#}"
    );
    fixture.assert_payload(b"old");
    assert!(!fixture.transaction.exists());
    drop(held);
    fixture.publish().unwrap();
    fixture.assert_payload(b"new");
}

#[test]
fn windows_publication_defers_locked_previous_file_cleanup_without_rolling_back_commit() {
    let fixture = Fixture::new();
    let lock = acquire_lock(&fixture.root).unwrap();
    let mut held = None;
    publish_update_observed(
        &fixture.root,
        &fixture.staging,
        &fixture.destination,
        ID,
        |phase| {
            if phase == "published" {
                held = Some(
                    OpenOptions::new()
                        .read(true)
                        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                        .open(fixture.previous().join("payload"))
                        .unwrap(),
                );
            }
        },
    )
    .unwrap();
    fixture.assert_payload(b"new");
    assert!(fixture.transaction.join(JOURNAL).is_file());
    drop(lock);
    let error = crate::extension_bundle::remove_installed(&fixture.root, ID).unwrap_err();
    assert!(
        format!("{error:#}").contains("cleanup is still pending"),
        "{error:#}"
    );
    fixture.assert_payload(b"new");
    drop(held);
    let lock = acquire_lock(&fixture.root).unwrap();
    fixture.assert_payload(b"new");
    assert!(!fixture.transaction.exists());
    drop(lock);
    crate::extension_bundle::remove_installed(&fixture.root, ID).unwrap();
    let _lock = acquire_lock(&fixture.root).unwrap();
    assert!(
        !fixture.destination.exists(),
        "recovery must not resurrect a removed bundle"
    );
}

#[test]
fn windows_recovery_rejects_malformed_or_oversized_intents_without_touching_previous() {
    for bytes in [
        b"{".to_vec(),
        vec![b'x'; MAX_JOURNAL_BYTES + 1],
        serde_json::to_vec(&serde_json::json!({ "schema_version": 2, "previous_record": "a".repeat(64), "candidate_record": "b".repeat(64) })).unwrap(),
        serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "previous_record": "../escape", "candidate_record": "b".repeat(64) })).unwrap(),
        serde_json::to_vec(&serde_json::json!({ "schema_version": 1, "previous_record": "a".repeat(64), "candidate_record": "b".repeat(64), "path": "../escape" })).unwrap(),
    ] {
        let fixture = Fixture::new();
        fixture.interrupt("displaced");
        fs::write(fixture.transaction.join(JOURNAL), bytes).unwrap();
        assert!(acquire_lock(&fixture.root).is_err());
        assert!(!fixture.destination.exists());
        assert_eq!(fs::read(fixture.previous().join("payload")).unwrap(), b"old");
    }
}

#[test]
fn windows_recovery_rejects_a_changed_previous_install_record() {
    let fixture = Fixture::new();
    fixture.interrupt("displaced");
    let record_path = fixture.previous().join("install.json");
    let mut record: serde_json::Value =
        serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    record["source"] = serde_json::json!("changed-archive.tar.gz");
    fs::write(record_path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(acquire_lock(&fixture.root).is_err());
    assert!(!fixture.destination.exists());
    assert_eq!(
        fs::read(fixture.previous().join("payload")).unwrap(),
        b"old"
    );
}

fn junction(link: &Path, target: &Path) {
    let output = Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn windows_recovery_refuses_junctions_in_transaction_and_previous_paths() {
    for link_previous in [false, true] {
        let fixture = Fixture::new();
        let outside = fixture.root.parent().unwrap().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("sentinel"), b"untouched").unwrap();
        if link_previous {
            fs::create_dir(&fixture.transaction).unwrap();
            junction(&fixture.previous(), &outside);
        } else {
            junction(&fixture.transaction, &outside);
        }
        assert!(acquire_lock(&fixture.root).is_err());
        fixture.assert_payload(b"old");
        assert_eq!(fs::read(outside.join("sentinel")).unwrap(), b"untouched");
    }
}

#[test]
fn windows_listing_ensure_and_discovery_recover_before_inspecting_installed_bundles() {
    use crate::resource_resolver::{ResourceKind, ResourceResolver};
    for entry in ["list", "ensure", "discovery"] {
        let fixture = Fixture::new();
        fixture.interrupt("displaced");
        match entry {
            "list" => assert_eq!(
                crate::extension_bundle::list_installed(&fixture.root).unwrap()[0].id,
                ID
            ),
            "ensure" => crate::extension_bundle::ensure_installed(&fixture.root, ID).unwrap(),
            "discovery" => {
                let resolver = ResourceResolver::with_global_octet_dir(
                    fixture.root.parent().unwrap().join("workspace"),
                    false,
                    fixture.root.parent().unwrap().to_owned(),
                );
                let snapshot = resolver.discover(ResourceKind::Extension, &[]);
                assert!(snapshot.get(ID).is_some(), "{:?}", snapshot.diagnostics());
            }
            _ => unreachable!(),
        }
        fixture.assert_payload(b"old");
        assert!(!fixture.transaction.exists());
    }
}

#[test]
fn windows_skill_discovery_recovers_before_scanning_bundled_skills() {
    let fixture = Fixture::new();
    // The previous bundle normally pins an older host. For this test it must
    // be current to qualify as a source of compatible installed skills.
    let record_path = fixture.destination.join("install.json");
    let record = fs::read_to_string(&record_path)
        .unwrap()
        .replace("0.9.5", env!("CARGO_PKG_VERSION"));
    fs::write(record_path, record).unwrap();
    fixture.interrupt("displaced");
    let roots = crate::extension_bundle::installed_skill_roots(&fixture.root);
    assert_eq!(roots, vec![fixture.destination.join("skills")]);
    fixture.assert_payload(b"old");
}

#[test]
fn windows_discovery_does_not_race_an_in_progress_update() {
    use crate::resource_resolver::{ResourceKind, ResourceResolver};
    let fixture = Fixture::new();
    let _lock = acquire_lock(&fixture.root).unwrap();
    publish_update_observed(
        &fixture.root,
        &fixture.staging,
        &fixture.destination,
        ID,
        |phase| {
            if phase == "displaced" {
                let resolver = ResourceResolver::with_global_octet_dir(
                    fixture.root.parent().unwrap().join("workspace"),
                    false,
                    fixture.root.parent().unwrap().to_owned(),
                );
                let snapshot = resolver.discover(ResourceKind::Extension, &[]);
                assert!(snapshot.resources().is_empty());
                assert!(snapshot
                    .diagnostics()
                    .iter()
                    .any(|d| d.message.contains("already running")));
                assert!(!fixture.destination.exists());
                assert!(fixture.previous().is_dir());
            }
        },
    )
    .unwrap();
    fixture.assert_payload(b"new");
}

#[test]
fn windows_project_and_explicit_discovery_do_not_recover_package_transactions() {
    use crate::resource_resolver::{ResourceKind, ResourceResolver};
    for source in ["untrusted-project", "trusted-project", "explicit"] {
        let fixture = Fixture::new();
        fixture.interrupt("displaced");
        let workspace = fixture.root.parent().unwrap().parent().unwrap();
        let resolver = ResourceResolver::with_global_octet_dir(
            workspace.to_owned(),
            source == "trusted-project",
            workspace.join("other-profile"),
        );
        let explicit = if source == "explicit" {
            vec![fixture.root.clone()]
        } else {
            Vec::new()
        };
        let _snapshot = resolver.discover(ResourceKind::Extension, &explicit);
        assert!(!fixture.destination.exists(), "{source}");
        assert!(fixture.transaction.join(JOURNAL).is_file(), "{source}");
        assert_eq!(
            fs::read(fixture.previous().join("payload")).unwrap(),
            b"old"
        );
    }
}

#[test]
fn windows_package_lock_rejects_a_redirected_installation_root() {
    let fixture = Fixture::new();
    let alias = fixture.root.parent().unwrap().join("redirected-root");
    junction(&alias, &fixture.root);
    assert!(super::super::lock_existing(&alias).is_err());
    fixture.assert_payload(b"old");
    assert!(!fixture.transaction.exists());
    assert!(!fixture.root.join(".package.lock").exists());
}

#[test]
fn windows_read_paths_do_not_create_missing_installation_roots() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("missing/extensions");
    assert!(super::super::lock_existing(&root).unwrap().is_none());
    assert!(crate::extension_bundle::list_installed(&root)
        .unwrap()
        .is_empty());
    assert!(crate::extension_bundle::installed_skill_roots(&root).is_empty());
    assert!(!root.exists());
}
