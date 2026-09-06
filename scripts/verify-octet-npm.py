#!/usr/bin/env python3
"""Validate the four octet npm tarballs before any registry mutation."""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import stat
import sys
import tarfile
from dataclasses import dataclass
from typing import Any, Dict, Iterable, List, Mapping, Sequence

LAUNCHER_NAME = "@skaft/octet"
PLATFORM_PACKAGES = {
    "octet-darwin-arm64": ("@skaft/octet-darwin-arm64", "darwin", "arm64"),
    "octet-darwin-x64": ("@skaft/octet-darwin-x64", "darwin", "x64"),
    "octet-linux-x64-gnu": ("@skaft/octet-linux-x64-gnu", "linux", "x64"),
}
REPOSITORY = "https://github.com/skaft-software/ygg"
SECRET_SCANNER_VERSION = "octet-npm-secret-rules-v1"
MAX_LAUNCHER_BYTES = 1 * 1024 * 1024
MAX_LAUNCHER_ENTRIES = 16
MAX_PACKAGE_BYTES = 128 * 1024 * 1024
MAX_EXPANDED_BYTES = 160 * 1024 * 1024
MAX_ENTRIES = 4096
MAX_MEMBER_BYTES = 64 * 1024 * 1024

LIFECYCLE_KEYS = {
    "scripts",
    "preinstall",
    "install",
    "postinstall",
    "prepare",
    "prepublish",
    "prepublishOnly",
    "publish",
    "postpublish",
}
SECRET_PATTERNS = (
    re.compile(rb"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"),
    re.compile(rb"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
    re.compile(rb"\b(?:ghp|gho|ghs|ghr)_[A-Za-z0-9]{20,}\b"),
    re.compile(rb"\bnpm_[A-Za-z0-9]{20,}\b"),
    re.compile(rb"\bxox[baprs]-[A-Za-z0-9-]{20,}\b"),
)


class VerificationError(Exception):
    pass


@dataclass(frozen=True)
class ExpectedPackage:
    artifact: str
    name: str
    platform: str | None
    os: str | None
    cpu: str | None


@dataclass
class Inspection:
    expected: ExpectedPackage
    members: Dict[str, tarfile.TarInfo]
    contents: Dict[str, bytes]
    expanded_bytes: int


def expected_packages(version: str) -> List[ExpectedPackage]:
    return [
        ExpectedPackage(f"octet-{version}.tgz", LAUNCHER_NAME, None, None, None),
        ExpectedPackage(
            f"octet-darwin-arm64-{version}.tgz",
            PLATFORM_PACKAGES["octet-darwin-arm64"][0],
            "octet-darwin-arm64",
            "darwin",
            "arm64",
        ),
        ExpectedPackage(
            f"octet-darwin-x64-{version}.tgz",
            PLATFORM_PACKAGES["octet-darwin-x64"][0],
            "octet-darwin-x64",
            "darwin",
            "x64",
        ),
        ExpectedPackage(
            f"octet-linux-x64-gnu-{version}.tgz",
            PLATFORM_PACKAGES["octet-linux-x64-gnu"][0],
            "octet-linux-x64-gnu",
            "linux",
            "x64",
        ),
    ]


def fail(message: str) -> None:
    raise VerificationError(message)


def parse_json(raw: bytes, path: str) -> Mapping[str, Any]:
    try:
        text = raw.decode("utf-8")

        def no_duplicates(pairs: Sequence[tuple[str, Any]]) -> Dict[str, Any]:
            result: Dict[str, Any] = {}
            for key, value in pairs:
                if key in result:
                    raise ValueError(f"duplicate key {key}")
                result[key] = value
            return result

        value = json.loads(text, object_pairs_hook=no_duplicates)
    except (UnicodeDecodeError, json.JSONDecodeError, ValueError) as error:
        fail(f"{path} is not valid unique-key JSON: {error}")
    if not isinstance(value, dict):
        fail(f"{path} must contain a JSON object")
    return value


def is_safe_member_name(name: str) -> bool:
    if not name or "\\" in name or name.startswith("/"):
        return False
    parts = pathlib.PurePosixPath(name).parts
    return bool(parts) and all(part not in ("", ".", "..") for part in parts)


def allowed_member(expected: ExpectedPackage, relative: str, is_directory: bool) -> bool:
    if relative == "":
        return is_directory
    if expected.platform is None:
        if relative in {"bin", "lib"}:
            return is_directory
        if relative in {"package.json", "README.md", "LICENSE"}:
            return not is_directory
        return relative in {
            "bin/octet",
            "bin/octet-host",
            "lib/launch.sh",
        } and not is_directory
    fixed = {
        "package.json",
        "README.md",
        "LICENSE",
        "bin",
        "bin/octet",
        "bin/octet-host",
        "share",
        "share/octet",
        "share/octet/.octet-version",
        "share/octet/README.md",
        "share/octet/docs",
        "share/octet/examples",
        "share/octet/sdk",
    }
    if relative in fixed:
        return True
    return any(relative.startswith(prefix) for prefix in ("share/octet/docs/", "share/octet/examples/", "share/octet/sdk/"))


def inspect_tarball(path: pathlib.Path, expected: ExpectedPackage) -> Inspection:
    try:
        compressed_bytes = path.stat().st_size
    except OSError as error:
        fail(f"cannot stat {path}: {error}")
    limit = MAX_LAUNCHER_BYTES if expected.platform is None else MAX_PACKAGE_BYTES
    if compressed_bytes > limit:
        fail(f"{path.name} exceeds its {limit}-byte compressed limit")
    members: Dict[str, tarfile.TarInfo] = {}
    contents: Dict[str, bytes] = {}
    expanded = 0
    try:
        archive = tarfile.open(path, mode="r:gz")
    except (OSError, tarfile.TarError) as error:
        fail(f"{path.name} is not a readable gzip tarball: {error}")
    with archive:
        entries = archive.getmembers()
        if len(entries) > (MAX_LAUNCHER_ENTRIES if expected.platform is None else MAX_ENTRIES):
            fail(f"{path.name} contains too many entries")
        for member in entries:
            name = member.name
            if not is_safe_member_name(name) or not name.startswith("package"):
                fail(f"{path.name} contains an unsafe member path: {name}")
            parts = pathlib.PurePosixPath(name).parts
            if parts[0] != "package" or (len(parts) > 1 and parts[1] == ""):
                fail(f"{path.name} contains a member outside package/: {name}")
            if name.rstrip("/") != "/".join(parts) or (name.endswith("/") and not member.isdir()):
                fail(f"{path.name} contains a non-canonical member path: {name}")
            relative = "/".join(parts[1:])
            if name in members:
                fail(f"{path.name} repeats member {name}")
            if member.mode & 0o7000 or member.mode & 0o002:
                fail(f"{path.name} has unsafe permissions on {name}")
            if member.size < 0 or member.size > MAX_MEMBER_BYTES:
                fail(f"{path.name} member {name} exceeds {MAX_MEMBER_BYTES} bytes")
            if not (member.isdir() or member.isreg()):
                fail(f"{path.name} contains a link or special member: {name}")
            if not allowed_member(expected, relative, member.isdir()):
                fail(f"{path.name} contains an unexpected member: {name}")
            members[name] = member
            expanded += member.size
            if expanded > (MAX_LAUNCHER_BYTES if expected.platform is None else MAX_EXPANDED_BYTES):
                fail(f"{path.name} exceeds its expanded size limit")
            if member.isreg():
                stream = archive.extractfile(member)
                if stream is None:
                    fail(f"{path.name} member cannot be read: {name}")
                data = stream.read(MAX_MEMBER_BYTES + 1)
                if len(data) != member.size:
                    fail(f"{path.name} member size changed while reading: {name}")
                contents[relative] = data
    return Inspection(expected, members, contents, expanded)


def require_files(inspection: Inspection, names: Iterable[str]) -> None:
    for name in names:
        if name not in inspection.contents:
            fail(f"{inspection.expected.artifact} is missing package/{name}")
        if not inspection.contents[name]:
            fail(f"{inspection.expected.artifact} contains an empty package/{name}")


def require_executable(inspection: Inspection, name: str) -> None:
    member = inspection.members.get(f"package/{name}")
    if member is None or not member.isreg() or not member.mode & 0o111:
        fail(f"{inspection.expected.artifact} package/{name} is not an executable regular file")


def check_manifest(inspection: Inspection, version: str) -> Mapping[str, Any]:
    manifest = parse_json(inspection.contents["package.json"], f"{inspection.expected.artifact}/package.json")
    if any(key in manifest for key in LIFECYCLE_KEYS):
        fail(f"{inspection.expected.artifact} declares an npm lifecycle hook")
    if manifest.get("name") != inspection.expected.name:
        fail(f"{inspection.expected.artifact} has the wrong package name")
    if manifest.get("version") != version:
        fail(f"{inspection.expected.artifact} version is not {version}")
    if manifest.get("license") != "MIT" or manifest.get("repository") != REPOSITORY:
        fail(f"{inspection.expected.artifact} has the wrong release identity or license")
    if manifest.get("description") is None or not isinstance(manifest["description"], str):
        fail(f"{inspection.expected.artifact} must have a description")
    if manifest.get("private", False) is not False:
        fail(f"{inspection.expected.artifact} must not be private")
    if inspection.expected.platform is None:
        expected_keys = {
            "name",
            "version",
            "description",
            "license",
            "repository",
            "files",
            "bin",
            "optionalDependencies",
        }
        if set(manifest) != expected_keys:
            fail(f"{inspection.expected.artifact} manifest identity has unexpected fields")
        if manifest["files"] != ["README.md", "LICENSE", "bin/", "lib/"]:
            fail(f"{inspection.expected.artifact} has an incorrect files allowlist")
        if manifest["bin"] != {"octet": "bin/octet", "octet-host": "bin/octet-host"}:
            fail(f"{inspection.expected.artifact} has an incorrect bin mapping")
        optional = manifest.get("optionalDependencies")
        expected_optional = {name: version for name, _, _ in PLATFORM_PACKAGES.values()}
        if optional != expected_optional:
            fail(f"{inspection.expected.artifact} platform dependency coupling is incorrect")
    else:
        expected_keys = {
            "name",
            "version",
            "description",
            "license",
            "repository",
            "os",
            "cpu",
            "files",
        }
        if set(manifest) != expected_keys:
            fail(f"{inspection.expected.artifact} manifest identity has unexpected fields")
        if manifest["os"] != [inspection.expected.os] or manifest["cpu"] != [inspection.expected.cpu]:
            fail(f"{inspection.expected.artifact} has incorrect npm platform constraints")
        if manifest["files"] != ["README.md", "LICENSE", "bin/", "share/octet/"]:
            fail(f"{inspection.expected.artifact} has an incorrect files allowlist")
    for forbidden in (
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "bundledDependencies",
        "bundleDependencies",
    ):
        if forbidden in manifest:
            fail(f"{inspection.expected.artifact} declares forbidden {forbidden}")
    return manifest


def scan_secrets(inspection: Inspection) -> None:
    for name, data in inspection.contents.items():
        for pattern in SECRET_PATTERNS:
            if pattern.search(data):
                fail(f"{inspection.expected.artifact} secret scanner found a match in package/{name}")


def validate(inspection: Inspection, version: str) -> None:
    check_manifest(inspection, version)
    if inspection.expected.platform is None:
        require_files(inspection, ("package.json", "README.md", "LICENSE", "bin/octet", "bin/octet-host", "lib/launch.sh"))
        for name in ("bin/octet", "bin/octet-host", "lib/launch.sh"):
            require_executable(inspection, name)
    else:
        require_files(
            inspection,
            (
                "package.json",
                "README.md",
                "LICENSE",
                "bin/octet",
                "bin/octet-host",
                "share/octet/.octet-version",
                "share/octet/README.md",
            ),
        )
        for name in ("bin/octet", "bin/octet-host"):
            require_executable(inspection, name)
        if inspection.contents["share/octet/.octet-version"].decode("utf-8").strip() != version:
            fail(f"{inspection.expected.artifact} packaged documentation version is not {version}")
        for root in ("share/octet/docs/", "share/octet/examples/", "share/octet/sdk/"):
            if not any(name.startswith(root) for name in inspection.contents):
                fail(f"{inspection.expected.artifact} is missing {root}")
    scan_secrets(inspection)


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="canonical release version without the leading v")
    parser.add_argument("package_directory", type=pathlib.Path)
    parser.add_argument("--json", action="store_true", dest="as_json")
    args = parser.parse_args(argv)
    version = args.version[1:] if args.version.startswith("v") else args.version
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[.-][0-9A-Za-z-]+)*", version):
        parser.error(f"invalid release version: {args.version}")
    if not args.package_directory.is_dir() or args.package_directory.is_symlink():
        raise VerificationError(f"package directory must be a real directory: {args.package_directory}")
    expected = expected_packages(version)
    actual_tgz = sorted(path.name for path in args.package_directory.iterdir() if path.suffix == ".tgz")
    expected_names = sorted(item.artifact for item in expected)
    if actual_tgz != expected_names:
        raise VerificationError(f"package directory artifact set mismatch: expected {expected_names}, found {actual_tgz}")
    for item in expected:
        path = args.package_directory / item.artifact
        if path.is_symlink() or not path.is_file():
            raise VerificationError(f"package artifact is not a regular file: {path}")
        inspection = inspect_tarball(path, item)
        validate(inspection, version)
    if args.as_json:
        print('{"schema":"octet.npm.verification.v1","status":"passed"}')
    else:
        print("npm package verification passed")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main(sys.argv[1:]))
    except VerificationError as error:
        print(f"npm package verification failed: {error}", file=sys.stderr)
        raise SystemExit(1)
