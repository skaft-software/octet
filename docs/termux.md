# Termux (Android)

[Documentation](README.md) · [Installation](installation.md) · [Terminal](terminal.md)

octet is released for macOS and Linux. **Termux on Android is not a supported or
qualified platform.** A few Termux-specific paths exist in the tree — clipboard
read/write through `termux-clipboard-get`/`termux-clipboard-set` when
`TERMUX_VERSION` is set, Termux cursor handling in the TUI, and Android in the
process-identity target list — but there is no Android/Termux release artifact
or CI job and no terminal-emulator qualification, so anything below is "may
work, unverified" rather than a compatibility claim.

## Why this is unqualified

- Installation and packaging target macOS/Linux release artifacts and the
  documented build profiles ([installation](installation.md),
  [build profiles](build-profiles.md)); there is no published Android/aarch64
  Termux artifact.
- Clipboard and image handling use desktop mechanisms plus the Termux clipboard
  helpers. Android image handling, terminal sizing and input quirks are not
  implemented or exercised.
- Keyboard/terminal behavior under Termux's emulator has not been exercised in
  the terminal qualification record ([terminal](terminal.md)).

## Building from source (unverified)

If you choose to experiment, build from a checkout with the same profile used for
Linux:

```sh
pkg update && pkg upgrade
pkg install rust clang git
cargo build --release -p octet-coding-agent --bin octet
```

Then run `./target/release/octet`. Expect rough edges: no image handling,
unverified clipboard behaviour beyond the Termux text helpers, uncertain
colour/keyboard capability detection, and no release-support channel.

## Reporting

Portability findings should include the build and terminal versions. An
unqualified platform observation is not evidence of a supported-platform regression.
