#!/usr/bin/env python3
"""Build a deterministic Windows ZIP from native PE files and tracked public assets."""
from __future__ import annotations

import argparse
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys
import zipfile

TARGET = "x86_64-pc-windows-msvc"
VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?")
FORBIDDEN = set('<>:\"|?*')
DEVICES = {"CON", "PRN", "AUX", "NUL", *(f"COM{i}" for i in range(1, 10)), *(f"LPT{i}" for i in range(1, 10))}


def tracked_files(source: Path) -> set[str]:
    result = subprocess.run(
        ["git", "-C", str(source), "ls-files", "-z"],
        check=True, capture_output=True,
    )
    return {os.fsdecode(item) for item in result.stdout.split(b"\0") if item}


def package(release: Path, output: Path, version: str, source: Path) -> Path:
    if VERSION.fullmatch(version) is None:
        raise ValueError("version must be a canonical release version")
    if source.is_symlink() or not source.is_dir():
        raise ValueError("source must be a real checkout directory")
    if release.is_symlink() or not release.is_dir():
        raise ValueError("release binaries must be in a real directory")
    tracked = tracked_files(source)
    inventory = source / "docs/package-assets.txt"
    if "docs/package-assets.txt" not in tracked or inventory.is_symlink() or not inventory.is_file():
        raise ValueError("tracked documentation inventory is missing")
    files = {"LICENSE", "README.md"}
    inventory_files: set[str] = set()
    for line in inventory.read_text(encoding="utf-8").splitlines():
        if not line or line.startswith("#"):
            continue
        kind, separator, relative = line.partition(" ")
        parts = PurePosixPath(relative).parts
        if (kind not in {"text", "asset"} or not separator or not relative
                or "\\" in relative or any(part in {"", ".", ".."} for part in parts)
                or relative in inventory_files or relative not in tracked):
            raise ValueError(f"unsafe, repeated, or untracked public asset: {relative}")
        inventory_files.add(relative)
        files.add(relative)
    artifact = f"octet-{version}-{TARGET}"
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"{artifact}.zip"
    if archive.exists() or archive.is_symlink():
        raise ValueError(f"refusing to overwrite {archive}")
    binaries = {}
    for name in ("octet.exe", "octet-host.exe"):
        path = release / name
        if path.is_symlink() or not path.is_file() or path.stat().st_size == 0:
            raise ValueError(f"Windows executable is missing or invalid: {path}")
        with path.open("rb") as stream:
            if stream.read(2) != b"MZ":
                raise ValueError(f"Windows executable is not a PE file: {path}")
        binaries[name] = path
    entries: dict[str, Path] = {**binaries}
    for relative in sorted(files):
        path = source / relative
        current = source
        for component in PurePosixPath(relative).parts:
            current = current / component
            metadata = current.lstat()
            if stat.S_ISLNK(metadata.st_mode):
                raise ValueError(f"public asset traverses a symlink: {relative}")
        if not stat.S_ISREG(path.lstat().st_mode):
            raise ValueError(f"public asset is not a regular file: {relative}")
        parts = PurePosixPath(relative).parts
        for part in parts:
            stem = part.rstrip(" .").split(".", 1)[0].upper()
            if any(char in FORBIDDEN for char in part) or part.endswith((" ", ".")) or stem in DEVICES:
                raise ValueError(f"public asset has a Windows-incompatible path: {relative}")
        entries[relative] = path
    normalized = [name.casefold() for name in entries]
    if len(normalized) != len(set(normalized)):
        raise ValueError("Windows ZIP paths collide case-insensitively")
    try:
        with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as zipped:
            for relative, path in sorted(entries.items()):
                info = zipfile.ZipInfo(f"{artifact}/{relative}", date_time=(1980, 1, 1, 0, 0, 0))
                executable = relative in binaries
                info.external_attr = ((stat.S_IFREG | (0o755 if executable else 0o644)) << 16)
                info.compress_type = zipfile.ZIP_DEFLATED
                with path.open("rb") as contents:
                    zipped.writestr(info, contents.read())
    except BaseException:
        archive.unlink(missing_ok=True)
        raise
    return archive


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release_directory", type=Path)
    parser.add_argument("output_directory", type=Path)
    parser.add_argument("version")
    parser.add_argument("source_directory", type=Path)
    args = parser.parse_args()
    try:
        result = package(args.release_directory, args.output_directory, args.version, args.source_directory)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"Windows ZIP packaging failed: {error}", file=sys.stderr)
        return 1
    print(f"created {result}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
