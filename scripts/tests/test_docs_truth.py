"""Truth gate over the documentation that ships in the release packages.

`README.md`, `SECURITY.md`, `CONTRIBUTING.md`, `CHANGELOG.md` and the Markdown
under `docs/` are shipped inside the native, npm and container packages
(`docs/package-assets.txt`), so a stale or retracted claim is a product defect,
not an editorial nit. This module re-reads the shipped Markdown and checks the
claims the repository itself can settle: the release identity, the extension
catalogs, the CLI and slash-command sources, the CI workflows and the recorded
compatibility evidence.

The checks are written as invariants, not snapshots: they fail when the
documentation contradicts the tree, and they stay valid across releases.
"""

import json
import re
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
INVENTORY = "docs/package-assets.txt"

# Never name the competing product the positioning rules exclude.
FORBIDDEN_NAME = re.compile(r"\bPiG\b")

# Products removed from this release line. A current page may mention them only
# where it says they are gone (git history, retained records, no longer built).
REMOVED_PRODUCTS = ("octet-browse", "octet-serve", "Serve")
REMOVAL_MARKERS = re.compile(
    r"removed|no longer|retained (in git history|evidence|records)|historical|"
    r"unavailable|not shipped|not built|stray|used to|previously",
    re.IGNORECASE,
)

# Unbacked speed claims. Owner-approved positioning may say the agent is fast;
# a comparison or superlative needs a reproducible measurement in the tree.
UNBACKED_PERFORMANCE = re.compile(
    r"high[- ]performance|blazing|fastest|faster than|speedup|"
    r"\d+(?:\.\d+)?\s*[x×]\s*(?:faster|quicker|less)",
    re.IGNORECASE,
)

SAFE_MODE_STOP = re.compile(r"stops?|stopped|prevents?|disables?|removes?|blocks?")
SAFE_MODE_EXCEPTION = re.compile(
    r"grant|trust|host authority|explicit(?:ly)? (?:enabled|authorized)|--extension-dir",
    re.IGNORECASE,
)

FENCED_CODE = re.compile(r"(?ms)^\s*(`{3,}|~{3,}).*?^\s*\1[^\n]*$")
HTML_COMMENT = re.compile(r"(?s)<!--.*?-->")
LINK_TARGET = re.compile(r"\]\(\s*<?[^)\s>]*>?\s*\)")


def _read(relative):
    return (ROOT / relative).read_text(encoding="utf-8")


def _shipped_markdown():
    """Every Markdown file the public documentation inventory ships."""
    files = {}
    for line in _read(INVENTORY).splitlines():
        if line.startswith("#") or not line:
            continue
        kind, name = line.split(" ", 1)
        if kind == "text" and name.endswith(".md"):
            files[name] = _read(name)
    return files


def _workspace_version():
    manifest = _read("Cargo.toml")
    return re.search(r'(?m)^version = "([0-9]+\.[0-9]+\.[0-9]+)"$', manifest).group(1)


def _current_documents():
    """The shipped pages that describe the current release, not old ones."""
    version = _workspace_version()
    shipped = _shipped_markdown()
    current = {
        name: text
        for name, text in shipped.items()
        if name
        in {
            "README.md",
            "SECURITY.md",
            "CONTRIBUTING.md",
            "CHANGELOG.md",
            "docs/README.md",
            "docs/testing/README.md",
            "docs/benchmarks/README.md",
            f"docs/releases/v{version}.md",
        }
        or (name.startswith("docs/") and name.count("/") == 1 and name.endswith(".md"))
    }
    # Historical release notes, qualification records and design records keep
    # their period-accurate statements; the release notes for the candidate are
    # current, not historical.
    changelog = current.pop("CHANGELOG.md", None)
    if changelog is not None:
        current["CHANGELOG.md"] = _release_section(changelog, version)
    return current


def _release_section(changelog, version):
    match = re.search(
        rf"(?ms)^## \[{re.escape(version)}\][^\n]*\n(.*?)(?=^## \[|\Z)", changelog
    )
    if not match:
        raise AssertionError(f"CHANGELOG.md has no [{version}] section")
    return match.group(0)


def _prose(text):
    """Documentation with inert regions (code fences, comments, link targets) removed."""
    text = FENCED_CODE.sub("", text)
    text = HTML_COMMENT.sub("", text)
    text = LINK_TARGET.sub("]", text)
    return text


