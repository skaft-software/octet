"""Exercise release packaging without modifying Git or executing extensions."""

import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "package-octet-extension-release.sh"


class ExtensionBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "fixture-extension"
        self.source.mkdir()
        self.output = self.root / "output"
        self.entrypoint = self.source / "extension.py"
        self.entrypoint.write_text("#!/usr/bin/env python3\nraise RuntimeError('must not execute')\n")
        self.entrypoint.chmod(0o755)
        self.archive = self.output / "fixture-extension-0.8.0.tar.gz"

    def manifest(self, api="0.3", requirement="=0.8.0", command="extension.py"):
        (self.source / "extension.toml").write_text(
            'name = "fixture-extension"\nversion = "1.2.3"\n'
            f'api_version = "{api}"\nrequires_octet = "{requirement}"\n'
            f'[entrypoint]\ncommand = "{command}"\n'
            '[contributes]\ntools = []\n'
        )

    def package(self):
        return subprocess.run(
            ["bash", str(SCRIPT), "fixture-extension", str(self.output), "v0.8.0", str(self.source)],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def test_legacy_and_current_api_bundle_bytes_are_deterministic(self):
        for api in ("0.2", "0.3"):
            with self.subTest(api=api):
                self.manifest(api)
                result = self.package()
                self.assertEqual(result.returncode, 0, result.stderr)
                first = self.archive.read_bytes()
                self.assertEqual(self.package().returncode, 0)
                self.assertEqual(first, self.archive.read_bytes())
                with tarfile.open(self.archive) as archive:
                    self.assertEqual(archive.getnames(), [
                        "fixture-extension", "fixture-extension/extension.py",
                        "fixture-extension/extension.toml",
                    ])
                    self.assertEqual(archive.getmember("fixture-extension/extension.py").mode, 0o755)
                    self.assertEqual(archive.extractfile("fixture-extension/extension.toml").read(),
                                     (self.source / "extension.toml").read_bytes())
                self.archive.unlink()

    def test_unknown_and_unpackaged_only_apis_are_refused(self):
        for api in ("0.1", "0.3.0", "0.4", "", "latest"):
            with self.subTest(api=api):
                self.manifest(api)
                result = self.package()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("api_version", result.stderr)
                self.assertFalse(self.archive.exists())

    def test_current_api_still_requires_exact_host_version(self):
        for requirement in ("=0.7.6", ">=0.8.0", "0.8.0", "*"):
            with self.subTest(requirement=requirement):
                self.manifest(requirement=requirement)
                result = self.package()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("requires_octet", result.stderr)
                self.assertFalse(self.archive.exists())

    def test_current_api_does_not_relax_entrypoint_or_symlink_checks(self):
        self.manifest()
        self.entrypoint.chmod(0o644)
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("executable", result.stderr)
        self.assertFalse(self.archive.exists())
        self.entrypoint.chmod(0o755)
        (self.source / "linked.py").symlink_to(self.entrypoint)
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("links or special files", result.stderr)
        self.assertFalse(self.archive.exists())

    def test_directory_links_and_linked_command_ancestors_are_refused(self):
        outside = self.root / "outside"
        (outside / "bin").mkdir(parents=True)
        executable = outside / "bin" / "extension.py"
        executable.write_bytes(self.entrypoint.read_bytes())
        executable.chmod(0o755)
        linked = self.source / "linked"
        linked.symlink_to(outside, target_is_directory=True)
        for api in ("0.2", "0.3"):
            for command in ("extension.py", "linked/bin/extension.py"):
                with self.subTest(api=api, command=command):
                    self.manifest(api=api, command=command)
                    result = self.package()
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("links or special files", result.stderr)
                    self.assertFalse(self.archive.exists())

    def test_filtered_local_entrypoint_cannot_produce_an_incomplete_archive(self):
        cache = self.source / "__pycache__"
        cache.mkdir()
        executable = cache / "extension.py"
        executable.write_bytes(self.entrypoint.read_bytes())
        executable.chmod(0o755)
        self.manifest(command="__pycache__/extension.py")
        result = self.package()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("entrypoint.command is missing from the bundle", result.stderr)
        self.assertFalse(self.archive.exists())

    def test_nested_regular_entrypoint_is_present_and_executable(self):
        directory = self.source / "bin"
        directory.mkdir()
        self.entrypoint.rename(directory / "extension.py")
        self.manifest(command="bin/extension.py")
        result = self.package()
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.archive) as archive:
            member = archive.getmember("fixture-extension/bin/extension.py")
            self.assertTrue(member.isfile())
            self.assertEqual(member.mode, 0o755)


if __name__ == "__main__":
    unittest.main()
