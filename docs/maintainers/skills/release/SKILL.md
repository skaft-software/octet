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

## 1. Prepare the version bump

Run the [`/cl`](../../prompts/cl.md) audit, update `CHANGELOG.md`, add
`docs/releases/vX.Y.Z.md`, and bump the workspace and pinned extension versions.
Review release notes, required checks and any release-specific limitations.
For local candidate checks, read `scripts/package-octet-release.sh` first, package
into an isolated output directory, and smoke-test the packaged binary's startup,
version, help and model listing. A real provider prompt and the
[interactive-testing skill](../interactive-testing/SKILL.md) are separate,
explicitly authorized acceptance checks; startup alone is not a passing smoke test.

## 2. Qualify the exact main commit

The maintainer pushes directly to `main`. Contributors use squash PRs gated by
`ci-required`; there is no merge queue. PR CI is the fast tier unless labelled
`ci:full`. Pushes to main and manual `ci.yml` dispatches run the full matrix.
Identify the actual main commit, not a PR head or synthetic merge commit. Every
release source must be an ancestor of `origin/main`.

Before tagging, require a completed successful full-tier main **push** CI run for
that exact commit, with every release-required lane successful (not skipped).
GitHub runs push CI only for the pushed head. An intermediate main commit may
have no push run: prefer the qualified pushed head. Otherwise dispatch `ci.yml`
on the desired exact ref, verify its full-tier success, and use the manual
release recovery path below. A green fast-tier PR is not release qualification.

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
and all required lanes. A manual dispatch or repair accepts the latest
exact-source main push or manual CI run. A failed run or timeout stops
publication; inspect the failure rather than treating it as qualified. If no
main push run exists, a tag-push run cannot use dispatch evidence: keep the tag
immutable and use manual recovery. After the existing protected publish
approval, the workflow creates a draft GitHub Release from the checked-in notes,
attaches signed binary assets, verifies their GitHub digests, and then publishes
it as a stable release. A separate job marks it Latest only after every
published-install smoke passes, so `octet update` and the startup notice never
advertise assets that failed public installation. A failed upload leaves the
previous Latest release available.
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

For a source with no main push CI run, dispatch `ci.yml` on that exact source
ref and wait for the full matrix to succeed. Then dispatch `release-octet.yml`
from the unchanged canonical `vX.Y.Z` tag with `release_tag=vX.Y.Z` (without a
waiver); once it succeeds, dispatch `release-serve.yml` from that same tag. This
also recovers a failed automatic extension waiter. A dedicated tooling tag is
needed only when the workflow itself needs repair, not to rerun unchanged code.

Only an authorized manual binary-workflow dispatch can set `force_release=true`;
it must also supply a non-empty, single-line `force_reason`. The job summary
records **Source CI qualification WAIVED: <reason> by <actor>**. The protected
binary and installer publish jobs also put the waiver first in the release notes;
npm-only dispatches record it in the summary. This waives only the source CI
gate, not main ancestry, version, audit, protected publishing or signing.
A binary draft is owned by its workflow run. A rerun resumes it when every
existing asset matches the new candidate byte for byte or, for the freshly
signed Sigstore bundles, still verifies the identical candidate under the exact
workflow identity; the extension and npm evidence uploads follow the same rule.
Anything else stops for inspection instead of replacing assets. Inspect failed
jobs before recovery and never replace immutable assets or silently relabel a
waiver as a CI pass.
