"""Public-tree hygiene gate for the owner's public-content rules.

`test_docs_truth` only reads the Markdown listed in `docs/package-assets.txt`, so an
internal reference in a script, fixture or tracked workstation record could still
reach a public tree unnoticed. The removed release-comparison harness was exactly
that case: it was not in the documentation inventory and no other check saw it.
This gate scans every tracked text file instead.

Rules checked:
  * no competitor product name, case-insensitively, as a word or with the
    configuration-variable prefix,
  * no maintainer workstation path or machine name,
  * no assistant attribution line.

The two recorded exceptions under `crates/sexy-tui-rs/tests/fixtures/mermaid/`
predate the public 0.8.2-rc base commit (`0c5b86ae`). One is a reporter's own graph
and the other is the upstream Pi render that `mermaid_parity.rs` compares
byte-for-byte; editing them would falsify recorded upstream evidence. Their two
occurrences each are pinned here, including the exact match count, so any further
occurrence anywhere in the tree still fails.

Binary assets are not text and are skipped: the legacy demo GIFs contain the byte
sequence by compression accident, which is not a naming reference. This gate runs
from a Git checkout because untracked build output is not repository content.
"""

import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def _rule(*fragments):
    """Assemble a rule so its own text cannot match the tree the rule scans.

    This module is scanned like any other, so the fragments stay split instead of
    spelling out the strings the rule forbids.
    """
    return "".join(fragments)


COMPETITOR_NAME = re.compile(
    "|".join((_rule(r"\bpi", "g\b"), _rule("PI", "G", "_"))),
    re.IGNORECASE,
)
WORKSTATION_IDENTITY = re.compile(
    "|".join(
        (
            _rule("/Users/", "achu", "mukundan", r"\b"),
            _rule("/home/", "achu", r"\b"),
            _rule("Achus", "-MacBook-Air"),
        )
    )
)
ASSISTANT_ATTRIBUTION = re.compile(
    "|".join(
        (
            _rule("co-authored-by:", r"\s*", "Clau", "de"),
            _rule("generated with ", "Clau", "de code"),
            _rule("noreply@anthrop", "ic.com"),
        )
    ),
    re.IGNORECASE,
)

# path -> the exact number of recorded matches it is allowed to keep.
RECORDED = {
    "crates/sexy-tui-rs/tests/fixtures/mermaid/reported-architecture.mmd": 2,
    "crates/sexy-tui-rs/tests/fixtures/mermaid/reported-architecture.pi.txt": 2,
}


def _tracked_paths():
    """Tracked repository-relative paths, or None outside a Git checkout."""
    try:
        listing = subprocess.run(
            ["git", "ls-files", "-z"],
            cwd=ROOT,
            check=True,
            capture_output=True,
        )
    except (FileNotFoundError, subprocess.CalledProcessError):
        return None
    return [name for name in listing.stdout.decode("utf-8").split("\0") if name]


def _text_of(path):
    """Decoded UTF-8 text, or None for a binary file."""
    try:
        return (ROOT / path).read_bytes().decode("utf-8")
    except (UnicodeDecodeError, FileNotFoundError):
        return None


class PublicTreeHygieneTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.paths = _tracked_paths()
        if cls.paths is None:
            raise unittest.SkipTest("tree hygiene needs a Git checkout")
        cls.texts = {
            path: text
            for path in cls.paths
            if (text := _text_of(path)) is not None
        }

    def _scan(self, pattern):
        return {
            path: sorted({match.group(0) for match in pattern.finditer(text)})
            for path, text in self.texts.items()
            if pattern.search(text)
        }

    def test_competitor_name_is_absent(self):
        self.assertFalse(
            self._scan(COMPETITOR_NAME),
            "the competitor name must not appear in the tree",
        )

    def test_workstation_identity_is_confined_to_recorded_fixtures(self):
        found = self._scan(WORKSTATION_IDENTITY)
        unexpected = {path: hits for path, hits in found.items() if path not in RECORDED}
        self.assertFalse(unexpected, f"private workstation paths are not public content: {unexpected}")
        counts = {path: len(WORKSTATION_IDENTITY.findall(text)) for path, text in self.texts.items()}
        drifted = {
            path: (counts.get(path, 0), expected)
            for path, expected in RECORDED.items()
            if counts.get(path, 0) != expected
        }
        self.assertFalse(drifted, f"recorded fixtures changed their pinned identity count: {drifted}")

    def test_assistant_attribution_is_absent(self):
        self.assertFalse(
            self._scan(ASSISTANT_ATTRIBUTION),
            "no assistant attribution belongs in the repository",
        )


if __name__ == "__main__":
    unittest.main()
