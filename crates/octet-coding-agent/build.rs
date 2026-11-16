#![allow(missing_docs)]

use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Builder, Header};

// A finite public inventory works in both Git checkouts and source archives.
// Do not walk workstation directories or filter explicit source-text entries by suffix.
fn documentation_assets(source_root: &Path) -> io::Result<Vec<PathBuf>> {
    let manifest = source_root.join("docs/package-assets.txt");
    println!("cargo:rerun-if-changed={}", manifest.display());
    let mut assets = Vec::new();
    let mut seen = HashSet::new();
    for line in fs::read_to_string(manifest)?.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((kind, name)) = line.split_once(' ') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid documentation inventory",
            ));
        };
        let path = PathBuf::from(name);
        if !matches!(kind, "text" | "asset")
            || name.is_empty()
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
            || !seen.insert(path.clone())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe or duplicate documentation inventory path",
            ));
        }
        if kind == "text" {
            assets.push(path);
        }
    }
    Ok(assets)
}

fn append_documentation_file(
    builder: &mut Builder<GzEncoder<File>>,
    source_root: &Path,
    relative: &Path,
) -> io::Result<()> {
    let mut source = source_root.to_owned();
    for component in relative.components() {
        source.push(component);
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.file_type().is_symlink()
            || (source != source_root.join(relative) && !metadata.is_dir())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "documentation path traverses a link or non-directory",
            ));
        }
    }
    let metadata = fs::symlink_metadata(&source)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "documentation asset is not a regular file",
        ));
    }
    // Explicit text entries must actually be UTF-8, never binary SDK artifacts.
    let contents = fs::read_to_string(&source)?;
    let mut header = Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(contents.len() as u64);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    builder.append_data(&mut header, relative, contents.as_bytes())
}

/// Windows reserves only 1 MiB for a program's main thread, where Linux and
/// macOS reserve 8 MiB. Both binaries run their Tokio entry future on that
/// thread, and native interactive startup overflowed it. Reserve the same
/// 8 MiB on Windows; the reservation is address space committed on demand.
fn reserve_windows_main_thread_stack() {
    const MAIN_THREAD_STACK_BYTES: u32 = 8 * 1024 * 1024;
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/STACK:{MAIN_THREAD_STACK_BYTES}");
    } else {
        println!("cargo:rustc-link-arg-bins=-Wl,--stack,{MAIN_THREAD_STACK_BYTES}");
    }
}

/// These executables have no in-process plugin ABI. On Darwin, exporting
/// every linked Rust/C symbol makes otherwise unreachable sections dead-strip
/// roots and leaves a large export trie for dyld. Keep the SDK/library's symbol
/// policy untouched; extensions communicate with a process, not `dlsym`.
fn optimize_macos_executable_linking() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-arg-bins=-Wl,-no_exported_symbols");
        // arm64 already requires macOS 11, where chained fixups are supported.
        // Let dyld/the kernel fix pages on demand instead of eagerly walking
        // the legacy rebase stream. Do not raise the older x86_64 minimum.
        if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64") {
            println!("cargo:rustc-link-arg-bins=-Wl,-fixup_chains");
        }
    }
}

fn main() -> io::Result<()> {
    reserve_windows_main_thread_stack();
    optimize_macos_executable_linking();
    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("Cargo must provide CARGO_MANIFEST_DIR to the build script"),
    );
    let source_root = manifest_dir.join("../..");
    let out_dir = PathBuf::from(
        std::env::var_os("OUT_DIR").expect("Cargo must provide OUT_DIR to the build script"),
    );
    let archive_path = out_dir.join("octet-documentation.tar.gz");

    let canonical_assets_available = ["README.md", "docs", "examples", "sdk"]
        .iter()
        .all(|path| source_root.join(path).exists());
    let (asset_root, assets) = if canonical_assets_available {
        (
            source_root,
            documentation_assets(&manifest_dir.join("../.."))?,
        )
    } else {
        // A crates.io package cannot contain files outside its package root.
        println!(
            "cargo:warning=octet documentation sources are unavailable; packaged binaries will use the published documentation URL"
        );
        (manifest_dir.clone(), vec![PathBuf::from("README.md")])
    };
    let mut inventory = String::from("const EMBEDDED_DOCUMENTATION_FILES: &[&str] = &[\n");
    for relative in &assets {
        println!(
            "cargo:rerun-if-changed={}",
            asset_root.join(relative).display()
        );
        writeln!(
            inventory,
            "    {:?},",
            relative.to_str().expect("inventory is ASCII")
        )
        .unwrap();
    }
    inventory.push_str("];\n");
    fs::write(out_dir.join("octet-documentation-files.rs"), inventory)?;

    let output = File::create(&archive_path)?;
    let encoder = GzEncoder::new(output, Compression::best());
    let mut builder = Builder::new(encoder);
    for relative in assets {
        append_documentation_file(&mut builder, &asset_root, &relative)?;
    }
    let encoder = builder.into_inner()?;
    encoder.finish()?.sync_all()?;

    println!(
        "cargo:rustc-env=OCTET_EMBEDDED_DOCS_ARCHIVE={}",
        archive_path.display()
    );
    Ok(())
}
