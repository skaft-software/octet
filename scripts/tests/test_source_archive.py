"""Behavioral source-archive tests; Git commits exist only in disposable fixtures."""

import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "create-source-archive.py"
SPEC = importlib.util.spec_from_file_location("source_archive", SCRIPT)
archive = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(archive)


class SourceArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "--quiet")
        for name, text in {
            "Cargo.toml": '[workspace.package]\nversion = "1.2.3"\n',
            "Cargo.lock": "fixture lock\n", "README.md": "readme\n", "LICENSE": "MIT\n",
        }.items():
            (self.repo / name).write_text(text)
        self.git("add", ".")
        self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                 "commit", "--quiet", "-m", "fixture")
        self.commit = self.git("rev-parse", "HEAD").decode().strip()

    def git(self, *args):
        env = {**os.environ, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull}
        return subprocess.run(["git", "-C", str(self.repo), *args], env=env,
                              check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE).stdout

    def test_bytes_are_deterministic_and_dirty_untracked_files_never_enter(self):
        before_index = (self.repo / ".git/index").read_bytes()
        first, second = self.root / "one.tar.gz", self.root / "two.tar.gz"
        digest = archive.create_archive(self.repo, "1.2.3", self.commit, first)
        (self.repo / "README.md").write_text("uncommitted private text\n")
        (self.repo / "private.txt").write_text("untracked private text\n")
        self.assertEqual(digest, archive.create_archive(self.repo, "1.2.3", self.commit, second))
        self.assertEqual(first.read_bytes(), second.read_bytes())
        self.assertEqual(digest, hashlib.sha256(first.read_bytes()).hexdigest())
        self.assertEqual(before_index, (self.repo / ".git/index").read_bytes())
        with tarfile.open(first) as contents:
            self.assertNotIn("octet-1.2.3/private.txt", contents.getnames())
            self.assertEqual(contents.extractfile("octet-1.2.3/README.md").read(), b"readme\n")
        if os.name == "posix":
            self.assertEqual(first.stat().st_mode & 0o777, 0o600)

    def test_mismatched_version_bad_ref_and_bad_version_publish_nothing(self):
        for version, ref in [("1.2.4", self.commit), ("1.2.3", "missing"), ("../escape", self.commit)]:
            with self.subTest(version=version, ref=ref):
                output = self.root / "failed.tar.gz"
                with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                    archive.create_archive(self.repo, version, ref, output)
                self.assertFalse(output.exists())

    def test_existing_artifact_and_symlink_are_never_replaced(self):
        output = self.root / "existing.tar.gz"
        output.write_bytes(b"keep")
        with self.assertRaises(FileExistsError):
            archive.create_archive(self.repo, "1.2.3", self.commit, output)
        self.assertEqual(output.read_bytes(), b"keep")
        linked = self.root / "link.tar.gz"
        linked.symlink_to(self.root / "missing")
        with self.assertRaises(FileExistsError):
            archive.create_archive(self.repo, "1.2.3", self.commit, linked)
        self.assertTrue(linked.is_symlink())

    def test_required_paths_and_archive_size_are_checked_before_publication(self):
        output = self.root / "missing.tar.gz"
        with patch.object(archive, "REQUIRED_PATHS", {"absent"}):
            with self.assertRaisesRegex(ValueError, "required"):
                archive.create_archive(self.repo, "1.2.3", self.commit, output)
        with patch.object(archive, "MAX_ARCHIVE_BYTES", 1):
            with self.assertRaisesRegex(ValueError, "bound"):
                archive.create_archive(self.repo, "1.2.3", self.commit, output)
        self.assertFalse(output.exists())
        self.assertEqual(list(self.root.glob(".octet-source-*")), [])

    def test_committed_build_output_cannot_ship(self):
        for component in ("target", "node_modules", ".build", "DerivedData", ".swiftpm"):
            with self.subTest(component=component):
                generated = self.repo / "apps" / "fixture" / component / "cache"
                generated.parent.mkdir(parents=True)
                generated.write_bytes(b"build output")
                self.git("add", "apps")
                self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                         "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                         "commit", "--quiet", "-m", "accidental artifact")
                output = self.root / "refused.tar.gz"
                with self.assertRaisesRegex(ValueError, "build output"):
                    archive.create_archive(self.repo, "1.2.3", "HEAD", output)
                self.assertFalse(output.exists())
                self.assertEqual(list(self.root.glob(".octet-source-*")), [])
                self.git("rm", "-r", "apps")
                self.git("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                         "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null",
                         "commit", "--quiet", "-m", "remove fixture artifact")

    def test_cli_requires_an_explicit_ref_and_reports_checksum(self):
        args = ["python3", str(SCRIPT), "--repo", str(self.repo), "--version", "1.2.3",
                "--out", str(self.root / "cli.tar.gz")]
        missing = subprocess.run(args, capture_output=True)
        self.assertNotEqual(missing.returncode, 0)
        result = subprocess.run(args + ["--ref", self.commit], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"^[0-9a-f]{64}  ")


if __name__ == "__main__":
    unittest.main()
