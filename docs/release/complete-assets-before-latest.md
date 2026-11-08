# Complete assets before Latest

A canonical `vX.Y.Z` release must carry its **complete** asset set before it
becomes the channel default (GitHub's *Latest* release). Incomplete assets must
never be Latest, because `octet update`, the public installers and every
`releases/latest` consumer resolve the channel default, not a tag.

This page records what is enforced now, what remains an unimplemented proof, and
the exact work an automated promotion would still need.

## Status at a glance

| Part | State |
| --- | --- |
| Core staging (`publish` in `release-octet.yml`, `publish` in `release-serve.yml`) refuses uploads when the target is observed as Latest | **Enforced** as a pre-upload observation |
| Core staging must be created `--latest=false` before any asset | **Documented precondition**; a violation is refused |
| Installer portability repair and npm evidence uploads | **Post-publication supplements**, deliberately not staging-guarded |
| Completeness validator (`scripts/validate-release-completeness.py`) | **Implemented and tested offline** |
| Signature / source authentication of the manifests | **Not implemented** |
| Automated or authorized promotion to Latest | **BLOCKED** — nothing sets `make_latest` |

Nothing in this repository sets `make_latest`. A maintainer must not promote on
the strength of the validator's output alone: its receipt is a completeness
observation, not authority.

## What was wrong before

- `resolve` required the release to exist already and refused drafts and
  prereleases, so assets could only be attached to a published release.
- `publish` attached the signed native archives and installers, and
  `release-serve.yml` attached the six official bundles in a *separate*
  protected workflow. Neither workflow saw the union.
- Nothing set or validated `make_latest`. A release created without
  `make_latest=false` was Latest-eligible at creation with zero assets, and no
  step promoted a verified release afterwards. `octet update` would then resolve
  a release whose installers and bundles were still arriving.

## Release ordering (current, partly manual)

1. **Stage.** Create the canonical release as *published but not Latest*, before
   any asset exists:

   ```sh
   gh release create vX.Y.Z --verify-tag --notes-file docs/releases/vX.Y.Z.md \
     --latest=false
   ```

   The release must be published, not a draft. `verify-published` and
   `verify-installer-repair` download
   `releases/download/vX.Y.Z/install-octet.sh` to prove the public install path,
   and drafts serve no public asset URL. Draft staging would require deleting
   that validation, so it is deliberately not supported.

2. **Attach the native assets.** Dispatch `release-octet.yml` for the tag (tag
   push, or `workflow_dispatch` with `release_tag`). It signs, verifies and
   attaches the native archives, both installers, `OCTET_SHA256SUMS` and
   `OCTET_RELEASE_METADATA.json` with their Sigstore bundles.

3. **Attach the bundles.** Dispatch `release-serve.yml` from the canonical tag.
   It builds the six catalogued bundles (including the per-target native
   codemode runner), signs them, and verifies install/update/remove from the
   published release.

4. **Promote.** Blocked. The release is not made Latest by any workflow. Before
   promotion is authorised by a human, all of the following must be recorded
   separately: authenticated signatures and source binding for both manifests,
   complete qualification evidence for the release, and the protected
   environment approvals. The completeness receipt alone is **not** sufficient.

## Enforced: the two core staging guards

`publish` in `release-octet.yml` and `publish` in `release-serve.yml` read
`repos/{repo}/releases/latest` and refuse to upload when the target tag *is* the
channel default. A release that is already Latest while its assets are still
arriving is exactly the incomplete-Latest state this ordering forbids.

This is a **pre-upload observation, not atomic protection**. It closes the
mistake where a release is created as Latest and then filled in by our own
workflows. It cannot stop a concurrent change between the observation and the
upload, and it does not stop a human from promoting a release directly.

The guard is strict about the API shape. `gh api` renders the raw GitHub **REST**
document, where the latest tag field is `tag_name`; `gh release view` uses
camelCase `tagName`. A `tagName` filter returns `null` and would silently disable
the guard, so the guard also refuses an empty or `null` answer.
`scripts/tests/test_release_completeness.py` runs the real guard body against a
REST document with the real `jq`, and fails if the filter regresses to camelCase.

The check runs only when there is something to upload, so an idempotent re-run
that finds every asset already byte-identical still passes.

### Post-publication supplements are not guarded

`publish-installer-repair` (the portable-installer portability repair) and
`publish-npm` (npm evidence) add supplemental assets to a release that was
already published. Both are explicitly authorized protected operations that
legitimately target the current release, including one that is already Latest, so
neither acquires a staging precondition or a demotion requirement. They keep
their existing protections unchanged: protected environment approval, keyless
signing and verification, immutable-digest refusal for an existing asset name,
and the source/tooling-tag ancestry checks.

## Implemented: completeness validator

`scripts/validate-release-completeness.py` answers "is every required asset
present, byte-identical to its manifest entry, and accounted for?". It fails
unless all of the following hold and prints every problem instead of stopping at
the first:

- the input is the GitHub **REST** release document
  (`gh api repos/{repo}/releases/tags/{tag}`); a camelCase `gh release view`
  document is rejected rather than compared as null;
- the release is published, stable, and its tag matches the resolved tag; the
  resolved tag must be exactly `v<version>` and the source commit is required;
- `OCTET_SHA256SUMS` covers exactly the native publication set — the four
  platform archives (Linux GNU, macOS Intel, macOS Apple silicon, Windows MSVC)
  and `install-octet.sh` / `install-octet.ps1`;
- `SHA256SUMS` covers exactly the bundle inventory the extension workflow
  builds: one `<id>-<version>.tar.gz` for every non-codemode entry in
  `extensions/release-catalog.txt`, and one
  `octet-codemode-<version>-<target>.tar.gz` for each supported codemode target.
  A generic `octet-codemode-<version>.tar.gz` (the historical launcher bundle),
  an unknown target, a target-suffixed generic bundle and any extra variant are
  rejected;
- every manifest entry exists as a release asset whose immutable GitHub
  `digest` equals the manifest digest (exact bytes, not just presence);
- **the three control documents are the published bytes**: the supplied
  `OCTET_SHA256SUMS`, `SHA256SUMS` and `OCTET_RELEASE_METADATA.json` each hash to
  their own `assets[].digest`, so a locally edited or reformatted document is
  refused even when it parses to the same content;
- every payload, manifest and metadata file carries its `.sigstore.json`
  sibling, and a signature bundle for an absent optional asset is refused;
- `OCTET_RELEASE_METADATA.json` is the expected schema, is bound to the tag,
  version and source commit, records the manifest digest of the attached
  `OCTET_SHA256SUMS`, and lists exactly the same native assets and digests, with
  no duplicate asset records;
- no other asset is present. Two closed, version-derived sets are recognised and
  must be signed when present: the tag-pinned installer repair
  (`install-octet-vX.Y.Z.sh`) and the npm evidence set (`octet-*.tgz`,
  `OCTET_NPM_SHA256SUMS`, `OCTET_NPM_MANIFEST.json`). Nothing is matched by
  wildcard and every optional payload still needs its signature bundle.

There is no flag, environment variable or partial mode that skips a check. The
validator produces a JSON receipt (`--report`) with the verified asset counts,
the covered extensions, the codemode targets and an `assets.fingerprint`.

**The validator is offline and unauthenticated.** It never runs cosign, and a
sender who controls the release document can satisfy every check without any
signature. Digest equality with `assets[].digest` proves the checked bytes are
the published bytes; it does not prove who published them.

### Historical releases

`v0.10.3` was published with a single generic `octet-codemode-0.10.3.tar.gz`,
which predates the per-target native runner. That release cannot satisfy the
current bundle inventory, and it is deliberately not special-cased: the
post-publication repair paths keep working, but the completeness contract
describes the *next* release's asset set, not a retroactive one.

## BLOCKED: the unimplemented promotion proof

Automated promotion is not shipped because the authentication half does not exist
yet. To promote only a genuinely verified release, a gate would need to establish,
without trusting any downloaded file:

1. `OCTET_SHA256SUMS.sigstore.json` and
   `OCTET_RELEASE_METADATA.json.sigstore.json` verify those files for the
   identity
   `https://github.com/skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/octet-binaries-vX.Y.Z`
   with the `octet-binaries-vX.Y.Z` tooling commit as
   `--certificate-github-workflow-sha`. This chain already exists in
   `publish-installer-repair` and can be reused verbatim.
2. `SHA256SUMS.sigstore.json` verifies `SHA256SUMS`. `release-serve.yml` signs
   with `github.workflow_ref`, so exactly two identities are legal for one
   release: `...release-serve.yml@refs/tags/octet-serve-vX.Y.Z` for a tag push
   (certificate SHA = the `octet-serve-vX.Y.Z` tooling commit) or
   `...release-serve.yml@refs/tags/vX.Y.Z` for a dispatch from the canonical tag
   (certificate SHA = the source commit). Two identities are not inherently
   weaker than one: each is an immutable ref plus a pinned workflow SHA. What is
   missing is code that resolves **both** pins independently, verifies against
   whichever one the certificate actually carries, and records which one matched.
3. A record that binds the two verified manifests into the same release
   decision — today the only shared context is the asset list itself.

So the gap is a specific, implementable proof, not an impossibility. It is
unimplemented, which is why nothing here promotes.

### Implementation options

Either shape works once the proof above exists:

- **A. Verify in a promotion job.** A protected job downloads the three control
  documents and their signature bundles, verifies all three documents with
  `cosign verify-blob` using the pinned identities and SHAs, then runs the completeness validator on
  the verified bytes before patching `make_latest`.
- **B. Verify inside publication.** Extend the protected `publish` path so a
  single run produces, signs, verifies and then evaluates the union, and only
  that run may patch `make_latest`.

Whichever is chosen, the promotion must also be gated on the protected
environment approvals and on complete qualification evidence for the release, not
on the completeness receipt alone.

## Deliberately unchanged

- Signing, Sigstore verification and attestations, the exact-byte immutability
  checks, source ancestry and tooling-tag checks, the protected environments and
  their required reviewers, npm trusted publishing and provenance verification,
  and the CI/quality gates are untouched.
- Publication permissions are not broadened: this change adds no privileged job,
  no new environment and no new `contents: write` holder.
- Draft staging stays rejected, so the public installer and bundle validation is
  never removed to make a workflow simpler.
- npm publication is not gated on this contract. It is a different channel that
  consumes the same already-verified native manifest.

## Remaining human prerequisites

- **Staging creation stays manual.** No workflow creates the release: creating
  tags and release objects would widen write permission beyond attaching assets
  to an existing release. The guards detect and refuse the mistake instead.
- **First release in a repository without a Latest.** `releases/latest` does not
  exist yet, so the guards fail closed with an explicit message instead of
  guessing.


## Verification

CI auto-discovers the test through `python3 -m unittest discover -s scripts/tests`
and names both the validator and its test in the explicit `py_compile` list.
These local contracts do not authenticate a release.

```sh
python3 scripts/tests/test_release_completeness.py
python3 scripts/validate-release-completeness.py --help
```

The two core staging guards, the REST field contract, the installer-repair
exemption, the retained public-installer validation and the absence of any
`make_latest` step are asserted in `scripts/tests/test_release_completeness.py`,
so a later edit cannot quietly remove them. Those are shape and behaviour tests:
they are not release qualification, and nothing here was exercised against a real
release.
