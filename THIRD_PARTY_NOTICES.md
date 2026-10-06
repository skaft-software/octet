# Third-Party Notices

octet is [MIT licensed](LICENSE). The projects below retain their own licenses.

## Design influences

octet's design draws on **Pi** and the **Terminus 2 agent**. 
Absolutely no benchmark evaluation data/traces was used, or should ever be used to develop octet. 

Benchmark results are used only to evaluate octet's measured results against published
leaderboard results after benchmarking+adjudication.

## Pi

[Pi](https://github.com/earendil-works/pi) informed octet's agent architecture
and terminal interaction patterns. The vendored `sexy-tui-rs` crate is a Rust
port of Pi's TUI architecture.

- Copyright (c) 2025 Mario Zechner
- License: MIT
- [License text](third_party/licenses/PI-MIT.txt)

The optional `extensions/octet-pi-compat` package uses the pinned
`@earendil-works/pi-tui` 1.0.2 MIT-licensed component/utilities library inside
its separate Node process. It does not install, import, or execute Pi's
coding-agent runtime. Node, jiti, TypeBox, and these Pi libraries are not
required by the ordinary Rust binary or native executable extensions.

Acceptance tests can load original third-party extensions from explicitly
supplied paths; those sources and assets are not bundled into octet. In
particular, `badlogic/pi-doom` declares GPL-2.0, and its shareware WAD retains
its own distribution terms. Ben Vinegar's `pi-stuff` drawing extension and
`pi-agent-extensions` retain their upstream MIT notices. Keep those notices
with separately installed packages; compatibility does not relicense them.

## Pi codemode and QuickJS WASI

The optional [octet-codemode](extensions/octet-codemode/README.md) extension
vendors the published MIT `@earendil-works/pi-codemode` 1.0.0 (Copyright (c) 2025
Mario Zechner) and `quickjs-wasi` 3.6.2 (Copyright (c) 2026 Vercel, Inc.) for
offline QuickJS/WASM execution. This optional Node runtime is not a dependency
of octet's Rust host. The only Pi runtime patch relocates its QuickJS import to
the bundled relative path; optional native `.so` modules are not extracted or
used. QuickJS-NG's separate MIT and WASI/LLVM runtime-component notices, plus
notices for optional modules inside the original archive, are retained too.

Complete licenses, original npm archives, pinned registry integrity/SHA256,
Pi's published Git head and offline regeneration checks are retained in the
bundle. See its [third-party notices](extensions/octet-codemode/THIRD_PARTY_NOTICES.md),
[Pi MIT license](extensions/octet-codemode/vendor/pi-codemode/LICENSE),
[QuickJS WASI MIT license](extensions/octet-codemode/vendor/quickjs-wasi/LICENSE),
and [provenance](extensions/octet-codemode/vendor/PROVENANCE.json).

## grok-mermaid and grok-build

The terminal flowchart layout and label cleanup in
`crates/sexy-tui-rs/src/rich_text/mermaid/layout.rs` and `labels.rs` are adapted from
[xai-org/grok-build](https://github.com/xai-org/grok-build)'s
`xai-grok-markdown/src/mermaid.rs`, the Rust origin of
[grok-mermaid](https://github.com/xl0/grok-mermaid) 0.2.3 used by Pi.
Octet modifications include plain-text output, its bounded graph-parser adapter,
removal of the ratatui dependency, Rust 2021 compatibility, and hard canvas
limits. This does not add a Node/npm build or runtime dependency, nor imply
complete upstream renderer equivalence.

- Copyright 2023–2026 SpaceXAI
- Copyright 2026 Alexey Zaytsev
- License: Apache License 2.0 (separate from Pi's MIT license)
- [License text](third_party/licenses/GROK-MERMAID-APACHE-2.0.txt)

## Terminus 2 and Terminal-Bench

The Terminus 2 agent informed octet's design patterns.
[Terminal-Bench](https://github.com/harbor-framework/terminal-bench) provides
the benchmark and evaluation tooling. These are separate roles: design
inspiration is not use of benchmark evaluation data for agent development.

- Attribution: Terminal-Bench project and contributors
- License: Apache License 2.0
- [License text](third_party/licenses/TERMINAL-BENCH-APACHE-2.0.txt)

The upstream Terminal-Bench repository had no separate `NOTICE` file when this
notice was prepared.

## Local typeface

The ChatGPT sign-in pages use Skaft Software's
[Local 0.53](https://github.com/skaft-software/local-typeface/releases/tag/v0.53.0)
Local Grotesk Regular, retained independently of the removed web UI in
`docs/assets/fonts/`, with its lineage notice.

- **Local Grotesk** is a modified and renamed version of TeX Gyre Heros 2.004.
  License: GUST Font License (`docs/assets/fonts/GUST-FONT-LICENSE.txt`).
