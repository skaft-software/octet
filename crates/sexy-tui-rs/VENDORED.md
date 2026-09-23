# Vendored provenance

This crate is vendored into the octet workspace so a fresh clone builds the exact
terminal renderer used by `octet-coding-agent`.

- Standalone source: <https://github.com/achuthanmukundan00/sexy-tui-rs>
- Imported standalone revision: `7770c3ef52d1df5b554f597f77d9e85803d8976d`
- Imported standalone version: `0.2.0`
- Current octet workspace package: `0.3.1`
- Combined package license: `MIT AND Apache-2.0`; the imported code remains
  MIT (`LICENSE`), while the Mermaid adaptations below retain Apache-2.0.

The imported revision belongs to the standalone repository's local
`pre-rich-rendering` line and is not currently reachable from one of its public
branches or a release tag. The octet workspace is therefore the source of truth for
package `0.3.1`; synchronize the standalone history deliberately before
advertising an external release.

Historical Pi reference retained from the earlier port documentation:

- Pi source: <https://github.com/earendil-works/pi/tree/20be4b18d4c57487f8993d2762bace129f0cf7c6/packages/tui>
- Pi tag/package: `v0.81.1` / `@earendil-works/pi-tui@0.81.1`
- Pi revision: `20be4b18d4c57487f8993d2762bace129f0cf7c6`
- Pi copyright: Copyright (c) 2025 Mario Zechner
- Pi license: MIT; the upstream notice is preserved in this crate's `LICENSE`
  and in the workspace `THIRD_PARTY_NOTICES.md`.

The current Pi parity target is `0.84.4`. Its exact revision, 33-test-file
inventory, and incomplete audit status (`release_status: in_progress`) are
recorded in [`UPSTREAM-PARITY.md`](UPSTREAM-PARITY.md) and the
[`0.84.4 ledger`](upstream/pi-tui-0.84.4.json). The historical `0.81.1` reference
above does not mean the older ports were imported from `0.84.4`, nor does the
current target imply complete parity.

Core ports must cite and reproduce the pinned Pi tests. Rust-only rich rendering
and octet native-scrollback behavior are additive layers and must not redefine
core Pi APIs or semantics. See `UPSTREAM-PARITY.md` for the port gate and order.

## Mermaid layout and label cleanup

`src/rich_text/mermaid/layout.rs` and `labels.rs` adapt the Apache-2.0
[grok-build](https://github.com/xai-org/grok-build) Rust renderer and
[grok-mermaid](https://github.com/xl0/grok-mermaid) 0.2.3, Pi's diagram dependency.
Copyright 2023–2026 SpaceXAI and Copyright 2026 Alexey Zaytsev are retained in
the source and `src/rich_text/mermaid/LICENSE-APACHE`.
Octet changes include plain-text output, its bounded parser adapter, removal of
ratatui, Rust 2021 compatibility, and hard canvas limits. No new build/runtime
Node, npm, subprocess, or network dependency is introduced.

The inspected upstream Rust file has SHA-256
`e53c81fc02cddc78a3f7f3729237f83acd7c2678535a830210d2b1320f60529f`.
`tests/fixtures/mermaid/README.md` records the package-tarball hash, Pi reference,
regeneration procedure, and exact oracle scope.

The crate-local license accompanies Cargo/source distributions. The workspace
copy at `third_party/licenses/GROK-MERMAID-APACHE-2.0.txt` accompanies native
packages; both are intentional. These components are not relicensed under Pi's
MIT terms. See [`docs/rich-rendering.md`](docs/rich-rendering.md#math-and-diagrams)
for the bounded behavior and [`UPSTREAM-PARITY.md`](UPSTREAM-PARITY.md) for the
separate fixture-evidence boundary.

The vendored source includes octet-specific integration changes maintained in
this workspace. Future updates should be imported deliberately and validated
with the full octet workspace test, formatting, and lint gates.