def _paragraphs(text):
    return [block.strip() for block in re.split(r"\n\s*\n", text) if block.strip()]


def _excerpt(detail, limit=180):
    return re.sub(r"\s+", " ", detail)[:limit]


def _failures(check):
    return "\n".join(f"{name}: {detail}" for name, detail in check)


class DocsTruthTests(unittest.TestCase):
    def test_release_identity_is_consistent(self):
        version = _workspace_version()
        shipped = _shipped_markdown()
        notes = f"docs/releases/v{version}.md"
        self.assertIn(notes, shipped, f"{notes} must ship with the candidate docs")
        heading = _release_section(shipped["CHANGELOG.md"], version).splitlines()[0]
        candidate = heading == f"## [{version}] - Release candidate"
        dated = re.fullmatch(rf"## \[{re.escape(version)}\] - \d{{4}}-\d{{2}}-\d{{2}}", heading)
        self.assertTrue(candidate or dated, "the changelog heading must be candidate or dated")
        target = re.search(r"/releases/download/v([0-9]+\.[0-9]+\.[0-9]+)/install-octet\.sh",
                           shipped["README.md"])
        self.assertIsNotNone(target, "README.md must name a version-pinned install target")
        install_version = target.group(1)
        releases = re.findall(r"(?m)^## \[([0-9]+\.[0-9]+\.[0-9]+)\]", shipped["CHANGELOG.md"])
        self.assertEqual(releases[0], version)
        self.assertIn(install_version, releases[:2], "docs must install the workspace or previous release")
        pre_publication = install_version != version
        if not pre_publication:
            self.assertTrue(dated, "published install target requires a dated changelog heading")
        mode = "candidate" if pre_publication else "release"
        # A version-only badge identifies the source without claiming publication.
        self.assertRegex(
            shipped["README.md"],
            rf"(?:{mode}|version)-{re.escape(version)}|(?:{mode}|Version): {re.escape(version)}",
            "the README badge must name the current distribution version",
        )
        self.assertIn(
            f"]({notes})",
            shipped["README.md"],
            "the README must link the release notes for the candidate version",
        )
        self.assertRegex(
            shipped["docs/README.md"],
            rf"octet\s+\**{re.escape(version)}\**\s+{mode}",
            "docs/README.md must name the distribution version and state",
        )
        self.assertRegex(shipped[notes], rf"^# octet {re.escape(version)}\b")
        if candidate:
            self.assertRegex(shipped[notes], r"unreleased|candidate")
        else:
            self.assertNotIn("unreleased source candidate", shipped[notes])
        self.assertIn(f"## [{version}]", shipped["CHANGELOG.md"])
        for name in ("README.md", "docs/installation.md", "docs/getting-started.md"):
            if pre_publication:
                self.assertNotRegex(
                    shipped[name],
                    rf"/releases/download/v{re.escape(version)}/|--tag v{re.escape(version)}\b|"
                    rf"gh release download v{re.escape(version)}\b",
                    f"{name} must not install unpublished {version}",
                )
            self.assertNotRegex(
                shipped[name],
                rf"\bcandidate: {re.escape(version)}\b.*\bpublished\b",
                f"{name} must not describe the candidate as published",
            )

    def test_shipped_extension_table_matches_the_release_catalog(self):
        catalog = [line.strip() for line in _read("extensions/release-catalog.txt").splitlines()
                   if line.strip() and not line.lstrip().startswith("#")]
        table = re.findall(r"(?m)^\| `(octet-[a-z-]+)` \|", _read("docs/installation.md"))
        self.assertEqual(sorted(table), sorted(catalog))
        self.assertIn("octet-pi-compat", catalog)
        self.assertRegex(
            _read("docs/releases/v0.9.6.md"),
            r"`octet-pi-compat` adapter\s+does not ship in this release",
        )
        version = _workspace_version()
        for name in ("README.md", "docs/pi-compatibility.md"):
            text = " ".join(_read(name).split()).lower()
            self.assertIn(f"v{version}", text)
            self.assertIn("first pi-compatible release", text)
            self.assertIn("octet-pi-compat", text)
            self.assertIn("optional", text)
            self.assertIn("os permissions", text)

    def test_removed_products_are_not_advertised(self):
        failures = {}
        for name, text in _current_documents().items():
            prose = _prose(text)
            for paragraph in _paragraphs(prose):
                mentioned = [p for p in REMOVED_PRODUCTS if re.search(rf"\b{p}\b", paragraph)]
                if mentioned and not REMOVAL_MARKERS.search(paragraph):
                    failures[name] = f"{', '.join(mentioned)}: {_excerpt(paragraph)!r}"
        self.assertFalse(failures, _failures(failures.items()))

    def test_no_forbidden_name(self):
        failures = {
            name: m.group(0)
            for name, text in _shipped_markdown().items()
            for m in FORBIDDEN_NAME.finditer(text)
        }
        self.assertFalse(failures, _failures(failures.items()))

    def test_readme_positioning_credits_pi(self):
        readme = _shipped_markdown()["README.md"]
        tagline = [p for p in _paragraphs(_prose(readme)) if "coding agent" in p]
        self.assertTrue(tagline, "the README needs a positioning line")
        self.assertRegex(tagline[0], r"\bfast\b")
        self.assertRegex(tagline[0], r"\bnative\b")
        self.assertIn("octet-pi-compat", readme, "the README must name the Pi compat extension")
        self.assertRegex(
            readme,
            r"\bPi\b[^.]*\bMario Zechner\b|\bPi\b.*extension adapter|extension adapter.*\bPi\b",
            "the README must credit Pi and describe the adapter as optional",
        )

    def test_safe_mode_extension_claims(self):
        failures = {}
        for name, text in _current_documents().items():
            for paragraph in _paragraphs(_prose(text)):
                if not re.search(r"safe[- ]mode", paragraph, re.IGNORECASE):
                    continue
                if not re.search(r"extension", paragraph, re.IGNORECASE):
                    continue
                if SAFE_MODE_EXCEPTION.search(paragraph):
                    continue
                for offset, line in enumerate(paragraph.splitlines()):
                    if SAFE_MODE_STOP.search(line) and "extension" in line.lower():
                        failures[f"{name}:{offset + 1}"] = _excerpt(paragraph)
                        break
        self.assertFalse(
            failures,
            "safe-mode text must keep the explicit-grant exception:\n" + _failures(failures.items()),
        )

    def test_goal_command_is_documented(self):
        commands = _read("crates/octet-coding-agent/src/commands.rs")
        if not re.search(r'"goal",\s*\n\s*"/goal ', commands):
            self.skipTest("this tree has no /goal command")
        context = _shipped_markdown()["docs/context.md"]
        self.assertNotRegex(
            context,
            r"no separate goal store or\s+goal command",
            "docs/context.md denies a durable goal store that src/commands.rs registers",
        )
        self.assertRegex(
            _shipped_markdown()["docs/commands.md"],
            r"(?m)^\| `/goal",
            "docs/commands.md must list the registered /goal command",
        )
        self.assertRegex(
            _shipped_markdown()["docs/sessions.md"],
            r"(?i)\bgoal\b",
            "docs/sessions.md is the documented home for the durable session goal",
        )

    def test_termux_claims_match_the_tree(self):
        termux = _shipped_markdown()["docs/termux.md"]
        supported = "termux" in _read("crates/sexy-tui-rs/src/tui/cursor.rs").lower()
        clipboard = "termux-clipboard-set" in _read("crates/octet-coding-agent/src/tui/view.rs")
        if supported or clipboard:
            self.assertNotRegex(
                termux,
                r"contains no Termux-specific code path",
                "the tree contains Termux-specific code",
            )
        if clipboard:
            self.assertNotRegex(
                termux,
                r"no clipboard integration|clipboard/image integration would require new host "
                r"code and is not implemented",
                "the tree ships a Termux clipboard writer",
            )

    def test_performance_claims_are_repo_backed(self):
        failures = {}
        for name, text in _current_documents().items():
            for paragraph in _paragraphs(_prose(text)):
                claim = UNBACKED_PERFORMANCE.search(paragraph)
                if not claim:
                    continue
                referenced = re.findall(r"(?<![\w/.-])((?:docs|scripts|evaluation)/[\w./-]+)", paragraph)
                if all(not (ROOT / path).exists() for path in referenced):
                    failures[name] = f"{claim.group(0)!r}: {_excerpt(paragraph)!r}"
        self.assertFalse(
            failures,
            "performance comparisons need a reproducible measurement in the tree:\n"
            + _failures(failures.items()),
        )

    def test_homebrew_tap_matches_the_release_workflow(self):
        workflow = _read(".github/workflows/homebrew-formula.yml")
        tap = re.search(r"tap_repository:.*?default: (\S+)", workflow, re.DOTALL).group(1)
        self.assertIn(
            tap,
            _shipped_markdown()["docs/distribution.md"],
            f"docs/distribution.md must name the tap the workflow publishes to ({tap})",
        )

    def test_pi_tui_notice_matches_the_adapter_manifest(self):
        manifest = json.loads(_read("extensions/octet-pi-compat/package.json"))
        version = manifest["dependencies"]["@earendil-works/pi-tui"]
        self.assertIn(
            f"`@earendil-works/pi-tui` {version}",
            _shipped_markdown()["THIRD_PARTY_NOTICES.md"],
            "the Pi TUI notice must match the adapter's pinned dependency",
        )

    def test_pi_module_counts_match_the_tree(self):
        page = _shipped_markdown()["docs/pi-compat-release-status.md"]
        rows = re.findall(r"(?m)^\| `([\w:]+)` \| (\d+)(?: \(ignored\))? \|", page)
        self.assertTrue(rows, "the release status needs its per-module test table")
        mismatches = {}
        for module, documented in rows:
            sources = list((ROOT / "crates").rglob(f"{module.split('::')[-1]}.rs"))
            self.assertEqual(len(sources), 1, f"{module} resolves to {sources}")
            found = len(re.findall(r"#\[(?:tokio::)?test\b", sources[0].read_text()))
            if found != int(documented):
                mismatches[module] = f"documented {documented}, found {found}"
        self.assertFalse(
            mismatches,
            "the per-module counts must match the test functions in the tree:\n"
            + _failures(mismatches.items()),
        )

    def test_changelog_and_release_notes_are_user_facing(self):
        version = _workspace_version()
        shipped = _shipped_markdown()
        section = _release_section(shipped["CHANGELOG.md"], version)
        # Release text renders inside the agent: it states what is new or fixed.
        # Open issues live in the tracker, not in a known-limitations list.
        self.assertNotRegex(section, r"\bU\d{1,2}\b|Unit:", "unit ids are internal, not user-facing")
        self.assertRegex(section, r"Pi")
        self.assertRegex(
            shipped[f"docs/releases/v{version}.md"],
            r"(?m)^### (Changed|Fixed|Added)$",
            "generated notes must retain a Changed, Fixed or Added changelog subsection",
        )

        self.assertEqual(
            shipped[f"docs/releases/v{version}.md"],
            f"# octet {version}\n\n" + section.rstrip("\n") + "\n",
            "current notes must preserve the entire changelog section verbatim",
        )

    def test_documented_ci_lanes_exist(self):
        workflow = _read(".github/workflows/ci.yml")
        jobs = {
            line.split(":")[0].strip()
            for line in workflow.splitlines()
            if re.fullmatch(r"  [a-z][a-z0-9_-]*:", line)
        }
        page = _shipped_markdown()["docs/testing/README.md"]
        table = re.search(
            r"(?ms)^## CI tiers\n(.*?)(?=^## |\Z)", page
        ).group(1)
        lanes = [re.match(r"^\| `([^`]+)`", line).group(1) for line in table.splitlines() if line.startswith("| `")]
        self.assertTrue(lanes)
        missing = sorted({lane.split(" (")[0] for lane in lanes} - jobs)
        self.assertFalse(missing, f"docs/testing/README.md documents CI lanes that do not exist: {missing}")
        for workflow_name in re.findall(r"`(models-dev-freshness\.yml|security\.yml|provider-acceptance\.yml|production-panic-audit\.yml)`", page):
            self.assertTrue((ROOT / ".github/workflows" / workflow_name).exists())

    def test_documented_paths_in_testing_page_exist(self):
        page = _read("docs/testing/README.md")
        tokens = set(
            re.findall(
                r"`((?:apps|crates|docs|extensions|scripts|sdk|\.github)/[\w./-]+?\.(?:md|py|sh|rs|yml|json|toml|txt))`",
                page,
            )
        )
        self.assertTrue(tokens)
        missing = sorted(token for token in tokens if not (ROOT / token).exists())
        self.assertFalse(missing, f"docs/testing/README.md names paths that do not exist: {missing}")


