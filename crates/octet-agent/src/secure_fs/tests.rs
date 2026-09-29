//! Tests for the platform-independent surface in `super`.
//!
//! These assert the properties that must hold on every host — bounded
//! reads reject the first byte past the limit, symlink and reparse-point
//! traversal fails closed, a replaced file is not silently overwritten,
//! private objects are owner-only — rather than the internals of any one
//! `imp` backend. A behaviour that genuinely differs per platform is
//! cfg-marked in place, next to the test that differs.
//!
//! This is a separate file because the `imp` backends are large and
//! platform-specific; keeping the shared expectations out of them means
//! the backends stay pure implementation and a reader looking for "what
//! does this module guarantee?" has one file to read.

use super::*;

#[test]
fn bounded_read_rejects_extra_byte() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("large");
    std::fs::write(&path, vec![b'x'; 17]).unwrap();
    assert!(matches!(
        read_regular_file_bounded(&path, 16),
        Err(SecureFileError::TooLarge { .. })
    ));
}

#[cfg(any(unix, windows))]
#[test]
fn unique_private_directories_are_distinct_and_owner_only() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap().join("private");

    let first = create_unique_private_directory(&parent, "team-").unwrap();
    let second = create_unique_private_directory(&parent, "team-").unwrap();

    assert_ne!(first, second);
    assert_eq!(first.parent(), Some(parent.as_path()));
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("team-"));
    open_private_directory_for_lock(&first).unwrap();
    open_private_directory_for_lock(&second).unwrap();
}

#[cfg(any(unix, windows))]
#[test]
fn bound_private_directory_rejects_a_replacement_path() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap().join("private");
    let bound = create_bound_private_directory(&parent, "team-").unwrap();
    let moved = parent.join("original-team");
    std::fs::rename(bound.path(), &moved).unwrap();
    create_private_directory_all(bound.path()).unwrap();
    let child = bound.path().join("child.jsonl");

    assert!(matches!(
        bound.create_regular_file_for_append(&child),
        Err(SecureFileError::Changed)
    ));
    assert!(!child.exists());
    assert!(moved.exists());
}

#[cfg(any(unix, windows))]
#[test]
fn empty_private_directory_cleanup_is_bounded_to_empty_private_directories() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap().join("private");
    let empty = create_unique_private_directory(&parent, "team-").unwrap();
    let nonempty = create_unique_private_directory(&parent, "team-").unwrap();
    std::fs::write(nonempty.join("provenance.jsonl"), b"record").unwrap();

    assert!(remove_empty_private_directory_if_exists(&empty).unwrap());
    assert!(!empty.exists());
    assert!(!remove_empty_private_directory_if_exists(&empty).unwrap());
    assert!(remove_empty_private_directory_if_exists(&nonempty).is_err());
    assert_eq!(
        std::fs::read(nonempty.join("provenance.jsonl")).unwrap(),
        b"record"
    );
    assert_eq!(
        std::fs::read_dir(&parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>(),
        vec![nonempty]
    );
}

#[cfg(unix)]
#[test]
fn private_directory_cleanup_rejects_symlinks() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let outside = create_unique_private_directory(&root.join("private"), "outside-").unwrap();
    let link = root.join("link");
    symlink(&outside, &link).unwrap();

    assert!(remove_empty_private_directory_if_exists(&link).is_err());
    assert!(outside.exists());
}

#[cfg(unix)]
#[test]
fn unique_private_directory_rejects_symlinked_parent() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let outside = root.join("outside");
    let parent = root.join("private");
    std::fs::create_dir(&outside).unwrap();
    symlink(&outside, &parent).unwrap();

    assert!(create_unique_private_directory(&parent, "team-").is_err());
    assert_eq!(std::fs::read_dir(outside).unwrap().count(), 0);
}

#[test]
fn unique_private_directory_rejects_path_prefixes() {
    let directory = tempfile::tempdir().unwrap();
    let parent = directory.path().canonicalize().unwrap().join("private");

    assert!(matches!(
        create_unique_private_directory(&parent, "../team-"),
        Err(SecureFileError::InvalidPath(_))
    ));
    assert!(!parent.exists());
}

