---
description: Audit octet changelog entries before release
argument-hint: "[optional focus]"
---
Audit `CHANGELOG.md` for every commit since the last release.

Focus: ${@:-all user-visible commits since the last release}

## Process

1. Find the last release tag:

   ```sh
   git tag --sort=-version:refname | head -1
   ```

2. List commits since that tag:

   ```sh
   git log <tag>..HEAD --oneline
   ```

3. Read the `## [Unreleased]` section of the root `CHANGELOG.md`.

4. For each commit, use `git show <hash> --stat` to decide whether it is
   user-visible:
   - Skip changelog edits, doc-only changes, release housekeeping, and
     regeneration-only diffs (checked-in model metadata, generated protocol
     artifacts) unless they carry an intentional product-facing change.
   - Otherwise verify an entry exists under `## [Unreleased]`.

5. Keep entries in octet's voice: one sentence, user-visible effect first, no
   internal jargon, and links to the owning doc where useful.

6. Report commits with missing entries, entries that describe internal refactors
   with no user-visible effect, and any entry that overstates unverified
   behavior (source-only or unrun checks must not be described as qualified).

Do not run the repository-wide formatter or commit as part of a changelog audit.
