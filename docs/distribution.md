# Distribution channels

Octet 0.7.5 native, Serve and four executable-bundle asset availability is recorded
on the [version-pinned GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.5).
Public-install results are recorded there. See [installation](installation.md) for
native installation or a source build, and [release notes](releases/v0.7.5.md)
for changes; the GitHub release records publication verification.
npm, Homebrew, crates.io and SDK registries remain separate, unpublished channels.
The repository is now `skaft-software/octet`. The immutable v0.7.0 assets retain
their original `skaft-software/ygg` signing identity; v0.7.1 and later use the new
identity. Existing clone and release-asset URLs redirect to the same repository.
Do not recreate the old name.

## Package identities

These source identities do not establish publication of every channel.
The distribution version does not establish publication or change independent
API and schema versions. See the [0.7.5 release](releases/v0.7.5.md) for the
version-matched assets and channel boundaries.

| Surface | Source identity |
| --- | --- |
| Product and native commands | lowercase octet; `octet`, `octet-host` |
| Core crates | `octet-ai`, `octet-agent`, `octet-coding-agent`, `octet-migrate-types` |
| Product library | `octet_sdk` |
| Python distribution / import | `octet-extension-sdk` / `octet_extension` |
| Canonical TypeScript source package | `@skaft-software/octet-extension-api-v03` |
| First-party extensions | `octet-browse`, `octet-mcp`, `octet-subagents`, `octet-web-search`, `octet-pi-compat`, `octet-serve` |
| Product environment and roots | `OCTET_*`, `~/.octet`, project `.octet`; packaged docs `share/octet` |
| Product, SDK and first-party extension distribution versions | `0.7.5`; installed compatibility `requires_octet = "=0.7.5"` |
| Independent contracts | extension APIs `0.1` / `0.2` / `0.3`; native-host protocol `1`; schema revisions remain independent |
| Source/release repository | `skaft-software/octet`; v0.7.0 signatures retain `skaft-software/ygg` |
| Website | `https://octet.skaft.org`; deployment is separate from native publication |

Current extension authoring targets API 0.3. The four bundled executable-extension
manifests still declare 0.2; the Python `Extension` runtime remains a legacy
0.1/0.2 implementation. Generated 0.3 types are not a complete 0.3 runtime.
Renaming first-party wire fields such as `octet_version` does not renumber APIs
or preserve old-name aliases. Historical releases, measurements, upstream
copyrights, Pi pins, independent example versions and mismatch fixtures retain
their original scope.

## Channel selection

The release workflows do not publish every source package automatically:

| Channel | Release path | Publication boundary |
| --- | --- | --- |
| Native archives and shell installer | `release-octet.yml` | Signed, version-pinned GitHub release assets; verify public installation after upload. |
| Four executable bundles | `release-serve.yml` | Separate exact-version `octet-browse`, `octet-mcp`, `octet-subagents`, and `octet-web-search` archives; install/update never enables them or persists trust grants. |
| npm CLI | `release-octet.yml` with `publish_npm=true` | Four `@skaft/octet*` packages, platform-first. Requires verified registry ownership and trusted publishers for all four packages; disabled by default. |
| Cargo installation | Build the canonical Git tag | No crates.io publication required; the public tag and its complete source must exist. |
| crates.io | Not provided by the current workflows | Do not advertise registry installation. Publishing the CLI/dependency graph and verifying registry ownership is separate work. |
| Homebrew | `homebrew-formula.yml` | Separate signed-asset handoff and protected tap pull request; not automatic with the binary release. |
| Serve | `release-serve.yml` | Separate exact-version application package and installation checks. |
| Python/TypeScript SDK registries | Not provided by the CLI release workflow | Source SDKs/generated bindings are not automatically published to PyPI or npm by the four-package CLI job. |

Native publication uses the matching `octet-binaries-vX.Y.Z` tooling tag at the
canonical release commit. Its metadata generator requires that ref; the
protected environment must admit the tag without removing required reviewers.

Cargo can build the published canonical tag's exact source
(Rust 1.86+ and ripgrep):

```sh
cargo install --locked --git https://github.com/skaft-software/octet --tag v0.7.5 --bins octet-coding-agent
```

The public `v0.7.5` tag must exist before using this command. `cargo install octet`
and registry-based `cargo install octet-coding-agent` are not the supported
Cargo path.

The npm channel remains unpublished pending functional first-package bootstrap,
trusted publishers and registry provenance verification. Its future command is:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@0.7.5
```

## Homebrew

The formula is generated from the signed `OCTET_RELEASE_METADATA.json` produced
by the protected binary-release workflow. The generator reads neither the
package manifest for a version nor a mutable `latest` or release API. Before
rendering, the workflow verifies the metadata's Sigstore bundle, canonical
tag/workflow/source identity, `OCTET_SHA256SUMS` digest, and both macOS archive
digests. Release channels consume the same immutable native assets.

The proposed tap identity is `skaft-software/tap/octet`, with
`Formula/octet.rb` and class `Octet`. An authorized handoff must first verify
channel ownership, signed metadata, and hosted acceptance on macOS Apple
silicon and Intel. The formula declares `ripgrep` and has no Linux runtime;
Linux qualification follows its own native/npm gates.

The formula uses the archive's versioned top-level directory and installs only
`octet` and `octet-host`. It does not run an npm lifecycle hook, invoke Cargo, or
build from source. Check a formula generated from local release assets with:

```sh
scripts/test-homebrew-formula.sh
```

That offline check covers deterministic metadata parsing, archive checksum
handoff, formula syntax, expected architecture URLs, and rejection of a changed
digest. It is not hosted macOS acceptance, tap publication, or public release
verification.

## Release handoff and tap publication

A release candidate needs a stable `vX.Y.Z` tag, signed native checksum manifest,
and signed immutable metadata document. The Homebrew workflow downloads that
exact metadata and signature from the canonical release, verifies the Sigstore
identity against the release workflow commit, and renders `Formula/octet.rb` in
a clean tap checkout. Rendering and publication require explicit dispatch from
the matching `octet-binaries-vX.Y.Z` tooling tag; binary release completion never
mutates the tap. The workflow checks the formula diff and opens a protected tap
pull request, never replacing a formula directly on the default branch.

Configure the tap repository and GitHub App/installation permission in the
protected release environment, not as source-controlled credentials. A missing
token, non-canonical tap repository, metadata mismatch, failed formula check, or
unavailable hosted macOS acceptance must stop the handoff without changing the
tap.

If an update used a wrong or incomplete release, close the pull request without
merging and regenerate from the same immutable release metadata. Never edit
SHA-256 values by hand. If a bad formula was merged, revert the tap commit and
open a replacement from a newly reviewed metadata record; never use a mutable
release alias.

## Other channels

The v0.7.5 version-pinned shell installer targets macOS
arm64/x64 and GNU/Linux x64. The no-lifecycle npm launcher targets the same
platforms but remains unavailable through npm. See
the [npm release contract](release/npm-trusted-publishing.md) for platform-first
publication and provenance checks. Bun is unqualified.

Cargo can build/install the local checkout without a registry channel. Public
canonical-tag installation requires the version-pinned GitHub tag, not a
crates.io package.