class ReleaseIdentityStateTests(unittest.TestCase):
    """Exercise the truth gate without changing checked-in release surfaces."""

    def setUp(self):
        self.version = "1.2.4"
        self.section = "## [1.2.4] - 2026-11-12\n\n### Fixed\n\n- Editing works.\n"
        self.shipped = {
            "CHANGELOG.md": self.section + "\n## [1.2.3] - 2026-11-01\n",
            "docs/releases/v1.2.4.md": "# octet 1.2.4\n\n" + self.section,
            "README.md": "candidate-1.2.4 [notes](docs/releases/v1.2.4.md)\n"
                         "1.2.4 release candidate; install the published version:\n"
                         "https://github.com/skaft-software/octet/releases/download/v1.2.3/install-octet.sh\n",
            "docs/README.md": "octet **1.2.4** candidate source\n",
            "docs/installation.md": "1.2.4 release candidate\n"
                                    "https://github.com/skaft-software/octet/releases/download/v1.2.3/install-octet.sh\n",
            "docs/getting-started.md": "Install the published 1.2.3.\n",
        }

    def check_identity(self):
        with mock.patch(__name__ + "._workspace_version", return_value=self.version), mock.patch(
            __name__ + "._shipped_markdown", return_value=self.shipped
        ):
            DocsTruthTests("test_release_identity_is_consistent").test_release_identity_is_consistent()

    def test_dated_changelog_with_pre_publication_docs_passes(self):
        self.check_identity()

    def test_pre_publication_docs_reject_every_current_install_command(self):
        for name in ("README.md", "docs/installation.md", "docs/getting-started.md"):
            for command in (
                "https://github.com/skaft-software/octet/releases/download/v1.2.4/install-octet.sh",
                "https://github.com/skaft-software/octet/releases/download/v1.2.4/install-octet.ps1",
                "cargo install --tag v1.2.4",
                "gh release download v1.2.4 --pattern OCTET_SHA256SUMS",
            ):
                with self.subTest(path=name, command=command):
                    original = self.shipped[name]
                    self.shipped[name] += command + "\n"
                    with self.assertRaisesRegex(AssertionError, "must not install unpublished"):
                        self.check_identity()
                    self.shipped[name] = original

    def test_candidate_heading_with_current_install_target_fails(self):
        for name in ("CHANGELOG.md", "docs/releases/v1.2.4.md"):
            self.shipped[name] = self.shipped[name].replace("2026-11-12", "Release candidate")
        self.shipped["README.md"] = self.shipped["README.md"].replace("download/v1.2.3", "download/v1.2.4")
        with self.assertRaisesRegex(AssertionError, "published install target requires a dated"):
            self.check_identity()

    def test_dated_pre_publication_docs_still_require_candidate_state(self):
        self.shipped["README.md"] = self.shipped["README.md"].replace("candidate-", "release-")
        with self.assertRaisesRegex(AssertionError, "README badge"):
            self.check_identity()
        self.shipped["README.md"] = self.shipped["README.md"].replace("release-", "candidate-")
        self.shipped["docs/README.md"] = "octet **1.2.4** release\n"
        with self.assertRaisesRegex(AssertionError, "distribution version and state"):
            self.check_identity()

    def test_candidate_notes_follow_the_changelog_not_the_install_target(self):
        self.shipped["CHANGELOG.md"] = self.shipped["CHANGELOG.md"].replace("2026-11-12", "Release candidate")
        with self.assertRaises(AssertionError):
            self.check_identity()
        self.shipped["docs/releases/v1.2.4.md"] = self.shipped["docs/releases/v1.2.4.md"].replace(
            "2026-11-12", "Release candidate"
        )
        self.check_identity()

    def test_published_docs_require_release_state_and_dated_notes(self):
        for name in self.shipped:
            self.shipped[name] = self.shipped[name].replace("download/v1.2.3", "download/v1.2.4")
        self.shipped["README.md"] = self.shipped["README.md"].replace("candidate-", "release-")
        self.shipped["docs/README.md"] = "octet **1.2.4** release\n"
        self.check_identity()


if __name__ == "__main__":
    unittest.main()
