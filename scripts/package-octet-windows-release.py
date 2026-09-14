#!/usr/bin/env python3
"""Package an explicitly probed native Windows x64 release candidate.

The Windows target is deliberately separate from the Unix shell packager.  A
cross-built PE file must not be executed on the packaging host; a probe record
created by a native Windows job binds the observed version/hello responses and
both executable hashes to the bytes copied into the deterministic archive.
"""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from typing import Any, Iterable, Mapping, Sequence

try:
    from octet_release_identity import release_repository
except ModuleNotFoundError:  # pragma: no cover - supports direct module loading.
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from octet_release_identity import release_repository


WINDOWS_TARGET = "x86_64-pc-windows-gnu"
PROBE_SCHEMA = "octet.windows.build-probe.v1"
BINARY_NAMES = ("octet.exe", "octet-host.exe")
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
VERSION_PATTERN = re.compile(
    r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
)
DIGEST_PATTERN = re.compile(r"[0-9a-f]{64}")
INVENTORY_NAME_PATTERN = re.compile(r"[A-Za-z0-9_./-]+")
MAX_PROBE_BYTES = 1024 * 1024
MAX_ARCHIVE_BYTES = 128 * 1024 * 1024
MAX_EXPANDED_BYTES = 160 * 1024 * 1024
MAX_MEMBER_BYTES = 64 * 1024 * 1024
MAX_ENTRIES = 4096
WINDOWS_FORBIDDEN = set('<>:"|?*')
WINDOWS_DEVICES = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{number}" for number in range(1, 10)),
    *(f"LPT{number}" for number in range(1, 10)),
}


class ReleaseError(Exception):
    """A candidate packaging or identity validation failure."""


def fail(message: str) -> None:
    raise ReleaseError(message)


def regular_file(path: Path, label: str) -> os.stat_result:
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"{label} is not readable: {path}: {error}")
    if not stat.S_ISREG(metadata.st_mode):
        fail(f"{label} must be a regular, non-symlink file: {path}")
    return metadata


def real_directory(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as error:
        fail(f"{label} is not readable: {path}: {error}")
    if not stat.S_ISDIR(metadata.st_mode):
        fail(f"{label} must be a real directory: {path}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def unique_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"probe JSON repeats the field: {key}")
        result[key] = value
    return result


def read_probe(path: Path) -> Mapping[str, Any]:
    metadata = regular_file(path, "Windows probe identity")
    if metadata.st_size > MAX_PROBE_BYTES:
        fail(f"Windows probe identity exceeds {MAX_PROBE_BYTES} bytes: {path}")
    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=unique_object
        )
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"Windows probe identity is not valid UTF-8 JSON: {path}: {error}")
    if not isinstance(value, dict):
        fail("Windows probe identity must be a JSON object")
    return value


def string_field(value: Mapping[str, Any], name: str) -> str:
    field = value.get(name)
    if not isinstance(field, str) or not field:
        fail(f"Windows probe identity field is malformed: {name}")
    return field


def integer_field(value: Mapping[str, Any], name: str, expected: int) -> None:
    field = value.get(name)
    if isinstance(field, bool) or not isinstance(field, int) or field != expected:
        fail(f"Windows probe identity field is malformed: {name}")


def validate_hello(hello: Any, version: str) -> None:
    if not isinstance(hello, dict):
        fail("Windows host probe response must be a JSON object")
    integer_field(hello, "protocol_version", 1)
    if hello.get("request_id") != "release-probe":
        fail("Windows host probe response has the wrong request ID")
    integer_field(hello, "seq", 1)
    if hello.get("type") != "hello":
        fail("Windows host probe response is not a hello event")
    data = hello.get("data")
    if not isinstance(data, dict):
        fail("Windows host probe response has no data object")
    integer_field(data, "protocol_version", 1)
    if data.get("sdk_version") != version:
        fail("Windows host probe SDK version does not match the release")