#[test]
fn public_conditional_atomic_write_rejects_a_stale_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "newer version").unwrap();

    assert!(matches!(
        write_atomic_if_unchanged(&path, Some(b"older version"), b"stale replacement", 1024,),
        Err(SecureFileError::Changed)
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "newer version");
}

#[test]
fn private_conditional_atomic_write_rejects_a_stale_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("private/target.json");
    write_private_atomic(&path, b"newer version", 1024).unwrap();

    assert!(matches!(
        write_private_atomic_if_unchanged(
            &path,
            Some(b"older version"),
            b"stale replacement",
            1024,
        ),
        Err(SecureFileError::Changed)
    ));
    assert_eq!(
        read_private_file_bounded(&path, 1024).unwrap(),
        b"newer version"
    );
}

#[test]
fn concurrent_target_change_is_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "version one").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    std::fs::write(&path, "version two").unwrap();

    assert!(matches!(
        prepared.commit(b"stale replacement"),
        Err(SecureFileError::Changed)
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "version two");
}

#[cfg(any(unix, windows))]
#[test]
fn same_content_target_replacement_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    let displaced = root.join("displaced.txt");
    std::fs::write(&path, "unchanged bytes").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();

    std::fs::rename(&path, &displaced).unwrap();
    std::fs::write(&path, "unchanged bytes").unwrap();

    assert!(matches!(
        prepared.commit(b"stale replacement"),
        Err(SecureFileError::Changed)
    ));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "unchanged bytes");
}

#[cfg(any(unix, windows))]
#[test]
fn target_created_immediately_before_publish_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    let cancellation_checks = std::cell::Cell::new(0);

    let result = prepared.commit_if(b"stale replacement", || {
        let check = cancellation_checks.get() + 1;
        cancellation_checks.set(check);
        // The third check occurs after the final unchanged-state check and
        // immediately before publication.
        if check == 3 {
            std::fs::write(&path, "competing creation").unwrap();
        }
        false
    });

    assert!(matches!(result, Err(SecureFileError::Changed)));
    assert_eq!(std::fs::read_to_string(path).unwrap(), "competing creation");
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn existing_target_changed_after_final_check_is_rolled_back() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    let displaced = root.join("prepared-version.txt");
    std::fs::write(&path, "prepared version").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    let cancellation_checks = std::cell::Cell::new(0);

    let result = prepared.commit_if(b"stale replacement", || {
        let check = cancellation_checks.get() + 1;
        cancellation_checks.set(check);
        // The third check occurs after the final unchanged-state check and
        // immediately before publication.
        if check == 3 {
            std::fs::rename(&path, &displaced).unwrap();
            std::fs::write(&path, "competing replacement").unwrap();
        }
        false
    });

    assert!(matches!(result, Err(SecureFileError::Changed)));
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "competing replacement"
    );
    assert_eq!(
        std::fs::read_to_string(displaced).unwrap(),
        "prepared version"
    );
}

fn directory_names(directory: &Path) -> Vec<String> {
    let mut names = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    names.sort();
    names
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn existing_target_is_replaced_without_residue() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "prepared version").unwrap();

    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    assert_eq!(prepared.original(), Some(&b"prepared version"[..]));
    prepared.commit(b"replacement").unwrap();
    write_atomic_if_unchanged(&path, Some(b"replacement"), b"second", 1024).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
    assert_eq!(directory_names(&root), ["target.txt"]);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn private_existing_target_is_replaced_and_stays_private() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("private/state.json");
    write_private_atomic(&path, b"first", 1024).unwrap();
    write_private_atomic(&path, b"second", 1024).unwrap();
    write_private_atomic_if_unchanged(&path, Some(b"second"), b"third", 1024).unwrap();

    assert_eq!(read_private_file_bounded(&path, 1024).unwrap(), b"third");
    assert_eq!(directory_names(&root.join("private")), ["state.json"]);
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn compare_and_delete_removes_only_the_expected_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "current").unwrap();

    assert!(matches!(
        remove_regular_file_if_unchanged(&path, b"stale", 1024),
        Err(SecureFileError::Changed)
    ));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "current");
    remove_regular_file_if_unchanged(&path, b"current", 1024).unwrap();
    assert!(!path.exists());

    let private = root.join("private/token.json");
    write_private_atomic(&private, b"token", 1024).unwrap();
    remove_private_file_if_unchanged(&private, b"token", 1024).unwrap();
    assert!(!private.exists());
    assert!(directory_names(&root.join("private")).is_empty());
}

