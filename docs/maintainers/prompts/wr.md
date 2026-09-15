---
description: Finish an octet task end to end with changelog, docs, and evidence
argument-hint: "[extra instructions]"
---
Wrap up the current task. Extra instructions: ${@:-none}

Determine context from the conversation first. Unless the request overrides it,
do the following in order:

1. Verify the change compiles and the regression test passes. Run the narrowest
   command that proves the behavior and report the observed output. Never claim an
   unrun check passed; separate pre-existing failures from your own.

2. Add a `## [Unreleased]` entry to [CHANGELOG.md](../../../CHANGELOG.md) when the
   change is user-visible, in octet's one-sentence voice.

3. Update the owning documentation. User-facing topics link from
   [docs/README.md](../../README.md); provider behavior belongs in
   `docs/providers.md`, themes in `docs/themes.md`, and Pi parity in
   `docs/parity/README.md` / `docs/parity/repo-tooling.md`.

4. Inspect the diff (`git diff` for your paths only) and confirm nothing
   unrelated was touched. Do not touch another worker's paths.

5. Stage explicit paths only (`git add <path>`), never `git add -A`. Commit only
   when the user asks, and never run a repository-wide formatter, reset, rebase,
   stash, or checkout.

6. Report: files changed, the exact commands run and their results, and any
   remaining blocker with the precise missing primitive or owning path.
