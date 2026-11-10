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


def documented_versions(root=SCRIPTS.parent):
    """Workspace version, candidate state and the install target the docs name.

    A candidate's checked-in installer targets its source version, while public
    download instructions retain the preceding release until explicit promotion.
    These offline checks cannot prove publication; SDK/registry channels remain
    independent.
    """
    version = re.search(r'^version = "([^"]+)"$',
                        (root / "Cargo.toml").read_text(), re.MULTILINE).group(1)
    changelog = (root / "CHANGELOG.md").read_text()
    releases = re.findall(r"^## \[([0-9]+\.[0-9]+\.[0-9]+)\]", changelog, re.MULTILINE)
    assert releases and releases[0] == version, f"CHANGELOG.md must open with {version}"
    if f"## [{version}] - Release candidate" not in changelog.splitlines():
        return version, False, version
    assert len(releases) > 1, f"CHANGELOG.md lists no release before candidate {version}"
    return version, True, releases[1]


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

    def test_metadata_accepts_canonical_tag_only_for_exact_source(self):
        version = "0.10.3"
        source = "a" * 40
        ref = f"{CANONICAL_REPOSITORY}/.github/workflows/release-octet.yml@refs/tags/v{version}"
        metadata.validate_identity(version, f"v{version}", source, source, ref, CANONICAL_REPOSITORY)
        with self.assertRaises(metadata.MetadataError):
            metadata.validate_identity(version, f"v{version}", source, "b" * 40, ref, CANONICAL_REPOSITORY)
        for wrong_ref in (ref + "-rc.1", ref.replace(version, "0.10.2"),
                          ref.replace("refs/tags/", "refs/heads/")):
            with self.subTest(ref=wrong_ref), self.assertRaises(metadata.MetadataError):
                metadata.validate_identity(version, f"v{version}", source, source,
                                           wrong_ref, CANONICAL_REPOSITORY)

    def test_formula_accepts_current_fixture_and_historical_metadata(self):
        for value in [self.fixture, self.legacy_metadata()]:
            with self.subTest(repository=value["repository"]):
                tag, version, assets, checksum = formula.parse_metadata(value)
                rendered = formula.render_formula(tag, version, value["source_commit"], value["workflow_commit"], value["workflow_ref"], checksum, assets)
                self.assertIn(f'https://github.com/{CANONICAL_REPOSITORY}/releases/download/v0.7.0/', rendered)
                self.assertIn(value["workflow_ref"], rendered)

    def test_formula_and_npm_require_exact_canonical_or_legacy_signer(self):
        value = copy.deepcopy(self.fixture)
        value["workflow_commit"] = value["source_commit"]
        prefix = f"{CANONICAL_REPOSITORY}/.github/workflows/release-octet.yml@refs/tags/"
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "metadata.json"
            def validate():
                path.write_text(json.dumps(value))
                formula.parse_metadata(value)
                npm.read_release_metadata(path, "0.7.0", "v0.7.0", value["source_commit"], value["workflow_commit"])
            for ref in (prefix + "v0.7.0", prefix + "octet-binaries-v0.7.0"):
                value["workflow_ref"] = ref
                validate()
            for ref in (prefix + "v0.7.1", prefix + "v0.7.0-rc.1", prefix.replace("tags", "heads") + "v0.7.0",
                        prefix + "octet-binaries-v0.7.1"):
                value["workflow_ref"] = ref
                with self.subTest(ref=ref):
                    with self.assertRaises(formula.FormulaError):
                        formula.parse_metadata(value)
                    path.write_text(json.dumps(value))
                    with self.assertRaises(npm.ManifestError):
                        npm.read_release_metadata(path, "0.7.0", "v0.7.0", value["source_commit"], value["workflow_commit"])
            value["workflow_ref"] = prefix + "v0.7.0"
            value["workflow_commit"] = "b" * 40
            with self.assertRaises(formula.FormulaError):
                formula.parse_metadata(value)
            path.write_text(json.dumps(value))
            with self.assertRaises(npm.ManifestError):
                npm.read_release_metadata(path, "0.7.0", "v0.7.0", value["source_commit"], value["workflow_commit"])

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

    def test_repair_installer_does_not_trust_canonical_ref_for_a_different_signer_commit(self):
        text = (SCRIPTS / "install.sh").read_text()
        block = 'signing_repository="$repository"' + text.split('    signing_repository="$repository"', 1)[1].split('    python3 - \\\n', 1)[0]
        for workflow_commit, canonical_allowed in (("a" * 40, True), ("b" * 40, False)):
            script = (f'repository="{CANONICAL_REPOSITORY}"\nversion="0.10.3"\n'
                      f'release_source_commit="{"a" * 40}"\nrelease_workflow_commit="{workflow_commit}"\n'
                      + block + '\nprintf "%s\\n" "$identity"\n')
            identity = subprocess.check_output(["sh", "-eu", "-c", script], text=True, env={"PATH": os.defpath}).strip()
            prefix = f"https://github.com/{CANONICAL_REPOSITORY}/.github/workflows/release-octet.yml@refs/tags/"
            self.assertEqual(bool(re.fullmatch(identity, prefix + "v0.10.3")), canonical_allowed)
            self.assertIsNotNone(re.fullmatch(identity, prefix + "octet-binaries-v0.10.3"))
            self.assertIsNone(re.fullmatch(identity, prefix + "v0.10.4"))


