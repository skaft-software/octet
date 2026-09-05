<p align="center">
  <img src="docs/assets/octet/marks/mark-gradient.svg" alt="octet mark" width="112">
</p>

# octet

**The high-performance coding agent, extensible in any language. Free and open source.**

Turn a repository task into a reviewable change. Add your own tools and workflow
context through language-neutral subprocess extensions, delegate bounded
investigations, or put the same agent runtime behind your application. The native
Rust core works with cloud models and explicitly configured local endpoints;
append-only sessions keep the work inspectable and resumable.

[Documentation](docs/README.md) · [Workflows](docs/workflows.md) ·
[Examples](examples/README.md) · [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md)

> **octet 0.7.0 source, unpublished and not yet release-qualified.** This checkout
> defines `octet` and `octet-host`, `octet-*` crates/packages, `OCTET_*`
> environment variables, and separate `.octet` user/project roots. There are no
> Ygg aliases, old-root fallbacks, or automatic first-party data migrations.
> Existing Ygg installations and data remain separate. The actual source repository
> remains [skaft-software/ygg](https://github.com/skaft-software/ygg).
> [Release notes and qualification gates](docs/releases/v0.7.0.md).

## What will you build?

| Outcome | Start here |
| --- | --- |
| Turn an audio reference into a reviewed implementation brief | [Start with sound](docs/workflows.md#start-with-an-audio-reference): attach native WAV/MP3 on a compatible OpenAI Chat route, optionally compare images, and review the brief before coding. |
| Build a tool, use it on a real task, and reuse it | [Repository-contract extension](docs/workflows.md#2-build-and-use-a-domain-extension): adapt the Git example into a bounded contract-inspection tool, test it, and use its evidence in a second repository. |
| Delegate two audits and integrate a continued worker's findings | [Read-only delegation](docs/workflows.md#delegate-independent-investigations): separate investigations, verify unique findings, and resume a useful worker in its durable session. |
| Add a repository assistant to your own application | [Embed a read-only assistant](docs/workflows.md#3-embed-a-read-only-repository-assistant): negotiate the native host protocol, stream a bounded run, and retain a resumable session. |

The [reviewable-change recipe](docs/workflows.md#1-make-a-reviewable-repository-change)
also covers inspect/edit/verify and session handoff. These are source-grounded
recipes, not fabricated demo transcripts or measured 0.7.0 results.

## Build this checkout

On macOS or GNU/Linux, install Rust 1.86+ and
[ripgrep](https://github.com/BurntSushi/ripgrep), then from the repository root:

```sh
cargo build --release --locked -p octet-coding-agent --bins
./target/release/octet --version
./target/release/octet --help
```

This builds locally without replacing an installed binary. If you set
`CARGO_TARGET_DIR`, use that directory instead of `target`. Select an available
model through [provider setup](docs/current-reference.md#quick-start), then use
[the workflow recipes](docs/workflows.md). Provider access and extension runtime
dependencies are separate from the free, MIT-licensed agent.

There is no verified octet download, npm package, or Homebrew installation
command advertised here. Historical Ygg channels are documented separately in
the [historical Ygg reference](docs/current-reference.md#historical-ygg-distribution-reference).

## Control and boundaries

Built-in tools read, edit, write, and run shell commands; workspace search is
opt-in. The default full-access policy uses the launching user's OS authority.
`--safe-mode` requires approval for workspace mutation and every shell call and
does not start executable extensions. Neither mode is an OS sandbox: use a
container, VM, or restricted account for untrusted work. Read the
[security policy](SECURITY.md) before enabling tools or extensions.

Extensions can be written in any language that can exchange bounded JSON-RPC
lines over stdio. Installation, enablement, and exact-source trust are separate
steps; runtime dependencies remain the extension author's responsibility. The
[extension contracts](docs/extensions.md), [Python SDK](sdk/python/README.md),
and [native host contract](docs/sdk.md) describe the actual supported surfaces.
[Pi compatibility](docs/pi-migration.md) is a bounded subset, not full parity.

## Historical measurement

Ygg **v0.6.2**, GPT-5.6 Sol/max, Terminal-Bench 2.1, 89 tasks × 5 trials:
**87.87% raw**, **86.97% local audit** (strict: **86.52%**). Not official
maintainer adjudication or an octet 0.7.0 measurement. [Evidence and audit
limits](docs/benchmarks/tb21-v0.6.2/README.md) ·
[Compact historical evidence ZIP and qualifications](docs/assets/evidence/README.md)
—not the full raw trajectories or a new candidate comparison.

## Project and license

Built by [Achu](https://github.com/achuthanmukundan00). octet is distributed under
the [MIT License](LICENSE). Architecture and terminal interaction patterns draw
on [Pi](https://github.com/earendil-works/pi); development and evaluation also draw
on [Terminal-Bench](https://github.com/harbor-framework/terminal-bench). Preserve
upstream identity and licensing in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

[Canonical identity assets](docs/assets/octet/README.md) include the
[mark](docs/assets/octet/marks/mark-gradient.svg) and
[wordmark](docs/assets/octet/marks/wordmark-black.svg). Documentation and examples
in this repository are the product authority; no external website is required.
