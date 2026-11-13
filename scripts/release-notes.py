#!/usr/bin/env python3
"""Render only the workspace version's release text from CHANGELOG.md.

sync writes the docs and bundled copies; check never writes. render --github
prints just the promoted subsections. artifact --commit SHA prints release.json
for a separate website deploy. Historical notes are frozen: no command scans, rewrites or deletes them.
"""
from __future__ import annotations

import argparse
from datetime import date
import json
from pathlib import Path
import re
import sys
import tomllib

from changelog import changelog_sections

ROOT = Path(__file__).resolve().parents[1]
BUNDLED = Path("crates/octet-coding-agent/src/tui/view/releases")


def workspace_version(root: Path) -> str:
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    version = manifest["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("workspace version must be X.Y.Z")
    return version


def section(changelog: Path, version: str) -> tuple[str, str | None]:
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version):
        raise ValueError("version must be X.Y.Z")
    found = [lines for _, lines in changelog_sections(changelog.read_text(encoding="utf-8"))
             if re.match(rf"^## \[{re.escape(version)}\](?:\s|$)", lines[0])]
    if len(found) != 1:
        raise ValueError(f"expected exactly one [{version}] changelog section")
    heading = re.fullmatch(rf"## \[{re.escape(version)}\] - (\d{{4}}-\d{{2}}-\d{{2}}|Release candidate)", found[0][0])
    if not heading:
        raise ValueError("release heading needs a date or Release candidate")
    label = heading[1]
    released = None if label == "Release candidate" else date.fromisoformat(label).isoformat()
    # Keep all Markdown, wrapping, credits and interior blank lines untouched.
    return "\n".join(found[0]).rstrip("\n") + "\n", released


def render(changelog: Path, version: str) -> str:
    content, _ = section(changelog, version)
    # No generation comment: the in-agent rich renderer receives only Markdown.
    return f"# octet {version}\n\n" + content


def render_github(markdown: str) -> str:
    """Render generated notes for GitHub, which already displays title and date."""
    # render() always prefixes the verbatim section with an H1 and a blank line.
    # Drop those and the section's version/date heading, not any release content.
    body = markdown.split("\n", 3)[3].lstrip("\n")
    return re.sub(r"(?m)^### ", "## ", body)


def artifact(changelog: Path, version: str, commit: str) -> dict:
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("commit must be a full lowercase SHA")
    _, released = section(changelog, version)
    if released is None:
        raise ValueError("release.json requires a dated changelog section")
    return dict(schema="octet.release.v1", generated_from="CHANGELOG.md",
                version=version, tag=f"v{version}", commit=commit, date=released,
                notes_markdown=render_github(render(changelog, version)))


def sync(root: Path, changelog: Path, check: bool = False) -> list[Path]:
    version = workspace_version(root)
    data = render(changelog, version).encode("utf-8")
    changed = []
    for directory in (Path("docs/releases"), BUNDLED):
        path = root / directory / f"v{version}.md"
        if path.is_file() and path.read_bytes() == data:
            continue
        changed.append(path)
        if not check:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
    return changed


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--changelog", type=Path)
    parser.add_argument("--check", action="store_true")
    sub = parser.add_subparsers(dest="command")
    for name in ("sync", "check", "render", "artifact"):
        command = sub.add_parser(name)
        if name in ("render", "artifact"):
            command.add_argument("version", nargs="?")
        if name == "render":
            command.add_argument("--github", action="store_true", help="omit title/date and promote subsections")
        if name == "artifact":
            command.add_argument("--commit", required=True)
    args = parser.parse_args(argv)
    try:
        version = workspace_version(args.root)
        if getattr(args, "version", None) not in (None, version):
            raise ValueError("only the current workspace version may be generated; historical notes are frozen")
        changelog = args.changelog or args.root / "CHANGELOG.md"
        if args.command == "render":
            markdown = render(changelog, version)
            print(render_github(markdown) if args.github else markdown, end="")
        elif args.command == "artifact":
            print(json.dumps(artifact(changelog, version, args.commit), ensure_ascii=False, indent=2))
        elif args.command in ("sync", "check") or args.check:
            check = args.check or args.command == "check"
            changed = sync(args.root, changelog, check)
            for path in changed:
                print(f"{'out of date' if check else 'synced'}: {path}",
                      file=sys.stderr if check else sys.stdout)
            if check and changed:
                print("Run python3 scripts/release-notes.py sync", file=sys.stderr)
                return 1
        else:
            parser.error("choose render, sync, check or artifact")
    except (OSError, ValueError, KeyError) as error:
        print(f"release-notes: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
