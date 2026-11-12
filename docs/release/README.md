# Release operations

Use Python 3.11+, Git and authenticated `gh` from the repository root. One owner
at a time. Required environment reviewers stay enabled; no settings change is
needed. Version preparation and the full local gate happen before this flow.

```sh
python3 scripts/release-notes.py sync
python3 scripts/check-changelog.py
python3 scripts/release-notes.py check
# Commit, gate and push through the normal owner process. Refresh origin/main.
python3 scripts/release.py prepare X.Y.Z
python3 scripts/release.py start X.Y.Z
python3 scripts/release.py status X.Y.Z
# After status says complete:
python3 scripts/release.py ship X.Y.Z
python3 scripts/release.py status X.Y.Z
```

`prepare` changes nothing and contacts no service. It checks the workspace
version, dated/linted changelog, synchronized current notes, clean tree and
HEAD equal to the candidate on local `origin/main`. `start` verifies the exact
SHA has a completed successful full-tier **main push** CI run, including every
required lane. It creates one draft (no tag) and dispatches one candidate run
from `main` with the immutable source SHA as input. A pending/failed/missing CI
run fails immediately. Wait locally, not on a release runner.

The candidate builds Linux, Intel/Apple-silicon macOS and qualified native Windows
archives and all six extension bundles in parallel. npm tarballs are packed,
not published. Dependency audits, reproducibility, package inclusion and Windows
qualification are retained. No candidate job has signing permission or a
protected environment. It uploads unsigned assets and a JSON build receipt with
source, versions, run ids and SHA-256 digests to the same draft.

## Pause, continue, rebuild or discard

```sh
python3 scripts/release.py pause X.Y.Z
python3 scripts/release.py status X.Y.Z
python3 scripts/release.py resume X.Y.Z
# Explicit rebuild, same SHA:
python3 scripts/release.py respin X.Y.Z
# Or check out a newly gated, prepared main SHA with green main push CI:
python3 scripts/release.py respin X.Y.Z --sha FULL_SHA
python3 scripts/release.py abort X.Y.Z
```

Pause records the paused receipt before cancelling in-flight candidate runs.
Ship refuses paused candidates. Cancellation is asynchronous. Resume clears
pause and reruns only failed/cancelled jobs of the existing run (`gh run rerun
--failed`); successful build jobs/artifacts are reused. Respin explicitly creates
a new generation, even for the same SHA; a new SHA requires fresh CI qualification.
It never reuses old digests. Unselected generation assets remain in the draft.
Only a complete build receipt selects the new generation's entire asset set.
An interrupted upload cannot expose a partially replaced set.

Status reconstructs state from the draft's receipt pointer, immutable JSON
receipts and Actions runs; no local cache is required. Candidate workers never
edit the pointer, so cannot clear a concurrent pause. Receipts and candidate
assets have immutable names. Rerun an interrupted command; `start` recovers draft
creation/upload and accepted-dispatch interruptions. If a dispatch response is
lost and Actions exposes no run yet, the CLI fails closed rather than dispatching
a duplicate. Inspect Actions, wait for visibility and retry; only after confirming
no dispatch was accepted, use `respin` for a fresh generation. Do not hand-edit
receipts. Respin itself is an explicit new-build request; use resume for retries.

Abort deletes only the owned untagged draft, never a published release or tag.
Repeating it is safe. With no remaining remote object, status reports
`absent-or-aborted`; it does not invent an audit tombstone. The version can be
started again. Pause, respin and abort stop once ship freezes the receipt.

## Ship and recovery

Ship verifies the complete receipt, successful build, hosted digests **and
actual downloaded bytes**, pause state and current main ancestry. It freezes
the npm decision before creating `vX.Y.Z`, `octet-binaries-vX.Y.Z` and
`octet-serve-vX.Y.Z` at the exact candidate SHA. A different-SHA collision stops
before any write. Existing same-SHA tags from an interrupted ship are reused,
never moved or recreated. An unrelated existing canonical tag is refused.

