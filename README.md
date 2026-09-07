<p align="center">
  <img src="docs/assets/octet/marks/mark-gradient.svg" alt="octet mark" width="112">
</p>

# octet

**A high-performance coding agent, extensible in any language.**

[![Release candidate: 0.7.1](https://img.shields.io/badge/release-0.7.1_candidate-536dfe?style=flat-square)](docs/releases/v0.7.1.md)

octet reads code, edits files, and runs commands from your terminal. It has a
native Rust core, supports cloud and local models, saves resumable sessions,
and lets you add tools through subprocess extensions.

[Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## Install

**Native installer (after v0.7.1 publication):** macOS Apple silicon/Intel
and GNU/Linux x86-64:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.7.1/install-octet.sh | sh
```

octet 0.7.1 is a release candidate; publication and public-install verification
are pending. See [release notes](docs/releases/v0.7.1.md) and
[installation](docs/installation.md) for scope, prerequisites and channel availability.
When moving from Ygg, install octet afresh: older installations and data remain
separate; no automatic migration is performed.

**From source:** on macOS or GNU/Linux, install Rust 1.86+ and
[ripgrep](https://github.com/BurntSushi/ripgrep), then run from this checkout:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet
```

This builds `octet` and `octet-host` in `target/release` without replacing an
installed copy. Continue with [getting started](docs/getting-started.md).

## Codebase

| Directory | Contents |
| --- | --- |
| `crates/` | Model clients, agent runtime, terminal interface, and native host |
| `extensions/` | Browser, search, MCP, subagents, and graphical Serve packages |
| `sdk/` | Integration libraries and protocol types |
| `examples/` | Extensions, skills, and prompt templates |
| `apps/web/` | Serve's browser interface |
| `docs/` | Guides, API reference, and architecture |

## More

[Benchmarks and performance](docs/benchmarks/README.md) ·
[Download benchmark results](docs/assets/evidence/README.md) ·
[Brand Kit](docs/assets/octet/README.md) ·
[Roadmap](https://github.com/orgs/skaft-software/projects/5) ·
[Changelog](CHANGELOG.md)

Built by [Achu](https://github.com/achuthanmukundan00). [MIT licensed](LICENSE).
Design patterns draw on Pi and the Terminus 2 agent. The project author states
that benchmark evaluation data was not used to develop the agent; comparisons
follow benchmarking. See [third-party notices](THIRD_PARTY_NOTICES.md).