class SourceDistributionVersionTests(unittest.TestCase):
    """Release distribution identities stay aligned; API versions are independent."""

    def setUp(self):
        self.root = SCRIPTS.parent
        self.version = re.search(r'^version = "([^"]+)"$',
                                 (self.root / "Cargo.toml").read_text(), re.MULTILINE).group(1)

    def test_first_party_source_manifests_and_installer_pins(self):
        name = "Cargo.lock"
        entries = re.findall(r'name = "(octet-[^"]+)"\nversion = "([^"]+)"',
                             (self.root / name).read_text())
        self.assertEqual(len(entries), 5)
        for package, version in entries:
            with self.subTest(path=name, package=package):
                self.assertEqual(version, self.version)
        for name in ("crates/octet-agent/Cargo.toml", "crates/octet-coding-agent/Cargo.toml"):
            versions = re.findall(r'^octet-[^ ]+ = \{ version = "=([^"]+)"',
                                  (self.root / name).read_text(), re.MULTILINE)
            self.assertTrue(versions, name)
            self.assertEqual(set(versions), {self.version}, name)
        # Source package/installer versions follow the current release. Public download
        # links are checked separately against the published release.
        self.assertIn(f'\nversion = "{self.version}"\n',
                      (self.root / "sdk/python/pyproject.toml").read_text())
        self.assertEqual(json.loads((self.root / "sdk/typescript/package.json").read_text())["version"],
                         self.version)
        self.assertIn(f'\nversion="{self.version}"\n', (SCRIPTS / "install.sh").read_text())
        # The release workflow republishes this installer verbatim, so its
        # embedded pin is what a Windows install actually fetches.
        self.assertIn(f"[string] $Version = '{self.version}'",
                      (SCRIPTS / "install-octet.ps1").read_text())
        for package in ("octet-codemode", "octet-computer-use", "octet-mcp", "octet-pi-compat", "octet-subagents", "octet-web-search"):
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
    def test_workspace_tests_have_separate_build_and_execution_budgets(self):
        ci = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        job = ci.split("\n  test:\n", 1)[1].split("\n  windows:\n", 1)[0]
        command = "cargo test --workspace --all-targets --all-features --profile ci-test --locked"
        build = "\n        run: " + command + " --no-run\n"
        execute = "\n        run: " + command + " --no-fail-fast\n"
        self.assertIn(build, job)
        self.assertIn(execute, job)
        self.assertLess(job.index(build), job.index(execute))
        # Cold compilation must not consume the existing test execution budget.
        for name, run in (("Build workspace test binaries", build),
                          ("Run workspace tests", execute)):
            self.assertIn("      - name: " + name + "\n"
                          "        timeout-minutes: 15" + run, job)
        self.assertIn('          OCTET_PTY_TRACE: "1"', job)

    def test_node_adapter_tests_reuse_the_prepared_pi_reference(self):
        ci = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        job = ci.split("\n  test:\n", 1)[1].split("\n  windows:\n", 1)[0]
        self.assertLess(job.index("Install reviewed Pi adapter test dependencies"),
                        job.index("Build workspace test binaries"))
        self.assertLess(job.index("Provide the pinned Pi provider package"),
                        job.index("Build workspace test binaries"))
        step = job.split("      - name: Test the Node extension adapters in the release catalog\n", 1)[1]
        run = step.split("        run: |\n", 1)[1].split("      - run:", 1)[0]
        script = "\n".join(line[10:] for line in run.splitlines())
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "extensions").mkdir()
            (root / "extensions/release-catalog.txt").write_text("octet-pi-compat\ntest-node-adapter\n")
            for name in ("octet-pi-compat", "test-node-adapter"):
                package = root / "extensions" / name
                package.mkdir()
                (package / "package.json").write_text("{}")
            result = subprocess.run(
                ["bash", "-euc", 'npm() { printf "%s\\n" "$*"; }\n' + script],
                cwd=root, env={"PATH": os.defpath}, text=True, capture_output=True,
            )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), [
            "test --prefix extensions/octet-pi-compat",
            "ci --prefix extensions/test-node-adapter --ignore-scripts --no-audit --no-fund",
            "test --prefix extensions/test-node-adapter",
        ])

    def test_workspace_packaging_installs_its_explicit_toolchain(self):
        ci = (SCRIPTS.parent / ".github/workflows/ci.yml").read_text()
        quality = ci.split("\n  quality:\n", 1)[1].split("\n  first-party-extension-tests:\n", 1)[0]
        install = "rustup toolchain install 1.90.0 --profile minimal --no-self-update"
        package = "cargo +1.90.0 package --workspace --exclude octet-coding-agent --locked --no-verify"
        self.assertIn(install, quality)
        self.assertIn(package, quality)
        self.assertLess(quality.index(install), quality.index(package))
        # This immutable action is generated for 1.89, not the configurable action.
        self.assertNotRegex(
            ci,
            r"uses: dtolnay/rust-toolchain@599c45df1a24c1c63ae1129a7e59f46654b8fb66"
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
        version, candidate, install_version = documented_versions(root)
        notes = (root / "docs/releases" / f"v{version}.md").read_text()
        self.assertEqual(notes.splitlines()[0], f"# octet {version}")
        normalized = " ".join(notes.lower().split())
        self.assertRegex(notes, r"(?m)^## (Fixed|Added|Changed|Highlights)$")
        if candidate:
            self.assertIn("unreleased source candidate", normalized)
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
        version, candidate, install_version = documented_versions()
        self.assertIn(f"/releases/tag/v{install_version}", text)
        self.assertIn(f"distribution target is **{version}**", text)
        if candidate:
            self.assertIn("source candidate, not a published release", text)
        else:
            self.assertNotIn("source candidate, not a published release", text)
            self.assertIn("`octet-pi-compat`", text)
        self.assertIn("does not change independent API and schema versions", " ".join(text.split()))
        self.assertIn(f"planned npm channel is `@skaft/octet@{version}` (launcher plus four signed platform packages, with provenance). Homebrew, crates.io and SDK registries remain separate, unpublished channels.", " ".join(text.split()))
        self.assertIn(f"**{version} is not published to npm.**", text)


if __name__ == "__main__":
    unittest.main()
