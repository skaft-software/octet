#!/usr/bin/env python3
"""Offline repository-rename regressions; no signing or publication requests."""

import copy
import importlib.util
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile
import unittest

from octet_release_identity import CANONICAL_REPOSITORY, LEGACY_RELEASE_COMMIT, release_repository

SCRIPTS = Path(__file__).resolve().parent
LEGACY_REPOSITORY = "skaft-software/ygg"


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


metadata = load_script("generate-octet-release-metadata")
formula = load_script("generate-homebrew-formula")
npm = load_script("create-octet-npm-manifest")


def workflow(repository, version):
    return f"{repository}/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v{version}"


class RepositoryIdentityTests(unittest.TestCase):
    def setUp(self):
        self.fixture = json.loads((SCRIPTS / "fixtures/homebrew/OCTET_RELEASE_METADATA.json").read_text())

    def legacy_metadata(self):
        value = copy.deepcopy(self.fixture)
        value.update(repository=LEGACY_REPOSITORY, source_commit=LEGACY_RELEASE_COMMIT,
                     workflow_commit=LEGACY_RELEASE_COMMIT,
                     workflow_ref=workflow(LEGACY_REPOSITORY, "0.7.0"))
        for asset in value["assets"]:
            asset["url"] = asset["url"].replace(CANONICAL_REPOSITORY, LEGACY_REPOSITORY)
        return value

    def test_only_exact_published_source_uses_legacy_identity(self):
        self.assertEqual(release_repository("0.7.0", LEGACY_RELEASE_COMMIT, LEGACY_RELEASE_COMMIT), LEGACY_REPOSITORY)
        for args in [("0.7.1", LEGACY_RELEASE_COMMIT, LEGACY_RELEASE_COMMIT),
                     ("0.7.0", "a" * 40, LEGACY_RELEASE_COMMIT),
                     ("0.7.0", LEGACY_RELEASE_COMMIT, "a" * 40)]:
            with self.subTest(args=args):
                self.assertEqual(release_repository(*args), CANONICAL_REPOSITORY)

    def test_metadata_requires_exact_repository_and_workflow(self):
        for version, source, commit, repository in [
            ("0.7.0", LEGACY_RELEASE_COMMIT, LEGACY_RELEASE_COMMIT, LEGACY_REPOSITORY),
            ("0.7.1", "a" * 40, "b" * 40, CANONICAL_REPOSITORY),
        ]:
            with self.subTest(version=version):
                metadata.validate_identity(version, f"v{version}", source, commit, workflow(repository, version), repository)
                other = CANONICAL_REPOSITORY if repository == LEGACY_REPOSITORY else LEGACY_REPOSITORY
                with self.assertRaises(metadata.MetadataError):
                    metadata.validate_identity(version, f"v{version}", source, commit, workflow(other, version), other)
                with self.assertRaises(metadata.MetadataError):
                    metadata.validate_identity(version, f"v{version}", source, commit, workflow(other, version), repository)

    def test_formula_accepts_current_fixture_and_historical_metadata(self):
        for value in [self.fixture, self.legacy_metadata()]:
            with self.subTest(repository=value["repository"]):
                tag, version, assets, checksum = formula.parse_metadata(value)
                rendered = formula.render_formula(tag, version, value["source_commit"], value["workflow_commit"], value["workflow_ref"], checksum, assets)
                self.assertIn(f'https://github.com/{CANONICAL_REPOSITORY}/releases/download/v0.7.0/', rendered)
                self.assertIn(value["workflow_ref"], rendered)

    def test_formula_rejects_mixed_or_unbound_historical_identity(self):
        for field, value in [("source_commit", "a" * 40), ("workflow_commit", "b" * 40),
                             ("repository", CANONICAL_REPOSITORY),
                             ("workflow_ref", workflow(CANONICAL_REPOSITORY, "0.7.0"))]:
            changed = self.legacy_metadata()
            changed[field] = value
            with self.subTest(field=field), self.assertRaises(formula.FormulaError):
                formula.parse_metadata(changed)
        changed = self.legacy_metadata()
        changed["assets"][0]["url"] = changed["assets"][0]["url"].replace(LEGACY_REPOSITORY, CANONICAL_REPOSITORY)
        with self.assertRaises(formula.FormulaError):
            formula.parse_metadata(changed)

    def test_npm_can_consume_unchanged_historical_native_metadata(self):
        value = self.legacy_metadata()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "metadata.json"
            path.write_text(json.dumps(value))
            npm.read_release_metadata(path, "0.7.0", "v0.7.0", LEGACY_RELEASE_COMMIT, LEGACY_RELEASE_COMMIT)
            value["repository"] = CANONICAL_REPOSITORY
            path.write_text(json.dumps(value))
            with self.assertRaises(npm.ManifestError):
                npm.read_release_metadata(path, "0.7.0", "v0.7.0", LEGACY_RELEASE_COMMIT, LEGACY_RELEASE_COMMIT)
        self.assertEqual(npm.REPOSITORY, CANONICAL_REPOSITORY)

    def test_installer_selects_one_exact_signature_identity(self):
        text = (SCRIPTS / "install.sh").read_text()
        block = text.split('    signing_repository="$repository"', 1)[1].split('    python3 - \\\n', 1)[0]
        block = 'signing_repository="$repository"' + block
        for version, source, expected in [
            ("0.7.0", LEGACY_RELEASE_COMMIT, LEGACY_REPOSITORY),
            ("0.7.0", "a" * 40, CANONICAL_REPOSITORY),
            ("0.7.1", LEGACY_RELEASE_COMMIT, CANONICAL_REPOSITORY),
        ]:
            with self.subTest(version=version, source=source):
                script = f'repository="{CANONICAL_REPOSITORY}"\nversion="{version}"\nrelease_source_commit="{source}"\n' + block + '\nprintf "%s\\n%s\\n" "$signing_repository" "$identity"\n'
                lines = subprocess.check_output(["sh", "-eu", "-c", script], text=True, env={"PATH": os.defpath}).splitlines()
                self.assertEqual(lines[0], expected)
                escaped_version = version.replace(".", r"\.")
                self.assertEqual(lines[1], rf"^https://github\.com/{expected}/\.github/workflows/release-octet\.yml@refs/tags/(v{escaped_version}|octet-binaries-v{escaped_version})$")