def validate_probe(value: Mapping[str, Any], version: str, target: str) -> None:
    expected_fields = {
        "schema",
        "target",
        "version",
        "repository",
        "source_commit",
        "workflow_commit",
        "workflow_ref",
        "observed_platform",
        "binaries",
    }
    if set(value) != expected_fields:
        fail("Windows probe identity has unexpected or missing fields")
    if value.get("schema") != PROBE_SCHEMA:
        fail("Windows probe identity has an unsupported schema")
    if value.get("target") != target or target != WINDOWS_TARGET:
        fail("Windows probe identity target is not the explicit Windows x64 target")
    if value.get("version") != version or VERSION_PATTERN.fullmatch(version) is None:
        fail("Windows probe identity version does not match the release")
    if value.get("observed_platform") != "windows-x86_64":
        fail("Windows probe identity was not recorded on native Windows x64")

    source_commit = string_field(value, "source_commit")
    workflow_commit = string_field(value, "workflow_commit")
    if COMMIT_PATTERN.fullmatch(source_commit) is None:
        fail("Windows probe source commit is malformed")
    if COMMIT_PATTERN.fullmatch(workflow_commit) is None:
        fail("Windows probe workflow commit is malformed")
    repository = string_field(value, "repository")
    if repository != release_repository(version, source_commit, workflow_commit):
        fail(f"Windows probe repository is not the canonical release identity: {repository}")
    expected_workflow_ref = (
        f"{repository}/.github/workflows/release-octet.yml@refs/tags/"
        f"octet-binaries-v{version}"
    )
    if value.get("workflow_ref") != expected_workflow_ref:
        fail("Windows probe workflow ref is not the immutable binary release workflow tag")

    binaries = value.get("binaries")
    if not isinstance(binaries, dict) or set(binaries) != set(BINARY_NAMES):
        fail("Windows probe identity must describe exactly octet.exe and octet-host.exe")
    octet = binaries["octet.exe"]
    if not isinstance(octet, dict) or set(octet) != {"sha256", "version_stdout"}:
        fail("Windows octet probe identity is malformed")
    if not isinstance(octet["sha256"], str) or DIGEST_PATTERN.fullmatch(octet["sha256"]) is None:
        fail("Windows octet probe digest is malformed")
    if not isinstance(octet["version_stdout"], str):
        fail("Windows octet version probe output is malformed")
    version_lines = octet["version_stdout"].splitlines()
    if version_lines != [f"octet {version}"]:
        fail("Windows octet version probe did not produce the matching one-line result")

    host = binaries["octet-host.exe"]
    if not isinstance(host, dict) or set(host) != {"sha256", "hello_stdout"}:
        fail("Windows host probe identity is malformed")
    if not isinstance(host["sha256"], str) or DIGEST_PATTERN.fullmatch(host["sha256"]) is None:
        fail("Windows host probe digest is malformed")
    if not isinstance(host["hello_stdout"], str):
        fail("Windows host probe output is malformed")
    hello_lines = host["hello_stdout"].splitlines()
    if len(hello_lines) != 1:
        fail("Windows host probe did not produce exactly one output frame")
    try:
        hello = json.loads(hello_lines[0], object_pairs_hook=unique_object)
    except (json.JSONDecodeError, TypeError) as error:
        fail(f"Windows host probe output is not valid JSON: {error}")
    validate_hello(hello, version)


def validate_binaries(source: Path, probe: Mapping[str, Any]) -> None:
    binaries = probe["binaries"]
    release_directory = source / "target" / WINDOWS_TARGET / "release"
    for directory in (
        source / "target",
        source / "target" / WINDOWS_TARGET,
        release_directory,
    ):
        real_directory(directory, "Windows release binary directory")
    for name in BINARY_NAMES:
        path = release_directory / name
        metadata = regular_file(path, f"Windows release binary {name}")
        if metadata.st_size == 0:
            fail(f"Windows release binary is empty: {path}")
        # This is a format/name boundary, not an execution test.  A PE image is
        # expected for a Windows target even when the packager runs elsewhere.
        with path.open("rb") as stream:
            if stream.read(2) != b"MZ":
                fail(f"Windows release binary is not a PE image: {path}")
        expected = binaries[name]["sha256"]
        actual = sha256_file(path)
        if actual != expected:
            fail(f"Windows probe digest disagrees with the packaged binary: {name}")


