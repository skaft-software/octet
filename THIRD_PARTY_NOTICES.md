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
