"""Keep repository instructions available in fresh checkouts and packaged docs."""

import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class AgentConventionsTests(unittest.TestCase):
    def test_root_instructions_are_tracked(self):
        result = subprocess.run(
            ["git", "ls-files", "--error-unmatch", "AGENTS.md", "CLAUDE.md"],
            cwd=ROOT, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_root_instructions_are_not_ignored(self):
        result = subprocess.run(
            ["git", "check-ignore", "--no-index", "AGENTS.md", "CLAUDE.md"],
            cwd=ROOT, capture_output=True, text=True,
        )
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)

    def test_conventions_have_one_source(self):
        self.assertEqual((ROOT / "CLAUDE.md").read_text(), "@AGENTS.md\n")
        for name, link in (
            ("CONTRIBUTING.md", "[AGENTS.md](AGENTS.md)"),
            ("docs/maintainers/README.md", "[AGENTS.md](../../AGENTS.md)"),
            ("docs/maintainers/conventions.md", "[AGENTS.md](../../AGENTS.md)"),
        ):
            with self.subTest(name=name):
                text = (ROOT / name).read_text()
                self.assertIn(link, text)
                self.assertNotIn("local-only", text)
                self.assertNotIn("tracked mirror", text)

    def test_instruction_files_ship_with_their_pointers(self):
        inventory = (ROOT / "docs/package-assets.txt").read_text().splitlines()
        for name in ("AGENTS.md", "CLAUDE.md"):
            with self.subTest(name=name):
                self.assertIn(f"text {name}", inventory)


if __name__ == "__main__":
    unittest.main()
