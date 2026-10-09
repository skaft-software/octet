# Distribution channels

Octet 0.9.0 is a **source candidate, not a published release**. Its canonical
GitHub tag, native/executable-bundle release assets and all five npm
packages are unavailable. Use the source-build route in
[installation](installation.md). The commands below describe publication
contracts and planned post-publication installation, not currently usable
0.9.0 downloads.

The preceding release's [version-pinned GitHub
release](https://github.com/skaft-software/octet/releases/tag/v0.8.2) records its
signed assets and public-install verification once approved. See the
[candidate release notes](releases/v0.9.0.md) for scope and remaining
qualification. The
planned npm channel is `@skaft/octet@0.9.0` (launcher plus four signed platform
packages, with provenance). Homebrew, crates.io and SDK registries remain
separate, unpublished channels.

The repository is now `skaft-software/octet`. The immutable v0.7.0 assets keep
their original `skaft-software/ygg` signing identity, and v0.7.1 and later use
the new identity. Existing clone and release-asset URLs redirect to the same
repository. Don't recreate the old name.

## Package identities

This candidate's distribution target is **0.9.0**. The distribution version does not
change independent API and schema versions. The [0.7.6
release](releases/v0.7.6.md) keeps its historical version-matched assets and
channel evidence.

| Surface | Source identity |
| --- | --- |
| Product and native commands | lowercase octet; `octet`, `octet-host` |
| Core crates | `octet-ai`, `octet-agent`, `octet-coding-agent`, `octet-migrate-types` |
| Product library | `octet_sdk` |
| Python distribution / import | `octet-extension-sdk` / `octet_extension` |
| Canonical TypeScript source package | `@skaft-software/octet-extension-api-v03` |
| Executable bundles | `octet-codemode`, `octet-computer-use`, `octet-mcp`, `octet-pi-compat`, `octet-subagents`, `octet-web-search` |
| Product environment and roots | `OCTET_*`, `~/.octet`, project `.octet`; packaged docs `share/octet` |
| Product, SDK and six executable-bundle distribution versions | `0.9.0`; installed compatibility `requires_octet = "=0.9.0"` |
| Independent contracts | current extension API `0.4`, retained `0.1` / `0.2` and canonical `0.3`; native-host protocol `1`; schema revisions remain independent |
| Source/release repository | `skaft-software/octet`; v0.7.0 signatures retain `skaft-software/ygg` |
| Website | `https://octet.skaft.org`; deployment is separate from native publication |

Current extension authoring and the six executable-bundle manifests target API
`0.4`, the feature-negotiated wire the Python `Extension` runtime supports
alongside the retained `0.1` and `0.2`. Canonical API `0.3` stays a separate
supported wire, and the generated `0.3` types aren't a complete `0.3` process
runtime. The minimal canonical process example and independent Aider adapter
keep version `0.1.0` with exact `=0.8.0` host pins. The Pi adapter is now a
version-matched catalog bundle; its configured local bridge is separate. Renaming
first-party wire fields such as `octet_version` doesn't renumber APIs or keep
old-name aliases. Historical releases, measurements, upstream copyrights, Pi
pins, independent example versions and mismatch fixtures keep their original
scope.

## Channel selection

The release workflows don't publish every source package automatically:

| Channel | Release path | Publication boundary |
| --- | --- | --- |
| Native archives and shell installer | `release-octet.yml` | Signed, version-pinned GitHub release assets. Verify public installation after upload. |
| Six executable bundles | `release-serve.yml` | Separate exact-version `octet-codemode`, `octet-computer-use`, `octet-mcp`, `octet-pi-compat`, `octet-subagents` and `octet-web-search` archives. Install and update never enable them or persist trust grants. |
| npm CLI | `release-octet.yml` with `publish_npm=true` | Five `@skaft/octet*` packages, platform-first. Needs verified registry ownership and trusted publishers for all five. Disabled by default. |
| Cargo installation | Build the canonical Git tag | No crates.io publication needed. The public tag and its complete source must exist. |
| crates.io | Not provided by the current workflows | Don't advertise registry installation. Publishing the CLI and dependency graph and verifying registry ownership is separate work. |
| Homebrew | `homebrew-formula.yml` | A separate signed-asset handoff and protected tap pull request. Not automatic with the binary release. |
| Python and TypeScript SDK registries | Not provided by the CLI release workflow | The five-package CLI job doesn't publish the source SDKs or generated bindings to PyPI or npm. |

Native publication uses the matching `octet-binaries-vX.Y.Z` tooling tag at the
canonical release commit. Its metadata generator requires that ref, and the
protected environment must admit the tag without removing required reviewers.

Cargo can build the published canonical tag's exact source (Rust 1.88+ and
ripgrep):

