# Installation

[Documentation](README.md) · [Getting started](getting-started.md)

<a id="binary-availability"></a>

## Install native binaries

**0.8.2 is a source candidate, not a published release.** Its GitHub tag, release
assets and npm packages are unavailable. Use [a checkout](#build-from-a-checkout)
until qualification and publication are approved. The commands in the native
and npm sections below apply only after verified publication.

Planned native release packages target macOS Apple silicon and Intel and GNU/Linux
x86-64. The [v0.8.2 notes](releases/v0.8.2.md) list the candidate changes.
After publication, signed assets and public-install verification must be recorded
on the version-pinned
[GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.8.2).
Then install with its matching installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.8.2/install-octet.sh | sh
octet --version   # octet 0.8.2
```

When moving from Ygg, install octet afresh. Older installations and data stay
untouched, and there's no automatic migration. Homebrew, crates.io and SDK
registries remain separate, unpublished channels. Bun is unqualified.

<a id="npm-availability"></a>

## Install from npm

After approved npm publication, Node.js can install the same signed platform
package on macOS Apple silicon and Intel and GNU/Linux x86-64:

```sh
npm install -g @skaft/octet@0.8.2
octet --version   # octet 0.8.2
```

This installs the `@skaft/octet` launcher plus the matching platform package
(`@skaft/octet-darwin-arm64`, `@skaft/octet-darwin-x64` or
`@skaft/octet-linux-x64-gnu`) with npm provenance, from the same verified
release assets as the native installer. The launcher has no install-time
lifecycle scripts. To pin an exact version instead of tracking `latest`, use
`npm install -g @skaft/octet@0.8.2` ([distribution](distribution.md)). There's
no Windows npm package: on Windows the launcher installs but can't start octet.
Build from source or use a pull-request test build instead
([Windows](windows.md)). [Distribution channels](distribution.md) has the exact
boundaries. The [historical Ygg
instructions](reference/historical-installation.md) describe older releases, not
a way to install or migrate to octet.

The repository is now `skaft-software/octet`, and the existing v0.7.0 signatures
and assets are unchanged. For a source install, use a checkout or the Git-tag
Cargo command in [distribution](distribution.md). The immutable v0.7.0
installer's `--from-source` mode expects the old repository archive directory
name and isn't supported after the rename. Its default native mode is unchanged.

## Installer progress

On an interactive terminal, the installer and `octet update` show the monochrome
octet byte mark and the target version, with progress on stderr. The mark uses
your terminal's default foreground, with an ASCII fallback outside UTF-8 locales
and a compact layout on narrow terminals. Redirected output and a missing or
`dumb` `TERM` get plain stage messages without animated terminal controls.

<details>
<summary>How progress is measured</summary>

The native installer uses curl's measured, per-download progress bar when stderr
width is known and at least 22 columns, and otherwise keeps plain stage
messages. The width is sampled before each download, not continuously. A
transfer reaching 100% means **downloaded**, not verified or installed:
signature and checksum verification, archive validation, executable checks and
installation stay separate named stages. Transfers of unknown size get no
invented percentage. Unmeasured updater checks and package-manager work use an
indeterminate activity bar, with command output streamed live. There's no
simulated overall percentage or ETA. The installed executable must report the
requested version before the final success message, and the existing signature,
archive, path and trust checks are still required.

</details>

## Update and release notes

Run `octet update` to update through the channel that installed the binary, then
restart octet. `octet update --check` checks without installing. Starting an
interactive session also makes one quiet, bounded release check unless
`--offline` is set. A newer stable release adds an `octet update` hint to the
splash, or a single notice at the live tail if the splash is already in history.
Failures don't block startup or show an error. There's no automatic
installation, provider request, credentials, periodic polling or persistent
update cache. Proxy-only networks don't get this optional direct-HTTPS startup
notice.

From 0.7.5 onward, `/changelog` opens the current binary's bundled release notes
in the TUI as rich Markdown. Up and Down, PageUp and PageDown, Home and End
scroll, and Escape or Left returns to the input. It works offline and doesn't
send the notes or the command to the model.

## Build from a checkout

This checkout targets the octet 0.8.2 candidate. Check `octet --version`, and use matching
source extension manifests from this checkout. A source build isn't a signed
release artifact and doesn't replace an installed binary.

On macOS or GNU/Linux, install Rust 1.88+ and
[ripgrep](https://github.com/BurntSushi/ripgrep). From the source checkout:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet --safe-mode
```

This builds `octet` and `octet-host` under `target/release` without replacing an
installed copy. For only the terminal binary, use
`cargo build --release --locked -p octet-coding-agent --bin octet`. Normal
builds use the checked-in model metadata and never refresh the catalog over the
network. Next, [connect a model](providers.md).

On Windows, build the `x86_64-pc-windows-gnu` target with a MinGW-w64 toolchain
as described in [Windows](windows.md#target-and-toolchain). Bash commands there
need Git for Windows or another Bash-compatible shell.

## Optional packages

Executable extension bundles and the separate Serve application are pinned to
the host version. After publication, install assets matching octet `0.8.2` from
its version-pinned release. Until then, run reviewed source extensions from this
checkout with `--extension-dir ./extensions`. The 0.8.0 bundles need their 0.8.0 host.

```sh
octet extension install octet-web-search
octet extension list
```

For a reviewed local archive instead, use
`octet extension install --path ./bundle.tar.gz`. Installing is inert: it
doesn't enable a package, save a trust grant, start code or set up dependencies.
Full access implicitly trusts selected executable extensions, but enabling stays
explicit. Safe mode starts only enabled extensions with host authority for their
selected source; ungranted sources stay stopped. `--trust-extension`, an explicit
`--extension-dir`, or a source-bound `trusted_extensions` grant can supply that
authority, but never enables a bundle. `--no-process`/`--no-shell`, workspace trust,
and compatibility/integrity checks still apply. Granted code runs with your OS
permissions outside the tool-effect broker: safe mode is not an OS sandbox.
Executable bundles are separate from the terminal binary and graphical Serve app. [Resource
discovery](resources.md) covers source selection, and
[extensions](extensions.md) covers packaging, trust, atomic update and removal.
Catalog install and update select the package that matches the running octet
version. The command forms are in the [CLI
reference](cli.md#packages-and-serve).

| Package | What it adds |
| --- | --- |
| `octet-web-search` | Public web search and fetch, via [Brave Search (recommended) or SearXNG](../extensions/octet-web-search/README.md). Not a browser. |
| `octet-browse` | **Deprecated**, still installable. A [visible, isolated browser](../extensions/octet-browse/README.md) you sign in to yourself. Prefer the computer-use extension for new automation ([deprecation notes](../extensions/octet-browse/README.md#deprecation)). |
| `octet-computer-use` | [Native desktop control](../extensions/octet-computer-use/README.md) (macOS, Windows, Linux) through a locally installed, MIT-licensed Cua Driver. Provisioning is explicit, and the OS permissions are yours to grant. |
| `octet-codemode` | [Pi's offline JavaScript tool composition](../extensions/octet-codemode/README.md), with QuickJS/WASM and Node 22.19+. Explicitly enabled tools retain normal host policy and budgets. |
| `octet-mcp` | An [MCP bridge](../extensions/octet-mcp/README.md). Local stdio works. Remote Streamable HTTP is blocked by default. |
| `octet-subagents` | [Bounded workers](../extensions/octet-subagents/README.md). Enabling is explicit, and full-access trust follows host policy. |
| `octet-serve` | A [graphical interface on loopback](experimental/octet-serve/README.md). A separate, version-matched application package, not an executable-extension activation target. |

The six executable-bundle manifests declare API `0.4`, distribution version
`0.8.2`, and require octet `=0.8.2`. Older bundles stay pinned to their host.
Distribution and host versions are independent boundaries, and an API number
doesn't bypass the exact host pin. See [current authoring](extensions.md) for
the Python API 0.4 process recipe and the retained API 0.3 conformance example.
Generated contract bindings alone aren't a process runtime.

## Container

The included **linux/amd64** image is a source-build route, not evidence of a
published image. These local-only commands label the source checkout and don't
pull a published image:

```sh
scripts/build-octet-image.sh octet:local
docker run --rm -it \
  -e ANTHROPIC_API_KEY \
  -v "$PWD:/workspace" \
  octet:local --model claude-sonnet-4-6
```

The script builds a clean copy of your tracked Git files, refuses tracked
changes and leaves out untracked workstation content. It uses digest-pinned base
images and Debian package snapshots, runs unprivileged, and needs an explicit
workspace mount. Pass only the credentials and paths you need. The packaged docs
are read-only at `/usr/local/share/octet`, exposed through `OCTET_PACKAGE_DIR`.
Read [Security](../SECURITY.md) before granting access to sensitive files.
