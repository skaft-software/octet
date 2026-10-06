# npm trusted publishing

**The octet 0.9.0 npm CLI is not published.** It is a planned channel for the
source candidate; native GitHub releases and npm publication need independent
approval and verification. See [installation](../installation.md) for the
currently available source-build route. This maintainer reference describes packaging,
protected publication, and recovery for the four immutable packages. The
source/release repository is `skaft-software/octet`; future npm releases use
trusted publishers for that identity. The already-published native v0.7.0
signatures retain `skaft-software/ygg`.

The source contract defines four immutable packages:

- `@skaft/octet`: a shell-only launcher.
- `@skaft/octet-darwin-arm64`, `@skaft/octet-darwin-x64`, and
  `@skaft/octet-linux-x64-gnu`: the native runtime and packaged docs.

The npm scope remains `@skaft`. The GitHub repository rename does not change
these package names or the Rust CLI crate (`octet-coding-agent`).

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
  npm-assets npm-assets/OCTET_NPM_MANIFEST.json npm-assets/OCTET_NPM_SHA256SUMS \
  --core-workflow-commit CORE_WORKFLOW_COMMIT
python3 scripts/verify-octet-npm.py VERSION npm-assets
```

`OCTET_RELEASE_METADATA.json` is generated from the signed native checksum
manifest and records the tag, source commit, release-workflow identity, pinned
URLs, and SHA-256 values. The protected release job verifies its Sigstore
bundle and regenerates the document before packaging. The npm manifest records
the metadata digest and every tarball's SHA-256/SHA-512 digests. Local scripts
check deterministic packing, tarball/path/lifecycle/secret handling, and an
offline install; they do **not** prove registry publication or macOS acceptance.

All files in the [public documentation inventory](../package-assets.txt) are
retained byte-for-byte from the verified native assets. The 0.8.1 npm publication
followed the signed native release, so its bundled docs are that release-time
snapshot and may still describe npm as unpublished. That historical publication
does not qualify the 0.9.0 candidate. Do not rewrite or republish immutable 0.8.1
packages to refresh docs; a later native release can carry updated documentation.
Packing restores files that npm's ignore rules would omit before final checksums
and provenance are computed; it never substitutes checkout documentation.
Ordinary npm installation renames `.gitignore` metadata to `.npmignore`. Its
bytes are verified at that installed name; other inventoried paths remain
unchanged. This is documentation, not a Git checkout: use the version-pinned
source checkout for benchmark reproduction and its original Git ignore rules.
No lifecycle hook repairs files.

The protected job downloads a fixed npm CLI tarball, verifies its recorded
SHA-512 integrity, and installs it with lifecycle scripts, audit, and funding
disabled. After publication, verification checks registry package integrity and
requires a provenance attestation binding the same artifact digest, repository,
release workflow, and source/workflow identity. A non-empty provenance field
alone is insufficient.

## Protected publication

Trusted publishers for all four packages must target the repository's
`release-octet.yml` workflow and `stable-release-publish` environment. The
workflow uses GitHub OIDC with `npm publish --provenance`, never `NPM_TOKEN`,
`NODE_AUTH_TOKEN`, a checked-in `.npmrc`, or another long-lived registry
credential. The environment is the human approval boundary.

For a future version, canonical binary tag runs leave npm publication disabled.
After verifying trusted-publisher configuration, dispatch `release-octet.yml`
from protected `main` with `release_tag=vX.Y.Z` and `publish_npm=true`. This
npm-only path verifies and smokes the existing immutable native release; it does
not rebuild or re-sign its assets. The npm manifest binds the publication
workflow commit while separately checking the original native signer commit
recorded in `OCTET_RELEASE_METADATA.json`. A release-environment approval may
be required.

The publication job pins npm CLI `11.5.1` (or a later explicitly reviewed version
supporting trusted publishing) before requesting OIDC provenance. It:

1. verifies the signed GitHub binary release and published installer smoke;
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

The planned CLI is `@skaft/octet@0.9.0`; it is not published yet, and no 0.9.0
registry provenance or public-install acceptance is established. See the
[installation guide](../installation.md) for the source-build route. Only after
verified publication, use the version-pinned install command:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@0.9.0
```

`octet update` offers npm automatically only for a physically validated global
layout. Local project and `npx` layouts get a manual command instead, so an octet
process never mutates a project's dependencies implicitly.
