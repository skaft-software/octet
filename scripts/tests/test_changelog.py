"""Behavioral changelog extraction/link-repair tests over disposable fixtures."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "changelog.py"
SPEC = importlib.util.spec_from_file_location("changelog", SCRIPT)
changelog = importlib.util.module_from_spec(SPEC)
# Register before execution so the frozen dataclass can resolve its module.
sys.modules[SPEC.name] = changelog
SPEC.loader.exec_module(changelog)

SAMPLE = """# Changelog

## [Unreleased]

## [0.7.6]

- See [hotfix notes](docs/releases/v0.7.6.md).
- See [the directory](docs/releases/).
- See [root README](../README.md) and [external](https://example.com/doc).
- Local [anchor](#settings) and a [legacy link](https://github.com/skaft-software/ygg/pull/12).
- Floating [blob](https://github.com/skaft-software/octet/blob/main/docs/faq.md#q).

## [0.7.5] - 2026-09-11

- Older note.

## Not a version

- ignored body
"""


class ChangelogTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "CHANGELOG.md"
        self.path.write_text(SAMPLE, encoding="utf-8")

    def test_parses_versioned_sections_and_skips_non_versions(self):
        entries = changelog.parse_changelog(self.path)
        self.assertEqual([entry.version for entry in entries], ["0.7.6", "0.7.5"])
        self.assertTrue(entries[0].content.startswith("## [0.7.6]"))
        self.assertIn("ignored body", SAMPLE)
        self.assertNotIn("ignored body", "".join(e.content for e in entries))

    def test_missing_changelog_yields_no_entries(self):
        self.assertEqual(changelog.parse_changelog(Path(self.temp.name) / "absent.md"), [])

    def test_relative_and_directory_links_become_tag_pinned(self):
        notes = changelog.release_notes(self.path, "0.7.6")
        self.assertIn(
            "[hotfix notes](https://github.com/skaft-software/octet/blob/v0.7.6/docs/releases/v0.7.6.md)",
            notes,
        )
        self.assertIn(
            "[the directory](https://github.com/skaft-software/octet/tree/v0.7.6/docs/releases/)",
            notes,
        )
        # A repository-root escape cannot be resolved and is left unchanged.
        self.assertIn("[root README](../README.md)", notes)

    def test_external_anchor_legacy_and_floating_links(self):
        notes = changelog.release_notes(self.path, "0.7.6")
        self.assertIn("[external](https://example.com/doc)", notes)
        self.assertIn("[anchor](#settings)", notes)
        self.assertIn(
            "[legacy link](https://github.com/skaft-software/octet/pull/12)", notes
        )
        self.assertIn(
            "[blob](https://github.com/skaft-software/octet/blob/v0.7.6/docs/faq.md#q)",
            notes,
        )

    def test_release_version_must_be_exact_and_semver_shaped(self):
        self.assertIsNone(changelog.release_notes(self.path, "9.9.9"))
        with self.assertRaises(ValueError):
            changelog.release_notes(self.path, "v0.7.6")

    def test_get_new_entries_orders_and_filters(self):
        entries = changelog.parse_changelog(self.path)
        self.assertEqual(
            [entry.version for entry in changelog.get_new_entries(entries, "0.7.5")],
            ["0.7.6"],
        )
        self.assertEqual(len(changelog.get_new_entries(entries, "0.0.0")), 2)
        self.assertEqual(changelog.get_new_entries(entries, "0.7.6"), [])

    def test_normalize_tag_accepts_string_or_entry(self):
        entry = changelog.ChangelogEntry(1, 2, 3, "")
        self.assertEqual(changelog.normalize_tag(entry), "v1.2.3")
        self.assertEqual(changelog.normalize_tag("v1.2.3"), "v1.2.3")

    def test_cli_extracts_since_and_reports_absent_release(self):
        base = ["python3", str(SCRIPT), "--changelog", str(self.path)]
        extracted = subprocess.run(
            base + ["extract", "--version", "0.7.6"], capture_output=True, text=True
        )
        self.assertEqual(extracted.returncode, 0, extracted.stderr)
        self.assertIn("blob/v0.7.6/docs/releases/v0.7.6.md", extracted.stdout)
        since = subprocess.run(
            base + ["since", "--last", "0.7.5"], capture_output=True, text=True
        )
        self.assertEqual(since.returncode, 0, since.stderr)
        self.assertIn("## [0.7.6]", since.stdout)
        self.assertNotIn("Older note", since.stdout)
        absent = subprocess.run(
            base + ["extract", "--version", "9.9.9"], capture_output=True, text=True
        )
        self.assertEqual(absent.returncode, 1)
        self.assertEqual(absent.stdout, "")

    def test_script_is_importable_and_has_no_side_effects(self):
        probe = subprocess.run(
            [sys.executable, "-c", f"import runpy; runpy.run_path({str(SCRIPT)!r}, run_name='probe')"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(probe.returncode, 0, probe.stderr)


if __name__ == "__main__":
    unittest.main()
