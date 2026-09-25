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
# Explicitly promoted native release; SDK/registry publication stays independent.
PUBLISHED_NATIVE_VERSION = "0.8.0"


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


class SourceDistributionVersionTests(unittest.TestCase):
    """Release distribution identities stay aligned; API versions are independent."""

    def setUp(self):
        self.root = SCRIPTS.parent
        self.version = re.search(r'^version = "([^"]+)"$',
                                 (self.root / "Cargo.toml").read_text(), re.MULTILINE).group(1)

    def test_first_party_source_manifests_and_installer_pins(self):
        for name in ("Cargo.lock", "extensions/octet-serve/Cargo.lock"):
            entries = re.findall(r'name = "(octet-[^"]+)"\nversion = "([^"]+)"',
                                 (self.root / name).read_text())
            self.assertEqual(len(entries), 5 if name == "Cargo.lock" else 3)
            for package, version in entries:
                with self.subTest(path=name, package=package):
                    self.assertEqual(version, self.version)
        for name in ("crates/octet-agent/Cargo.toml", "crates/octet-coding-agent/Cargo.toml",
                     "extensions/octet-serve/Cargo.toml"):
            versions = re.findall(r'^octet-[^ ]+ = \{ version = "=([^"]+)"',
                                  (self.root / name).read_text(), re.MULTILINE)
            self.assertTrue(versions, name)
            self.assertEqual(set(versions), {self.version}, name)
        self.assertIn(f'\nversion = "{self.version}"\n',
                      (self.root / "extensions/octet-serve/Cargo.toml").read_text())
        # Source package/installer versions follow the candidate. Public download
        # links are checked separately against the preceding published release.
        self.assertIn(f'\nversion = "{self.version}"\n',
                      (self.root / "sdk/python/pyproject.toml").read_text())
        self.assertEqual(json.loads((self.root / "sdk/typescript/package.json").read_text())["version"],
                         self.version)
        self.assertIn(f'\nversion="{self.version}"\n', (SCRIPTS / "install.sh").read_text())
        for package in ("octet-browse", "octet-mcp", "octet-subagents", "octet-web-search"):
            manifest = (self.root / "extensions" / package / "extension.toml").read_text()
            with self.subTest(package=package):
                self.assertIn(f'\nversion = "{self.version}"\n', manifest)
                self.assertIn(f'\nrequires_octet = "={self.version}"\n', manifest)
                self.assertIn('\napi_version = "0.4"\n', manifest)

    def test_current_notes_are_identical_and_in_the_finite_documentation_inventory(self):
        name = f"docs/releases/v{self.version}.md"
        canonical = (self.root / name).read_bytes()
        bundled = self.root / f"crates/octet-coding-agent/src/tui/view/releases/v{self.version}.md"
        self.assertEqual(canonical, bundled.read_bytes())
        self.assertIn(f"text {name}", (self.root / "docs/package-assets.txt").read_text().splitlines())


class ReleaseToolchainTests(unittest.TestCase):
    def test_workspace_packaging_installs_its_explicit_toolchain(self):
        ci = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        quality = ci.split("\n  quality:\n", 1)[1].split("\n  first-party-extension-tests:\n", 1)[0]
        install = "rustup toolchain install 1.90.0 --profile minimal --no-self-update"
        package = "cargo +1.90.0 package --workspace --exclude octet-coding-agent --locked --no-verify"
        self.assertIn(install, quality)
        self.assertIn(package, quality)
        self.assertLess(quality.index(install), quality.index(package))
        # This immutable action is generated for 1.86, not the configurable action.
        self.assertNotRegex(
            ci,
            r"uses: dtolnay/rust-toolchain@52699249a776424c51ebc9ee197baf0f9dbf0d8a"
            r"\n\s+with:\n\s+toolchain:",
        )


class ReleaseDocumentationTests(unittest.TestCase):
    """Keep candidate and published installation guidance distinct and versioned.

    A candidate's checked-in installer targets its source version, while public
    download instructions retain the preceding release until explicit promotion.
    These checks do not establish publication, signatures or live acceptance.
    """

    def test_current_version_has_release_only_notes(self):
        root = SCRIPTS.parent
        version = re.search(r'^version = "([^"]+)"$',
                            (root / "Cargo.toml").read_text(), re.MULTILINE).group(1)
        notes = (root / "docs/releases" / f"v{version}.md").read_text()
        self.assertEqual(notes.splitlines()[0], f"# octet {version}")
        normalized = " ".join(notes.lower().split())
        self.assertRegex(notes, r"(?m)^## (Fixed|Added|Changed|Highlights)$")
        changelog = (root / "CHANGELOG.md").read_text()
        candidate = f"## [{version}] - Release candidate" in changelog.splitlines()
        install_version = version
        if candidate:
            releases = re.findall(r"^## \[([0-9]+\.[0-9]+\.[0-9]+)\]", changelog, re.MULTILINE)
            self.assertEqual(releases[0], version)
            self.assertGreater(len(releases), 1)
            install_version = releases[1]
            self.assertIn("unreleased source candidate", normalized)
            self.assertEqual(install_version, PUBLISHED_NATIVE_VERSION)
        for name in ("README.md", "docs/installation.md"):
            with self.subTest(path=name):
                guide = (root / name).read_text()
                self.assertIn(f"/releases/download/v{install_version}/install-octet.sh", guide)
                if candidate:
                    self.assertNotIn(f"/releases/download/v{version}/install-octet.sh", guide)
                    self.assertIn(version, guide)
                    self.assertRegex(guide.lower(), r"release candidate|local " + re.escape(version) + r" rc")

    def test_distribution_notes_keep_candidate_and_publication_distinct(self):
        text = (SCRIPTS.parent / "docs/distribution.md").read_text()
        version = re.search(r'^version = "([^"]+)"$',
                            (SCRIPTS.parent / "Cargo.toml").read_text(), re.MULTILINE).group(1)
        self.assertIn(f"/releases/tag/v{PUBLISHED_NATIVE_VERSION}", text)
        self.assertIn(f"distribution is **{version}**", text)
        self.assertIn("does not change independent API and schema versions", " ".join(text.split()))
        self.assertIn("npm, Homebrew, crates.io and SDK registries remain separate, unpublished channels.", text)


if __name__ == "__main__":
    unittest.main()