/// A foreign advisory lock on the target (an external tool) delays a
/// conditional write by the bounded wait at most; it never blocks it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn a_foreign_lock_on_the_target_delays_but_never_blocks_a_conditional_write() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().canonicalize().unwrap().join("held.txt");
    std::fs::write(&path, "before").unwrap();
    let holder = std::fs::File::open(&path).unwrap();
    fs2::FileExt::lock_exclusive(&holder).unwrap();

    let started = std::time::Instant::now();
    PreparedMutation::prepare(&path, false, 64)
        .unwrap()
        .commit(b"after")
        .unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_secs(2));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "after");
}

/// Every conditional read-modify-write either commits against the exact
/// bytes it read or reports `Changed`; none is lost or applied twice.
#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn concurrent_conditional_writers_never_lose_an_update() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = std::sync::Arc::new(root.join("counter.txt"));
    std::fs::write(path.as_ref(), "0").unwrap();
    let committed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let writers = (0..4)
        .map(|_| {
            let path = std::sync::Arc::clone(&path);
            let committed = std::sync::Arc::clone(&committed);
            std::thread::spawn(move || {
                for _ in 0..25 {
                    let Ok(prepared) = PreparedMutation::prepare(&path, false, 64) else {
                        continue;
                    };
                    let Some(current) = prepared.original() else {
                        continue;
                    };
                    let value: usize = std::str::from_utf8(current).unwrap().parse().unwrap();
                    let next = (value + 1).to_string();
                    match prepared.commit(next.as_bytes()) {
                        Ok(()) => {
                            committed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        Err(SecureFileError::Changed) => {}
                        // Windows reports a sharing violation when another
                        // writer holds its pin past the retry budget.
                        #[cfg(windows)]
                        Err(SecureFileError::Io(error))
                            if error.raw_os_error()
                                == Some(
                                    windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32,
                                ) => {}
                        Err(error) => panic!("unexpected conditional write failure: {error}"),
                    }
                }
            })
        })
        .collect::<Vec<_>>();
    for writer in writers {
        writer.join().unwrap();
    }

    let committed = committed.load(std::sync::atomic::Ordering::SeqCst);
    assert!(committed > 0);
    assert_eq!(
        std::fs::read_to_string(path.as_ref()).unwrap(),
        committed.to_string()
    );
    assert_eq!(directory_names(&root), ["counter.txt"]);
}

#[cfg(windows)]
#[test]
fn windows_replacement_keeps_the_on_disk_name_and_attributes() {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        SetFileAttributesW, FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_READONLY,
    };

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let actual = root.join("README.md");
    std::fs::write(&actual, "prepared version").unwrap();
    let mut permissions = std::fs::metadata(&actual).unwrap().permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(&actual, permissions).unwrap();
    let wide = actual
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let attributes = std::fs::metadata(&actual).unwrap().file_attributes();
    // SAFETY: `wide` is a NUL-terminated path that outlives the call.
    assert_ne!(
        unsafe { SetFileAttributesW(wide.as_ptr(), attributes | FILE_ATTRIBUTE_HIDDEN) },
        0
    );

    // NTFS resolves the lowercase spelling to the existing entry.
    let prepared = PreparedMutation::prepare(&root.join("readme.md"), false, 1024).unwrap();
    prepared.commit(b"replacement").unwrap();

    assert_eq!(directory_names(&root), ["README.md"]);
    assert_eq!(std::fs::read_to_string(&actual).unwrap(), "replacement");
    let attributes = std::fs::metadata(&actual).unwrap().file_attributes();
    assert_ne!(attributes & FILE_ATTRIBUTE_READONLY, 0);
    assert_ne!(attributes & FILE_ATTRIBUTE_HIDDEN, 0);

    let mut permissions = std::fs::metadata(&actual).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    std::fs::set_permissions(&actual, permissions).unwrap();
}

