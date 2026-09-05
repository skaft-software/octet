# Installation

[Documentation](README.md) · [Getting started](getting-started.md)

## Build from a checkout

On macOS or GNU/Linux, install Rust 1.86+ and
[ripgrep](https://github.com/BurntSushi/ripgrep). From the source checkout:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet --safe-mode
```

This builds `octet` and `octet-host` under `target/release` without replacing an
installed copy. To build only the terminal binary, use
`cargo build --release --locked -p octet-coding-agent --bin octet`.
Command execution is Unix-only. Normal builds use checked-in model metadata;
they do not refresh the catalog over the network. Continue with
[provider setup](providers.md).

## Binary availability

**octet 0.7.0 is unpublished and not release-qualified.** There is no verified
current octet binary download or public extension bundle here. Proposed
curl, Git-Cargo, npm, and Homebrew channels remain publication/platform gated;
Bun is unqualified. Do not substitute those proposed commands for the released
website's Ygg v0.6.7 installation instructions before a separate promotion.

The repository's release location remains
[GitHub Releases](https://github.com/skaft-software/ygg/releases).
[Historical Ygg instructions](reference/historical-installation.md) describe
older releases, not a way to install or migrate to octet.

## Optional packages

After reviewing a locally built archive, installation is inert:

```sh
octet extension install --path ./bundle.tar.gz
octet extension list
```

Installation does not enable, trust, start code, or provision dependencies.
Executable bundles are separate from the terminal binary and from the graphical
Serve application. Use [resource discovery](resources.md) for source selection
and [extensions](extensions.md) for exact packaging, trust, atomic update, and
removal rules. Catalog install/update commands require separately verified,
exact-version publication; their forms are in the [CLI reference](cli.md#packages-and-serve).

| Package | Canonical setup and limits |
| --- | --- |
| `octet-web-search` | [Brave Search (recommended) or SearXNG](../extensions/octet-web-search/README.md); public search/fetch, not a browser. |
| `octet-browse` | [Visible isolated browser](../extensions/octet-browse/README.md); authentication is manual. |
| `octet-mcp` | [MCP bridge](../extensions/octet-mcp/README.md); local stdio is supported, remote Streamable HTTP is blocked by default. |
| `octet-subagents` | [Bounded workers](../extensions/octet-subagents/README.md); enable and trust separately. |
| `octet-serve` | [Loopback graphical interface](experimental/octet-serve/README.md); separate version-matched application package, not an executable-extension activation target. |

The four executable bundles in this snapshot still declare API 0.2. They are
legacy implementation references, not API 0.3 authoring examples. New authoring
uses [Extension API 0.3](extensions/API-0.3-REFERENCE.md); generated Python 0.3
types alone are not a complete 0.3 `Extension` runtime. A qualified current-API
end-to-end example remains missing.

## Container

The included **linux/amd64** image is a source-build route, not evidence of a
published image:

```sh
scripts/build-octet-image.sh octet:0.7.0
docker run --rm -it \
  -e ANTHROPIC_API_KEY \
  -v "$PWD:/workspace" \
  octet:0.7.0 --model claude-sonnet-4-6
```

The script builds a clean tracked Git snapshot, refuses tracked changes, and
excludes untracked workstation content. It uses digest-pinned base images and
Debian package snapshots, runs unprivileged, and requires an explicit workspace
mount. Pass only needed credentials and paths. Read-only packaged documentation
lives at `/usr/local/share/octet`, exposed through `OCTET_PACKAGE_DIR`.
See [security](../SECURITY.md) before granting access to sensitive files.
