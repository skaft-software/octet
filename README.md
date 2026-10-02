<p align="center">
  <img src="docs/assets/octet/marks/mark-gradient.svg" alt="octet mark" width="112">
</p>

# octet

**A high-performance coding agent.**

[![Candidate: 0.8.2](https://img.shields.io/badge/candidate-0.8.2-536dfe?style=flat-square)](docs/releases/v0.8.2.md)

octet reads code, edits files, and runs commands from your terminal. It has a
native Rust core, supports cloud and local models, saves resumable sessions,
and lets you add tools through subprocess extensions in any language.

Extensions add bounded, host-shaped integrations—not an everything-as-extension
platform or a promise to run unchanged Pi extensions. Browse, MCP, web search,
and host-owned subagents remain optional integrations; Serve is a separate
graphical application. The host keeps authority over sessions, lifecycle, and
resource limits.

**By default octet has full access and no sandbox.** Commands, file edits, and
enabled extensions run with your operating-system permissions, and nothing asks
first. `--safe-mode` asks before every shell call and file change, but it is an
approval policy, not a sandbox. See [Security](SECURITY.md#permissions).

This checkout is the **octet 0.8.2 candidate**, not a published release. See the
[release notes](docs/releases/v0.8.2.md) for scope and remaining qualification.

[Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## Install

**0.8.2 is not published yet.** Use the source-build instructions below for this
candidate. The native and npm commands are planned post-publication instructions,
not currently available 0.8.2 installation channels.

**Native installer (after publication):** macOS Apple silicon/Intel and GNU/Linux
x86-64, no Node.js required:

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.8.2/install-octet.sh | sh
```

**npm (after publication):** same platforms, if you already have Node.js:

```sh
npm install -g @skaft/octet@0.8.2
octet --version   # octet 0.8.2
```

The planned launcher pulls the matching signed platform package
(`@skaft/octet-darwin-arm64`, `@skaft/octet-darwin-x64`, or
`@skaft/octet-linux-x64-gnu`) with npm provenance. Both lanes must be published
from the same verified release assets; see
[distribution](docs/distribution.md) to pin an exact version.

After approved publication, signed assets and public-install results must be
recorded on the
[GitHub release](https://github.com/skaft-software/octet/releases/tag/v0.8.2).
See [release notes](docs/releases/v0.8.2.md) and
[installation](docs/installation.md) for scope, prerequisites and channel availability.
When moving from Ygg, install octet afresh: older installations and data remain
separate; no automatic migration is performed.

**From source:** on macOS or GNU/Linux, install Rust 1.88+ and
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
[OpenRouter Batch API](docs/openrouter-batches.md) ·
[Download benchmark results](docs/assets/evidence/README.md) ·
[Brand Kit](docs/assets/octet/README.md) ·
[Changelog](CHANGELOG.md)

Built by [Achu Mukundan](https://github.com/achuthanmukundan00). [MIT licensed](LICENSE).
Design patterns draw on Pi and the Terminus 2 agent. Benchmark evaluation data is
not used to develop the agent; comparisons follow benchmarking.
See [third-party notices](THIRD_PARTY_NOTICES.md).
