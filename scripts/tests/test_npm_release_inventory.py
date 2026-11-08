#!/usr/bin/env python3
"""Bind the expected npm documentation set to the canonical release source.

The native archives are produced and signed from the canonical `vX.Y.Z` tag
commit, so that commit's `docs/package-assets.txt` defines which public
documentation the npm packages must contain. The packaging checkout is mutable
and must never widen that set. These fixtures prove the binding, the fail-closed
behaviour, and the regression that motivated it: a tooling checkout that ships
extra documentation could never verify against the immutable release assets.
"""

from __future__ import annotations

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
VERIFIER = ROOT / "scripts/verify-octet-npm.py"
PACKAGER = ROOT / "scripts/package-octet-npm.sh"
SOURCE_INVENTORY = """text README.md
text docs/index.md
asset docs/assets/a.png
"""
TOOLING_INVENTORY = SOURCE_INVENTORY + "asset docs/assets/readme/extra.jpg\n"


class Repository:
    """A disposable Git fixture holding the verifier and one inventory."""

    def __init__(self, directory: pathlib.Path) -> None:
        self.root = directory
        (self.root / "scripts").mkdir(parents=True)
        (self.root / "docs").mkdir()
        shutil.copyfile(VERIFIER, self.root / "scripts/verify-octet-npm.py")
        self.git("init", "--quiet", "--initial-branch=fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("config", "user.name", "fixture")

    def git(self, *arguments: str) -> str:
        return subprocess.run(
            ["git", "-C", str(self.root), *arguments],
            check=True,
            capture_output=True,
            text=True,
        ).stdout

    def commit_inventory(self, inventory: str, message: str) -> str:
        (self.root / "docs/package-assets.txt").write_text(inventory, encoding="utf-8")
        self.git("add", "--all")
        self.git("commit", "--quiet", "--message", message)
        return self.git("rev-parse", "HEAD").strip()

    def documentation(self, commit: str) -> tuple[set[str], set[str]]:
        script = (
            "import runpy, json\n"
            "module = runpy.run_path('scripts/verify-octet-npm.py')\n"
            "print(json.dumps({'files': sorted(module['DOCUMENTATION_FILES']),\n"
            "                  'extras': sorted(module['DOCUMENTATION_EXTRAS'])}))\n"
        )
        result = subprocess.run(
            [sys.executable, "-c", script],
            cwd=self.root,
            env={**os.environ, "OCTET_NPM_SOURCE_COMMIT": commit},
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise AssertionError(result.stderr)
        import json

        payload = json.loads(result.stdout)
        return set(payload["files"]), set(payload["extras"])

    def failure(self, package_directory: pathlib.Path, environment: dict[str, str]) -> str:
        result = subprocess.run(
            [sys.executable, "scripts/verify-octet-npm.py", "0.10.3", str(package_directory)],
            cwd=self.root,
            env=environment,
            capture_output=True,
            text=True,
        )
        if result.returncode == 0:
            raise AssertionError("verifier accepted an unusable documentation identity")
        return (result.stdout + result.stderr).strip()


class InventoryContractTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = pathlib.Path(self.temporary.name)
        self.repository = Repository(self.directory / "repo")
        self.source = self.repository.commit_inventory(SOURCE_INVENTORY, "canonical source")
        self.tooling = self.repository.commit_inventory(TOOLING_INVENTORY, "tooling checkout")
        self.packages = self.directory / "packages"
        self.packages.mkdir()

    def environment(self, commit: str | None) -> dict[str, str]:
        environment = dict(os.environ)
        environment.pop("OCTET_NPM_SOURCE_COMMIT", None)
        if commit is not None:
            environment["OCTET_NPM_SOURCE_COMMIT"] = commit
        return environment

    def test_canonical_source_inventory_defines_the_expected_set(self) -> None:
        files, extras = self.repository.documentation(self.source)
        self.assertEqual(files, {"README.md", "docs/index.md", "docs/assets/a.png"})
        self.assertEqual(extras, {"README.md"})

    def test_tooling_inventory_cannot_widen_the_released_set(self) -> None:
        source_files, _ = self.repository.documentation(self.source)
        tooling_files, _ = self.repository.documentation(self.tooling)
        extra = "docs/assets/readme/extra.jpg"
        self.assertNotIn(extra, source_files)
        self.assertIn(extra, tooling_files)
        # The released assets carry exactly the source set. Requiring the tooling
        # set (the pre-fix behaviour) fails; requiring the source set passes.
        script = (
            "import runpy, sys, types\n"
            "module = runpy.run_path('scripts/verify-octet-npm.py')\n"
            "names = set(sys.argv[1].split(','))\n"
            "inspection = types.SimpleNamespace("
            "contents={'share/octet/' + name: b'x' for name in names},"
            "expected=types.SimpleNamespace(artifact='octet-0.10.3.tgz'))\n"
            "try:\n"
            "    module['require_files'](inspection, "
            "('share/octet/' + name for name in sorted(set(sys.argv[2].split(',')))), allow_empty=True)\n"
            "except module['VerificationError'] as error:\n"
            "    print('failed:', error)\n"
            "    raise SystemExit(3)\n"
        )
        packaged = ",".join(sorted(source_files))
        for expected, returncode in ((packaged, 0), (",".join(sorted(tooling_files)), 3)):
            result = subprocess.run(
                [sys.executable, "-c", script, packaged, expected],
                cwd=self.repository.root,
                env=self.environment(self.source),
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, returncode, result.stderr)

    def test_unset_identity_fails_closed(self) -> None:
        message = self.repository.failure(self.packages, self.environment(None))
        self.assertIn("OCTET_NPM_SOURCE_COMMIT", message)

    def test_malformed_identity_fails_closed(self) -> None:
        for malformed in ("HEAD", "A" * 40, "a" * 39, "a" * 41, " " + "a" * 40):
            with self.subTest(identity=malformed):
                message = self.repository.failure(
                    self.packages, self.environment(malformed)
                )
                self.assertIn("full lowercase commit SHA", message)

    def test_unknown_commit_fails_closed(self) -> None:
        message = self.repository.failure(self.packages, self.environment("0" * 40))
        self.assertIn("no readable docs/package-assets.txt", message)

    def test_commit_without_inventory_fails_closed(self) -> None:
        (self.repository.root / "docs/package-assets.txt").unlink()
        self.repository.git("add", "--all")
        self.repository.git("commit", "--quiet", "--message", "drop inventory")
        without = self.repository.git("rev-parse", "HEAD").strip()
        message = self.repository.failure(self.packages, self.environment(without))
        self.assertIn("no readable docs/package-assets.txt", message)

    def test_invalid_inventory_entries_fail_closed(self) -> None:
        cases = {
            "malformed": ("text docs/index.md\nasset docs/assets/a.png\nplain README.md\n",
                          "invalid documentation inventory entry"),
            "duplicate": ("text docs/index.md\ntext docs/index.md\n",
                          "unsafe or repeated documentation inventory path"),
            "unsafe": ("text ../escape.md\n",
                       "unsafe or repeated documentation inventory path"),
            "empty": ("# nothing shipped\n",
                      "documentation inventory is empty"),
        }
        for label, (inventory, message) in cases.items():
            with self.subTest(case=label):
                commit = self.repository.commit_inventory(inventory, label)
                observed = self.repository.failure(self.packages, self.environment(commit))
                self.assertIn(message, observed)

    def test_packager_requires_the_identity_and_a_known_commit(self) -> None:
        usage = subprocess.run(
            ["bash", str(PACKAGER)], cwd=self.repository.root, capture_output=True, text=True
        )
        self.assertEqual(usage.returncode, 2)
        self.assertIn("SOURCE_COMMIT", usage.stderr)
        sums = self.directory / "sums"
        sums.write_text("", encoding="utf-8")
        unknown = subprocess.run(
            [
                "bash",
                str(PACKAGER),
                "0.10.3",
                str(self.directory),
                str(self.directory / "out"),
                str(sums),
                "0" * 40,
            ],
            cwd=self.repository.root,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(unknown.returncode, 0)
        self.assertIn("canonical source commit is unavailable", unknown.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
