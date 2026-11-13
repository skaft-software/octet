"""Current-version release-note generation; all fixtures are offline."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("release_notes", SCRIPTS / "release-notes.py")
notes = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(notes)
sys.path.pop(0)

BODY = "\n### Fixed\n\n- Sessions keep `code` and [links](docs/cli.md).\n  Frozen wrapping stays verbatim.\n\n- Unicode stays: π.\n"
SECTION = "## [0.10.4] - 2026-11-12\n" + BODY
CHANGELOG = "# Changelog\n\n## [Unreleased]\n\n" + SECTION + "\n## [0.10.3]\n\nOld hand-written notes.\n"


class ReleaseNotesTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.10.4"\n')
        self.changelog = self.root / "CHANGELOG.md"
        self.changelog.write_text(CHANGELOG)

    def cli(self, *args):
        return subprocess.run([sys.executable, str(SCRIPTS / "release-notes.py"),
                               "--root", str(self.root), *args], capture_output=True, text=True)

    def test_render_preserves_section_verbatim_without_visible_marker(self):
        actual = notes.render(self.changelog, "0.10.4")
        self.assertEqual(actual, "# octet 0.10.4\n\n" + SECTION)
        self.assertNotIn("<!--", actual)
        self.assertNotIn("blob/v", actual)
        self.assertEqual(self.cli("render").stdout, actual)

    def test_github_body_starts_at_promoted_sections_and_preserves_content(self):
        content = BODY.lstrip("\n").replace("### Fixed", "## Fixed")
        self.assertEqual(notes.render_github(notes.render(self.changelog, "0.10.4")), content)
        result = self.cli("render", "0.10.4", "--github")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, content)
        self.assertNotIn("0.10.4", result.stdout)
        self.assertNotIn("2026-11-12", result.stdout)
        self.changelog.write_text(CHANGELOG.replace("### Fixed", "### Changed\n\n- A change.\n\n### Fixed"))
        self.assertEqual(self.cli("render", "--github").stdout,
                         "## Changed\n\n- A change.\n\n" + content)
        self.assertFalse((self.root / "docs/releases/v0.10.4.md").exists())

    def test_sync_and_check_are_current_version_only_even_for_old_generated_pages(self):
        old = []
        for directory in (Path("docs/releases"), notes.BUNDLED):
            directory = self.root / directory
            directory.mkdir(parents=True)
            for version in ("0.4.0", "0.10.3", "9.9.9"):
                path = directory / f"v{version}.md"
                path.write_text("<!-- Generated from CHANGELOG.md; do not edit. -->\nHistorical bytes\n")
                old.append(path)
        before = {p: p.read_bytes() for p in old}
        self.assertEqual(len(notes.sync(self.root, self.changelog, check=True)), 2)
        self.assertFalse((self.root / "docs/releases/v0.10.4.md").exists())
        self.assertEqual(len(notes.sync(self.root, self.changelog)), 2)
        self.assertEqual(notes.sync(self.root, self.changelog), [])
        self.assertEqual(notes.sync(self.root, self.changelog, check=True), [])
        self.assertEqual({p: p.read_bytes() for p in old}, before)
        self.assertEqual((self.root / "docs/releases/v0.10.4.md").read_bytes(),
                         (self.root / notes.BUNDLED / "v0.10.4.md").read_bytes())

    def test_check_is_read_only_and_detects_one_byte_drift(self):
        self.assertEqual(self.cli("check").returncode, 1)
        self.assertEqual(self.cli("sync").returncode, 0)
        path = self.root / notes.BUNDLED / "v0.10.4.md"
        path.write_bytes(path.read_bytes() + b" ")
        before = path.read_bytes()
        result = self.cli("--check")
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn(str(path), result.stderr)
        self.assertEqual(path.read_bytes(), before)

    def test_cli_cannot_generate_historical_versions(self):
        result = self.cli("render", "0.10.3")
        self.assertEqual(result.returncode, 1)
        self.assertIn("historical notes are frozen", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_artifact_uses_github_body_and_records_date_sha(self):
        result = self.cli("artifact", "--commit", "a" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        data = json.loads(result.stdout)
        self.assertEqual(data["notes_markdown"], self.cli("render", "--github").stdout)
        self.assertEqual((data["tag"], data["commit"], data["date"]),
                         ("v0.10.4", "a" * 40, "2026-11-12"))
        self.assertEqual(self.cli("artifact", "--commit", "main").returncode, 1)

    def test_missing_duplicate_and_malformed_sections_fail(self):
        for text in ("## [Unreleased]\n", CHANGELOG + "\n" + SECTION,
                     CHANGELOG.replace("2026-11-12", "yesterday"),
                     CHANGELOG.replace("2026-11-12", "2026-02-30")):
            self.changelog.write_text(text)
            with self.assertRaises(ValueError):
                notes.render(self.changelog, "0.10.4")

    def test_candidate_can_sync_but_cannot_produce_publication_artifact(self):
        self.changelog.write_text(CHANGELOG.replace("2026-11-12", "Release candidate"))
        self.assertIn("Release candidate", notes.render(self.changelog, "0.10.4"))
        with self.assertRaisesRegex(ValueError, "dated changelog"):
            notes.artifact(self.changelog, "0.10.4", "a" * 40)


if __name__ == "__main__":
    unittest.main()