def git_state(source: Path) -> tuple[set[str], str]:
    real_directory(source, "release source")
    try:
        inside = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "--is-inside-work-tree"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        if inside != "true":
            fail(f"release source must be a Git checkout: {source}")
        subprocess.run(
            ["git", "-C", str(source), "diff-index", "--quiet", "HEAD", "--"],
            check=True,
            capture_output=True,
        )
        listing = subprocess.run(
            ["git", "-C", str(source), "ls-files", "-z"],
            check=True,
            capture_output=True,
        ).stdout
        head = subprocess.run(
            ["git", "-C", str(source), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
    except FileNotFoundError as error:
        fail(f"required release command is unavailable: git: {error}")
    except subprocess.CalledProcessError as error:
        if error.cmd[3:5] == ["diff-index", "--quiet"]:
            fail("release source has tracked changes; package an immutable clean commit")
        fail(f"release source Git validation failed: {source}")
    try:
        tracked = {
            item.decode("utf-8")
            for item in listing.split(b"\0")
            if item
        }
    except UnicodeDecodeError as error:
        fail(f"release source contains a non-UTF-8 tracked path: {error}")
    if COMMIT_PATTERN.fullmatch(head) is None:
        fail("release source HEAD is not a full commit identity")
    return tracked, head


def source_path_for(source: Path, name: str) -> Path:
    relative = PurePosixPath(name)
    components = name.split("/")
    if (
        not name
        or name.startswith("/")
        or "\\" in name
        or not INVENTORY_NAME_PATTERN.fullmatch(name)
        or any(part in {"", ".", ".."} for part in components)
    ):
        fail(f"unsafe documentation asset path: {name}")
    for component in components:
        if (
            component.endswith((".", " "))
            or len(component.encode("utf-8")) > 255
            or any(ord(character) < 32 or ord(character) == 127 for character in component)
            or any(character in WINDOWS_FORBIDDEN for character in component)
            or component.split(".", 1)[0].upper() in WINDOWS_DEVICES
        ):
            fail(f"documentation asset is not portable to Windows: {name}")
    current = source
    for index, component in enumerate(relative.parts):
        current = current / component
        metadata = current.lstat()
        if stat.S_ISLNK(metadata.st_mode) or (
            index < len(relative.parts) - 1 and not stat.S_ISDIR(metadata.st_mode)
        ):
            fail(f"documentation asset traverses a link or non-directory: {name}")
    return current


def copy_documentation(
    source: Path,
    package: Path,
    tracked: Iterable[str],
) -> set[str]:
    tracked_set = set(tracked)
    inventory_name = "docs/package-assets.txt"
    inventory = source / inventory_name
    source_path_for(source, inventory_name)
    regular_file(inventory, "documentation inventory")
    if inventory_name not in tracked_set:
        fail("documentation inventory must be tracked")
    copied: set[str] = set()
    portable_copied: set[tuple[str, ...]] = set()
    portable_reserved = {(name.casefold(),) for name in BINARY_NAMES}
    try:
        lines = inventory.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeDecodeError) as error:
        fail(f"documentation inventory is not valid UTF-8: {error}")
    for line in lines:
        if not line or line.startswith("#"):
            continue
        kind, separator, name = line.partition(" ")
        components = name.split("/")
        portable_name = tuple(component.casefold() for component in components)
        if (
            kind not in {"text", "asset"}
            or not separator
            or name in copied
            or portable_name in portable_copied
            or portable_name in portable_reserved
            or name not in tracked_set
        ):
            fail(f"unsafe, duplicate, or untracked documentation asset: {name}")
        source_path = source_path_for(source, name)
        metadata = regular_file(source_path, "documentation asset")
        if kind == "text":
            try:
                source_path.read_text(encoding="utf-8")
            except (OSError, UnicodeDecodeError) as error:
                fail(f"text documentation asset is not UTF-8: {name}: {error}")
        elif not name.startswith("docs/"):
            fail(f"non-text documentation assets must be under docs/: {name}")
        destination = package / Path(*PurePosixPath(name).parts)
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source_path, destination)
        destination.chmod(0o755 if metadata.st_mode & 0o111 else 0o644)
        copied.add(name)
        portable_copied.add(portable_name)

    public_paths = {
        name
        for name in tracked_set
        if name.startswith(("docs/", "examples/", "sdk/"))
    }
    if public_paths - copied:
        fail(
            "public documentation is missing from docs/package-assets.txt: "
            + ", ".join(sorted(public_paths - copied))
        )
    for required_file in (
        "LICENSE",
        "README.md",
        "SECURITY.md",
        "CONTRIBUTING.md",
        "CHANGELOG.md",
        "THIRD_PARTY_NOTICES.md",
    ):
        if required_file not in copied:
            fail(f"tracked {required_file} is missing from release assets")
    for required_root in ("docs", "examples", "sdk"):
        if not any(path.startswith(required_root + "/") for path in copied):
            fail(f"tracked {required_root}/ assets are missing from the release package")
    return copied


