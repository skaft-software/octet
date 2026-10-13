"""Provision Cua's signed macOS host inside octet-owned state.

The wheel has no app bundle. macOS's default cursor runtime therefore also
needs the version-matched upstream app archive. Setup never replaces a global
application, runs an installer script, or grants an OS permission.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import tarfile
import tempfile
import time
import urllib.request
from typing import Any, Callable, Optional

from octet_computer_use import driver

RELEASE_ROOT = "https://github.com/trycua/cua/releases/download"
MAX_ARCHIVE_BYTES = 256 * 1024 * 1024
MAX_APP_BYTES = 256 * 1024 * 1024
DOWNLOAD_TIMEOUT_SECONDS = 180
# Require Cua's Developer ID, not merely any valid or ad-hoc signature.
SIGNING_REQUIREMENT = (
    'anchor apple generic and identifier "com.trycua.driver" '
    'and certificate leaf[subject.OU] = "YCK386LBJ7"'
)


def _download(url: str, destination: Path, limit: int) -> str:
    driver._check_cancelled()
    digest = hashlib.sha256()
    size = 0
    deadline = time.monotonic() + DOWNLOAD_TIMEOUT_SECONDS
    with urllib.request.urlopen(url, timeout=20) as response, destination.open("xb") as output:
        destination.chmod(0o600)
        while True:
            driver._check_cancelled()
            if time.monotonic() >= deadline:
                raise driver.ProvisionError("signed Cua app download timed out")
            chunk = response.read(65536)
            if not chunk:
                break
            size += len(chunk)
            if size > limit:
                raise driver.ProvisionError("signed Cua app download exceeds its size limit")
            digest.update(chunk)
            output.write(chunk)
    driver._check_cancelled()
    return digest.hexdigest()


def _extract_app(archive: Path, destination: Path, version: str) -> Path:
    prefix = f"cua-driver-rs-{version}-darwin-universal"
    total = 0
    count = 0
    with tarfile.open(archive, "r|gz") as source:
        for member in source:
            driver._check_cancelled()
            count += 1
            if count > 4096:
                raise driver.ProvisionError("signed Cua app archive has too many members")
            relative = PurePosixPath(member.name)
            if relative.is_absolute() or ".." in relative.parts or "\\" in member.name:
                raise driver.ProvisionError("unsafe path in signed Cua app archive")
            if relative.parts[:2] != (prefix, "CuaDriver.app"):
                continue
            target = destination.joinpath(*relative.parts[1:])
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                total += member.size
                if total > MAX_APP_BYTES:
                    raise driver.ProvisionError("signed Cua app exceeds its expanded size limit")
                target.parent.mkdir(parents=True, exist_ok=True)
                with source.extractfile(member) as incoming, target.open("xb") as outgoing:
                    shutil.copyfileobj(incoming, outgoing, 65536)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            else:
                raise driver.ProvisionError("unsupported link or special file in signed Cua app")
    app = destination / "CuaDriver.app"
    if not (app / "Contents/MacOS/cua-driver").is_file():
        raise driver.ProvisionError("Cua release archive did not contain its desktop app")
    return app


def _verify(app: Path) -> None:
    result = driver._run([
        "/usr/bin/codesign", "--verify", "--deep", "--strict",
        "-R=" + SIGNING_REQUIREMENT, str(app),
    ])
    if result.returncode != 0:
        raise driver.ProvisionError("Cua desktop app failed Developer ID signature verification")


def provision(
    paths: driver.DriverPaths,
    binary: Path,
    *,
    progress: Optional[Callable[[str], None]] = None,
) -> Optional[Path]:
    """Complete the default macOS runtime; direct/other-platform installs are inert."""

    if not driver.cursor_host_required():
        return None
    report = progress or (lambda _message: None)
    existing = driver.desktop_app()
    host_binary = driver.desktop_app_binary(existing) if existing is not None else None
    version = driver.driver_version(binary)
    if not version or not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise driver.ProvisionError("signed Cua app setup requires a stable driver release version")
    if host_binary is not None:
        # Verify before executing even the version probe. Explicit developer
        # hosts remain an opt-in; do not rewrite or require a Cua signature.
        if not os.environ.get("OCTET_CUA_DESKTOP_APP"):
            _verify(existing)
        if driver.driver_version(host_binary) == version:
            report(f"Cua desktop host {version} is already installed")
            return existing
    if os.environ.get("OCTET_CUA_DESKTOP_APP"):
        raise driver.ProvisionError(
            "the explicitly selected Cua desktop app is missing or does not match the driver; "
            "repair OCTET_CUA_DESKTOP_APP before setup"
        )

    driver._ensure_directories(paths)
    directory = paths.root / "desktop"
    driver._reject_link(directory)
    directory.mkdir(mode=0o700, exist_ok=True)
    destination = directory / "CuaDriver.app"
    if destination.is_symlink():
        raise driver.ProvisionError("Cua desktop host destination must not be a symlink")
    release = f"{RELEASE_ROOT}/cua-driver-rs-v{version}"
    archive_name = f"cua-driver-rs-{version}-darwin-universal.tar.gz"
    report(f"Downloading Cua's signed desktop host {version}…")
    with tempfile.TemporaryDirectory(prefix=".host-setup-", dir=directory) as temporary:
        staging = Path(temporary)
        manifest_path = staging / "release-manifest.json"
        _download(release + "/release-manifest.json", manifest_path, 1024 * 1024)
        try:
            manifest: Any = json.loads(manifest_path.read_text(encoding="utf-8"))
            asset = next(asset for asset in manifest["assets"] if asset["name"] == archive_name)
            digest = asset["sha256"]
            size = asset["bytes"]
            if (not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest)
                    or type(size) is not int or not 0 < size <= MAX_ARCHIVE_BYTES):
                raise ValueError("invalid archive metadata")
        except (KeyError, TypeError, ValueError, StopIteration) as error:
            raise driver.ProvisionError("Cua release manifest does not describe its signed app") from error
        archive = staging / "host.tar.gz"
        if (_download(release + "/" + archive_name, archive, size) != digest
                or archive.stat().st_size != size):
            raise driver.ProvisionError("signed Cua app archive checksum mismatch")
        report("Verifying the signed Cua desktop host…")
        try:
            app = _extract_app(archive, staging, version)
        except tarfile.TarError as error:
            raise driver.ProvisionError("invalid signed Cua app archive") from error
        _verify(app)
        if driver.driver_version(app / "Contents/MacOS/cua-driver") != version:
            raise driver.ProvisionError("signed Cua app does not match the provisioned driver version")
        # Replace only octet-owned state, retaining the old app until validation
        # succeeds and rolling its rename back if the final handoff fails.
        driver._check_cancelled()
        previous = staging / "previous.app"
        if destination.exists():
            destination.rename(previous)
        try:
            app.rename(destination)
        except OSError:
            if previous.exists():
                previous.rename(destination)
            raise
    report(f"Installed the signed Cua desktop host at {destination}")
    return destination