The CLI dispatches `release-serve.yml` on `octet-serve-vX.Y.Z`, waits **locally**
for signing/approval, then dispatches `release-octet.yml` on
`octet-binaries-vX.Y.Z`. Those protected jobs download receipt-bound bytes and
sign with Sigstore keyless on the accepted tooling-tag identities. No runner
polls another workflow. The binary job verifies signed extensions before
publishing the stable, non-draft release with Latest unchanged. Native install
smokes cover Linux, both macOS targets and Windows; official extension
install/update/removal is checked on Linux. npm publishes platform-first with
provenance, verifies registry integrity/provenance and installs on all four
platforms. Only after those gates and all five npm `latest` checks does GitHub
Latest move.

If npm trusted publishing is unavailable, explicitly waive it before first ship:

```sh
python3 scripts/release.py ship X.Y.Z --skip-npm --reason 'registry incident and tracking reference'
```

The immutable ship receipt and job summaries record the reason. All native,
signing, approval and installation gates remain required. The default is mandatory
npm. Cross-service publication is not atomic: npm may advance during verification;
a failure leaves GitHub Latest unchanged. Never claim the channels are reconciled
until the [read-only audit](../distribution.md#read-only-channel-audit) is green.
Website deployment remains separate; consume the attached `release.json`.

Repeat `ship X.Y.Z` after interruption. It resumes the extension/binary run or
reruns failed jobs, reusing immutable published bytes and tags. Once public,
assets/receipts are never replaced. A changed asset/signature identity stops
recovery. Inspect failures; never delete a public release to reuse its version.
A lost dispatch response is reconciled by run title. A durable dispatch intent
prevents blind replay if Actions has not exposed a run. If no run is visible,
inspect Actions and wait. Only after confirming no request was accepted, manually
dispatch the correct tooling workflow with `--ref octet-serve-vX.Y.Z` (extensions)
or `--ref octet-binaries-vX.Y.Z` (binary), `-f release_tag=vX.Y.Z` and
`-f candidate=GENERATION_FROM_STATUS`; then repeat ship. Never guess a generation.
The local approval/watch wait has a 24-hour deadline and can be resumed.
Retain candidate artifacts until ship completes (Actions retention is 14 days).
If artifacts expire before a failed-job retry, explicitly respin before shipping.

All release workflows are **dispatch-only**, including tooling/installer repair
tags. Pushing a canonical tag cannot launch a duplicate release. Existing manual
recovery still supports `force_release=true` plus a nonempty single-line
`force_reason`, recorded as **WAIVED, not passed**. It waives only exact-source CI,
not ancestry, version, audits, Windows checks, approvals or signing. Normal
candidate start/respin never waives CI.

```sh
gh workflow run release-octet.yml --ref octet-binaries-vX.Y.Z \
  -f release_tag=vX.Y.Z -f force_release=true -f force_reason='reviewed CI incident'
gh workflow run release-serve.yml --ref octet-serve-vX.Y.Z -f release_tag=vX.Y.Z
gh workflow run release-octet.yml --ref octet-installer-vX.Y.Z -f release_tag=vX.Y.Z
```

Repair tags may equal the source commit; otherwise their tooling commit must
descend from source and change only release workflows. Installer repair appends
its versioned installer, never replaces the original. Homebrew is a separate
protected handoff. See [signer compatibility](legacy-identity-boundary.md) and
[npm requirements](npm-trusted-publishing.md). Offline tests are not evidence of
hosted approvals, tag creation, OIDC, provenance, native installation or a real
0.10.3-to-new-version update; rehearse those with explicit owner authorization.

## Release text

Only edit the dated current-version section in `CHANGELOG.md`. `release-notes.py
sync` updates only that version's public and bundled notes; historical notes are
frozen. The GitHub body and attached `release.json` use the same generated text.
Do not write separate release prose. `release-surfaces.yml` audits Latest, npm,
documentation and the separately deployed website after ship and daily.
