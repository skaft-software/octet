#!/usr/bin/env python3
"""Read-only audit of GitHub Latest, npm latest, install docs and the website.

No writes, workflow dispatches, issues, registry changes or installations. Remote
requests are GETs with deadlines. Local docs describe published installs; Cargo
and candidate badges may legitimately be ahead of the published version.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import quote
from urllib.request import Request, urlopen

REPOSITORY = "skaft-software/octet"
WEBSITE_RELEASE_URL = "https://skaft.org/octet/release.json"
PACKAGES = ("@skaft/octet", "@skaft/octet-darwin-arm64", "@skaft/octet-darwin-x64",
            "@skaft/octet-linux-x64-gnu", "@skaft/octet-win32-x64")
DOCS = ("README.md", "docs/README.md", "docs/installation.md",
        "docs/getting-started.md", "docs/distribution.md")
VERSION = r"[0-9]+\.[0-9]+\.[0-9]+"
ROOT = Path(__file__).resolve().parents[1]


def get_json(url):
    request = Request(url, headers={"Accept": "application/json", "User-Agent": "octet-release-audit"},
                      method="GET")
    with urlopen(request, timeout=20) as response:
        data = response.read(2 * 1024 * 1024 + 1)
    if len(data) > 2 * 1024 * 1024:
        raise ValueError("JSON response exceeds 2 MiB")
    return json.loads(data)


def github_latest():
    result = subprocess.run(["gh", "api", "--method", "GET",
                             f"repos/{REPOSITORY}/releases/latest",
                             "--jq", "{tag_name, name, body, draft, prerelease}"], check=True,
                            capture_output=True, text=True, timeout=30)
    return json.loads(result.stdout)


def version(value, tag=False):
    pattern = ("v" if tag else "") + VERSION
    if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
        raise ValueError(f"expected {'tag vX.Y.Z' if tag else 'version X.Y.Z'}, got {value!r}")
    return value[1:] if tag else value


def doc_versions(text):
    # Match install/publication identities, not candidate/source labels, old
    # signature history or unrelated SDK versions. Keep every matched pin so a
    # stale secondary command cannot hide behind an up-to-date first heading.
    patterns = (rf"github\.com/{REPOSITORY}/releases/(?:download|tag)/v({VERSION})(?=[/\s)\"`]|$)",
                rf"--tag v({VERSION})(?=[\s`]|$)",
                rf"(?m)^## octet ({VERSION})\b", rf"Release: ({VERSION})\b")
    return sorted({match for pattern in patterns for match in re.findall(pattern, text)})


def audit(root=ROOT, fetch=get_json, github=github_latest):
    rows = []
    errors = []
    expected = None
    try:
        latest = github()
        if latest.get("draft") is not False or latest.get("prerelease") is not False:
            raise ValueError("Latest is not a published stable release")
        expected = version(latest["tag_name"], tag=True)
        rows.append(("GitHub latest", expected))
        if latest.get("name") != f"octet {expected}":
            errors.append(f"GitHub latest: title must be 'octet {expected}', observed {latest.get('name')!r}")
        if re.match(rf"^#{{1,6}}[ \t]+[^\n]*(?<![\d.])v?{re.escape(expected)}(?![\d.])",
                    (latest.get("body") or "").lstrip()):
            errors.append("GitHub latest: body starts with a version heading; render just the release subsections")
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        errors.append(f"GitHub latest: {error}")
    for package in PACKAGES:
        label = f"npm {package} latest"
        try:
            tags = fetch(f"https://registry.npmjs.org/-/package/{quote(package, safe='')}/dist-tags")
            rows.append((label, version(tags["latest"])))
        except (OSError, ValueError, KeyError, TypeError) as error:
            errors.append(f"{label}: {error}")
    for name in DOCS:
        try:
            text = (root / name).read_text(encoding="utf-8")
            values = doc_versions(text)
            if name == "README.md" and expected:
                target = re.search(rf"/releases/download/v({VERSION})/install-octet\.sh", text)
                if target and tuple(map(int, target.group(1).split("."))) < tuple(map(int, expected.split("."))):
                    errors.append(f"GitHub latest is {expected}; docs still install {target.group(1)}; "
                                  "land the post-ship docs commit")
            if not values:
                raise ValueError("no published install/release version found")
            rows.append((name, ", ".join(values)))
        except (OSError, ValueError) as error:
            errors.append(f"{name}: {error}")
    try:
        site = fetch(WEBSITE_RELEASE_URL)
        if not isinstance(site, dict):
            raise ValueError("website JSON is not an object")
        # The deployed site originally used `release`; the new attached artifact
        # uses `tag` and `version`. Check all identities that the public JSON has.
        identities = []
        for key in ("release", "tag", "version"):
            if key in site:
                identities.append(version(site[key], tag=key != "version"))
        if not identities:
            raise ValueError("JSON has no release, tag or version")
        rows.append((f"website {WEBSITE_RELEASE_URL}", ", ".join(sorted(set(identities)))))
    except (OSError, ValueError, KeyError, TypeError) as error:
        errors.append(f"website {WEBSITE_RELEASE_URL}: {error}")
    if expected:
        for directory in ("docs/releases", "crates/octet-coding-agent/src/tui/view/releases"):
            path = f"{directory}/v{expected}.md"
            try:
                text = (root / path).read_text(encoding="utf-8")
                # Both historical and generated headings are recognizable. The
                # offline release-notes gate checks exact generated bytes.
                found = re.search(rf"(?m)^(?:# octet |## \[)({VERSION})(?:\]|\b)", text)
                if not found:
                    raise ValueError("release notes have no version heading")
                rows.append((path, found.group(1)))
            except (OSError, ValueError) as error:
                errors.append(f"{path}: {error}")
    mismatches = [f"{label}: expected {expected}, observed {observed}"
                  for label, observed in rows if expected and observed != expected]
    return rows, mismatches + errors


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT, help="documentation checkout (default: this repository)")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--fixtures", type=Path, help="offline JSON evidence: github, npm and website")
    mode.add_argument("--live", action="store_true", help="GET-only audit inside the release-surfaces workflow")
    args = parser.parse_args(argv)
    if args.live:
        if os.environ.get("GITHUB_ACTIONS") != "true" or os.environ.get("GITHUB_WORKFLOW") != "octet release surfaces":
            parser.error("network audits run only in the octet release surfaces workflow; use --fixtures offline")
        rows, failures = audit(args.root)
    else:
        try:
            evidence = json.loads(args.fixtures.read_text(encoding="utf-8"))
            def fixture(url):
                if url == WEBSITE_RELEASE_URL:
                    return evidence["website"]
                for package in PACKAGES:
                    if url == f"https://registry.npmjs.org/-/package/{quote(package, safe='')}/dist-tags":
                        value = evidence["npm"][package]
                        if isinstance(value, dict) and "error" in value:
                            raise OSError(value["error"])
                        return value
                raise ValueError("unknown fixture URL")
            rows, failures = audit(args.root, fetch=fixture, github=lambda: evidence["github"])
        except (OSError, ValueError, KeyError, TypeError) as error:
            print(f"invalid audit fixture: {error}", file=sys.stderr)
            return 1
    for label, observed in rows:
        print(f"{label}: {observed}")
    if failures:
        print("Release surfaces disagree or could not be read:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("All release surfaces agree.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