class ReleaseDocumentationTests(unittest.TestCase):
    """Bundled user docs must not describe the release as an unavailable candidate.

    These checks validate source prose, not publication, signatures or physical
    acceptance. Those remain release-workflow and maintainer evidence.
    """

    def test_current_version_has_release_only_notes(self):
        root = SCRIPTS.parent
        version = re.search(r'^version = "([^"]+)"$',
                            (root / "Cargo.toml").read_text(), re.MULTILINE).group(1)
        notes = (root / "docs/releases" / f"v{version}.md").read_text()
        self.assertEqual(notes.splitlines()[0], f"# octet {version}")
        normalized = " ".join(notes.lower().split())
        for stale in ("unpublished", "source candidate", "publication remains blocked",
                      "release gate — open", "acceptance is unrun"):
            self.assertNotIn(stale, normalized)
        self.assertRegex(notes, r"(?m)^## (Fixed|Added|Changed|Highlights)$")
        for name in ("README.md", "docs/installation.md"):
            with self.subTest(path=name):
                self.assertIn(f"/releases/download/v{version}/install-octet.sh",
                              (root / name).read_text())

    def test_current_installation_guides_do_not_keep_candidate_gate_text(self):
        for name in ("README.md", "docs/README.md", "docs/installation.md",
                     "docs/distribution.md"):
            normalized = " ".join((SCRIPTS.parent / name).read_text().lower().split())
            with self.subTest(path=name):
                self.assertNotIn("unpublished candidate", normalized)
                self.assertNotIn("unpublished source candidate", normalized)
                self.assertNotIn("physical acceptance is unrun", normalized)
                self.assertNotIn("physical acceptance remains unrun", normalized)


if __name__ == "__main__":
    unittest.main()
