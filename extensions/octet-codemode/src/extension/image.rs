//! Retain the host-admitted executable before initialize replies. Octet stages
//! executable entrypoints only through initialization, then removes that path.
//! Guest processes must use this private lifetime-owned copy, never a mutable
//! bundle path, PATH lookup, or a subsequently missing current_exe().
use anyhow::{ensure, Context, Result};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub(super) struct LaunchImage {
    pub executable: PathBuf,
    _directory: TempDir,
}
impl LaunchImage {
    pub fn pin_current(scratch: Option<&Path>) -> Result<Self> {
        Self::pin(&std::env::current_exe()?, scratch)
            .context("Could not preserve the admitted Codemode launch image")
    }
    fn pin(path: &Path, scratch: Option<&Path>) -> Result<Self> {
        let before = fs::symlink_metadata(path)?;
        ensure!(
            before.is_file(),
            "Launch image must be a regular non-symlink file"
        );
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let opened = source.metadata()?;
        ensure!(
            opened.is_file() && opened.dev() == before.dev() && opened.ino() == before.ino(),
            "Launch image identity changed while opening"
        );
        ensure!(
            (1..=MAX_IMAGE_BYTES).contains(&opened.len()),
            "Launch image exceeds the 64 MiB bound or is empty"
        );
        let mut builder = tempfile::Builder::new();
        builder.prefix("codemode-runner-");
        let directory = if let Some(scratch) = scratch {
            ensure!(
                scratch.is_absolute(),
                "OCTET_EXTENSION_SCRATCH must be an absolute host-owned directory"
            );
            builder.tempdir_in(scratch)?
        } else {
            builder.tempdir()?
        };
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700))?;
        let executable = directory.path().join("runner");
        let mut destination = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&executable)?;
        let copied = std::io::copy(
            &mut Read::by_ref(&mut source).take(MAX_IMAGE_BYTES + 1),
            &mut destination,
        )?;
        let after = source.metadata()?;
        ensure!(
            copied == opened.len()
                && copied <= MAX_IMAGE_BYTES
                && after.len() == opened.len()
                && after.mtime() == opened.mtime()
                && after.mtime_nsec() == opened.mtime_nsec()
                && after.ctime() == opened.ctime()
                && after.ctime_nsec() == opened.ctime_nsec(),
            "Launch image changed while copying"
        );
        destination.flush()?;
        destination.set_permissions(fs::Permissions::from_mode(0o500))?;
        // Executable files must not remain open for writing when spawned.
        drop(destination);
        Ok(Self {
            executable,
            _directory: directory,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pin_survives_source_removal_and_replacement_and_cleans_up() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("admitted");
        fs::write(&source, b"admitted executable image").unwrap();
        let image = LaunchImage::pin(&source, Some(root.path())).unwrap();
        fs::remove_file(&source).unwrap();
        fs::write(&source, b"changed bundle image").unwrap();
        assert_eq!(
            fs::read(&image.executable).unwrap(),
            b"admitted executable image"
        );
        assert_eq!(
            fs::metadata(&image.executable)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
        let directory = image.executable.parent().unwrap().to_owned();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(image);
        assert!(!directory.exists());
    }
    #[test]
    fn pin_rejects_symlinks_and_oversized_images() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("admitted");
        fs::write(&source, b"image").unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(LaunchImage::pin(&link, Some(root.path())).is_err());
        OpenOptions::new()
            .write(true)
            .open(&source)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        assert!(LaunchImage::pin(&source, Some(root.path())).is_err());
    }
}