def validate_archive(archive: Path, artifact_name: str, package: Path) -> None:
    expected = [
        artifact_name,
        *[
            f"{artifact_name}/{path.relative_to(package).as_posix()}"
            for path in [
                package,
                *sorted(
                    package.rglob("*"),
                    key=lambda item: item.relative_to(package).as_posix(),
                ),
            ][1:]
        ],
    ]
    try:
        with tarfile.open(archive, mode="r:gz") as packaged:
            members = packaged.getmembers()
            names = [member.name.rstrip("/") for member in members]
            if names != expected:
                fail(f"Windows release archive has an unexpected layout: {names!r}")
            for member in members:
                if not (member.isdir() or member.isreg()) or member.issym() or member.islnk():
                    fail("Windows release archive contains a link or special file")
                if "\\" in member.name or member.name.startswith("/"):
                    fail("Windows release archive contains an unsafe path")
                logical_name = member.name[:-1] if member.name.endswith("/") else member.name
                if member.name.endswith("/") and not member.isdir():
                    fail("Windows release archive contains an unsafe path")
                member_components = logical_name.split("/")
                parts = PurePosixPath(logical_name).parts
                if (
                    not logical_name
                    or any(part in {"", ".", ".."} for part in member_components)
                    or len(parts) != len(member_components)
                ):
                    fail("Windows release archive contains an unsafe path")
                if logical_name in {
                    f"{artifact_name}/octet.exe",
                    f"{artifact_name}/octet-host.exe",
                }:
                    if not member.isfile() or member.mode != 0o755:
                        fail("Windows release archive executable mode is not deterministic")
    except (OSError, tarfile.TarError) as error:
        fail(f"Windows release archive cannot be read: {archive}: {error}")


def create_archive(package: Path, archive: Path, artifact_name: str, epoch: int) -> str:
    paths = [
        package,
        *sorted(
            package.rglob("*"),
            key=lambda item: item.relative_to(package).as_posix(),
        ),
    ]
    if len(paths) > MAX_ENTRIES:
        fail(f"Windows release package has too many entries: {len(paths)}")
    expanded = 0
    for path in paths:
        metadata = path.lstat()
        if stat.S_ISREG(metadata.st_mode):
            if metadata.st_size > MAX_MEMBER_BYTES:
                fail(f"Windows release package member exceeds {MAX_MEMBER_BYTES} bytes: {path}")
            expanded += metadata.st_size
    if expanded > MAX_EXPANDED_BYTES:
        fail(f"Windows release package exceeds {MAX_EXPANDED_BYTES} expanded bytes")
    opened = False
    completed = False
    try:
        with archive.open("xb") as raw:
            opened = True
            with gzip.GzipFile(
                filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=epoch
            ) as compressed:
                with tarfile.open(
                    fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT
                ) as output:
                    for path in paths:
                        metadata = path.lstat()
                        if not (stat.S_ISDIR(metadata.st_mode) or stat.S_ISREG(metadata.st_mode)):
                            fail(f"Windows release archive cannot contain links or special files: {path}")
                        relative = path.relative_to(package)
                        name = (
                            artifact_name
                            if not relative.parts
                            else f"{artifact_name}/{relative.as_posix()}"
                        )
                        info = output.gettarinfo(str(path), arcname=name)
                        info.uid = 0
                        info.gid = 0
                        info.uname = ""
                        info.gname = ""
                        info.mtime = epoch
                        info.mode = (
                            0o755
                            if info.isdir() or path.name in BINARY_NAMES
                            else 0o644
                        )
                        if info.isfile():
                            with path.open("rb") as contents:
                                output.addfile(info, contents)
                        else:
                            output.addfile(info)
        if archive.stat().st_size > MAX_ARCHIVE_BYTES:
            fail(f"Windows release archive exceeds {MAX_ARCHIVE_BYTES} bytes: {archive}")
        validate_archive(archive, artifact_name, package)
        digest = sha256_file(archive)
        completed = True
        return digest
    except FileExistsError:
        fail(f"Windows release archive already exists: {archive}")
    finally:
        if opened and not completed and (archive.exists() or archive.is_symlink()):
            archive.unlink()