#[cfg(windows)]
#[test]
fn windows_pinned_target_cannot_be_changed_after_the_final_check() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    let displaced = root.join("prepared-version.txt");
    std::fs::write(&path, "prepared version").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    let cancellation_checks = std::cell::Cell::new(0);

    prepared
        .commit_if(b"replacement", || {
            let check = cancellation_checks.get() + 1;
            cancellation_checks.set(check);
            // The third check occurs after the target is pinned and
            // verified, immediately before it is renamed aside.
            if check == 3 {
                // Rename and delete still fail: the pin denies delete
                // sharing, so the verified name cannot move.
                assert!(std::fs::rename(&path, &displaced).is_err());
                assert!(std::fs::remove_file(&path).is_err());
                // A cooperative writer is no longer locked out: its open
                // succeeds (the residual content race matches Unix, where
                // the staged replacement still wins the name).
                std::fs::write(&path, "competing replacement").unwrap();
                // Readers that share deletion still read through the pin.
                assert_eq!(
                    std::fs::read_to_string(&path).unwrap(),
                    "competing replacement"
                );
            }
            false
        })
        .unwrap();

    assert_eq!(cancellation_checks.get(), 3);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
    assert_eq!(directory_names(&root), ["target.txt"]);
}

#[cfg(windows)]
#[test]
fn windows_target_created_after_displacement_is_not_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "prepared version").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();

    let competitor = path.clone();
    imp::AFTER_DISPLACEMENT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            std::fs::write(competitor, "competing creation").unwrap();
        }));
    });
    assert!(matches!(
        prepared.commit(b"stale replacement"),
        Err(SecureFileError::Changed)
    ));

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "competing creation"
    );
    // The displaced original is kept rather than destroyed, and the
    // staged replacement is removed.
    let names = directory_names(&root);
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[1], "target.txt");
    assert!(names[0].starts_with(".octet-old-"), "{names:?}");
    assert_eq!(
        std::fs::read_to_string(root.join(&names[0])).unwrap(),
        "prepared version"
    );
}

#[cfg(windows)]
#[test]
fn windows_file_held_open_by_another_writer_is_reported_in_use() {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("target.txt");
    std::fs::write(&path, "prepared version").unwrap();
    let prepared = PreparedMutation::prepare(&path, false, 1024).unwrap();
    // Another program holds the file open for writing without sharing
    // write or delete access.
    let holder = std::fs::OpenOptions::new()
        .write(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();

    let error = prepared.commit(b"replacement").unwrap_err();
    assert!(
        matches!(&error, SecureFileError::Io(error)
            if error.raw_os_error()
                == Some(windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION as i32)),
        "{error:?}"
    );
    drop(holder);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "prepared version");
    assert_eq!(directory_names(&root), ["target.txt"]);
    assert!(matches!(
        remove_regular_file_if_unchanged(&path, b"prepared version", 1024),
        Ok(())
    ));
}

#[cfg(windows)]
#[test]
fn windows_publication_stays_bound_to_original_parent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(workspace.join("slot")).unwrap();
    let target = workspace.join("slot/new/victim.txt");
    let prepared = PreparedMutation::prepare(&target, true, 1024).unwrap();

    match std::fs::rename(workspace.join("slot"), workspace.join("original-slot")) {
        Ok(()) => {
            std::fs::create_dir_all(workspace.join("slot/new")).unwrap();
            prepared.commit(b"bound to original parent").unwrap();

            assert!(!target.exists());
            assert_eq!(
                std::fs::read_to_string(workspace.join("original-slot/new/victim.txt")).unwrap(),
                "bound to original parent"
            );
        }
        // NTFS refuses to rename a directory while a descendant is open,
        // so the parent handle held by the prepared mutation already pins
        // the ancestor path and the swap cannot happen.
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            prepared.commit(b"bound to original parent").unwrap();

            assert!(!workspace.join("original-slot").exists());
            assert_eq!(
                std::fs::read_to_string(&target).unwrap(),
                "bound to original parent"
            );
        }
        Err(error) => panic!("unexpected ancestor rename failure: {error}"),
    }
}

