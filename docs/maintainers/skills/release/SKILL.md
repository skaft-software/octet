---
name: release
description: Prepare, package, publish, verify, and recover octet releases. Use for release preparation, local packaging checks, publishing, and failed release CI.
version: 0.2.0
required-tools:
  - read
  - bash
tags:
  - maintainer
  - release
---
# Releasing octet

Read the [owner runbook](../../../release/README.md), workflows and scripts first.
Use one owner at a time. Tags and GitHub writes require explicit authorization.
The workspace version is authoritative; extension `requires_octet` pins match it.

Run the [`/cl`](../../prompts/cl.md) audit, prepare versions and a dated changelog
section, then generate current notes only. Historical notes are frozen.

```sh
python3 scripts/release-notes.py sync
python3 scripts/release-notes.py check
python3 scripts/check-changelog.py
```

The changelog is the only release-text source. Notes preserve its section
verbatim, including `###` subsections. `render` prints the GitHub body;
`artifact --commit FULL_SHA` prints website JSON without deploying it.
Review, commit, run the full gate and use the normal owner main push process.

```sh
python3 scripts/release.py prepare X.Y.Z
python3 scripts/release.py start X.Y.Z
python3 scripts/release.py status X.Y.Z
python3 scripts/release.py ship X.Y.Z
```

Prepare is local/read-only. Start requires completed successful full-tier main
push CI for the exact SHA with all release-required lanes successful, not skipped.
One unsigned candidate run builds the native matrix (including Windows), six
extension bundles and prepacked npm tarballs into one draft. No tags or signing
until ship. No runner polls CI or another workflow.

```sh
python3 scripts/release.py pause X.Y.Z
python3 scripts/release.py resume X.Y.Z
python3 scripts/release.py respin X.Y.Z
python3 scripts/release.py respin X.Y.Z --sha FULL_NEW_MAIN_SHA
python3 scripts/release.py abort X.Y.Z
```

Resume reuses successful build jobs; respin requests a new generation. Paused
receipts block ship. Abort deletes only an untagged draft. State is reconstructed
from remote receipts and runs. If a dispatch outcome is unknown, reconcile before
replaying; never launch duplicate builds to work around delayed Actions visibility.

Ship freezes verified digests and the npm decision, creates three same-SHA tags,
and dispatches protected signing on `octet-serve-vX.Y.Z` then
`octet-binaries-vX.Y.Z`. Keep `stable-release-publish` settings/reviewers unchanged.
The CLI waits locally for the extension handoff. Published-install, npm provenance
and npm install verification gate Latest. npm is mandatory unless first ship uses
`--skip-npm --reason 'reviewed incident'`; retain the receipt/summary reason.
Never move/recreate tags or replace public assets. Repeat ship to recover.

All release workflows are dispatch-only. Dedicated workflow-only repair tags
(including same-commit tags), `octet-installer-v*`, and explicit
`force_release=true` plus `force_reason` remain available for manual recovery.
A waiver says **WAIVED, not passed**, and waives CI only. See the runbook for
exact repair commands, [distribution](../../../distribution.md) and
[npm trusted publishing](../../../release/npm-trusted-publishing.md).

Inspect actual runs, assets and public install/update results; don't claim unrun
checks passed. [Signer policy tests](../../../release/legacy-identity-boundary.md)
are not hosted OIDC or end-to-end update qualification. Local packaging/startup
checks aren't live-provider or [interactive testing](../interactive-testing/SKILL.md).
Website and Homebrew deployment remain separate owner handoffs. Require the
read-only channel audit to reconcile public surfaces after ship.
