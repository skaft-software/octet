# Third-Party Notices

octet is [MIT licensed](LICENSE). The projects below retain their own licenses.

## Design influences

octet's design draws on **Pi** and the **Terminus 2 agent**. The project author
states that no benchmark evaluation data was used to develop octet. Benchmark
results are used only to compare octet's measured results with published
leaderboard results after benchmarking.

## Pi

[Pi](https://github.com/earendil-works/pi) informed octet's agent architecture
and terminal interaction patterns. The vendored `sexy-tui-rs` crate is a Rust
port of Pi's TUI architecture.

- Copyright (c) 2025 Mario Zechner
- License: MIT
- [License text](third_party/licenses/PI-MIT.txt)

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