#[cfg(windows)]
#[test]
fn windows_private_files_reject_hard_links_and_reparse_points() {
    use std::io::Write as _;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("private/state.json");
    write_private_atomic(&path, b"secret", 1024).unwrap();
    assert_eq!(read_private_file_bounded(&path, 1024).unwrap(), b"secret");

    let lock_path = root.join("private/state.lock");
    let mut lock = open_private_lock_file(&lock_path).unwrap();
    lock.write_all(b"locked").unwrap();
    drop(lock);
    assert_eq!(
        read_private_file_bounded(&lock_path, 1024).unwrap(),
        b"locked"
    );

    let alias = root.join("private/alias.json");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        read_private_file_bounded(&path, 1024),
        Err(SecureFileError::InsecurePrivateObject(_))
    ));
    assert!(matches!(
        write_private_atomic(&path, b"replacement", 1024),
        Err(SecureFileError::InsecurePrivateObject(_))
    ));

    let link = root.join("private/link.json");
    match std::os::windows::fs::symlink_file(&alias, &link) {
        Ok(()) => {
            assert!(read_regular_file_bounded(&link, 1024).is_err());
            assert!(write_private_atomic(&link, b"replacement", 1024).is_err());
            assert_eq!(std::fs::read(&alias).unwrap(), b"secret");
        }
        // Standard accounts without Developer Mode lack the symlink privilege.
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error.raw_os_error()
                    == Some(windows_sys::Win32::Foundation::ERROR_PRIVILEGE_NOT_HELD as i32) => {}
        Err(error) => panic!("could not create test symlink: {error}"),
    }
}

#[cfg(windows)]
#[test]
fn windows_directory_creation_rolls_back_created_descendants_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let existing = root.join("existing");
    std::fs::create_dir(&existing).unwrap();
    let created = existing.join("created");
    let too_long = "x".repeat(512);

    assert!(create_private_directory_all(&created.join(&too_long)).is_err());
    assert!(existing.is_dir());
    assert!(!created.exists());

    let target = created.join(too_long).join("state.json");
    assert!(PreparedMutation::prepare(&target, true, 1024).is_err());
    assert!(!created.exists());
}

#[cfg(unix)]
#[test]
fn parent_symlink_swap_cannot_redirect_a_prepared_mutation() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir_all(workspace.join("slot")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let target = workspace.join("slot/new/victim.txt");
    let prepared = PreparedMutation::prepare(&target, true, 1024).unwrap();

    std::fs::rename(workspace.join("slot"), workspace.join("original-slot")).unwrap();
    symlink(&outside, workspace.join("slot")).unwrap();
    prepared.commit(b"bound to original parent").unwrap();

    assert!(!outside.join("new/victim.txt").exists());
    assert_eq!(
        std::fs::read_to_string(workspace.join("original-slot/new/victim.txt")).unwrap(),
        "bound to original parent"
    );
}

#[cfg(unix)]
#[test]
fn append_open_rejects_parent_replacement_with_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir_all(workspace.join("slot")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let candidate = workspace.join("slot/session.jsonl");
    std::fs::write(&candidate, "inside\n").unwrap();
    std::fs::write(outside.join("session.jsonl"), "outside\n").unwrap();

    std::fs::rename(workspace.join("slot"), workspace.join("original-slot")).unwrap();
    symlink(&outside, workspace.join("slot")).unwrap();

    assert!(open_regular_file_for_append(&candidate).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.join("session.jsonl")).unwrap(),
        "outside\n"
    );
}

