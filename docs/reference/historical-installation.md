# Historical Ygg installation

[Documentation](../README.md) · [Current octet source builds](../installation.md)

**Ygg 0.6.x history only—not octet 0.7.0 installation or update instructions.**
These retained commands may install an older Ygg release or alter a Ygg
installation. They do not establish current channel publication, a Ygg-to-octet
migration, or current octet platform availability. See [Ygg v0.6.7 notes](../releases/v0.6.7.md).

## Historical installer

The version-pinned installer verified the release archive and installed `ygg`
and `ygg-host` under `~/.local/bin`. It required
[ripgrep](https://github.com/BurntSushi/ripgrep), but no Rust toolchain for
prebuilt binaries:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/ygg/releases/download/v0.6.7/install-ygg.sh | sh

ygg --version   # ygg 0.6.7
ygg --help
```

The historical platforms were GNU/Linux x86-64, macOS x86-64, and macOS Apple
silicon; Linux musl was not included. The same binary could use cloud providers
or a configured local endpoint. This is not qualification of today's models or
renamed binary.

The pre-rename development snapshot identified itself as `ygg 0.7.0-dev`, not a
released `v0.7.0` tag and not the current checkout identity.

To compile the pinned historical tag through that installer, Rust 1.86+ was
required:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/ygg/releases/download/v0.6.7/install-ygg.sh \
  | sh -s -- --from-source
```

## npm distribution

The historical contract was conditional on publication to the configured scope:

```sh
npm install --global --ignore-scripts --no-audit --no-fund @skaft-software/ygg@VERSION
```

An exact-version native launcher selected the matching signed macOS or
GNU/Linux x86-64 runtime and executed it directly. Ordinary `ygg`/`ygg-host`
execution did not start Node or run an npm lifecycle hook. Unsupported CPUs and
GNU/Linux musl were rejected instead of fetching a fallback.

`ygg update` recognized only a physically validated global npm layout and used
the same exact-version install command. Local-project and `npx` installations
required explicit project updates. The retained
[npm publication/recovery contract](../release/npm-trusted-publishing.md) is not
proof of an available octet npm package.

## Homebrew distribution

The historical maintained macOS tap contract, conditional on formula publication:

```sh
brew install skaft-software/tap/ygg
ygg --version
```

The macOS-only formula installed both native commands, required ripgrep, and was
generated from signed immutable release metadata rather than a mutable lookup.
[Distribution channel gates](../distribution.md) remain separate evidence.

## Cargo

The pinned historical source install did not change a shell startup file:

```sh
cargo install --locked \
  --git https://github.com/skaft-software/ygg \
  --tag v0.6.7 \
  --bins \
  ygg-coding-agent

export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
```

Ygg git/checkout Cargo installs embedded text documentation and materialized it
under `${CARGO_HOME:-$HOME/.cargo}/share/ygg` on first use. Later `ygg update`
refreshed that managed documentation tree with the binary. These are Ygg paths,
not octet old-root readers.

## From a checkout

This was the command for a **Ygg checkout with the old crate path**, not the
current renamed tree:

```sh
git clone https://github.com/skaft-software/ygg.git
cd ygg
cargo install --locked --path crates/ygg-coding-agent --bins
```

Use the separate [current source build](../installation.md#build-from-a-checkout)
for octet.

## Updating

Releases through v0.4.0 had no `ygg update`. The upgrade path was rerunning the
v0.6.7 installer with the same `YGG_INSTALL_DIR`, or the pinned Cargo command for
Cargo installations. The installer replaced `ygg`, `ygg-host`, and packaged
documentation without removing `~/.ygg` config, credentials, or sessions.

From v0.5.0 onward, Ygg updated through its detected installation channel, never
by replacing the running process. The installer or Cargo swapped files; a
restart picked up the new version:

- Installer: `ygg update` reran the latest release's version-pinned installer with the same archive verification as fresh installation.
- Cargo: `ygg update` reinstalled the latest tagged release with locked, tag-pinned `cargo install`.

```sh
ygg update --check   # Report release and the command that would run.
ygg update           # Update through the detected installation method.
```

The TUI `/update` checked for a newer release and directed users to `ygg update`.
Extension packages normally stayed separate from core updates.

The first v0.6.2 startup had a **one-time historical hotfix**: atomically refresh
managed first-party bundles and Ygg Serve installed by v0.6.0/v0.6.1, and remove
the retired `ygg-hermes-memory` package while preserving its data. If download
failed, Ygg continued startup and printed `ygg extension update <name>` recovery.
This does not imply an automatic earlier-first-party migration in octet.
