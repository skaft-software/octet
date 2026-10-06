<p align="center">
  <img src="docs/assets/octet/marks/mark-gradient.svg" alt="octet mark" width="112">
</p>

# octet

**A fast, native coding agent.**

[![Candidate: 0.9.0](https://img.shields.io/badge/candidate-0.9.0-536dfe?style=flat-square)](docs/releases/v0.9.0.md)

octet reads code, edits files, and runs commands from your terminal. The core is
native Rust: one process owns the agent loop, sessions, policy, persistence, the
terminal interface and resource limits. It works with cloud and local models,
saves resumable sessions, and lets you add tools through subprocess extensions
in any language.

Your Pi setup can come with you. The optional
[octet-pi-compat](extensions/octet-pi-compat/README.md) extension imports a
reviewed Pi setup — extensions, skills, prompt templates, themes, keybindings,
models and context — or mirrors it read-only at startup. Pi is Mario Zechner's
coding agent; the adapter targets the pinned **Pi 1.0.2 public extension API**,
not arbitrary third-party packages, private Pi internals, or the Pi CLI/SDK.
Implementation and real-host acceptance are tracked per feature in the
[27-row ledger](docs/pi-extension-api.md), and
[compatibility](docs/pi-compatibility.md) records what is qualified, including
the extensions that still fail.

MCP, web search, subagents and computer use remain optional first-party
integrations. The Rust host owns sessions, policy, persistence, terminal UI and
resource limits.

**By default octet has full access and no sandbox.** Commands, file edits, and
enabled extensions run with your operating-system permissions, and nothing asks
first. `--safe-mode` asks before every shell call and file change and removes
the implicit authority that would start an enabled extension; an explicit
`--trust-extension` or `--extension-dir` grant still starts one, outside the
tool-effect broker. Safe mode is an approval policy, not a sandbox. See
[Security](SECURITY.md#permissions).

This checkout is the **octet 0.9.0 candidate**, not a published release. See the
[release notes](docs/releases/v0.9.0.md) for scope and known limitations.

[Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## Install

**The 0.9.0 release candidate is not published yet.** Use the source-build
instructions below for this candidate. The native and npm commands are planned
post-publication instructions, not available installation channels. Until an
approved promotion, the public URLs keep naming the preceding 0.8.2 candidate,
whose assets are not published either.

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
recorded on the version's
[GitHub release](https://github.com/skaft-software/octet/releases). See
[release notes](docs/releases/v0.9.0.md) and
[installation](docs/installation.md) for scope, prerequisites and channel
availability. When moving from an older pre-rename installation, install octet
afresh: older installations and data remain separate; no automatic migration is
performed.

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
| `extensions/` | Optional executable bundles: codemode, computer use, MCP, Pi compat, subagents, web search; snap-compact is source-only |
| `sdk/` | Integration libraries and protocol types |
| `examples/` | Extensions, skills, and prompt templates |
| `docs/` | Guides, API reference, and architecture |

## More

[Benchmarks and performance](docs/benchmarks/README.md) ·
[OpenRouter Batch API](docs/openrouter-batches.md) ·
[Download benchmark results](docs/assets/evidence/README.md) ·
[Brand Kit](docs/assets/octet/README.md) ·
[Changelog](CHANGELOG.md)

Built by [Achu Mukundan](https://github.com/achuthanmukundan00). [MIT licensed](LICENSE).
Design patterns draw on Pi and the Terminus 2 agent; Pi's own licence and
attribution are retained in [third-party notices](THIRD_PARTY_NOTICES.md).
Benchmark evaluation data is not used to develop the agent; comparisons follow
benchmarking.
