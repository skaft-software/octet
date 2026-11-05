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
  * no assistant attribution line, except the pinned maintainer backups under
    `docs/maintainers/` (see `RECORDED_ATTRIBUTION`).

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

# Maintainer backups kept verbatim under docs/maintainers/: patch files whose headers carry the
# original author metadata, which cannot be rewritten without breaking `git am`, and a brief that
# quotes one. Each file's exact match count is pinned like RECORDED above, so any further
# occurrence in these files, or anywhere else in the tree, still fails.
RECORDED_ATTRIBUTION = {
    "docs/maintainers/pi-usability-brief.md": 2,
    "docs/maintainers/pi-usability-wip/D-commits/0001-Pi-compat-compact-grouped-path-free-extension-diagno.patch": 3,
    "docs/maintainers/pi-usability-wip/F-commits/0001-pi-eval-add-frame-by-frame-flicker-checker-and-times.patch": 3,
    "docs/maintainers/pi-usability-wip/F-commits/0002-pi-eval-footer-repro-driver-flicker-mask-parametrise.patch": 3,
    "docs/maintainers/pi-usability-wip/F-commits/0003-pi-compat-mirror-the-user-agent-dir-when-cwd-is-the-.patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0001-docs-usability-performance-pass-brief-and-workstream.patch": 5,
    "docs/maintainers/pi-usability-wip/J-commits/0002-docs-20-second-frame-by-frame-screen-stability-accep.patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0003-eval-fixture-mirroring-the-maintainer-s-real-Pi-1.4..patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0004-eval-jank-recorder-frame-level-PTY-capture-of-octet-.patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0005-eval-extension-API-traffic-capture-for-Pi-preload-an.patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0006-eval-log-event-delivery-before-the-handler-settles.patch": 3,
    "docs/maintainers/pi-usability-wip/J-commits/0007-eval-jank-fixture-octet-model-declares-reasoning.patch": 1,
    "docs/maintainers/pi-usability-wip/J-commits/0008-eval-jank-trace-path-is-rewritten-on-every-run.patch": 1,
    "docs/maintainers/pi-usability-wip/M-commits/0001-WIP-saved-octet-defaults-outrank-Pi-defaults-unverif.patch": 3,
    "docs/maintainers/pi-usability-wip/M-commits/0002-Pi-defaults-fill-only-gaps-notice-once-keep-RPC-thin.patch": 6,
    "docs/maintainers/pi-usability-wip/M-uncommitted.patch": 3,
    "docs/maintainers/pi-usability-wip/P-commits/0001-fix-pi-compat-isolate-before_agent_start-handlers-th.patch": 3,
    "docs/maintainers/pi-usability-wip/P-commits/0002-fix-pi-compat-do-not-charge-a-tool-guard-for-the-tim.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0001-pi-compat-start-the-transform-warm-up-child-from-a-d.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0002-pi-compat-keep-shared-closure-files-with-their-live-.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0003-pi-compat-fingerprint-every-package.json-up-to-the-d.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0004-octet-pi-compat-remove-temporary-file-when-a-persist.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0005-octet-pi-compat-realpath-module-alias-targets-use-pa.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0006-octet-pi-compat-refuse-foreign-owned-startup-cache-d.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0007-octet-pi-compat-keep-files-that-assign-module.export.patch": 3,
    "docs/maintainers/pi-usability-wip/R-commits/0008-octet-pi-compat-sweep-persisted-cache-temporaries-ab.patch": 3,
    "docs/maintainers/pi-usability-wip/S-commits/0001-eval-warm-full-UI-and-exit-timing-harness-Pi-vs-octe.patch": 3,
    "docs/maintainers/pi-usability-wip/S-commits/0002-fix-pi-compat-a-quit-stops-waiting-for-a-slow-sessio.patch": 3,
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
        counts = {
            path: len(ASSISTANT_ATTRIBUTION.findall(text))
            for path, text in self.texts.items()
            if ASSISTANT_ATTRIBUTION.search(text)
        }
        unexpected = {path: hits for path, hits in counts.items() if path not in RECORDED_ATTRIBUTION}
        self.assertFalse(unexpected, f"no assistant attribution belongs in the repository: {unexpected}")
        drifted = {
            path: (counts.get(path, 0), expected)
            for path, expected in RECORDED_ATTRIBUTION.items()
            if counts.get(path, 0) != expected
        }
        self.assertFalse(drifted, f"recorded maintainer backups changed their pinned attribution count: {drifted}")


if __name__ == "__main__":
    unittest.main()
