"""Surface auditing with fake GET responses; no registry or GitHub mutation."""
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("release_surfaces", ROOT / "scripts/check-release-surfaces.py")
surfaces = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(surfaces)


class ReleaseSurfacesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        for name in surfaces.DOCS:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("Candidate 1.2.4; install [release](https://github.com/skaft-software/octet/releases/tag/v1.2.3).\n")
        for directory in ("docs/releases", "crates/octet-coding-agent/src/tui/view/releases"):
            path = self.root / directory / "v1.2.3.md"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("<!-- Generated from CHANGELOG.md; do not edit. -->\n## [1.2.3]\n\n- Fixed.\n")
        self.site = dict(version="1.2.3", tag="v1.2.3")
        self.tags = {package: "1.2.3" for package in surfaces.PACKAGES}
        self.requests = []

    def fetch(self, url):
        self.requests.append(url)
        if url == surfaces.WEBSITE_RELEASE_URL:
            return self.site
        from urllib.parse import unquote
        package = unquote(url.split("/package/", 1)[1].split("/dist-tags", 1)[0])
        return {"latest": self.tags[package]}

    def audit(self, **kwargs):
        return surfaces.audit(self.root, fetch=self.fetch,
                              github=lambda: dict(tag_name="v1.2.3", draft=False, prerelease=False), **kwargs)

    def test_agreement_covers_every_package_docs_and_bundled_notes(self):
        rows, failures = self.audit()
        self.assertEqual([], failures)
        self.assertEqual(len(surfaces.PACKAGES) + 1, len(self.requests))
        self.assertEqual(14, len(rows))
        self.assertTrue(all(url.startswith("https://") for url in self.requests))

    def test_all_mismatches_reported_in_one_pass(self):
        for package in surfaces.PACKAGES:
            self.tags[package] = "0.8.1"
        self.site = dict(release="v1.2.0")
        (self.root / "docs/installation.md").write_text("## octet 1.2.2\n")
        _, failures = self.audit()
        self.assertEqual(7, len(failures))
        self.assertTrue(all("expected 1.2.3, observed" in failure for failure in failures))
        self.assertIn("website", failures[-1])

    def test_secondary_pin_or_conflicting_site_id_is_not_hidden(self):
        path = self.root / "README.md"
        path.write_text(path.read_text() + "cargo install --tag v1.2.1\n")
        self.site["tag"] = "v1.2.2"
        _, failures = self.audit()
        self.assertEqual(2, len(failures))
        self.assertIn("1.2.1, 1.2.3", failures[0])
        self.assertIn("1.2.2, 1.2.3", failures[1])

    def test_newer_published_release_requests_post_ship_docs_commit(self):
        path = self.root / "README.md"
        path.write_text("Candidate 1.2.4; install "
                        "https://github.com/skaft-software/octet/releases/download/v1.2.2/install-octet.sh\n")
        _, failures = self.audit()
        self.assertIn("GitHub latest is 1.2.3; docs still install 1.2.2; land the post-ship docs commit", failures)
        # Compare numeric components, not strings, and never suggest promotion
        # when the documented version is already ahead of Latest.
        path.write_text(path.read_text().replace("v1.2.2", "v1.2.10"))
        _, failures = self.audit()
        self.assertFalse(any("post-ship docs commit" in failure for failure in failures))
        self.assertTrue(any("expected 1.2.3, observed 1.2.10" in failure for failure in failures))

    def test_missing_malformed_and_unreachable_sources_fail_closed(self):
        for value in ({}, {"version": "latest"}, []):
            self.site = value
            _, failures = self.audit()
            self.assertTrue(any("website" in failure for failure in failures), failures)
        (self.root / "docs/README.md").unlink()
        (self.root / "crates/octet-coding-agent/src/tui/view/releases/v1.2.3.md").unlink()
        self.tags[surfaces.PACKAGES[0]] = "next"
        _, failures = self.audit()
        for source in ("docs/README.md", "tui/view/releases", "npm @skaft/octet latest"):
            self.assertTrue(any(source in failure for failure in failures), (source, failures))
        rows, failures = surfaces.audit(self.root, fetch=mock.Mock(side_effect=OSError("unavailable")),
                                        github=mock.Mock(side_effect=OSError("unavailable")))
        self.assertTrue(failures)
        self.assertTrue(any("GitHub latest" in failure for failure in failures))
        self.assertEqual(7, sum("unavailable" in failure for failure in failures))

    def test_fixture_cli_cannot_contact_network(self):
        import json
        path = self.root / "evidence.json"
        path.write_text(json.dumps({
            "github": dict(tag_name="v1.2.3", draft=False, prerelease=False),
            "npm": {p: {"latest": v} for p, v in self.tags.items()},
            "website": self.site,
        }))
        with mock.patch.object(surfaces, "urlopen", side_effect=AssertionError("no network")), mock.patch.object(
            surfaces.subprocess, "run", side_effect=AssertionError("no subprocess")
        ):
            self.assertEqual(surfaces.main(["--root", str(self.root), "--fixtures", str(path)]), 0)

    def test_live_mode_is_restricted_to_the_audit_workflow(self):
        with mock.patch.dict(surfaces.os.environ, {}, clear=True), mock.patch.object(
            surfaces, "audit", side_effect=AssertionError("must not audit")
        ):
            with self.assertRaises(SystemExit) as error:
                surfaces.main(["--live"])
            self.assertEqual(error.exception.code, 2)
        with mock.patch.dict(surfaces.os.environ, {"GITHUB_ACTIONS": "true", "GITHUB_WORKFLOW": "octet release surfaces"}), mock.patch.object(
            surfaces, "audit", return_value=([], [])
        ) as audit:
            self.assertEqual(surfaces.main(["--live"]), 0)
            audit.assert_called_once()

    def test_unpublished_or_missing_stable_state_is_not_accepted_as_latest(self):
        for flags in ({}, dict(draft=True, prerelease=False), dict(draft=False, prerelease=True)):
            _, failures = surfaces.audit(self.root, fetch=self.fetch,
                                         github=lambda: dict(tag_name="v1.2.3", **flags))
            self.assertTrue(any("published stable" in f for f in failures), failures)

    def test_windows_401_is_reported_without_hiding_other_surfaces(self):
        fetch = self.fetch
        def failing(url):
            if "win32" in url:
                raise OSError("HTTP 401")
            return fetch(url)
        rows, failures = surfaces.audit(self.root, fetch=failing,
                                         github=lambda: dict(tag_name="v1.2.3", draft=False, prerelease=False))
        self.assertEqual(len(failures), 1)
        self.assertIn("win32-x64 latest: HTTP 401", failures[0])
        self.assertTrue(any("website" in label for label, _ in rows))

    def test_github_command_is_explicit_get_and_has_a_deadline(self):
        with mock.patch.object(surfaces.subprocess, "run", return_value=mock.Mock(stdout='{"tag_name":"v1.2.3"}')) as run:
            self.assertEqual("v1.2.3", surfaces.github_latest()["tag_name"])
        args, kwargs = run.call_args
        self.assertEqual(["gh", "api", "--method", "GET", "repos/skaft-software/octet/releases/latest",
                          "--jq", "{tag_name, draft, prerelease}"], args[0])
        self.assertEqual(30, kwargs["timeout"])

    def test_http_client_only_gets_json_with_timeout_and_size_limit(self):
        response = mock.MagicMock()
        response.__enter__.return_value.read.return_value = b'{"version":"1.2.3"}'
        with mock.patch.object(surfaces, "urlopen", return_value=response) as opening:
            self.assertEqual({"version": "1.2.3"}, surfaces.get_json(surfaces.WEBSITE_RELEASE_URL))
        args, kwargs = opening.call_args
        self.assertEqual("GET", args[0].get_method())
        self.assertEqual(20, kwargs["timeout"])
        response.__enter__.return_value.read.return_value = b" " * (2 * 1024 * 1024 + 1)
        with mock.patch.object(surfaces, "urlopen", return_value=response):
            with self.assertRaisesRegex(ValueError, "exceeds"):
                surfaces.get_json(surfaces.WEBSITE_RELEASE_URL)


if __name__ == "__main__":
    unittest.main()
