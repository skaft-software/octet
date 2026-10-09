#!/usr/bin/env python3
"""Acceptance checks for the public Windows binary/npm release surface."""

from __future__ import annotations

import importlib.util
import json
import pathlib
import tempfile
import unittest
import zipfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
PACKAGER_PATH = ROOT / "scripts/package-octet-windows-zip.py"
SPEC = importlib.util.spec_from_file_location("windows_zip_packager", PACKAGER_PATH)
assert SPEC and SPEC.loader
PACKAGER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGER)


class WindowsReleaseSurfaceTests(unittest.TestCase):
    def test_release_workflow_builds_and_verifies_windows_target(self):
        workflow = (ROOT / ".github/workflows/release-octet.yml").read_text()
        self.assertIn(TARGET, workflow)
        self.assertIn("targets: x86_64-pc-windows-msvc", workflow)
        self.assertIn("windows-2022", workflow)
        self.assertIn("OCTET_SHA256SUMS", workflow)
        self.assertIn(".zip", workflow)
        self.assertIn("octet-host.exe", workflow)

    def test_msvc_artifact_is_qualified_before_upload_and_signing(self):
        workflow = (ROOT / ".github/workflows/release-octet.yml").read_text()
        job = workflow.split("\n  build-windows:\n", 1)[1].split("\n  publish:\n", 1)[0]
        for contract in (
            "--lib tui::terminal", "-- secure_fs tools::bash::tests",
            "--test windows_process_current", "Expand-Archive $archive",
            "$env:PATH = \"$env:SystemRoot\\System32;$env:SystemRoot\"",
            "$env:OCTET_CONPTY_BINARY = Join-Path $package 'octet.exe'",
            "--test windows_conpty", "Windows archives differ",
            "path: ${{ runner.temp }}/octet-release/*.zip",
            r"-notmatch '^rustc 1\.97\.1(?: |$)'",
        ):
            self.assertIn(contract, job)
        self.assertLess(job.index("Expand-Archive $archive"), job.index("--test windows_conpty"))
        self.assertLess(job.index("--test windows_conpty"), job.index("uses: actions/upload-artifact"))
        self.assertIn("needs: [resolve, build, build-windows]", workflow)
        self.assertNotIn("continue-on-error", job)

    def test_npm_launcher_resolves_windows_x64_platform_package(self):
        launcher = json.loads(
            (ROOT / "packages/npm/launcher/package.json.in").read_text()
            .replace("__VERSION__", "0.9.0")
        )
        self.assertEqual(
            launcher["optionalDependencies"].get("@skaft/octet-win32-x64"),
            "0.9.0",
        )
        resolver = (ROOT / "packages/npm/launcher/lib/launch.js").read_text()
        self.assertIn("'win32-x64': '@skaft/octet-win32-x64'", resolver)
        self.assertIn("process.platform === 'win32'", resolver)
        self.assertIn("`${commandName}.exe`", resolver)

    def test_windows_zip_contains_pe_binaries_and_all_public_inventory(self):
        with tempfile.TemporaryDirectory(prefix="octet-u31-windows-zip-") as temporary:
            scratch = pathlib.Path(temporary)
            binaries = scratch / "release"
            output_a = scratch / "a"
            output_b = scratch / "b"
            binaries.mkdir()
            for name in ("octet.exe", "octet-host.exe"):
                (binaries / name).write_bytes(b"MZ" + name.encode())
            archive_a = PACKAGER.package(binaries, output_a, "0.9.0", ROOT)
            archive_b = PACKAGER.package(binaries, output_b, "0.9.0", ROOT)
            self.assertEqual(archive_a.read_bytes(), archive_b.read_bytes())
            root = f"octet-0.9.0-{TARGET}/"
            with zipfile.ZipFile(archive_a) as archive:
                names = set(archive.namelist())
                self.assertIn(root + "octet.exe", names)
                self.assertIn(root + "octet-host.exe", names)
                inventory = (ROOT / "docs/package-assets.txt").read_text().splitlines()
                expected = {line.split(" ", 1)[1] for line in inventory if line and not line.startswith("#")}
                expected.update(("LICENSE", "README.md"))
                self.assertEqual({name.removeprefix(root) for name in names}, expected | {"octet.exe", "octet-host.exe"})
                for name in ("octet.exe", "octet-host.exe"):
                    self.assertEqual(archive.read(root + name)[:2], b"MZ")

    def test_windows_zip_flows_through_npm_packaging_and_publication_contracts(self):
        package_script = (ROOT / "scripts/package-octet-npm.sh").read_text()
        verifier = (ROOT / "scripts/verify-octet-npm.py").read_text()
        manifest = (ROOT / "scripts/create-octet-npm-manifest.py").read_text()
        provenance = (ROOT / "scripts/verify-octet-npm-provenance.py").read_text()
        workflow = (ROOT / ".github/workflows/release-octet.yml").read_text()
        self.assertIn('NATIVE_ARCHIVES["x86_64-pc-windows-msvc"]', package_script)
        self.assertIn('"octet-win32-x64-{version}.tgz"', verifier)
        self.assertIn('"@skaft/octet-win32-x64"', manifest)
        self.assertIn('"@skaft/octet-win32-x64"', provenance)
        self.assertIn("@skaft/octet-win32-x64|octet-win32-x64-", workflow)
        self.assertIn("--include-windows-msvc", workflow)

    def test_windows_install_instructions_are_honest_and_present(self):
        docs = (ROOT / "docs/installation.md").read_text()
        self.assertIn("install-octet.ps1", docs)
        self.assertIn("Windows x86-64", docs)
        self.assertIn("not yet published", docs)


if __name__ == "__main__":
    unittest.main()
