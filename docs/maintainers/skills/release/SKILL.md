---
name: release
description: Prepare, package, publish, verify, and recover octet releases. Use for release preparation, local packaging checks, publishing, and failed release CI.
version: 0.1.0
required-tools:
  - read
  - bash
tags:
  - maintainer
  - release
---
# Releasing octet

Run repository commands from the repo root. Read the release workflows and
scripts before running them. The workspace version in `Cargo.toml` is the source
of truth; extension manifests also pin `requires_octet`.

## 1. Open the version-bump PR

Run the [`/cl`](../../prompts/cl.md) audit, update `CHANGELOG.md`, add
`docs/releases/vX.Y.Z.md`, and bump the workspace and pinned extension versions.
Review release notes, required checks and any release-specific limitations.
For local candidate checks, read `scripts/package-octet-release.sh` first, package
into an isolated output directory, and smoke-test the packaged binary's startup,
version, help and model listing. A real provider prompt and the
[interactive-testing skill](../interactive-testing/SKILL.md) are separate,
explicitly authorized acceptance checks; startup alone is not a passing smoke test.

## 2. Merge through the queue

Merge the reviewed PR to `main` via the GitHub merge queue. Identify the actual
main commit produced by the merge, not the PR head or merge-group test commit.
The source for every release tag must be an ancestor of `origin/main`.

## 3. Push one canonical tag

Before tagging, confirm that `stable-release-publish` allows `v*` tags. Its
currently observed custom policy allows `main`, `octet-binaries-v*` and
`octet-serve-v*`, but not canonical tags; an authorized administrator must update
that policy separately without removing the existing required reviewers.

Create and push exactly one new `vX.Y.Z` tag pointing at that main commit. Never
move or re-push an existing release tag. The tag version must match `Cargo.toml`
and the checked-in release notes. The tag triggers both the binary and extension
release workflows; no prior GitHub Release or second tooling tag is needed.

## 4. Let the workflows qualify and publish

On a canonical tag push, the binary workflow waits up to 75 minutes for the
latest exact-source `main` push CI run, then requires its successful conclusion
and all required lanes. Repairs also accept exact-source merge-group and manual
CI runs. A failed run or timeout stops publication; inspect the failure rather
than treating it as qualified. After the existing protected publish approval,
the workflow creates a draft GitHub Release from the checked-in notes, attaches
signed binary assets, verifies their GitHub digests, and only then publishes it as
Latest. A failed upload leaves the previous Latest release available.
The extension workflow waits for that exact binary tag-push run to succeed before
building and attaching the six official bundles. Its wait is bounded at 350
minutes, including binary build and approval time. Signing, environment approval,
and source/version checks remain mandatory.

npm and Homebrew remain separate, explicitly dispatched channels; a tag push
alone does not publish them. Their handoffs and the installer-repair path accept
both the canonical-tag signer (bound to the source commit) and the legacy binary
tooling-tag signer. See the
[npm trusted-publishing contract](../../../release/npm-trusted-publishing.md) and
[distribution guide](../../../distribution.md).

## 5. Verify

Inspect both workflow results and the exact-version GitHub Release. Verify the
native archives, installers, six extension bundles, checksums and signatures,
and the published install/update smoke results. Record what was actually
verified; do not describe an unrun check as qualified.

## Recovery and CI waiver

Keep the canonical tag immutable. `octet-binaries-vX.Y.Z` and
`octet-serve-vX.Y.Z` remain dedicated workflow-only repair tags: their tooling
commit must descend from the source and change only release workflows. They do
not replace the normal canonical-tag flow. `octet-installer-vX.Y.Z` remains the
separate installer repair path.

Only an authorized manual binary-workflow dispatch can set `force_release=true`;
it must also supply a non-empty, single-line `force_reason`. The job summary
records **Source CI qualification WAIVED: <reason> by <actor>**. The protected
binary and installer publish jobs also put the waiver first in the release notes;
npm-only dispatches record it in the summary. This waives only the source CI
gate, not main ancestry, version, audit, protected publishing or signing.
A binary draft is owned by its workflow run; reruns can resume it only when its
existing assets match the new candidate's digests. Otherwise stop and inspect
it rather than publishing or replacing assets. Inspect failed jobs before recovery
and never replace immutable assets or silently relabel a waiver as a CI pass.
