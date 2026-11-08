"""Complete-assets-before-Latest contracts; no GitHub, cosign or Cargo is invoked."""

import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / "scripts/validate-release-completeness.py"
SPEC = importlib.util.spec_from_file_location("release_completeness", PATH)
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)

OCTET = ROOT / ".github/workflows/release-octet.yml"
SERVE = ROOT / ".github/workflows/release-serve.yml"
VERSION = "0.10.4"
TAG = f"v{VERSION}"
SOURCE = "c" * 40
EXTENSIONS = (
    "octet-codemode",
    "octet-computer-use",
    "octet-mcp",
    "octet-pi-compat",
    "octet-subagents",
    "octet-web-search",
)
SIGNATURE = ".sigstore.json"
NPM_EVIDENCE = (
    f"octet-darwin-arm64-{VERSION}.tgz",
    f"octet-darwin-x64-{VERSION}.tgz",
    f"octet-linux-x64-gnu-{VERSION}.tgz",
    f"octet-win32-x64-{VERSION}.tgz",
    f"octet-{VERSION}.tgz",
    "OCTET_NPM_SHA256SUMS",
    "OCTET_NPM_MANIFEST.json",
)
# The GitHub REST release document, exactly as `gh api
# repos/{repo}/releases/tags/{tag}` renders it. The field names are snake_case;
# `gh release view --json` uses camelCase instead and is therefore not accepted.
REST_ASSET_KEYS = ("digest", "name", "size")
# The two core attachment jobs that stage a *new* release: the native
# publication and the extension-bundle publication. The installer portability
# repair and the npm evidence upload are post-publication supplements to an
# already published release, so they are deliberately not staging guards.
CORE_STAGING_JOBS = (("octet", "publish"), ("serve", "publish"))


