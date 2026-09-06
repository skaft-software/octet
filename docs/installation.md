# Installation

[Documentation](README.md) · [Getting started](getting-started.md)

<a id="binary-availability"></a>

## Install native binaries

The [v0.7.0 release](https://github.com/skaft-software/ygg/releases/tag/v0.7.0)
provides signed native archives and the version-pinned installer for macOS
Apple silicon/Intel and GNU/Linux x86-64. Public installation is verified on
all three targets.

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/ygg/releases/download/v0.7.0/install-octet.sh | sh
octet --version
```

Install octet afresh. Older installations and data remain untouched; no automatic
migration is performed. npm is not published yet; Homebrew,
crates.io and SDK registry publication are separate channels. Bun is unqualified.
See [distribution channels](distribution.md) for their exact boundaries.
[Historical Ygg instructions](reference/historical-installation.md) describe
older releases, not a way to install or migrate to octet.

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

## Optional packages

The four official executable bundles and Serve 0.7.0 are published. For example:

```sh
octet extension install octet-web-search
octet extension list
```

For a reviewed local archive instead, use
`octet extension install --path ./bundle.tar.gz`. Installation is inert.

Installation does not enable, trust, start code, or provision dependencies.
Executable bundles are separate from the terminal binary and from the graphical
Serve application. Use [resource discovery](resources.md) for source selection
and [extensions](extensions.md) for exact packaging, trust, atomic update, and
removal rules. Catalog install/update selects the package matching the running
octet version; command forms are in the [CLI reference](cli.md#packages-and-serve).

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
