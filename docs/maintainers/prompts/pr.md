---
description: Review an octet pull request from a URL without checking out its branch
argument-hint: "<PR-URL>"
---
Review these pull requests: ${@:-<PR-URL>}

For each PR URL:

1. Read the PR page in full: description, all comments, all commits, and every
   changed file. Add an `inprogress` label only if the user asks; do not post
   comments without approval.
2. Identify linked issues referenced anywhere (body, comments, commits, cross
   links) and read each in full.
3. Analyze the diff **without** checking out or switching to the PR branch. Use
   `gh pr diff`, `gh pr view`, `gh api`, and local default-branch files; when PR
   content is needed, read fetched refs with `git show <ref>:<path>`. Read every
   relevant file in full, including code paths outside the diff needed to
   validate behavior.
4. Do not expect a changelog entry in a contributor PR. Per
   [CONTRIBUTING.md](../../../CONTRIBUTING.md), maintainers add
   `CHANGELOG.md` entries; note the entry that will be needed.
5. Check whether user-facing docs need updating: `docs/README.md` links, the
   owning topic page, and `docs/parity/repo-tooling.md` when parity is affected.

Report with these sections, in order:

- **What it does** — one short paragraph with the change and its intent.
- **Good** — solid choices or improvements.
- **Bad** — concrete issues, regressions, missing tests, or risks.
- **Ugly** — subtle or high-impact problems.
- **Tests** — what is covered, what is missing, whether existing tests are
  adequate, and the exact command run.
- **Open questions** — only blockers that need the user's decision; omit if none.

Be direct about disagreement. Never run the repository-wide formatter.