def parse_epoch(value: str) -> int:
    if not re.fullmatch(r"[0-9]+", value):
        fail(f"SOURCE_DATE_EPOCH must be an unsigned integer: {value}")
    return int(value)


def package_windows_release(
    target: str,
    output_directory: Path,
    version_tag: str,
    source: Path,
    probe_path: Path,
    *,
    tracked: Iterable[str] | None = None,
    source_head: str | None = None,
    source_date_epoch: int = 0,
) -> tuple[Path, str]:
    if target != WINDOWS_TARGET:
        fail(f"unsupported Windows release target: {target}")
    if not version_tag.startswith("v"):
        fail(f"version must be a canonical release tag such as v0.7.0: {version_tag}")
    version = version_tag[1:]
    if VERSION_PATTERN.fullmatch(version) is None:
        fail(f"version must be a canonical release tag such as v0.7.0: {version_tag}")
    if source.is_symlink():
        fail(f"release source must not be a symlink: {source}")
    real_directory(source, "release source")
    if source_head is not None and COMMIT_PATTERN.fullmatch(source_head) is None:
        fail("release source HEAD is not a full commit identity")
    probe = read_probe(probe_path)
    validate_probe(probe, version, target)
    if source_head is not None and probe["source_commit"] != source_head:
        fail("Windows probe source commit does not match the checked-out release source")
    validate_binaries(source, probe)

    if output_directory.is_symlink():
        fail(f"Windows release output directory must not be a symlink: {output_directory}")
    if output_directory.exists() and not output_directory.is_dir():
        fail(f"Windows release output directory must be a directory: {output_directory}")
    output_directory.mkdir(parents=True, exist_ok=True)
    real_directory(output_directory, "Windows release output")
    artifact_name = f"octet-{version}-{target}"
    archive = output_directory / f"{artifact_name}.tar.gz"
    if archive.is_symlink() or archive.exists():
        fail(f"Windows release archive already exists: {archive}")

    with tempfile.TemporaryDirectory(prefix="octet-windows-release-") as temporary:
        package = Path(temporary) / artifact_name
        package.mkdir()
        release_directory = source / "target" / target / "release"
        for name in BINARY_NAMES:
            destination = package / name
            shutil.copyfile(release_directory / name, destination)
            destination.chmod(0o755)
            if sha256_file(destination) != probe["binaries"][name]["sha256"]:
                fail(f"Windows probe digest disagrees with the copied binary: {name}")
        if tracked is None:
            tracked, checked_out_head = git_state(source)
            if probe["source_commit"] != checked_out_head:
                fail("Windows probe source commit does not match the checked-out release source")
        copy_documentation(source, package, tracked)
        digest = create_archive(package, archive, artifact_name, source_date_epoch)
    return archive, digest


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("output_directory", type=Path)
    parser.add_argument("version")
    parser.add_argument("source_directory", type=Path)
    parser.add_argument("probe", type=Path)
    args = parser.parse_args(argv)
    try:
        if args.source_directory.is_symlink():
            fail(f"release source must not be a symlink: {args.source_directory}")
        tracked, source_head = git_state(args.source_directory)
        epoch_value = os.environ.get("SOURCE_DATE_EPOCH")
        if epoch_value is None:
            try:
                epoch_value = subprocess.run(
                    ["git", "-C", str(args.source_directory), "show", "-s", "--format=%ct", "HEAD"],
                    check=True,
                    capture_output=True,
                    text=True,
                ).stdout.strip()
            except (FileNotFoundError, subprocess.CalledProcessError) as error:
                fail(f"could not read the release source timestamp: {error}")
        archive, digest = package_windows_release(
            args.target,
            args.output_directory,
            args.version,
            args.source_directory,
            args.probe,
            tracked=tracked,
            source_head=source_head,
            source_date_epoch=parse_epoch(epoch_value),
        )
    except ReleaseError as error:
        print(f"Windows release packaging failed: {error}", file=sys.stderr)
        return 1
    print(f"created {archive} (sha256 {digest})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
