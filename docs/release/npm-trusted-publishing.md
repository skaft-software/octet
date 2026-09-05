# npm trusted publishing

octet's npm source packaging contract defines four immutable packages. These
names are not proof of registry rights, availability, or publication; use the
[local checkout build](../../README.md#build-this-checkout) today. The actual
source/release/OIDC repository remains `skaft-software/ygg`.

The source package identities are:

- `@skaft-software/octet` is the shell-only public launcher.
- `@skaft-software/octet-darwin-arm64`, `@skaft-software/octet-darwin-x64`, and
  `@skaft-software/octet-linux-x64-gnu` contain the native runtime and packaged
  documentation.

All four versions are the exact version of the canonical `vX.Y.Z` release tag.
The launcher has no npm lifecycle hook and resolves only the installed optional
platform package before `exec`-ing `octet` or `octet-host`. It does not download a
runtime. Linux musl and unsupported CPUs fail closed.

## Local release gate

Package only the already verified native release assets; do not build from a
mutable checkout or read `Cargo.toml` to choose the release version:

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
that metadata digest and the SHA-256/SHA-512 digest of every npm tarball. The
local scripts prove deterministic packing, tarball/path/lifecycle/secret
checks, and an offline install; they do **not** prove registry publication or
macOS-host acceptance.

The protected release job also downloads a fixed npm CLI tarball, checks its
recorded SHA-512 integrity, and installs it with lifecycle scripts, audit, and
funding disabled before publication. Post-publish verification checks the
registry's package integrity and requires the provenance attestation to bind the
same artifact digest, repository, release workflow, and source/workflow
identity; a mere non-empty provenance field is insufficient.

## Protected publication

A maintainer must configure npm trusted publishers for all four packages to the
repository's `release-octet.yml` workflow and the `stable-release-publish`
environment. The workflow uses GitHub OIDC with `npm publish --provenance`; it
must not use `NPM_TOKEN`, `NODE_AUTH_TOKEN`, a checked-in `.npmrc`, or another
long-lived registry credential. The environment is the human approval boundary.

Canonical binary tag runs leave npm publication disabled. Only after the four
trusted publishers and cryptographic provenance verification are ready, dispatch
`release-octet.yml` from the exact canonical `vX.Y.Z` tag with `release_tag` set to
that tag and `publish_npm=true`.

The publication job pins npm CLI `11.5.1` (or a later explicitly reviewed
version that supports trusted publishing) before it requests OIDC provenance.

1. waits for the signed GitHub binary release and published installer smoke;
2. verifies the immutable release metadata and builds/validates all four
   tarballs in an unprivileged job;
3. preflights each `name@version` and continues only when an existing package's
   integrity matches exactly;
4. publishes the three platform packages before the public launcher, with
   provenance and scripts disabled; and
5. verifies registry integrity and provenance, then runs version/help/host
   handshake/uninstall smokes on GNU/Linux, macOS Intel, and Apple silicon.

npm versions are immutable. A timeout after a successful upload must be
inspected with `npm view name@version dist.integrity` and the provenance field;
never blindly republish or overwrite a version.

## Partial publication recovery

Stop the workflow if any package is missing, has a different integrity, lacks
provenance, or fails a host smoke. Preserve the signed release evidence and the
failed package name/version. Do not use `npm unpublish` as an automatic repair
and do not reuse the version.

An authorized maintainer should deprecate the affected package version with a
short failure message, record the registry response, and cut a new canonical
octet patch release. Publish the new version platform-first, verify all four
packages, and announce the replacement. A pending GitHub/npm release should be
left untouched until its provenance is understood; revocation or closure is an
explicit maintainer action.

## Installation and updates

The following is a **future published-channel template**, not a current install
recommendation. Use it only after the exact package/version and provenance have
been independently verified and publication was authorized:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft-software/octet@VERSION
```

`octet update` automatically offers npm only for a physically validated global
layout. Local project and `npx` layouts receive a manual command instead, so a
octet process never mutates a project's dependencies implicitly.