#[cfg(unix)]
#[test]
fn create_open_rejects_parent_replacement_with_symlink() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir_all(workspace.join("slot")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let candidate = workspace.join("slot/session.jsonl");

    std::fs::rename(workspace.join("slot"), workspace.join("original-slot")).unwrap();
    symlink(&outside, workspace.join("slot")).unwrap();

    assert!(create_regular_file_for_append(&candidate).is_err());
    assert!(!outside.join("session.jsonl").exists());
}

#[cfg(unix)]
#[test]
fn private_files_require_owner_only_mode_and_one_link() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let path = root.join("private/state.json");
    write_private_atomic(&path, b"secret", 1024).unwrap();
    assert_eq!(read_private_file_bounded(&path, 1024).unwrap(), b"secret");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(matches!(
        read_private_file_bounded(&path, 1024),
        Err(SecureFileError::InsecurePrivateObject(_))
    ));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

    let alias = root.join("private/alias.json");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        read_private_file_bounded(&path, 1024),
        Err(SecureFileError::InsecurePrivateObject(_))
    ));
    assert!(matches!(
        write_private_atomic(&path, b"replacement", 1024),
        Err(SecureFileError::InsecurePrivateObject(_))
    ));
    assert_eq!(std::fs::read(&alias).unwrap(), b"secret");
}

#[cfg(unix)]
#[test]
fn private_atomic_write_rejects_a_symlink_target() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let private = root.join("private");
    let outside = root.join("outside.json");
    let link = private.join("state.json");
    create_private_directory_all(&private).unwrap();
    std::fs::write(&outside, b"outside").unwrap();
    symlink(&outside, &link).unwrap();

    assert!(write_private_atomic(&link, b"replacement", 1024).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    assert!(std::fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn secure_remove_rejects_symlinks_and_removes_only_regular_files() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let target = root.join("target.txt");
    let link = root.join("link.txt");
    std::fs::write(&target, b"target").unwrap();
    symlink(&target, &link).unwrap();

    assert!(remove_regular_file_if_exists(&link).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"target");
    assert!(remove_regular_file_if_exists(&target).unwrap());
    assert!(!target.exists());
    assert!(!remove_regular_file_if_exists(&target).unwrap());
}

#[cfg(unix)]
#[test]
fn private_directory_creation_rolls_back_created_descendants_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let existing = root.join("existing");
    std::fs::create_dir(&existing).unwrap();
    let created = existing.join("created");
    let too_long = "x".repeat(512);

    assert!(create_private_directory_all(&created.join(too_long)).is_err());
    assert!(existing.is_dir());
    assert!(!created.exists());
}

#[cfg(unix)]
#[test]
fn parent_creation_rolls_back_created_descendants_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let existing = root.join("existing");
    std::fs::create_dir(&existing).unwrap();
    let created = existing.join("created");
    let too_long = "x".repeat(512);
    let target = created.join(too_long).join("state.json");

    assert!(PreparedMutation::prepare(&target, true, 1024).is_err());
    assert!(existing.is_dir());
    assert!(!created.exists());
}

#[cfg(unix)]
#[test]
fn private_directory_repairs_only_an_owner_controlled_final_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let private = root.join("one/two");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o755)).unwrap();

    create_private_directory_all(&private).unwrap();
    assert_eq!(
        std::fs::metadata(private).unwrap().permissions().mode() & 0o777,
        0o700
    );
}

#[cfg(unix)]
#[test]
fn bounded_read_rejects_symlink_and_fifo_without_blocking() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let regular = root.join("regular");
    let link = root.join("link");
    let fifo = root.join("fifo");
    std::fs::write(&regular, "secret").unwrap();
    symlink(&regular, &link).unwrap();
    let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo_c` is a valid NUL-terminated path and mode is valid.
    assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);

    assert!(read_regular_file_bounded(&link, 1024).is_err());
    let started = std::time::Instant::now();
    assert!(matches!(
        read_regular_file_bounded(&fifo, 1024),
        Err(SecureFileError::NotRegular)
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}