```sh
cargo install --locked --git https://github.com/skaft-software/octet --tag v0.9.0 --bins octet-coding-agent
```

The public `v0.9.0` tag does not exist yet; it must exist before you use this command.
`cargo install octet` and registry-based `cargo install octet-coding-agent`
aren't the supported Cargo path.

The planned npm channel has five `@skaft/octet*` packages (launcher plus four
platform packages), built from verified release assets with trusted publishers
and registry provenance verification. **0.9.0 is not published to npm.** Only
after verified publication, pin the version to reproduce one exact release:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft/octet@0.9.0
```

`npm install -g @skaft/octet` tracks the `latest` dist-tag instead; it is not an
installation of this unpublished candidate. Published versions are immutable,
so a pinned install can't be silently replaced.

## Reviewed model metadata

Release preparation refreshes and reviews the checked-in models.dev names,
pricing, capabilities and source identity together. On a workspace
product-version change, or an explicit CI dispatch that asks for it, CI runs the
live models.dev `--check` gate. Stale snapshots must be refreshed and reviewed
before the release source is frozen, and the check never silently rewrites
release inputs.

Ordinary compilation uses checked-in metadata without network access. This gate
isn't an all-provider test or live-inference qualification. Interactive sessions
separately refresh checked live records at runtime, with the reviewed snapshot
as their fallback and baseline. See the [model-source
record](../crates/octet-ai/models/SOURCES.md) for metadata scope and
limitations.

## Homebrew

The Homebrew formula is generated from the signed `OCTET_RELEASE_METADATA.json`
that the protected binary-release workflow produces, and nothing in the process
reads a mutable `latest` or the release API. The proposed tap is
`skaft-software/homebrew-tap`, publishing the `octet` formula. It installs only
`octet` and `octet-host`, declares
`ripgrep`, has no Linux runtime, and doesn't run an npm lifecycle hook, invoke
Cargo or build from source. To check a formula generated from local release
assets:

```sh
scripts/test-homebrew-formula.sh
```

That offline check isn't hosted macOS acceptance, tap publication or public
release verification.

<details>
<summary>How the formula is verified</summary>

The generator doesn't read a version from the package manifest. Before
rendering, the workflow verifies the metadata's Sigstore bundle, the canonical
tag, workflow and source identity, the `OCTET_SHA256SUMS` digest and both macOS
archive digests. All release channels consume the same immutable native assets.

The tap is `Formula/octet.rb` with class `Octet`. Before an authorized handoff,
verify channel ownership, signed metadata and hosted acceptance on macOS Apple
silicon and Intel. Linux qualification follows its own native and npm gates. The
formula uses the archive's versioned top-level directory. The offline check
covers metadata parsing, archive checksum handoff, formula syntax, expected
architecture URLs and rejection of a changed digest.

</details>

## Release handoff and tap publication

Rendering and publishing the formula need an explicit dispatch from the matching
`octet-binaries-vX.Y.Z` tooling tag. Finishing a binary release never changes
the tap, and the workflow only ever opens a protected tap pull request. If an
update used a wrong or incomplete release, close the pull request unmerged and
regenerate from the same immutable metadata. Never edit SHA-256 values by hand.

<details>
<summary>Handoff requirements and recovery</summary>

A release candidate needs a stable `vX.Y.Z` tag, a signed native checksum
manifest and a signed immutable metadata document. The Homebrew workflow
downloads that exact metadata and signature from the canonical release, verifies
the Sigstore identity against the release workflow commit, and renders
`Formula/octet.rb` in a clean tap checkout. It checks the formula diff and opens
a protected tap pull request. It never replaces a formula on the default branch.

Configure the tap repository and GitHub App permissions in the protected release
environment, never as source-controlled credentials. A missing token, a
non-canonical tap repository, a metadata mismatch, a failed formula check or
unavailable hosted macOS acceptance must stop the handoff without changing the
tap. If a bad formula was merged, revert the tap commit and open a replacement
from a newly reviewed metadata record. Never use a mutable release alias.

</details>

## Other channels

The planned v0.9.0 version-pinned shell installer targets macOS arm64 and x64 and
GNU/Linux x64. The no-lifecycle npm launcher targets the same platforms plus
Windows x86-64 and would publish as `@skaft/octet@0.9.0`. See the [npm release
contract](release/npm-trusted-publishing.md) for platform-first publishing and
provenance checks. CLI installation with Bun is unqualified. Cargo can build and install the local
checkout without a registry channel. Installing from the public canonical tag
needs the version-pinned GitHub tag, not a crates.io package.
