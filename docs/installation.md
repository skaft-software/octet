# Installation

[Documentation](README.md) · [Getting started](getting-started.md)

<a id="binary-availability"></a>

## Install native binaries

Native release packages target macOS Apple silicon/Intel and GNU/Linux x86-64.
See the [v0.7.6 notes](releases/v0.7.6.md) for changes; availability, signed
assets and public-install verification are recorded on the version-pinned
GitHub release. Install using the matching installer from the
[GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.7.6):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.7.6/install-octet.sh | sh
octet --version   # octet 0.7.6
```

When moving from Ygg, install octet afresh. Older installations and data remain
untouched; no automatic migration is performed. npm is not published yet;
Homebrew, crates.io and SDK registries remain separate, unpublished channels.
Bun is unqualified.
See [distribution channels](distribution.md) for their exact boundaries.
[Historical Ygg instructions](reference/historical-installation.md) describe
older releases, not a way to install or migrate to octet.

The repository is now `skaft-software/octet`; existing v0.7.0 signatures and
assets are unchanged. For source installation, use a checkout or the Git-tag
Cargo command in [distribution](distribution.md). The immutable v0.7.0
installer's `--from-source` mode expects the old repository archive directory
name and is not supported after the rename; its default native mode is unchanged.

## Installer progress

The installer and `octet update` show the monochrome octet byte mark and target
version on an interactive terminal. Progress is written to stderr. The mark uses
the terminal's default foreground, with an ASCII fallback outside UTF-8 locales;
narrow terminals use a compact layout. Redirected output and a missing or `dumb`
`TERM` use plain stage messages without animated terminal controls.

The native installer uses curl's measured, per-download progress bar when stderr
width is known and at least 22 columns; otherwise it keeps plain stage messages.
The width is sampled before each download, not continuously during a transfer.
A transfer reaching 100% means **downloaded**, not verified or installed; signature/checksum
verification, archive validation, executable checks and installation remain
separate named stages. Unknown-size transfers have no invented percentage.
Unmeasured updater checks and package-manager work use an indeterminate activity
bar, with command output streamed live. There is no simulated overall percentage
or ETA. The installed executable must report the requested version before the
final success message. Existing signature, archive, path and trust checks remain
required.

## Update and release notes

Run `octet update` to update using the channel that installed the binary, then
restart octet. `octet update --check` checks without installing.
Starting an interactive session also makes one quiet, bounded release check unless
`--offline` is set. A newer stable release adds an `octet update` hint to the splash;
if the splash is already in history, a single notice appears at the live tail.
Failures do not block startup or produce an error. No automatic installation,
provider request, credentials, periodic polling or persistent update cache is used.
Proxy-only networks do not receive this optional direct-HTTPS startup notice.

From 0.7.5 onward, `/changelog` opens the current binary's bundled release notes
inside the TUI as rich Markdown. Up/Down, PageUp/PageDown, Home and End scroll;
Escape or Left returns to the composer. It works offline and does not send notes
or the command to the model.

## Build from a checkout

A source checkout can differ from the published release. Check `octet --version`
and use matching executable bundles; source builds do not establish signed
publication or replace an installed binary.

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

The four official executable bundles and the separate Serve application must
match octet 0.7.6. Their publication status is recorded on the GitHub release;
installation never substitutes another host version. For example:

```sh
octet extension install octet-web-search
octet extension list
```

For a reviewed local archive instead, use
`octet extension install --path ./bundle.tar.gz`. Installation is inert.

Installation does not enable, persist a trust grant, start code, or provision
dependencies. Full access implicitly trusts selected executable extensions;
enablement remains explicit. Safe mode keeps executable extensions stopped.
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
| `octet-subagents` | [Bounded workers](../extensions/octet-subagents/README.md); explicit enablement; full-access trust follows host policy. |
| `octet-serve` | [Loopback graphical interface](experimental/octet-serve/README.md); separate version-matched application package, not an executable-extension activation target. |

The four executable bundles in this snapshot still declare API 0.2. They are
legacy implementation references, not API 0.3 authoring examples. New authoring
uses [Extension API 0.3](extensions/API-0.3-REFERENCE.md); generated Python 0.3
types alone are not a complete 0.3 `Extension` runtime. A qualified current-API
end-to-end example remains missing.

## Container

The included **linux/amd64** image is a source-build route, not evidence of a
published image. The following local-only commands label the source checkout; they do not pull a
published image:

```sh
scripts/build-octet-image.sh octet:local
docker run --rm -it \
  -e ANTHROPIC_API_KEY \
  -v "$PWD:/workspace" \
  octet:local --model claude-sonnet-4-6
```

The script builds a clean tracked Git snapshot, refuses tracked changes, and
excludes untracked workstation content. It uses digest-pinned base images and
Debian package snapshots, runs unprivileged, and requires an explicit workspace
mount. Pass only needed credentials and paths. Read-only packaged documentation
lives at `/usr/local/share/octet`, exposed through `OCTET_PACKAGE_DIR`.
See [security](../SECURITY.md) before granting access to sensitive files.
