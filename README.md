<p align="center">
  <img src="docs/assets/octet/marks/mark-gradient.svg" alt="octet mark" width="112">
</p>

# octet

**A fast, native coding agent.**

[![Release: 0.9.3](https://img.shields.io/badge/release-0.9.3-536dfe?style=flat-square)](docs/releases/v0.9.3.md)

octet reads code, edits files, and runs commands from your terminal. The core is
native Rust: one process owns the agent loop, sessions, policy, persistence, the
terminal interface and resource limits. It works with cloud and local models,
saves resumable sessions, and lets you add tools through subprocess extensions
in any language.

MCP, web search, subagents, codemode and computer use are optional first-party
integrations. Pi is Mario Zechner's coding agent and informs some design choices;
the optional [octet-pi-compat](extensions/octet-pi-compat/README.md) adapter is
**experimental and unstable** source-only work and **does not ship in v0.9.3**:
full Pi backwards compatibility is not yet qualified. Several unchanged Pi extensions
already run through it in the source preview, but no Pi extension parity or
installed-Pi workflow acceptance is claimed.

**By default octet has full access and no sandbox.** Commands, file edits, and
enabled extensions run with your operating-system permissions, and nothing asks
first. `--safe-mode` asks before every shell call and file change and removes
the implicit authority that would start an enabled extension; an explicit
`--trust-extension` or `--extension-dir` grant still starts one, outside the
tool-effect broker. Safe mode is an approval policy, not a sandbox. See
[Security](SECURITY.md#permissions).

See the [v0.9.3 release notes](docs/releases/v0.9.3.md) for scope, qualification
boundaries and known limitations.

[Documentation](docs/README.md) · [Contributing](CONTRIBUTING.md) · [Security](SECURITY.md)

## Install

**Native installer:** macOS Apple silicon/Intel and GNU/Linux x86-64, no Node.js
required. Use the [v0.9.3 release page](https://github.com/skaft-software/octet/releases/tag/v0.9.3)
for signed assets and completed public-install receipts; do not substitute an
unsigned candidate artifact.

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/skaft-software/octet/releases/download/v0.9.3/install-octet.sh | sh
octet --version   # octet 0.9.3
```

**Windows support is a preview.** Windows x86-64 uses the version-matched `install-octet.ps1` and MSVC archive on
that release. npm, Homebrew and SDK registries are separate publication channels;
see [distribution](docs/distribution.md) for their actual availability.

When moving from an older pre-rename installation, install octet afresh: older
installations and data remain separate; no automatic migration is performed.

**From source:** install Rust 1.88+, then run from this checkout.
[ripgrep](https://github.com/BurntSushi/ripgrep) is the preferred optional search
helper; octet uses `grep` when `rg` is unavailable:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet
```

This builds `octet` and `octet-host` without replacing an installed copy.
Continue with [getting started](docs/getting-started.md).

## Codebase

| Directory | Contents |
| --- | --- |
| `crates/` | Model clients, agent runtime, terminal interface, and native host |
| `extensions/` | Shipped optional bundles: codemode, computer use, MCP, subagents, web search; Pi compat and snap-compact are source-only |
| `sdk/` | Integration libraries and protocol types |
| `examples/` | Extensions, skills, and prompt templates |
| `docs/` | Guides, API reference, and architecture |

## More

[Benchmarks and performance](docs/benchmarks/README.md) ·
[Download benchmark results](docs/assets/evidence/README.md) ·
[Brand Kit](docs/assets/octet/README.md) · [Changelog](CHANGELOG.md)

Built by [Achu Mukundan](https://github.com/achuthanmukundan00). [MIT licensed](LICENSE).
Pi's licence and attribution are retained in [third-party notices](THIRD_PARTY_NOTICES.md).
Benchmark evaluation data is not used to develop the agent; comparisons follow
benchmarking.
