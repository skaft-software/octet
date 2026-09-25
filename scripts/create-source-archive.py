#!/usr/bin/env python3
"""Create a deterministic, commit/version-pinned source archive without staging files."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib

MAX_ARCHIVE_BYTES = 512 * 1024 * 1024
REQUIRED_PATHS = {"Cargo.toml", "Cargo.lock", "README.md", "LICENSE"}


def git(repo: Path, *args: str) -> bytes:
    return subprocess.run(
        ["git", "-C", str(repo), *args], check=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
    ).stdout


def create_archive(repo: Path, version: str, source_ref: str, output: Path) -> str:
    """Archive only the resolved commit; refuse version mismatch or an existing output."""
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("version must be a release version")
    commit = git(repo, "rev-parse", "--verify", "--end-of-options", source_ref + "^{commit}").decode().strip()
    manifest = tomllib.loads(git(repo, "show", f"{commit}:Cargo.toml").decode("utf-8"))
    if manifest.get("workspace", {}).get("package", {}).get("version") != version:
        raise ValueError("version does not match the selected commit's workspace package")
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise FileExistsError("output already exists; archives are never overwritten")
    if not output.parent.is_dir():
        raise ValueError("output parent directory must already exist")
    prefix = f"octet-{version}/"
    # Never use the real or a temporary Git index: all model data is already
    # tracked in Octet. Dirty/untracked workstation files cannot enter the tar.
    with tempfile.TemporaryDirectory(prefix=".octet-source-", dir=output.parent) as scratch:
        raw = Path(scratch) / "source.tar"
        packed = Path(scratch) / "source.tar.gz"
        with raw.open("xb") as target:
            subprocess.run(
                ["git", "-C", str(repo), "archive", "--format=tar", f"--prefix={prefix}", commit],
                stdout=target, stderr=subprocess.PIPE, check=True, timeout=60,
            )
        if raw.stat().st_size > MAX_ARCHIVE_BYTES:
            raise ValueError("source archive exceeds the 512 MiB bound")
        found = set()
        with tarfile.open(raw, "r:") as archive:
            for entry in archive:
                if entry.isdir() and entry.name == prefix.rstrip("/"):
                    continue
                if not entry.name.startswith(prefix) or ".." in Path(entry.name).parts:
                    raise ValueError("archive contains a path outside its versioned root")
                relative = entry.name[len(prefix):]
                if entry.isfile():
                    found.add(relative)
                if {"node_modules", "target", ".build", "DerivedData", ".swiftpm"}.intersection(Path(relative).parts):
                    raise ValueError("archive contains dependency or build output")
        if not REQUIRED_PATHS <= found:
            raise ValueError("archive is missing required source files")
        # Empty gzip filename and mtime=0 remove output-path and wall-clock
        # nondeterminism. Git's tar records retain the commit timestamp.
        with packed.open("xb") as target, raw.open("rb") as source:
            os.chmod(packed, 0o600)
            with gzip.GzipFile(filename="", mode="wb", fileobj=target, mtime=0, compresslevel=9) as compressed:
                while chunk := source.read(1024 * 1024):
                    compressed.write(chunk)
            target.flush()
            os.fsync(target.fileno())
        with packed.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        # Same-directory hard-link publication is atomic and exclusive, including
        # against a concurrent creator. No --force mode can replace an artifact.
        os.link(packed, output)
    return digest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    parser.add_argument("--ref", required=True, dest="source_ref")
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--repo", type=Path, default=Path(__file__).resolve().parent.parent)
    args = parser.parse_args()
    try:
        digest = create_archive(args.repo, args.version, args.source_ref, args.out)
    except (ValueError, OSError, subprocess.SubprocessError) as error:
        # Git stderr and file contents are not echoed into release receipts.
        parser.exit(1, f"source archive refused: {type(error).__name__}\n")
    print(f"{digest}  {args.out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
