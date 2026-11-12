"""Offline changelog policy fixtures; historical text stays frozen."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
SPEC = importlib.util.spec_from_file_location("check_changelog", SCRIPTS / "check-changelog.py")
checker = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(checker)
sys.path.pop(0)

PREFIX = "# Changelog\n\n## [Unreleased]\n\n"
CREDIT = " ([#456](https://github.com/skaft-software/octet/pull/456) by [@some-user](https://github.com/some-user))"
GOOD = PREFIX + "### Added\n\n- Sessions retain bookmarks." + CREDIT + "\n\n## [0.10.4] - 2026-11-12\n\n### Fixed\n\n- Reports accept scrolling keys.\n"


class CheckChangelogTests(unittest.TestCase):
    def violation(self, text, message):
        errors = checker.check(text)
        self.assertTrue(any(message in error for _, error in errors), errors)
        self.assertTrue(all(number > 0 for number, _ in errors))
        return errors

    def test_good_and_empty_unreleased(self):
        for text in (GOOD, PREFIX, GOOD.replace("2026-11-12", "Release candidate")):
            self.assertEqual(checker.check(text), [])

    def test_current_structure_and_date(self):
        for text, message in (
            ("", "Unreleased"),
            (GOOD + "\n## [Unreleased]\n", "duplicate Unreleased"),
            (GOOD.replace(" - 2026-11-12", ""), "expected ##"),
            (GOOD.replace("2026-11-12", "2026-02-30"), "invalid calendar date"),
            (GOOD + "\n## [0.10.5] - Release candidate\n", "descending"),
            (GOOD + "\n## [0.10.4] - Release candidate\n", "descending"),
        ):
            with self.subTest(text=text):
                self.violation(text, message)

    def test_historical_structure_and_bodies_are_exempt_without_tags(self):
        history = "\n## 0.10.3\n### Known limitations\n- " + "x" * 300 + "\n  continuation\n## [0.9.5]\n## [0.9.5]\n### Security\n"
        self.assertEqual(checker.check(GOOD + history), [])
        self.violation(GOOD.replace("### Fixed", "### Known limitations"), "subsection must be")

    def test_current_and_future_bodies_are_always_checked(self):
        for version in ("0.10.4", "0.10.5", "1.0.0"):
            self.violation(GOOD.replace("0.10.4", version).replace("Reports accept scrolling keys.", "CI passes."), "CI chatter")

    def test_subsections(self):
        for body, message in (
            ("### Security\n- Tools ask for approval.\n", "subsection must be"),
            ("### Fixed\n", "omit empty"),
            ("### Fixed\n- Tools work.\n### Fixed\n- Sessions resume.\n", "duplicate subsection"),
            ("### Fixed\n- Tools work.\n### Added\n- Sessions resume.\n", "subsections must follow"),
            ("- Tools ask for approval.\n", "under a subsection"),
        ):
            self.violation(PREFIX + body, message)

    def test_single_line_plain_bullets_and_source_lines(self):
        self.assertIn((6, "bullet wraps onto lines 7-7; use one line"),
                      checker.check(PREFIX + "### Fixed\n- Sessions\n  resume."))
        for body, message in (
            ("* Sessions resume.\n", "single-line '- ' bullet"),
            ("Notes about sessions.\n", "single-line '- ' bullet"),
            ("- **Sessions** resume.\n", "plain text"),
            ("- \n", "needs a statement"),
            ("-  Sessions resume.\n", "surrounding whitespace"),
        ):
            self.violation(PREFIX + "### Fixed\n" + body, message)

    def test_160_character_boundary_excludes_only_valid_credit(self):
        statement = "Sessions retain " + "x" * (160 - len("Sessions retain "))
        for suffix in ("", CREDIT):
            text = PREFIX + "### Fixed\n- " + statement + suffix + "\n"
            self.assertEqual(checker.check(text), [])
            self.violation(text.replace(statement, statement + "x"), "161 characters")

    def test_credits_match_exact_form(self):
        for suffix in (
            " (#123)",
            " ([#123](https://github.com/skaft-software/octet/issues/123))",
            CREDIT.replace("pull/456", "pull/457"),
            CREDIT.replace("https://github.com/some-user", "https://github.com/other"),
            CREDIT.replace("https://", "http://", 1),
            CREDIT + ".", CREDIT + " extra", CREDIT + CREDIT,
        ):
            self.violation(PREFIX + "### Fixed\n- Sessions resume." + suffix, "credits must be")

    def test_conservative_prose_lint(self):
        for statement, message in (
            ("Windows updates are not yet fixed.", "limitations or caveats"),
            ("The adapter still fails on large histories.", "limitations or caveats"),
            ("Sessions resume, but large histories remain slow.", "limitations or caveats"),
            ("Sessions resume. Also, the picker shows prices.", "one user-visible change"),
            ("Refactor internal implementation.", "internal implementation"),
            ("Unit tests cover resume.", "CI chatter"),
            ("Added session bookmarks.", "present-tense"),
            ("The footer will show branches.", "present-tense"),
            ("The footer was empty.", "present-tense"),
        ):
            self.violation(PREFIX + "### Fixed\n- " + statement, message)
        for statement in ("OpenCode Go exposes its routes.", "The fake cursor uses reverse video."):
            self.assertEqual(checker.check(PREFIX + "### Fixed\n- " + statement), [])

    def test_cli_is_offline_and_read_only(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "CHANGELOG.md"
            command = [sys.executable, str(SCRIPTS / "check-changelog.py"), "--changelog", str(path)]
            for text, status in ((GOOD, 0), (PREFIX + "### Fixed\n- CI passes.\n", 1)):
                path.write_text(text)
                result = subprocess.run(command, text=True, capture_output=True, env={"PATH": directory})
                self.assertEqual(result.returncode, status, result.stderr)
                self.assertEqual(path.read_text(), text)
                if status:
                    self.assertIn(f"{path}:6:", result.stderr)


if __name__ == "__main__":
    unittest.main()
