#!/usr/bin/env python3
"""Offline deterministic fixtures for the native Windows release boundary.

The fixture uses PE-named bytes and a recorded native probe; it never executes
those bytes.  It is intentionally a standalone check because the release
workflow and the native Windows runner remain separate qualification gates.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest import mock


SCRIPTS = Path(__file__).resolve().parent
WINDOWS_TARGET = "x86_64-pc-windows-gnu"
VERSION = "0.7.6"
SOURCE_COMMIT = "a" * 40
WORKFLOW_COMMIT = "b" * 40
REPOSITORY = "skaft-software/octet"
WORKFLOW_REF = (
    f"{REPOSITORY}/.github/workflows/release-octet.yml@refs/tags/"
    f"octet-binaries-v{VERSION}"
)


def load_script(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {path}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


packager = load_script(
    "package_octet_windows_release", SCRIPTS / "package-octet-windows-release.py"
)
metadata = load_script(
    "generate_octet_release_metadata", SCRIPTS / "generate-octet-release-metadata.py"
)


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def write_bytes(path: Path, value: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(value)


def write_probe(path: Path, binary: bytes, host: bytes) -> None:
    hello = {
        "protocol_version": 1,
        "request_id": "release-probe",
        "seq": 1,
        "type": "hello",
        "data": {
            "sdk_version": VERSION,
            "protocol_version": 1,
            "features": {"streaming": True},
        },
    }
    value = {
        "schema": packager.PROBE_SCHEMA,
        "target": WINDOWS_TARGET,
        "version": VERSION,
        "repository": REPOSITORY,
        "source_commit": SOURCE_COMMIT,
        "workflow_commit": WORKFLOW_COMMIT,
        "workflow_ref": WORKFLOW_REF,
        "observed_platform": "windows-x86_64",
        "binaries": {
            "octet.exe": {
                "sha256": sha256_bytes(binary),
                "version_stdout": f"octet {VERSION}\r\n",
            },
            "octet-host.exe": {
                "sha256": sha256_bytes(host),
                "hello_stdout": json.dumps(hello, separators=(",", ":")) + "\r\n",
            },
        },
    }
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + "\n", encoding="utf-8")


class WindowsReleaseFixtureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="octet-windows-fixture-")
        self.root = Path(self.temporary.name)
        self.source = self.root / "source"
        self.source.mkdir()
        self.release = self.source / "target" / WINDOWS_TARGET / "release"
        self.release.mkdir(parents=True)
        self.binary = b"MZ\x90\x00fixture-octet\n"
        self.host = b"MZ\x90\x00fixture-octet-host\n"
        write_bytes(self.release / "octet.exe", self.binary)
        write_bytes(self.release / "octet-host.exe", self.host)

        root_files = [
            "CHANGELOG.md",
            "CONTRIBUTING.md",
            "LICENSE",
            "README.md",
            "SECURITY.md",
            "THIRD_PARTY_NOTICES.md",
        ]
        inventory_files = [
            *root_files,
            "docs/package-assets.txt",
            "docs/README.md",
            "examples/README.md",
            "sdk/README.md",
        ]
        inventory = "# deterministic fixture inventory\n" + "\n".join(
            f"text {name}" for name in inventory_files
        ) + "\n"
        for name in root_files:
            write_bytes(self.source / name, f"{name}\n".encode())
        write_bytes(self.source / "docs/package-assets.txt", inventory.encode())
        for name in inventory_files:
            if name == "docs/package-assets.txt":
                continue
            write_bytes(self.source / name, f"fixture {name}\n".encode())
        self.tracked = set(inventory_files)
        self.probe = self.root / "windows-probe.json"
        write_probe(self.probe, self.binary, self.host)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def package(self, output: Path) -> tuple[Path, str]:
        return packager.package_windows_release(
            WINDOWS_TARGET,
            output,
            f"v{VERSION}",
            self.source,
            self.probe,
            tracked=self.tracked,
            source_head=SOURCE_COMMIT,
            source_date_epoch=1_700_000_000,
        )

    def test_windows_pair_resources_and_archive_are_reproducible(self) -> None:
        first, first_digest = self.package(self.root / "first")
        second, second_digest = self.package(self.root / "second")
        self.assertEqual(first_digest, second_digest)
        self.assertEqual(first.read_bytes(), second.read_bytes())

        artifact = f"octet-{VERSION}-{WINDOWS_TARGET}"
        with tarfile.open(first, "r:gz") as archive:
            members = archive.getmembers()
            names = [member.name.rstrip("/") for member in members]
            self.assertEqual(names[0], artifact)
            self.assertIn(f"{artifact}/octet.exe", names)
            self.assertIn(f"{artifact}/octet-host.exe", names)
            self.assertIn(f"{artifact}/docs/README.md", names)
            self.assertIn(f"{artifact}/examples/README.md", names)
            self.assertIn(f"{artifact}/sdk/README.md", names)
            self.assertNotIn(f"{artifact}/octet", names)
            self.assertNotIn(f"{artifact}/octet-host", names)
            self.assertNotIn(f"{artifact}/windows-probe.json", names)
            self.assertTrue(all("\\" not in name for name in names))
            self.assertTrue(all(member.isdir() or member.isfile() for member in members))
            self.assertEqual(archive.extractfile(f"{artifact}/octet.exe").read(), self.binary)
            self.assertEqual(
                archive.extractfile(f"{artifact}/octet-host.exe").read(), self.host
            )
            self.assertEqual(archive.getmember(f"{artifact}/octet.exe").mode, 0o755)
            self.assertEqual(archive.getmember(f"{artifact}/octet-host.exe").mode, 0o755)

    def test_packager_does_not_execute_cross_built_files(self) -> None:
        with mock.patch.object(
            packager.subprocess,
            "run",
            side_effect=AssertionError("Windows packaging must not execute a binary"),
        ):
            self.package(self.root / "not-executed")

    def test_binary_parent_links_and_name_collisions_fail_closed(self) -> None:
        link = self.release
        real_release = self.source / "release-real"
        link.rename(real_release)
        try:
            link.symlink_to(real_release, target_is_directory=True)
        except (OSError, NotImplementedError):
            real_release.rename(link)
            self.skipTest("symbolic links are unavailable in this fixture environment")
        try:
            with self.assertRaises(packager.ReleaseError):
                self.package(self.root / "bad-binary-link")
        finally:
            link.unlink()
            real_release.rename(link)

        inventory_path = self.source / "docs/package-assets.txt"
        original_inventory = inventory_path.read_text(encoding="utf-8")
        write_bytes(self.source / "OCTET.EXE", b"fixture collision\n")
        inventory_path.write_text(original_inventory + "text OCTET.EXE\n", encoding="utf-8")
        self.tracked.add("OCTET.EXE")
        try:
            with self.assertRaises(packager.ReleaseError):
                self.package(self.root / "bad-binary-collision")
        finally:
            inventory_path.write_text(original_inventory, encoding="utf-8")
            self.tracked.remove("OCTET.EXE")
            (self.source / "OCTET.EXE").unlink()

    def test_probe_and_resource_mismatches_fail_closed(self) -> None:
        value = json.loads(self.probe.read_text(encoding="utf-8"))
        value["binaries"]["octet-host.exe"]["hello_stdout"] = "{}\n{}\n"
        self.probe.write_text(json.dumps(value), encoding="utf-8")
        with self.assertRaises(packager.ReleaseError):
            self.package(self.root / "bad-probe")

        write_probe(self.probe, self.binary, self.host)
        value = json.loads(self.probe.read_text(encoding="utf-8"))
        value["workflow_ref"] = value["workflow_ref"].replace(
            "octet-binaries-v0.7.6", "latest"
        )
        self.probe.write_text(json.dumps(value), encoding="utf-8")
        with self.assertRaises(packager.ReleaseError):
            self.package(self.root / "bad-identity")

        write_probe(self.probe, self.binary, self.host)
        value = json.loads(self.probe.read_text(encoding="utf-8"))
        value["source_commit"] = "c" * 40
        self.probe.write_text(json.dumps(value), encoding="utf-8")
        with self.assertRaises(packager.ReleaseError):
            self.package(self.root / "bad-source-identity")

        write_probe(self.probe, self.binary, self.host)
        write_bytes(self.release / "octet.exe", self.binary + b"changed")
        with self.assertRaises(packager.ReleaseError):
            self.package(self.root / "bad-hash")

        write_bytes(self.release / "octet.exe", self.binary)
        inventory_path = self.source / "docs/package-assets.txt"
        original_inventory = inventory_path.read_text(encoding="utf-8")
        inventory_path.write_text(original_inventory + "text ../escape\n", encoding="utf-8")
        self.tracked.add("../escape")
        try:
            with self.assertRaises(packager.ReleaseError):
                self.package(self.root / "bad-resource")
        finally:
            inventory_path.write_text(original_inventory, encoding="utf-8")
            self.tracked.remove("../escape")

    def test_shell_dispatch_requires_the_explicit_windows_probe(self) -> None:
        script = (SCRIPTS / "package-octet-release.sh").read_text(encoding="utf-8")
        self.assertIn("x86_64-pc-windows-gnu", script)
        self.assertIn("WINDOWS_PROBE_JSON", script)
        self.assertIn("package-octet-windows-release.py", script)
        self.assertIn("Windows release packaging requires a native Windows probe", script)

    def test_candidate_metadata_is_opt_in_and_old_fixture_is_unchanged(self) -> None:
        assets = self.root / "assets"
        assets.mkdir()
        names = [
            "install-octet.sh",
            *[
                f"octet-{VERSION}-{target}.tar.gz"
                for target in metadata.PUBLISHED_TARGETS
            ],
        ]
        windows_archive, _ = self.package(self.root / "windows-assets")
        names.append(windows_archive.name)
        for name in names:
            if name == windows_archive.name:
                continue
            write_bytes(assets / name, f"fixture {name}\n".encode())
        checksum_lines = []
        for name in sorted(names):
            asset = windows_archive if name == windows_archive.name else assets / name
            checksum_lines.append(
                f"{sha256_bytes(asset.read_bytes())}  ./{name}"
            )
        checksums = assets / "OCTET_SHA256SUMS"
        checksums.write_text("\n".join(checksum_lines) + "\n", encoding="ascii")
        # The generated Windows archive is a release asset beside the manifest.
        windows_archive.replace(assets / windows_archive.name)

        candidate = metadata.build_metadata(
            VERSION,
            f"v{VERSION}",
            SOURCE_COMMIT,
            WORKFLOW_COMMIT,
            WORKFLOW_REF,
            REPOSITORY,
            checksums,
            include_windows_candidate=True,
        )
        candidate_names = {asset["name"] for asset in candidate["assets"]}
        self.assertIn(f"octet-{VERSION}-{WINDOWS_TARGET}.tar.gz", candidate_names)
        self.assertEqual(len(candidate["assets"]), 5)
        self.assertEqual(candidate["checksum_manifest"]["name"], "OCTET_SHA256SUMS")
        self.assertEqual(
            candidate["checksum_manifest"]["sha256"],
            sha256_bytes(checksums.read_bytes()),
        )
        self.assertTrue(all("/latest" not in asset["url"] for asset in candidate["assets"]))
        with self.assertRaises(metadata.MetadataError):
            metadata.parse_checksums(checksums, VERSION)

        old_fixture_path = SCRIPTS / "fixtures/homebrew/OCTET_RELEASE_METADATA.json"
        old_fixture = json.loads(old_fixture_path.read_text(encoding="utf-8"))
        recomputed = metadata.build_metadata(
            old_fixture["version"],
            old_fixture["tag"],
            old_fixture["source_commit"],
            old_fixture["workflow_commit"],
            old_fixture["workflow_ref"],
            old_fixture["repository"],
            SCRIPTS / "fixtures/homebrew/assets/OCTET_SHA256SUMS",
        )
        self.assertEqual(recomputed, old_fixture)


if __name__ == "__main__":
    unittest.main()