def digest(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def payload(name: str) -> bytes:
    return f"immutable fixture payload for {name}".encode("utf-8")


def manifest(names) -> bytes:
    """Render `sha256sum ./*` output: digest, two spaces, `./name`."""
    return "".join(f"{digest(payload(name))}  ./{name}\n" for name in names).encode("ascii")


class Fixture:
    """A synthetic, complete published release plus every input the gate reads."""

    def __init__(self, root: Path):
        self.root = root
        self.native = [
            "install-octet.sh",
            "install-octet.ps1",
            f"octet-{VERSION}-aarch64-apple-darwin.tar.gz",
            f"octet-{VERSION}-x86_64-apple-darwin.tar.gz",
            f"octet-{VERSION}-x86_64-unknown-linux-gnu.tar.gz",
            f"octet-{VERSION}-x86_64-pc-windows-msvc.zip",
        ]
        self.bundles = [
            f"{extension}-{VERSION}.tar.gz"
            for extension in EXTENSIONS
            if extension != "octet-codemode"
        ]
        self.bundles += [
            f"octet-codemode-{VERSION}-{target}.tar.gz" for target in MODULE.CODEMODE_TARGETS
        ]
        self.extra_assets = []
        self.draft = False
        self.prerelease = False
        self.tag_name = TAG
        self.metadata_overrides = {}
        self.write()

    indent = 2

    def metadata_bytes(self) -> bytes:
        return (json.dumps(self.metadata(), indent=self.indent, sort_keys=True) + "\n").encode(
            "utf-8")

    def metadata(self) -> dict:
        document = {
            "schema": "octet.release.metadata.v1",
            "repository": "skaft-software/octet",
            "tag": TAG,
            "version": VERSION,
            "source_commit": SOURCE,
            "workflow_commit": SOURCE,
            "workflow_ref": (
                "skaft-software/octet/.github/workflows/release-octet.yml"
                "@refs/tags/octet-binaries-v" + VERSION
            ),
            "checksum_manifest": {
                "name": "OCTET_SHA256SUMS",
                "sha256": digest(manifest(self.native)),
            },
            "assets": [
                {
                    "name": name,
                    "kind": "installer" if name.startswith("install-octet") else "binary",
                    "sha256": digest(payload(name)),
                    "url": (
                        "https://github.com/skaft-software/octet/releases/download/"
                        f"{TAG}/{name}"
                    ),
                }
                for name in self.native
            ],
        }
        document.update(self.metadata_overrides)
        return document

    def control_payload(self, name):
        """The exact published bytes of a control document."""
        return (self.root / name).read_bytes()

    def published(self) -> dict:
        """The REST release document, with the real digest of every published byte."""
        core = [*self.native, *self.bundles]
        documents = ["OCTET_SHA256SUMS", "SHA256SUMS", "OCTET_RELEASE_METADATA.json"]
        entries = [
            {
                "name": name,
                "digest": f"sha256:{digest(payload(name))}",
                "size": len(payload(name)),
            }
            for name in core
        ]
        entries += [
            {
                "name": name,
                "digest": f"sha256:{digest(self.control_payload(name))}",
                "size": len(self.control_payload(name)),
            }
            for name in documents
        ]
        # Every core payload and control document carries its signature bundle.
        entries += [
            {
                "name": f"{entry['name']}{SIGNATURE}",
                "digest": f"sha256:{digest(payload(entry['name']))}",
                "size": len(payload(entry["name"])),
            }
            for entry in entries
        ]
        # `extra_assets` are literal names, modelled exactly as listed: a test
        # asks for a signature by naming one, which is how an unsigned or orphan
        # asset is expressed at all.
        entries += [
            {
                "name": name,
                "digest": f"sha256:{digest(payload(name))}",
                "size": len(payload(name)),
            }
            for name in self.extra_assets
        ]
        return {
            "tag_name": self.tag_name,
            "draft": self.draft,
            "prerelease": self.prerelease,
            "target_commitish": SOURCE,
            "assets": entries,
        }

    def write(self):
        """Regenerate every derived input from the fixture state."""
        self.root.mkdir(parents=True, exist_ok=True)
        (self.root / "OCTET_SHA256SUMS").write_bytes(manifest(self.native))
        (self.root / "SHA256SUMS").write_bytes(manifest(self.bundles))
        (self.root / "OCTET_RELEASE_METADATA.json").write_bytes(self.metadata_bytes())
        # The release document is written last because it carries the digests of
        # the control documents written immediately above.
        (self.root / "release.json").write_text(json.dumps(self.published()), encoding="utf-8")
        (self.root / "catalog.txt").write_text("\n".join(EXTENSIONS) + "\n", encoding="utf-8")
        return self

    def edit(self, name, mutate):
        """Rewrite one input after `write`, to model a single real-world defect."""
        path = self.root / name
        document = json.loads(path.read_text())
        mutate(document)
        path.write_text(json.dumps(document), encoding="utf-8")
        return self

    def edit_manifest(self, name, old_name, new_digest):
        path = self.root / name
        lines = path.read_text(encoding="ascii").splitlines()
        path.write_text("".join(
            f"{new_digest}  ./{entry.split('  ./', 1)[1]}\n"
            if entry.endswith("  ./" + old_name) else entry + "\n"
            for entry in lines
        ), encoding="ascii")
        return self

    def arguments(self):
        return [
            "--release-json", str(self.root / "release.json"),
            "--native-manifest", str(self.root / "OCTET_SHA256SUMS"),
            "--bundle-manifest", str(self.root / "SHA256SUMS"),
            "--metadata", str(self.root / "OCTET_RELEASE_METADATA.json"),
            "--catalog", str(self.root / "catalog.txt"),
            "--version", VERSION,
            "--tag", TAG,
            "--source-commit", SOURCE,
        ]


class CompletenessGateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.fixture = Fixture(self.root / "release")

    def run_gate(self, arguments=None):
        return subprocess.run(
            [sys.executable, str(PATH), *(arguments or self.fixture.arguments())],
            capture_output=True, text=True, timeout=60,
        )

    def assert_incomplete(self, message, arguments=None):
        result = self.run_gate(arguments)
        self.assertNotEqual(0, result.returncode, result.stdout)
        self.assertIn(message, result.stderr)
        self.assertNotIn("is complete", result.stdout)

    def test_release_document_shape_is_the_github_rest_document(self):
        document = self.fixture.published()
        self.assertEqual(set(document), {"tag_name", "draft", "prerelease", "target_commitish",
                                         "assets"})
        self.assertEqual(set(document["assets"][0]), set(REST_ASSET_KEYS))

    def test_complete_release_passes_and_reports_the_union(self):
        result = self.run_gate()
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn(f"release {TAG} is complete", result.stdout)
        with tempfile.TemporaryDirectory() as directory:
            report = Path(directory) / "report.json"
            rechecked = self.run_gate([*self.fixture.arguments(), "--report", str(report)])
            self.assertEqual(0, rechecked.returncode, rechecked.stderr)
            document = json.loads(report.read_text())
        self.assertEqual(document["schema"], "octet.release.completeness.v1")
        self.assertTrue(document["complete"])
        self.assertEqual([], document["problems"])
        self.assertEqual(document["tag"], TAG)
        self.assertEqual(document["version"], VERSION)
        self.assertEqual(document["source_commit"], SOURCE)
        self.assertEqual(document["bundles"]["extensions"], sorted(EXTENSIONS))
        self.assertEqual(document["bundles"]["codemode_targets"], sorted(MODULE.CODEMODE_TARGETS))
        self.assertEqual(len(document["native"]["entries"]), 6)
        self.assertEqual(len(document["bundles"]["entries"]), len(self.fixture.bundles))
        payloads = 6 + len(self.fixture.bundles) + 3
        self.assertEqual(document["assets"]["count"], 2 * payloads)
        self.assertEqual(document["assets"]["required"], 2 * payloads)
        self.assertEqual(document["assets"]["optional"], [])
        self.assertRegex(document["assets"]["fingerprint"], r"^[0-9a-f]{64}$")

    def test_camel_case_release_document_is_refused_instead_of_read_as_null(self):
        camel = {
            "tagName": TAG,
            "isDraft": False,
            "isPrerelease": False,
            "assets": self.fixture.published()["assets"],
        }
        (self.fixture.root / "release.json").write_text(json.dumps(camel), encoding="utf-8")
        result = self.run_gate()
        self.assertEqual(1, result.returncode)
        self.assertIn("release document is not the REST release document: no tag_name",
                      result.stderr)

    def test_every_catalog_extension_needs_its_exact_versioned_bundle(self):
        self.fixture.bundles = [
            name for name in self.fixture.bundles
            if not name.startswith("octet-subagents-")
        ]
        self.assert_incomplete(
            f"SHA256SUMS omits the required bundle: octet-subagents-{VERSION}.tar.gz",
            self.fixture.write().arguments())

    def test_codemode_requires_every_target_and_rejects_the_generic_launcher(self):
        for missing in MODULE.CODEMODE_TARGETS:
            with self.subTest(missing=missing):
                fixture = Fixture(self.root / f"codemode-{missing}")
                archive = f"octet-codemode-{VERSION}-{missing}.tar.gz"
                fixture.bundles = [name for name in fixture.bundles if name != archive]
                self.assert_incomplete(
                    f"SHA256SUMS omits the required bundle: {archive}",
                    fixture.write().arguments())
        generic = Fixture(self.root / "codemode-generic")
        generic.bundles = [*generic.bundles, f"octet-codemode-{VERSION}.tar.gz"]
        self.assert_incomplete(
            f"carries the historical generic octet-codemode bundle",
            generic.write().arguments())

    def test_unknown_targets_and_target_suffixed_generic_bundles_are_rejected(self):
        cases = (
            (f"octet-codemode-{VERSION}-arm-unknown-linux-gnueabihf.tar.gz", "unexpected bundle"),
            (f"octet-mcp-{VERSION}-x86_64-unknown-linux-gnu.tar.gz", "unexpected bundle"),
            (f"octet-mcp-0.9.6.tar.gz", "unexpected bundle"),
            (f"octet-unknown-{VERSION}.tar.gz", "unexpected bundle"),
        )
        for name, message in cases:
            with self.subTest(name=name):
                fixture = Fixture(self.root / re.sub(r"[^a-z0-9]+", "-", name.lower()))
                fixture.bundles = [*fixture.bundles, name]
                self.assert_incomplete(message, fixture.write().arguments())

    def test_missing_native_archive_fails(self):
        windows_zip = f"octet-{VERSION}-x86_64-pc-windows-msvc.zip"
        self.fixture.native = [name for name in self.fixture.native if name != windows_zip]
        self.assert_incomplete(
            f"OCTET_SHA256SUMS omits the native asset: {windows_zip}",
            self.fixture.write().arguments())

    def test_unmanifested_native_asset_fails(self):
        self.fixture.native = [*self.fixture.native, f"octet-{VERSION}-x86_64-pc-windows-gnu.zip"]
        self.assert_incomplete(
            "OCTET_SHA256SUMS carries an unexpected native asset",
            self.fixture.write().arguments())

    def test_published_digest_must_match_the_signed_manifest(self):
        target = f"octet-{VERSION}-x86_64-unknown-linux-gnu.tar.gz"
        self.fixture.edit_manifest("OCTET_SHA256SUMS", target, "0" * 64)
        self.assert_incomplete("release digest disagrees with the signed manifest for " + target)

    def test_published_asset_without_immutable_digest_fails(self):
        target = f"octet-{VERSION}-x86_64-apple-darwin.tar.gz"

        def drop(document):
            for asset in document["assets"]:
                if asset["name"] == target:
                    asset["digest"] = None

        self.assert_incomplete(
            "release asset has no immutable digest: " + target,
            self.fixture.edit("release.json", drop).arguments())

    def test_every_required_asset_and_signature_must_be_published(self):
        missing_signature = f"octet-mcp-{VERSION}.tar.gz{SIGNATURE}"

        def drop(document):
            document["assets"] = [
                asset for asset in document["assets"] if asset["name"] != missing_signature
            ]

        self.assert_incomplete(
            f"release is missing the required asset: {missing_signature}",
            self.fixture.edit("release.json", drop).arguments())
        other = Fixture(self.root / "no-sums")

        def drop_sums(document):
            document["assets"] = [
                asset for asset in document["assets"] if asset["name"] != "SHA256SUMS"
            ]

        self.assert_incomplete(
            "release is missing the required asset: SHA256SUMS",
            other.edit("release.json", drop_sums).arguments())

    def test_unaccounted_asset_fails_closed(self):
        self.fixture.extra_assets = ["notes.txt", f"notes.txt{SIGNATURE}"]
        self.assert_incomplete(
            "release carries an asset the signed manifests do not account for: notes.txt",
            self.fixture.write().arguments())

    def test_tag_pinned_installer_repair_is_recognised_only_when_signed(self):
        self.fixture.extra_assets = [f"install-octet-{TAG}.sh"]
        self.assert_incomplete(
            "release carries an unsigned optional asset", self.fixture.write().arguments())
        self.fixture.extra_assets = [f"install-octet-{TAG}.sh", f"install-octet-{TAG}.sh{SIGNATURE}"]
        result = self.run_gate(self.fixture.write().arguments())
        self.assertEqual(0, result.returncode, result.stderr)

    def test_npm_evidence_is_a_closed_version_derived_optional_set(self):
        self.fixture.extra_assets = list(NPM_EVIDENCE)
        self.assert_incomplete(
            "release carries an unsigned optional asset", self.fixture.write().arguments())
        self.fixture.extra_assets = [
            *NPM_EVIDENCE, *(f"{name}{SIGNATURE}" for name in NPM_EVIDENCE)
        ]
        result = self.run_gate(self.fixture.write().arguments())
        self.assertEqual(0, result.returncode, result.stderr)
        foreign = Fixture(self.root / "foreign-npm")
        foreign.extra_assets = [*NPM_EVIDENCE, "octet-0.9.6.tgz"]
        self.assert_incomplete(
            "release carries an asset the signed manifests do not account for: octet-0.9.6.tgz",
            foreign.write().arguments())

    def test_control_documents_are_bound_to_the_published_bytes(self):
        """One byte off on any control document must fail the digest binding.

        Dropping the final newline keeps every document parseable and
        semantically identical, so only the exact-byte comparison can catch it.
        """
        for name in ("OCTET_SHA256SUMS", "SHA256SUMS", "OCTET_RELEASE_METADATA.json"):
            with self.subTest(document=name):
                fixture = Fixture(self.root / re.sub(r"[^a-z]", "-", name.lower()))
                body = (fixture.root / name).read_bytes()
                self.assertTrue(body.endswith(b"\n"), name)
                (fixture.root / name).write_bytes(body[:-1])
                self.assert_incomplete(
                    f"supplied {name} bytes are not the published asset",
                    fixture.arguments())

    def test_valid_json_whitespace_is_still_a_different_byte_sequence(self):
        for indent in (4, None):
            with self.subTest(indent=indent):
                fixture = Fixture(self.root / f"whitespace-{indent}")
                fixture.indent = indent if indent is not None else 0
                original = json.loads((fixture.root / "OCTET_RELEASE_METADATA.json").read_text())
                reformatted = json.dumps(original, indent=indent, sort_keys=True) + "\n"
                self.assertEqual(json.loads(reformatted), original)
                (fixture.root / "OCTET_RELEASE_METADATA.json").write_text(
                    reformatted, encoding="utf-8")
                self.assert_incomplete(
                    "supplied OCTET_RELEASE_METADATA.json bytes are not the published asset",
                    fixture.arguments())

    def test_duplicate_metadata_asset_records_are_refused(self):
        metadata = self.fixture.metadata()
        self.fixture.metadata_overrides["assets"] = [
            *metadata["assets"], dict(metadata["assets"][0]),
        ]
        self.assert_incomplete(
            "release metadata lists the asset twice", self.fixture.write().arguments())

    def test_orphan_optional_signature_is_refused(self):
        self.fixture.extra_assets = [f"install-octet-{TAG}.sh{SIGNATURE}"]
        self.assert_incomplete(
            f"release carries an orphan signature for an absent asset: "
            f"install-octet-{TAG}.sh{SIGNATURE}",
            self.fixture.write().arguments())

    def test_tag_must_name_the_version(self):
        for tag, message in (("v0.10.5", "does not name version"),
                             ("v0.11.4", "does not name version")):
            with self.subTest(tag=tag):
                with self.assertRaisesRegex(MODULE.CompletenessError, message):
                    MODULE.validate(
                        release=self.fixture.published(),
                        native_manifest_payload=manifest(self.fixture.native),
                        bundle_manifest_payload=manifest(self.fixture.bundles),
                        metadata_payload=self.fixture.metadata_bytes(),
                        extensions=list(EXTENSIONS),
                        version=VERSION,
                        tag=tag,
                        source_commit=SOURCE,
                    )

    def test_source_commit_is_required(self):
        with self.assertRaises(TypeError):
            MODULE.validate(
                release=self.fixture.published(),
                native_manifest_payload=manifest(self.fixture.native),
                bundle_manifest_payload=manifest(self.fixture.bundles),
                metadata_payload=self.fixture.metadata_bytes(),
                extensions=list(EXTENSIONS),
                version=VERSION,
                tag=TAG,
            )
        arguments = self.fixture.arguments()
        result = self.run_gate(arguments[:arguments.index("--source-commit")])
        self.assertEqual(2, result.returncode)
        self.assertIn("--source-commit", result.stderr)

    def test_draft_and_prerelease_are_never_complete(self):
        for attribute, message in (("draft", "release is still a draft"),
                                   ("prerelease", "release is marked as a prerelease")):
            with self.subTest(attribute=attribute):
                fixture = Fixture(self.root / f"state-{attribute}")
                setattr(fixture, attribute, True)
                self.assert_incomplete(message, fixture.write().arguments())

    def test_release_tag_must_match_the_resolved_tag(self):
        self.fixture.tag_name = "v0.9.6"
        self.assert_incomplete("release tag is 'v0.9.6'", self.fixture.write().arguments())

    def test_metadata_is_bound_to_the_tag_version_and_source(self):
        for field, value, message in (
            ("tag", "v0.10.3", "release metadata tag is"),
            ("version", "0.10.3", "release metadata version is"),
            ("source_commit", "d" * 40, "release metadata source_commit is"),
            ("schema", "octet.release.metadata.v2", "release metadata schema is"),
        ):
            with self.subTest(field=field):
                fixture = Fixture(self.root / f"metadata-{field}")
                fixture.metadata_overrides[field] = value
                self.assert_incomplete(message, fixture.write().arguments())

    def test_metadata_manifest_digest_must_match_the_attached_manifest(self):
        self.fixture.metadata_overrides["checksum_manifest"] = {
            "name": "OCTET_SHA256SUMS", "sha256": "0" * 64,
        }
        self.assert_incomplete(
            "checksum manifest digest disagrees with the manifest",
            self.fixture.write().arguments())

    def test_metadata_must_publish_the_same_native_assets(self):
        metadata = self.fixture.metadata()
        self.fixture.metadata_overrides["assets"] = metadata["assets"][:-1]
        self.assert_incomplete(
            "release metadata omits the native asset", self.fixture.write().arguments())
        other = Fixture(self.root / "metadata-extra")
        metadata = other.metadata()
        stray = dict(metadata["assets"][0], name="octet-0.9.6-x86_64-unknown-linux-gnu.tar.gz")
        other.metadata_overrides["assets"] = [*metadata["assets"], stray]
        self.assert_incomplete(
            "release metadata publishes an unmanifested native asset", other.write().arguments())
        third = Fixture(self.root / "metadata-digest")
        metadata = third.metadata()
        metadata["assets"][0]["sha256"] = "0" * 64
        third.metadata_overrides["assets"] = metadata["assets"]
        self.assert_incomplete(
            "release metadata digest disagrees with the manifest for", third.write().arguments())

    def test_malformed_manifests_fail_before_any_asset_judgement(self):
        for content, message in (
            (b"not a checksum line\n", "has a malformed line"),
            (b"", "lists no assets"),
            (b"\xff\xfe", "is not ASCII"),
            (manifest([self.fixture.bundles[0]]) * 2, "twice"),
        ):
            with self.subTest(content=content[:8]):
                fixture = Fixture(self.root / f"malformed-{content[:6].hex()}")
                (fixture.root / "SHA256SUMS").write_bytes(content)
                result = self.run_gate(fixture.arguments())
                self.assertEqual(1, result.returncode)
                self.assertIn(message, result.stderr)

    def test_manifest_accepts_the_normalised_bare_name_form(self):
        # `sha256sum -c` accepts a manifest rewritten without the `./` prefix;
        # both spellings describe the same bytes. The release document is
        # rebuilt so its digest still names the rewritten manifest exactly.
        self.fixture.write()
        path = self.fixture.root / "SHA256SUMS"
        path.write_text(
            "".join(line.replace("  ./", "  ") + "\n"
                    for line in path.read_text().splitlines()),
            encoding="ascii",
        )
        (self.fixture.root / "release.json").write_text(
            json.dumps(self.fixture.published()), encoding="utf-8")
        result = self.run_gate()
        self.assertEqual(0, result.returncode, result.stderr)

    def test_fingerprint_tracks_the_published_asset_set(self):
        def fingerprint(fixture):
            report, problems = MODULE.validate(
                release=fixture.published(),
                native_manifest_payload=manifest(fixture.native),
                bundle_manifest_payload=manifest(fixture.bundles),
                metadata_payload=fixture.metadata_bytes(),
                extensions=list(EXTENSIONS),
                version=VERSION,
                tag=TAG,
                source_commit=SOURCE,
            )
            self.assertEqual([], problems)
            return report["assets"]["fingerprint"]

        first = fingerprint(self.fixture)
        self.assertEqual(first, fingerprint(Fixture(self.root / "identical")))
        changed = Fixture(self.root / "changed")
        changed.extra_assets = [f"install-octet-{TAG}.sh", f"install-octet-{TAG}.sh{SIGNATURE}"]
        self.assertNotEqual(first, fingerprint(changed.write()))

    def test_canonical_identities_are_refused_before_verification(self):
        for overrides, message in (
            ({"version": "0.10"}, "version is not canonical"),
            ({"tag": "0.10.4"}, "tag is not canonical"),
            ({"source_commit": "not-a-sha"}, "source commit is not canonical"),
        ):
            with self.subTest(overrides=overrides):
                arguments = dict(
                    release=self.fixture.published(),
                    native_manifest_payload=manifest(self.fixture.native),
                    bundle_manifest_payload=manifest(self.fixture.bundles),
                    metadata_payload=self.fixture.metadata_bytes(),
                    extensions=list(EXTENSIONS),
                    version=VERSION,
                    tag=TAG,
                    source_commit=SOURCE,
                )
                arguments.update(overrides)
                with self.assertRaisesRegex(MODULE.CompletenessError, message):
                    MODULE.validate(**arguments)


class StagingGuardTests(unittest.TestCase):
    """Execute the real staging guards against a stubbed GitHub REST API."""

    def setUp(self):
        self.octet = OCTET.read_text(encoding="utf-8")
        self.serve = SERVE.read_text(encoding="utf-8")

    def core_jobs(self):
        workflows = {"octet": self.octet, "serve": self.serve}
        return [(workflows[label], name) for label, name in CORE_STAGING_JOBS]

    def job(self, workflow, name):
        jobs = workflow.split("\njobs:\n", 1)[1]
        body = jobs.split(f"\n  {name}:\n", 1)[1]
        lines = body.splitlines(keepends=True)
        for index, line in enumerate(lines):
            if line.startswith("  ") and not line.startswith("   ") and line.rstrip().endswith(":"):
                return "".join(lines[:index])
        return body

    def guard_block(self, workflow, job_name):
        """The staging decision, extracted verbatim from the upload job."""
        job = self.job(workflow, job_name)
        start = job.index("# Complete assets before Latest")
        end = job.index("gh release upload", start)
        block = job[start:end]
        self.assertEqual(block.count("if ! latest_tag=$(gh api"), 1)
        return block

    def run_guard(self, workflow, job_name, latest_document):
        """Run a staging guard with `gh api --jq` emulated by the real jq.

        `gh api` applies its `--jq` filter to the raw REST document, so the stub
        does the same: a camelCase filter returns `null` here exactly as it would
        against GitHub, and the guard must fail closed on that instead of
        silently allowing an upload to the Latest release.
        """
        bash = shutil.which("bash")
        jq = shutil.which("jq")
        if bash is None or jq is None:
            self.skipTest("bash and jq are required for the guard simulation")
        stubs = """
set -euo pipefail
gh() {
  case "$*" in
    *'releases/latest'*)
      filter=""
      while (($#)); do
        case "$1" in
          --jq) filter=$2; shift 2 ;;
          *) shift ;;
        esac
      done
      jq -r "$filter" "$LATEST_DOCUMENT" ;;
    *) return 97 ;;
  esac
}
"""
        with tempfile.TemporaryDirectory() as temporary:
            document = Path(temporary) / "latest.json"
            document.write_text(json.dumps(latest_document), encoding="utf-8")
            # The extracted block stops before `gh release upload`, so the marker
            # only runs when every guard fell through; the closing `fi` completes
            # the surrounding `if ((${#uploads[@]})); then`.
            script = stubs + self.guard_block(workflow, job_name) + (
                "            printf '%s\\n' \"$latest_tag\" > \"$LATEST_SEEN\"\n"
                "            echo staging-guard-passed\n"
            )
            environment = {
                "PATH": os.pathsep.join([str(Path(jq).parent), os.defpath]),
                "HOME": temporary,
                "GITHUB_REPOSITORY": "skaft-software/octet",
                "RELEASE_TAG": TAG,
                "LATEST_DOCUMENT": str(document),
                "LATEST_SEEN": str(Path(temporary) / "latest-seen"),
            }
            result = subprocess.run([bash, "-c", script], env=environment,
                                    capture_output=True, text=True, timeout=60)
            seen = Path(environment["LATEST_SEEN"])
            return result, seen.read_text().strip() if seen.exists() else ""

    @staticmethod
    def rest_latest(tag):
        """The REST shape of `repos/{repo}/releases/latest`."""
        return {
            "tag_name": tag,
            "draft": False,
            "prerelease": False,
            "target_commitish": SOURCE,
            "assets": [],
        }

    def test_guards_read_the_rest_snake_case_latest_tag(self):
        for workflow, name in self.core_jobs():
            with self.subTest(job=name):
                block = self.guard_block(workflow, name)
                self.assertIn('releases/latest" --jq .tag_name', block)
                self.assertNotIn(".tagName", block)

    def test_guard_blocks_new_assets_when_the_tag_is_already_latest(self):
        for workflow, name in self.core_jobs():
            with self.subTest(job=name):
                result, seen = self.run_guard(workflow, name, self.rest_latest(TAG))
                self.assertNotEqual(0, result.returncode)
                self.assertIn("assets may only be attached while it is staged", result.stderr)
                self.assertEqual("", seen)

    def test_guard_allows_staging_when_a_different_release_is_latest(self):
        result, seen = self.run_guard(self.octet, "publish", self.rest_latest("v0.10.3"))
        self.assertEqual(0, result.returncode, result.stderr)
        self.assertIn("staging-guard-passed", result.stdout)
        self.assertEqual("v0.10.3", seen)

    def test_guard_fails_closed_when_github_returns_no_usable_tag(self):
        for tag in ("", None, "null"):
            with self.subTest(tag=tag):
                document = self.rest_latest(tag)
                result, _ = self.run_guard(self.octet, "publish", document)
                self.assertNotEqual(0, result.returncode)
                self.assertIn("refusing to mutate", result.stderr)

    def test_guards_precede_every_staging_upload(self):
        for workflow, name in self.core_jobs():
            with self.subTest(job=name):
                job = self.job(workflow, name)
                self.assertLess(
                    job.index("assets may only be attached while it is staged"),
                    job.index("gh release upload"),
                )
                self.assertIn("docs/release/complete-assets-before-latest.md", job)

    def test_npm_evidence_is_post_publication_evidence_not_staging(self):
        npm = self.job(self.octet, "publish-npm")
        self.assertIn("gh release upload", npm)
        self.assertNotIn("assets may only be attached while it is staged", npm)

    def test_installer_repair_stays_authorized_for_an_already_latest_release(self):
        # The portability repair of an existing published release is explicitly
        # authorized and may target the release that is already Latest, so it
        # must not acquire a staging precondition or a demotion requirement.
        job = self.job(self.octet, "publish-installer-repair")
        self.assertIn("gh release upload", job)
        self.assertNotIn("releases/latest", job)
        self.assertNotIn("assets may only be attached while it is staged", job)
        self.assertNotIn("--latest=false", job)
        # Everything that already protected the repair is untouched.
        self.assertIn("immutable release asset differs from the existing asset", job)
        self.assertIn("existing release asset has no immutable digest", job)
        self.assertIn("cosign verify-blob", job)
        self.assertIn('cosign sign-blob', job)
        self.assertIn("environment: stable-release-publish", self.job(self.octet,
                                                                     "publish-installer-repair"))
        self.assertIn("Supplemental post-publication repair", job)


class PromotionIsBlockedTests(unittest.TestCase):
    """No workflow may claim a verified promotion that this change cannot attest."""

    def setUp(self):
        self.octet = OCTET.read_text(encoding="utf-8")
        self.serve = SERVE.read_text(encoding="utf-8")

    def test_no_workflow_sets_latest(self):
        for workflow in (self.octet, self.serve):
            self.assertNotIn("make_latest", workflow)
            self.assertNotIn("promote_latest", workflow)
            self.assertNotIn("mode=promote", workflow)
            self.assertNotIn("gh release edit", workflow)

    def test_staging_is_a_documented_precondition_not_an_automation(self):
        doc = (ROOT / "docs/release/complete-assets-before-latest.md").read_text(encoding="utf-8")
        self.assertIn("gh release create vX.Y.Z", doc)
        self.assertIn("--latest=false", doc)
        self.assertIn("BLOCKED", doc)
        # The documented guard is an observation, not atomic protection.
        self.assertIn("pre-upload observation, not atomic protection", doc)
        # The receipt alone must never be presented as authority to promote.
        self.assertIn("not** sufficient", doc)
        self.assertIn("offline and unauthenticated", doc)
        self.assertIn("never runs cosign", doc)
        # The installer repair is documented as an exempt supplement.
        self.assertIn("Post-publication supplements are not guarded", doc)
        self.assertIn("publish-installer-repair", doc)
        # No claim that a second signer identity is inherently weaker.
        self.assertNotIn("accepted-but-weaker", doc.lower())
        self.assertIn("Two identities are not inherently", doc)
        self.assertIn("weaker than one", doc)
        workflows = {"octet": self.octet, "serve": self.serve}
        for label, name in CORE_STAGING_JOBS:
            jobs = workflows[label].split("\njobs:\n", 1)[1]
            body = jobs.split(f"\n  {name}:\n", 1)[1]
            self.assertIn("assets may only be attached while it is staged", body)

    def test_public_installer_validation_still_requires_a_published_release(self):
        # Drafts serve no `releases/download` URL, so draft staging was rejected
        # on purpose: the public installer and bundle smoketests are never
        # removed to make a draft-based workflow simpler.
        for workflow in (self.octet, self.serve):
            self.assertIn("is still a draft", workflow)
            self.assertIn("is marked as a prerelease", workflow)
        self.assertIn("/releases/download/$RELEASE_TAG/install-octet.sh", self.octet)
        self.assertIn("/releases/download/$RELEASE_TAG/install-octet-$RELEASE_TAG.sh", self.octet)
        self.assertIn('extension install "$extension"', self.serve)
        self.assertIn("published installer is not readable", self.octet)
        self.assertIn("published portable installer is not readable", self.octet)
        self.assertIn("verify-installer-repair", self.octet)

    def test_signing_attestation_and_ancestry_contracts_are_untouched(self):
        for fragment in (
            "environment: stable-release-publish",
            "id-token: write",
            'cosign sign-blob',
            'cosign verify-blob',
            '--certificate-github-workflow-sha "$WORKFLOW_COMMIT"',
            'sha256sum --check OCTET_SHA256SUMS',
            'git merge-base --is-ancestor',
            'immutable release asset differs from the existing asset',
        ):
            self.assertIn(fragment, self.octet)
        for fragment in ("environment: stable-release-publish", "id-token: write",
                        "immutable release asset"):
            self.assertIn(fragment, self.serve)


class CatalogueContractTests(unittest.TestCase):
    def setUp(self):
        self.serve = SERVE.read_text(encoding="utf-8")
        self.octet = OCTET.read_text(encoding="utf-8")

    def test_native_asset_contract_matches_the_installer_repair_checker(self):
        for fragment in (
            'f"octet-{version}-aarch64-apple-darwin.tar.gz"',
            'f"octet-{version}-x86_64-apple-darwin.tar.gz"',
            'f"octet-{version}-x86_64-unknown-linux-gnu.tar.gz"',
            'f"octet-{version}-x86_64-pc-windows-msvc.zip"',
        ):
            self.assertIn(fragment, self.octet)
        self.assertEqual(MODULE.native_assets(VERSION), {
            "install-octet.sh",
            "install-octet.ps1",
            f"octet-{VERSION}-aarch64-apple-darwin.tar.gz",
            f"octet-{VERSION}-x86_64-apple-darwin.tar.gz",
            f"octet-{VERSION}-x86_64-unknown-linux-gnu.tar.gz",
            f"octet-{VERSION}-x86_64-pc-windows-msvc.zip",
        })

    def test_bundle_inventory_matches_the_release_serve_build_matrix(self):
        job = self.serve.split("\n  build-codemode:\n", 1)[1].split("\n  publish:\n", 1)[0]
        matrix = tuple(sorted(re.findall(r"target: (\S+),", job)))
        self.assertEqual(matrix, tuple(sorted(MODULE.CODEMODE_TARGETS)))
        packager = (ROOT / "scripts/package-octet-extension-release.sh").read_text()
        supported = re.search(
            r"case \"\$target\" in\n\s+(x86_64-unknown-linux-gnu[^)]*)\)", packager)
        self.assertIsNotNone(supported)
        self.assertEqual(tuple(sorted(supported.group(1).split("|"))),
                         tuple(sorted(MODULE.CODEMODE_TARGETS)))

    def test_catalog_and_generic_bundle_naming_are_the_published_contract(self):
        catalog = (ROOT / "extensions/release-catalog.txt").read_text().splitlines()
        self.assertEqual([line for line in catalog if line and not line.startswith("#")],
                         list(EXTENSIONS))
        packager = (ROOT / "scripts/package-octet-extension-release.sh").read_text()
        self.assertIn('archive="$output_directory/$package_id-$package_version.tar.gz"', packager)
        self.assertIn(
            'archive="$output_directory/$package_id-$package_version-$target.tar.gz"', packager)
        self.assertIn("pattern: octet-codemode-*", self.serve)
        self.assertIn("while IFS= read -r extension", self.serve)


if __name__ == "__main__":
    unittest.main()
