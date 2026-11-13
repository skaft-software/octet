#!/usr/bin/env python3
"""Check Unreleased and releases from 0.10.4 onward without network access.

Prose checks catch common violations, not arbitrary English: human review still
checks user visibility, present tense and one change per bullet.
"""

from __future__ import annotations

import argparse
from datetime import date
from pathlib import Path
import re
import sys

from changelog import GITHUB_REPO, changelog_sections

ROOT = Path(__file__).resolve().parents[1]
SUBSECTIONS = ("Breaking Changes", "Added", "Changed", "Fixed", "Removed")
VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
HEADING = re.compile(rf"## \[({VERSION})\] - (\d{{4}}-\d{{2}}-\d{{2}}|Release candidate)$")
PR_LINK = re.compile(
    rf" \(\[#([1-9][0-9]*)\]\(https://github\.com/{GITHUB_REPO}/pull/\1\)"
    r" by \[@([A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?)\]"
    r"\(https://github\.com/\2\)\)$"
)
LINK_LIKE = re.compile(r"\]\(|https?://|(?<!\w)#\d+\b")
# Keep these conservative. API names and Pi compatibility are user-visible;
# implementation narratives, unresolved problems and engineering chores are not.
PROSE_RULES = (
    (r"\b(?:not (?:yet )?fixed|known limitations?|caveats?|not yet|unqualified|unverified|"
     r"remain(?:s)? unsupported|still (?:fails?|broken|unsupported)|however|but|except|although|unless)\b",
     "describe working behavior, not limitations or caveats"),
    (r"\b(?:refactor(?:s|ed|ing)?|implementation details?|internal implementation|"
     r"permit pool|protocol frames?|re-encodes|re-sends|digest|fsync|UTF-8 validation)\b",
     "omit internal implementation detail"),
    (r"\b(?:CI|unit tests?|regression tests?|test coverage|test suite|clippy|rustfmt)\b",
     "omit test and CI chatter"),
    (r"^(?:Added|Changed|Fixed|Removed|Updated|Implemented)\b|\b(?:will|used to|was|were)\b",
     "use a present-tense statement of what works now"),
    (r"[.!?;]\s*(?:Also|Separately)\b", "keep one user-visible change per bullet"),
)


def current_section(heading: str) -> bool:
    numeric = re.match(r"^##\s+\[?(\d+)\.(\d+)\.(\d+)", heading)
    return heading == "## [Unreleased]" or (
        numeric is not None and tuple(map(int, numeric.groups())) >= (0, 10, 4)
    )


def fix(text: str) -> tuple[str, list[str]]:
    """Stable-partition bullet blocks, leaving all other bytes in their slots."""
    lines = text.splitlines(keepends=True)
    replacements = []
    moved = []
    bullets = []
    active = False
    subsection = False

    def finish() -> None:
        ordered = [item for item in bullets if item[2]] + [item for item in bullets if not item[2]]
        for (start, end, _), (source, source_end, credited) in zip(bullets, ordered):
            if start == source:
                continue
            original = "".join(lines[source:source_end]).rstrip("\r\n")
            ending = lines[end - 1][len(lines[end - 1].rstrip("\r\n")):]
            replacements.append((start, end, original + ending))
            if credited:
                moved.append(f"moved credited contribution from line {source + 1} to line {start + 1}")
        bullets.clear()

    i = 0
    while i < len(lines):
        line = lines[i].rstrip("\r\n")
        if re.match(r"^##(?:\s|$)", line):
            finish()
            active = current_section(line)
            subsection = False
        elif line.startswith("### "):
            finish()
            subsection = line[4:] in SUBSECTIONS
        elif active and subsection and line.startswith("- "):
            end = i + 1
            while end < len(lines) and lines[end].strip() and not lines[end].startswith(("- ", "#")):
                end += 1
            body = " ".join(part.strip() for part in lines[i:end])
            bullets.append((i, end, PR_LINK.search(body) is not None))
            i = end
            continue
        i += 1
    finish()
    for start, end, replacement in sorted(replacements, reverse=True):
        lines[start:end] = [replacement]
    return "".join(lines), moved


