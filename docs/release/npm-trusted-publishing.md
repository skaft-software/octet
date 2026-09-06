# npm trusted publishing

**octet 0.7.0 is unpublished and not release-qualified.** Use the
[local checkout build](../../README.md#install). This maintainer reference
describes source packaging, not registry ownership or package availability. The
source/release/OIDC repository remains `skaft-software/ygg`.

The source contract defines four immutable packages:

- `@skaft/octet`: a shell-only launcher.
- `@skaft/octet-darwin-arm64`, `@skaft/octet-darwin-x64`, and
  `@skaft/octet-linux-x64-gnu`: the native runtime and packaged docs.

The npm scope is `@skaft`. This does not rename the GitHub source/release
repository (`skaft-software/ygg`) or the Rust CLI crate (`octet-coding-agent`).

All four versions must equal the canonical `vX.Y.Z` release tag's version. The
launcher has no npm lifecycle hook. It resolves only the installed optional
platform package before `exec`-ing `octet` or `octet-host`; it never downloads a
runtime. Linux musl and unsupported CPUs fail closed. Bun is unqualified.

## Local release gate

Package only verified native release assets. Never build from a mutable checkout
or read `Cargo.toml` to choose the release version:

```sh
scripts/package-octet-npm.sh VERSION release-assets npm-assets \
  release-assets/OCTET_SHA256SUMS
python3 scripts/create-octet-npm-manifest.py VERSION vVERSION \
  SOURCE_COMMIT WORKFLOW_COMMIT release-assets/OCTET_RELEASE_METADATA.json \
  npm-assets npm-assets/OCTET_NPM_MANIFEST.json npm-assets/OCTET_NPM_SHA256SUMS
python3 scripts/verify-octet-npm.py VERSION npm-assets
```

`OCTET_RELEASE_METADATA.json` is generated from the signed native checksum
manifest and records the tag, source commit, release-workflow identity, pinned
URLs, and SHA-256 values. The protected release job verifies its Sigstore
bundle and regenerates the document before packaging. The npm manifest records
the metadata digest and every tarball's SHA-256/SHA-512 digests. Local scripts
check deterministic packing, tarball/path/lifecycle/secret handling, and an
offline install; they do **not** prove registry publication or macOS acceptance.

The protected job downloads a fixed npm CLI tarball, verifies its recorded
SHA-512 integrity, and installs it with lifecycle scripts, audit, and funding
disabled. After publication, verification checks registry package integrity and
requires a provenance attestation binding the same artifact digest, repository,
release workflow, and source/workflow identity. A non-empty provenance field
alone is insufficient.

## Protected publication

A maintainer must configure trusted publishers for all four packages to the
repository's `release-octet.yml` workflow and `stable-release-publish`
environment. The workflow uses GitHub OIDC with `npm publish --provenance`, never
`NPM_TOKEN`, `NODE_AUTH_TOKEN`, a checked-in `.npmrc`, or another long-lived
registry credential. The environment is the human approval boundary.

Canonical binary tag runs leave npm publication disabled. Only after all four
trusted publishers and cryptographic provenance verification are ready, dispatch
`release-octet.yml` from the exact canonical `vX.Y.Z` tag with `release_tag` set
to that tag and `publish_npm=true`.

The publication job pins npm CLI `11.5.1` (or a later explicitly reviewed version
supporting trusted publishing) before requesting OIDC provenance. It:

1. waits for the signed GitHub binary release and published installer smoke;
2. verifies immutable release metadata and builds/validates all four tarballs in
   an unprivileged job;
3. preflights each `name@version` and continues only if an existing package's
   integrity matches exactly;
4. publishes all three platform packages before the launcher, with provenance
   and scripts disabled; and
5. verifies registry integrity and provenance, then runs version/help/host
   handshake/uninstall smokes on GNU/Linux, macOS Intel, and Apple silicon.

npm versions are immutable. After an upload timeout, inspect
`npm view name@version dist.integrity` and the provenance field. Never blindly
republish or overwrite a version.

## Partial publication recovery

Stop if any package is missing, has a different integrity, lacks provenance, or
fails a host smoke. Preserve signed release evidence and the failed package
name/version. Never use `npm unpublish` as automatic repair or reuse the version.

An authorized maintainer should deprecate the affected version with a short
failure message, record the registry response, and cut a new canonical octet
patch release. Publish platform-first, verify all four packages, and announce
the replacement. Leave a pending GitHub/npm release untouched until its
provenance is understood; revocation or closure is an explicit maintainer action.

## Installation and updates

This is a **future published-channel template**, not a current install
recommendation. Use it only after publication is authorized and the exact
package/version and provenance have been independently verified:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@VERSION
```

`octet update` offers npm automatically only for a physically validated global
layout. Local project and `npx` layouts get a manual command instead, so an octet
process never mutates a project's dependencies implicitly.
