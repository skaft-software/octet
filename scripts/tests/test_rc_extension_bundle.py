"""Exact-source RC packaging: no binaries/factories are executed by these fixtures."""

import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]
LINUX = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-gnu"
IDS = ["octet-codemode", "octet-computer-use"]


class RcExtensionBundleTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.source = self.root / "source"
        scripts = self.source / "scripts"
        scripts.mkdir(parents=True)
        for name in ("package-octet-extension-release.sh", "package-octet-rc-extensions.sh"):
            shutil.copy2(ROOT / "scripts" / name, scripts / name)
        for extension in IDS:
            directory = self.source / "extensions" / extension
            (directory / "bin").mkdir(parents=True)
            (directory / "extension.toml").write_text(
                f'name = "{extension}"\nversion = "0.9.5"\napi_version = "0.4"\n'
                'requires_octet = "=0.9.5"\n[entrypoint]\ncommand = "bin/codemode"\nargs = ["serve"]\n',
                encoding="utf-8",
            )
            entry = directory / "bin" / "codemode"
            entry.write_text("#!/bin/sh\necho must-not-execute >&2\nexit 99\n")
            entry.chmod(0o755)
        (self.source / "extensions" / "release-catalog.txt").write_text("\n".join(IDS) + "\n")
        self.git("init", "--quiet")
        self.git("config", "user.name", "RC fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("add", "scripts", "extensions")
        self.git("commit", "--quiet", "-m", "fixture")
        self.native(LINUX).parent.mkdir(parents=True)
        self.native(LINUX).write_bytes(b"fixture native Linux binary; never executed")
        self.native(WINDOWS).parent.mkdir(parents=True)
        self.native(WINDOWS).write_bytes(b"MZfixture native Windows binary; never executed")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.source), *args], text=True).strip()

    def native(self, target):
        suffix = ".exe" if target == WINDOWS else ""
        return self.source / "extensions/octet-codemode/target" / target / "release" / f"octet-codemode{suffix}"

    def package(self, target, output="output"):
        return subprocess.run(
            ["bash", str(self.source / "scripts/package-octet-rc-extensions.sh"),
             target, str(self.root / output), "v0.9.5", str(self.source)],
            env={**os.environ, "SOURCE_DATE_EPOCH": "1700000000"},
            capture_output=True, text=True, timeout=30,
        )

    def test_catalog_closure_native_payload_checksums_and_repeatability(self):
        # A workstation cache must never appear in an immutable-source bundle.
        (self.source / "extensions/octet-computer-use/local-canary").write_text("untracked")
        for output in ("first", "second"):
            result = self.package(LINUX, output)
            self.assertEqual(result.returncode, 0, result.stderr)
        first, second = self.root / "first", self.root / "second"
        self.assertEqual(sorted(path.name for path in first.glob("*.tar.gz")),
                         [f"{extension}-0.9.5.tar.gz" for extension in IDS])
        for path in first.iterdir():
            self.assertEqual(path.read_bytes(), (second / path.name).read_bytes())
        info = json.loads((first / "BUILD-INFO.json").read_text())
        self.assertEqual(info["source_commit"], self.git("rev-parse", "HEAD"))
        self.assertEqual(info["source_tree"], self.git("rev-parse", "HEAD^{tree}"))
        self.assertEqual(info["target"], LINUX)
        self.assertEqual(info["extensions"], IDS)
        self.assertTrue(info["factory_enablement"].startswith("none;"))
        for line in (first / "SHA256SUMS").read_text().splitlines():
            digest, name = line.split("  ")
            self.assertEqual(digest, hashlib.sha256((first / name).read_bytes()).hexdigest())
        with tarfile.open(first / "octet-codemode-0.9.5.tar.gz") as archive:
            executable = archive.getmember("octet-codemode/bin/codemode")
            self.assertEqual(executable.mode, 0o755)
            self.assertEqual(archive.extractfile(executable).read(), self.native(LINUX).read_bytes())
        with tarfile.open(first / "octet-computer-use-0.9.5.tar.gz") as archive:
            self.assertNotIn("octet-computer-use/local-canary", archive.getnames())
        self.assertEqual(self.git("diff", "--name-only", "HEAD"), "")

    def test_windows_bundle_uses_native_exe_and_retains_serve_argument(self):
        result = self.package(WINDOWS)
        self.assertEqual(result.returncode, 0, result.stderr)
        with tarfile.open(self.root / "output/octet-codemode-0.9.5.tar.gz") as archive:
            self.assertNotIn("octet-codemode/bin/codemode", archive.getnames())
            self.assertEqual(archive.extractfile("octet-codemode/bin/codemode.exe").read(),
                             self.native(WINDOWS).read_bytes())
            manifest = tomllib.loads(archive.extractfile("octet-codemode/extension.toml").read().decode())
            self.assertEqual(manifest["entrypoint"], {"command": "bin/codemode.exe", "args": ["serve"]})
            self.assertEqual(manifest["requires_octet"], "=0.9.5")
        # Platform staging must not change source, even for Windows.
        self.assertEqual(self.git("diff", "--name-only", "HEAD"), "")

    def test_dirty_source_fails_before_packaging(self):
        (self.source / "extensions/release-catalog.txt").write_text("octet-codemode\n")
        result = self.package(LINUX)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tracked changes", result.stderr)
        self.assertFalse((self.root / "output").exists())

    def test_missing_or_linked_native_executable_fails_closed(self):
        native = self.native(LINUX)
        native.unlink()
        for linked in (False, True):
            if linked:
                native.symlink_to(self.native(WINDOWS))
            result = self.package(LINUX)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("missing regular native", result.stderr)
            self.assertFalse((self.root / "output").exists())

    def test_unsupported_target_fails_before_packaging(self):
        result = self.package("arm-unknown-unqualified")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported dogfood target", result.stderr)
        self.assertFalse((self.root / "output").exists())

    def test_workflow_dispatch_is_unsigned_and_includes_each_platform_bundle(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        job = workflow.split("\n  release-candidate:\n", 1)[1].split("\n  msrv:\n", 1)[0]
        self.assertIn("if: github.event_name == 'workflow_dispatch'", job)
        for target in (LINUX, "x86_64-apple-darwin", "aarch64-apple-darwin"):
            self.assertIn(target, job)
        self.assertIn("package-octet-rc-extensions.sh", job)
        self.assertIn("octet-rc/", job)
        self.assertIn("-c SHA256SUMS", job)
        self.assertNotIn("contents: write", workflow)
        self.assertNotIn("gh release create", workflow)
        windows = workflow.split("\n  windows:\n", 1)[1].split("\n  release-candidate:\n", 1)[0]
        self.assertIn("package-octet-rc-extensions.sh", windows)
        self.assertIn("'octet-rc'", windows)
        self.assertIn("'workflow_dispatch' || success()", windows)


if __name__ == "__main__":
    unittest.main()