def check(text: str) -> list[tuple[int, str]]:
    errors: list[tuple[int, str]] = []

    def error(line: int, message: str) -> None:
        errors.append((line, message))

    sections = changelog_sections(text)
    if not sections or sections[0][1][0] != "## [Unreleased]":
        error(sections[0][0] if sections else 1, "## [Unreleased] must be the first section")

    previous: tuple[int, int, int] | None = None
    seen_unreleased = False
    for start, lines in sections:
        heading = lines[0]
        if not current_section(heading):
            continue  # Historical structure AND prose are frozen, tagged or not.
        match = HEADING.fullmatch(heading)
        raw_version = re.match(r"^##\s+\[([^]]+)\]", heading)
        version = raw_version[1] if raw_version else None
        if heading == "## [Unreleased]":
            if seen_unreleased:
                error(start, "duplicate Unreleased section")
            seen_unreleased = True
        elif not match:
            error(start, "expected ## [X.Y.Z] - YYYY-MM-DD (or - Release candidate)")
        # Date-less historical headings still participate in version ordering.
        if version and re.fullmatch(VERSION, version):
            numbers = tuple(int(part) for part in version.split("."))
            if previous is not None and numbers >= previous:
                error(start, "versions must be strictly descending (no duplicates)")
            previous = numbers
        if match:
            version, label = match.groups()
            if label != "Release candidate":
                try:
                    date.fromisoformat(label)
                except ValueError:
                    error(start, "invalid calendar date; use YYYY-MM-DD")

        seen: set[str] = set()
        subsection: str | None = None
        subsection_line = start
        bullet_count = 0
        first_uncredited: int | None = None

        def finish_subsection() -> None:
            if subsection is not None and bullet_count == 0:
                error(subsection_line, "omit empty subsections")

        i = 1
        while i < len(lines):
            line = lines[i]
            number = start + i
            if not line.strip():
                i += 1
                continue
            if line.startswith("### "):
                finish_subsection()
                subsection = line[4:]
                subsection_line = number
                bullet_count = 0
                first_uncredited = None
                if subsection not in SUBSECTIONS:
                    error(number, "subsection must be one of: " + ", ".join(SUBSECTIONS))
                else:
                    if subsection in seen:
                        error(number, "duplicate subsection; append to the existing subsection")
                    if any(SUBSECTIONS.index(item) > SUBSECTIONS.index(subsection) for item in seen):
                        error(number, "subsections must follow: " + ", ".join(SUBSECTIONS))
                    seen.add(subsection)
                i += 1
                continue
            if not line.startswith("- "):
                error(number, "expected a single-line '- ' bullet, not prose or a continuation")
                i += 1
                continue
            if subsection is None:
                error(number, "bullet must be under a subsection")
            bullet_count += 1
            parts = [line[2:]]
            end = i + 1
            while end < len(lines) and lines[end].strip() and not lines[end].startswith(("- ", "#")):
                parts.append(lines[end].strip())
                end += 1
            if end > i + 1:
                error(number, f"bullet wraps onto lines {number + 1}-{start + end - 1}; use one line")
            body = " ".join(parts)
            link = PR_LINK.search(body)
            if link and first_uncredited is not None:
                error(number, f"credited contributions lead their subsection; move above line {first_uncredited}")
            elif not link and first_uncredited is None:
                first_uncredited = number
            statement = body[:link.start()] if link else body
            if LINK_LIKE.search(statement):
                error(number, "credits must be a single trailing attributed PR link in the prescribed form")
            if not statement.strip():
                error(number, "bullet needs a statement of working behavior")
            if len(statement) > 160:
                error(number, f"bullet text is {len(statement)} characters; maximum is 160 excluding the trailing credit")
            if statement != statement.strip() or any(mark in statement for mark in ("**", "__")):
                error(number, "use plain text without bold labels or surrounding whitespace")
            for pattern, message in PROSE_RULES:
                if re.search(pattern, statement, re.IGNORECASE):
                    error(number, message)
            i = end
        finish_subsection()
    return sorted(errors)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--changelog", type=Path, default=ROOT / "CHANGELOG.md")
    parser.add_argument("--fix", action="store_true", help="move credited contributions above uncredited bullets")
    args = parser.parse_args()
    try:
        text = args.changelog.read_bytes().decode("utf-8")
        if args.fix:
            fixed, moved = fix(text)
            if fixed != text:
                args.changelog.write_bytes(fixed.encode("utf-8"))
            for message in moved:
                print(f"{args.changelog}: {message}")
            text = fixed
        errors = check(text.replace("\r\n", "\n"))
    except (OSError, UnicodeError) as exc:
        print(f"{args.changelog}:1: cannot check changelog: {exc}", file=sys.stderr)
        return 1
    for number, message in errors:
        print(f"{args.changelog}:{number}: {message}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
