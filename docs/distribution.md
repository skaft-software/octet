# Distribution channels

octet release-channel machinery consumes the same immutable native assets. The
Homebrew formula is generated from the signed `OCTET_RELEASE_METADATA.json`
record produced by the protected binary-release workflow. The generator does
not read the repository's package manifest for a version and does not query a
mutable `latest` or release API. Before a formula is rendered, the workflow
verifies the metadata's Sigstore bundle, its canonical tag/workflow/source
identity, the `OCTET_SHA256SUMS` digest, and the two macOS archive digests.

## Homebrew

No octet npm, Homebrew, crates.io, or native download channel is verified or
advertised as published here. Use the [local checkout build](../README.md#build-this-checkout).
The names below describe release tooling, not a public installation promise.
Source/release URLs and narrow OIDC checks remain bound to the actual
`skaft-software/ygg` repository until a separately authorized, verified cutover.

The generated source identity is `skaft-software/tap/octet`, with formula
`Formula/octet.rb` and class `Octet`. A future authorized handoff must first
verify channel ownership, signed metadata, and hosted acceptance on macOS Apple
silicon and Intel. The formula declares `ripgrep` and has no Linux runtime;
Linux package qualification follows its own native/npm gates.

The formula uses the archive's versioned top-level directory and installs only
`octet` and `octet-host`. It does not run an npm lifecycle hook, invoke Cargo, or
build from source. A formula generated from local release assets can be checked
offline with:

```sh
scripts/test-homebrew-formula.sh
```

That check proves deterministic metadata parsing, archive checksum handoff,
formula syntax, expected architecture URLs, and failure on a changed digest.
It is not hosted macOS acceptance and does not prove that a tap mutation or a
public release has completed.

## Release handoff and tap publication

A release candidate must first have a stable `vX.Y.Z` tag, a signed native
checksum manifest, and a signed immutable metadata document. The Homebrew
workflow downloads that exact metadata and its signature from the canonical
release, verifies the Sigstore identity against the release workflow commit,
and renders `Formula/octet.rb` in a clean tap checkout. Formula rendering and tap
publication require an explicit dispatch from the matching
`octet-binaries-vX.Y.Z` tooling tag; binary release completion alone never mutates
the tap. The workflow then checks the formula diff and opens a protected tap
pull request; it must never replace a formula directly on the default branch.

The tap repository and its GitHub App/installation permission are deployment
configuration, not source-controlled credentials. Configure them in the
protected release environment. A missing token, non-canonical tap repository,
metadata mismatch, failed formula check, or unavailable hosted macOS acceptance
must stop the handoff without changing the tap.

If a formula update was opened from a wrong or incomplete release, close the
pull request without merging it and regenerate from the same immutable release
metadata. Do not edit SHA-256 values by hand. If a bad formula was merged,
revert the tap commit and open a replacement from a newly reviewed metadata
record; do not point the formula at a mutable release alias.

## Other channels

The version-pinned shell installer and no-lifecycle npm launcher have source
support for macOS arm64/x64 and GNU/Linux x64. These are target contracts, not
claims of successful hosted acceptance or publication. See the
[npm release contract](release/npm-trusted-publishing.md) for the protected,
platform-first handoff and provenance checks. Cargo can build/install the local
checkout without requiring a new registry channel; any public canonical-tag
installation must wait for separately authorized release publication.
