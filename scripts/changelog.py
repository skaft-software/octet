#!/usr/bin/env python3
"""Extract CHANGELOG releases and repair their Markdown links for display.

Mirrors the upstream Pi changelog utility: `parseChangelog`,
`normalizeChangelogLinks`, `compareVersions` and `getNewEntries`. Relative
repository links become tag-pinned GitHub source links, legacy repository
names are canonicalized, and external or in-page links are left untouched.
No network access is performed.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import sys

GITHUB_REPO = "skaft-software/octet"
# Root-level CHANGELOG.md: local targets are already repository-root relative.
CHANGELOG_LINK_BASE_PATH = ""
LEGACY_REPO_RE = re.compile(r"^https://github\.com/skaft-software/(?:ygg|octet)(?=/|$)")
URL_SCHEME_RE = re.compile(r"^[a-z][a-z0-9+.-]*:", re.IGNORECASE)
INLINE_MARKDOWN_LINK_RE = re.compile(r"(!?\[[^\]\n]+\]\()([^\s)]+)((?:\s+[^)]*)?\))")
VERSION_HEADER_RE = re.compile(r"##\s+\[?(\d+)\.(\d+)\.(\d+)\]?")
RELEASE_VERSION_RE = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$")

MAX_HEADER_BYTES = 256


@dataclass(frozen=True)
class ChangelogEntry:
    """One parsed release section."""

    major: int
    minor: int
    patch: int
    content: str

    @property
    def version(self) -> str:
        return f"{self.major}.{self.minor}.{self.patch}"


@dataclass(frozen=True)
class _LocalTarget:
    fragment: str
    path_part: str
    query: str


def normalize_tag(version: str | ChangelogEntry) -> str:
    value = version if isinstance(version, str) else version.version
    return value if value.startswith("v") else f"v{value}"


def _split_local_target(target: str) -> _LocalTarget:
    hash_index = target.find("#")
    before_hash = target if hash_index == -1 else target[:hash_index]
    fragment = "" if hash_index == -1 else target[hash_index:]
    query_index = before_hash.find("?")
    if query_index == -1:
        return _LocalTarget(fragment, before_hash, "")
    return _LocalTarget(fragment, before_hash[:query_index], before_hash[query_index:])


def _normalize_path_part(value: str) -> str:
    return value.replace("\\", "/")


def _normalize_posix(path: str) -> str:
    """Resolve `.`/`..` segments exactly like `path.posix.normalize`."""
    absolute = path.startswith("/")
    trailing_slash = path.endswith("/")
    parts: list[str] = []
    for segment in path.split("/"):
        if segment in ("", "."):
            continue
        if segment == "..":
            if parts and parts[-1] != "..":
                parts.pop()
            elif not absolute:
                parts.append("..")
            continue
        parts.append(segment)
    joined = "/".join(parts)
    if absolute:
        return "/" + joined
    if not joined:
        return "/" if trailing_slash else "."
    return joined + "/" if trailing_slash else joined


def _resolve_repository_path(target_path: str) -> str | None:
    normalized = _normalize_path_part(target_path)
    if normalized.startswith("/"):
        joined = _normalize_posix(normalized.lstrip("/"))
    elif CHANGELOG_LINK_BASE_PATH:
        joined = _normalize_posix(f"{CHANGELOG_LINK_BASE_PATH}/{normalized}")
    else:
        joined = _normalize_posix(normalized)
    if joined in (".", "..") or joined.startswith("../"):
        return None
    return joined


def _is_directory_target(original_path: str, repository_path: str) -> bool:
    if original_path.endswith("/"):
        return True
    return "." not in repository_path.rsplit("/", 1)[-1]


def normalize_changelog_link_target(target: str, tag: str) -> str:
    """Canonicalize one link target for a specific release tag."""
    canonical = LEGACY_REPO_RE.sub(f"https://github.com/{GITHUB_REPO}", target)
    repo_url = f"https://github.com/{GITHUB_REPO}"
    for route in ("blob", "tree"):
        for branch in ("main", "master"):
            floating = f"{repo_url}/{route}/{branch}/"
            if canonical.startswith(floating):
                canonical = f"{repo_url}/{route}/{tag}/{canonical[len(floating):]}"
    if (
        canonical.startswith("#")
        or canonical.startswith("//")
        or URL_SCHEME_RE.match(canonical)
    ):
        return canonical
    parts = _split_local_target(canonical)
    if not parts.path_part:
        return canonical
    repository_path = _resolve_repository_path(parts.path_part)
    if repository_path is None:
        return canonical
    route = "tree" if _is_directory_target(parts.path_part, repository_path) else "blob"
    return f"{repo_url}/{route}/{tag}/{repository_path}{parts.query}{parts.fragment}"


def normalize_changelog_links(markdown: str, version: str | ChangelogEntry) -> str:
    """Rewrite every inline Markdown link in one release body."""
    tag = normalize_tag(version)
    return INLINE_MARKDOWN_LINK_RE.sub(
        lambda match: f"{match.group(1)}{normalize_changelog_link_target(match.group(2), tag)}{match.group(3)}",
        markdown,
    )


def parse_changelog(changelog_path: Path) -> list[ChangelogEntry]:
    """Parse `## [x.y.z]` sections, preserving their original body text."""
    path = Path(changelog_path)
    if not path.exists():
        return []
    entries: list[ChangelogEntry] = []
    current: tuple[int, int, int] | None = None
    current_lines: list[str] = []
    for line in path.read_text(encoding="utf-8").split("\n"):
        if line.startswith("## "):
            if current is not None and current_lines:
                entries.append(
                    ChangelogEntry(*current, content="\n".join(current_lines).strip())
                )
            match = VERSION_HEADER_RE.search(line)
            if match and len(line) <= MAX_HEADER_BYTES:
                current = (int(match.group(1)), int(match.group(2)), int(match.group(3)))
                current_lines = [line]
            else:
                current = None
                current_lines = []
        elif current is not None:
            current_lines.append(line)
    if current is not None and current_lines:
        entries.append(ChangelogEntry(*current, content="\n".join(current_lines).strip()))
    return entries


def compare_versions(left: ChangelogEntry, right: ChangelogEntry) -> int:
    """Return -1, 0 or 1 for left <, ==, > right."""
    for a, b in ((left.major, right.major), (left.minor, right.minor), (left.patch, right.patch)):
        if a != b:
            return -1 if a < b else 1
    return 0


def get_new_entries(entries: list[ChangelogEntry], last_version: str) -> list[ChangelogEntry]:
    """Return entries strictly newer than `last_version`, in file order."""
    parts = last_version.split(".")
    numbers = [int(part) if part.isdigit() else 0 for part in parts[:3]]
    while len(numbers) < 3:
        numbers.append(0)
    last = ChangelogEntry(numbers[0], numbers[1], numbers[2], "")
    return [entry for entry in entries if compare_versions(entry, last) > 0]


def release_notes(changelog_path: Path, version: str) -> str | None:
    """Normalized body of one exact release, or None when it is absent."""
    if not RELEASE_VERSION_RE.match(version):
        raise ValueError("version must be a release version")
    for entry in parse_changelog(changelog_path):
        if entry.version == version:
            return normalize_changelog_links(entry.content, entry)
    return None


def new_notes(changelog_path: Path, last_version: str) -> str:
    """Normalized bodies of all releases newer than `last_version`."""
    entries = get_new_entries(parse_changelog(changelog_path), last_version)
    return "\n\n".join(normalize_changelog_links(entry.content, entry) for entry in entries)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changelog", type=Path, default=Path("CHANGELOG.md"))
    subparsers = parser.add_subparsers(dest="command", required=True)
    extract = subparsers.add_parser("extract", help="normalized body of one release")
    extract.add_argument("--version", required=True)
    since = subparsers.add_parser("since", help="normalized bodies newer than a release")
    since.add_argument("--last", required=True)
    args = parser.parse_args()

    if args.command == "extract":
        try:
            notes = release_notes(args.changelog, args.version)
        except ValueError as error:
            parser.error(str(error))
        if notes is None:
            print(
                f"release {args.version} not found in {args.changelog}", file=sys.stderr
            )
            return 1
        print(notes)
        return 0

    print(new_notes(args.changelog, args.last))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
