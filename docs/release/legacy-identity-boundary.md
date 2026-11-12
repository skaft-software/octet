# Published 0.10.3 compatibility boundary

## Finding and decision required

A checksum signature produced by `release-octet.yml@refs/heads/main` is **not
accepted by the published 0.10.3 shell installer's verifier**. Keeping the workflow
filename does not preserve its accepted identity. Do not broaden that verifier,
claim a main-dispatched signature is compatible, or substitute an offline policy
replay for hosted signing evidence.

There is also a distinction between two requirements:

1. **An existing 0.10.3 binary updates to the new release.** Its updater downloads
   the *target version's* installer, which then verifies that target's assets.
2. **The frozen, published 0.10.3 shell installer verifies a new release's assets.**
   This cannot work unchanged: its version, signer workflow SHA and complete asset
   set are pinned to 0.10.3. Even a new checksum manifest with an otherwise accepted
   old identity fails its exact asset-set check.

The lifecycle/signing implementation is blocked on clarifying this boundary.
The existing tag workflows, repair entry points, approvals and verifiers are
unchanged. There is no new owner release CLI, candidate workflow or npm promotion
path in this change. The notes, lint and read-only surface audit are independent.

If the intended requirement is (1), an accepted tag-scoped signing continuation
*after* protected main-origin shipping creates the tag is a possible design to
validate next. It must retain the exact workflow filename, workflow-name,
repository, issuer and workflow-SHA binding below; neither a new signing filename
nor a main-ref identity is accepted just because it signs the same checksums.
No such continuation has been implemented or hosted-qualified here.

## Immutable evidence

Tagged source commit: `3ada5545684a28632ca6b2325a860b3e322b217b` (0.10.3).
Three GET-only downloads of the published assets were inspected. SHA-256:

| Published asset | SHA-256 |
| --- | --- |
| `install-octet.sh` | `6c58bb40bfb89c853c0e00e65c02c89fa549469c0682504e2d0edf78d5d5754b` |
| `install-octet.ps1` | `de5dd9c7eb92f953445695a371363b26054f07771cc5daae8bc83d1ccaf71902` |
| `OCTET_SHA256SUMS.sigstore.json` | `0941779df29119c7cb40b8ab278931d77f6006a78c16d0a7eec654e55cc9cad5` |

Published shell installer locations (line numbers are in the downloaded asset):

- Lines 6–12 bind repository, version, source SHA, workflow SHA and download tag.
- Lines 335–341 accept only
  `https://github.com/skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/(v0.10.3|octet-binaries-v0.10.3)`.
- Lines 358–365 hard-code the two installers and four 0.10.3 archives.
- Lines 402–406 additionally require issuer
  `https://token.actions.githubusercontent.com`, workflow name
  `octet binary release`, repository `skaft-software/octet` and workflow SHA
  `3ada5545684a28632ca6b2325a860b3e322b217b`.
- Lines 439–440 reject any other complete manifest asset set.

The certificate extracted from the published Sigstore bundle, inspected using
`openssl x509 -inform DER -noout -text`, contains:

```text
SAN: https://github.com/skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v0.10.3
issuer: https://token.actions.githubusercontent.com
workflow name: octet binary release
workflow repository: skaft-software/octet
workflow SHA: 3ada5545684a28632ca6b2325a860b3e322b217b
```

The published installer differs from the tagged template: the source placeholder
is filled, a separate `release_workflow_commit` is inserted and the verifier
receives that workflow SHA. The offline fixture preserves the **published**
configuration and verifier function, not the current checkout's verifier.

The published PowerShell installer is byte-identical to its tagged source. Lines
4 and 11–12 accept a target version and select its versioned asset URL. Lines
20–24 check SHA-256; line 34 explicitly says it does not independently verify the
Sigstore signature. There is no PowerShell certificate identity policy to claim
has accepted a candidate signature.

In the tagged `crates/octet-coding-agent/src/update.rs`, `run_installer` at lines
1094–1140 downloads `/v{version}/install-octet.sh` and executes the completed
bounded download. `run_windows_installer` at lines 1146–1214 similarly downloads
the target PowerShell installer and supplies `-Version`. The old Rust updater
itself does not verify Sigstore identities. This explains why (1) and (2) above
are different compatibility contracts.

## Offline regression evidence

`scripts/tests/test_release_legacy_identity.py` executes the frozen published
shell verifier against local manifests. Only cosign downloading and cryptography
are stubbed; the stub evaluates **all actual policy flags** passed by the old
function. Tests cover the accepted old canonical/repair identities, rejected main
and new-version identities, rejected new asset sets, and each binding mismatch.
The certificate DER and downloaded-asset hashes are recorded in
`scripts/fixtures/release-v0103/evidence.json`.

These are policy tests, **not cryptographic verification, a hosted OIDC
qualification, Windows qualification, or an end-to-end update test**. No new
candidate was built or signed. Published tags/assets and repository settings were
not changed.
