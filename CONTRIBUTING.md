# Contributing to octet

Focused bug fixes, protocol and provider improvements, terminal UX work, tests
and documentation corrections are welcome. The project is pre-1.0, so keep
changes small and back them with evidence.

Please follow the [code of conduct](CODE_OF_CONDUCT.md).

## How changes reach main

Send your change as a pull request. It is reviewed as untrusted code, and CI
must pass. Maintainers and their agents push directly to `main` instead, as
linear fast-forward commits, each checked by the full `scripts/gate.sh` on the
exact commit (see [AGENTS.md](AGENTS.md)). `main` is always releasable, and the
owner chooses when to release.

Opening or reopening a PR adds its human author as an assignee, preserving any
existing assignees. Bots and the repository owner are excluded. This automation
reads event metadata only; it never checks out or executes contribution code.

## Development setup

The octet source supports macOS and Linux and needs Rust 1.89 or newer. A native
Windows x64 GNU build is tested in CI; the MSVC release channel remains a
preview with separate qualification: see [Windows](docs/windows.md). Install Rust through
[rustup](https://rustup.rs/). `rg` (ripgrep) is preferred for content search when
available; `grep` is the fallback, so ripgrep is not required. Clone the
repository if you need a checkout:

```sh
git clone https://github.com/skaft-software/octet.git octet
cd octet
```

The public default branch has the current octet source. Use the `v0.7.0` tag to
reproduce that release, or work from the default branch when contributing. The
Cargo commands below run from the repository root.

```sh
cargo check --workspace --all-targets --all-features --locked
```

Run the binary without installing it:

```sh
cargo run -p octet-coding-agent --bin octet -- --help
```

Cargo doesn't clean up stale fingerprints from old toolchains, feature sets or
profiling runs. If `target/` grows unexpectedly, check it with `du -sh target`
and reclaim it with `cargo clean`. Use a separate `CARGO_TARGET_DIR` for one-off
instrumentation and benchmark builds. Build artifacts are excluded from both Git
and the Docker context.

## Before opening a change

1. Search existing issues and pull requests for the same behavior.
2. Check the [project](https://github.com/orgs/skaft-software/projects/5), then
   scope the change in an issue before starting and link it in the PR body
   (for example, `Refs #` followed by its number). Broad or unresolved ideas can
   start in [Discussions](https://github.com/skaft-software/octet/discussions).
3. For security-sensitive findings, use the private reporting path in
   [SECURITY.md](SECURITY.md). Don't open a public issue first.
4. Keep unrelated formatting, generated output, local notes, credentials and
   editor state out of the change.
5. Explain the user-visible problem and the boundary the fix should preserve.

## Change guidelines

- Keep canonical request and session types unless a compatibility break is
  explicitly required.
- Keep provider-specific behavior in protocol or compatibility layers, not the
  agent loop.
- Treat provider output, repository content, terminal text, resource files,
  session records and extension frames as untrusted, bounded input.
- Never weaken workspace trust, tool policy, no-follow paths, cancellation,
  persistence or redaction guarantees for convenience.
- Keep the default terminal experience stable across dark and light backgrounds,
  Unicode and ASCII, color and no-color, wide and narrow widths, and redirected
  output.
- Don't add network-dependent build steps. Checked-in model metadata is the
  deterministic build source.
- New dependencies need a product reason and must pass the license, advisory and
  source policy.

## Tests

For a first credential-free pass (Python 3.10+ and Rust 1.89+):

```sh
cargo fetch --locked                 # one-time dependency download, if needed
python3 scripts/check-contributor.py # use python on Windows
```

The script never installs packages or contacts a provider. Cargo checks run
`--offline`; a cold compile can take several minutes. `--list` shows the exact
commands. It runs selected policy/file/temporary-bulk tests, real agent-dispatch
approval regressions, the Python SDK and patch-whitespace checks. It does **not**
run the full workspace, Pi adapter, durable Windows bulk storage, paid providers
or attended desktop checks. Expected skips carry a reason; a failure stays a
failure. See the [testing lanes](docs/testing/README.md) for broader checks and
platform limitations.

Start with the narrowest regression that reproduces the behavior, then test the
affected crate. Before requesting review, run `scripts/gate.sh` (or at least):

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --profile ci-test --locked
cargo test --workspace --doc --profile ci-test --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo audit
cargo deny check
git diff --check
```

CI uses the additive `ci-test` profile. Ordinary `cargo test` keeps Cargo's
default test profile. See [build profiles](docs/build-profiles.md) for CI and
profiling commands.

Terminal changes should include a renderer, VT100 or PTY regression for cell,
cursor, scrollback, style or shutdown behavior. Protocol changes need exact wire
fixtures and malformed-stream coverage. Session changes should cover restart and
torn-tail behavior.

The live multimodal test is intentionally ignored unless an explicitly
configured compatible endpoint is available. Maintainers may also run
the separately approved credentialed checks in
[configured-provider acceptance](docs/experimental/octet-serve/provider-acceptance.md)
against the immutable release SHA. Live checks are optional; release qualification
does not require live credentials. An unselected check is recorded as **NOT RUN**,
not a pass or waiver.

## Finding a bounded first change

| Boundary | Start here | Protect |
| --- | --- | --- |
| Provider wire/auth | `crates/octet-ai/src/` | Typed responses, redaction, bounded parsing; no tool authority |
| Policy and tool dispatch | `crates/octet-agent/src/effect.rs`, `agent/`, `tools/` | Exact approvals before hooks/execution; original-byte edit hashes |
| Persistence/recovery | `crates/octet-agent/src/session/`, `secure_fs/` | Persist before execute; no automatic replay of unresolved mutations |
| CLI and terminal | `crates/octet-coding-agent/src/` | Runtime owns state; frontend is an observer |
| Extensions/SDK | `extensions/`, `sdk/` | Explicit host trust, protocol and resource bounds |

Good first issues name one boundary, a minimal reproducer, one owning module and
an executable acceptance check (for example an edit newline case or a sanitized
provider-error fixture). Keep architecture changes incremental: extract a focused
module when the relevant behavior changes, not as a repository-wide rewrite.
[Architecture](docs/README.md#development) links the deeper boundary documents.

## Identity and documentation

Use lowercase **octet**. Keep [documentation](docs/README.md), examples,
commands and generated references consistent with the code. Keep required
third-party notices and accurate version labels on benchmark results.

## AI-assisted contributions

AI-assisted issues and pull requests are welcome. The human contributor is
accountable for what their agent submits; requests without genuine human review
or diligence may be treated as spam.

- **In your own words (required):** 1–3 sentences written or explicitly approved
  by the human about why this matters. Agents must never invent or paraphrase
  this as the human's words.
- **AI provenance (required):** name the tool/agent, exact model name or ID
  (including fine-tunes and uncensored or abliterated variants), and reasoning
  effort. Use "not exposed" for any detail the tool does not expose; human-written
  contributions without AI assistance say "none".
- **Prior work checked:** search existing issues and PRs, including closed ones,
  and documentation first. Say what you found and link relevant work, or state
  that no match was found.
- **Verification:** report PASS / FAIL / NOT RUN with the exact commands and
  observed results (or why not run). Never claim tests, reproduction, review or
  measurements that did not happen.

**Security check (required at any risk level):** write one short paragraph saying
what the agent checked for and what it found: new network calls, process spawning
or shell commands, file or credential access outside the change, obfuscated or
encoded code, new dependencies, and CI/workflow/release changes. Use a plain
statement, not a form; for an unassisted contribution, describe your own check.

Include relevant prompts and enough context to show what you verified yourself;
redact secrets and private information. Requests for prompt context are welcome.

## Commits and pull requests

Use a short, imperative commit subject, for example:

```text
fix: preserve tool output across reconnect
```

A pull request should say what changed, why, the user or developer impact, a
defect's root cause, the exact checks that passed, and any known limitation or
compatibility effect. A maintainer decides at review whether the change fits the
agreed scope; an issue link does not guarantee acceptance. Contributors use squash
PRs gated by `ci-required`; the maintainer may push directly to `main`. PRs run
fast CI (or the full tier with the `ci:full` label). The full matrix runs on main
pushes and manual workflow dispatches; there is no merge queue.

Keep generated build artifacts, local reports, credentials, sessions and
private research notes out of commits. The repository `.gitignore` lists the
expected private paths. Repository agent conventions are tracked in
[AGENTS.md](AGENTS.md); update that source rather than keeping a separate mirror.

## Licensing

By contributing, you agree your contribution is distributed under the project's
[MIT License](LICENSE). Keep upstream notices when changing vendored or derived
code. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Project tracking

[Issues](https://github.com/skaft-software/octet/issues) and the [engineering
project](https://github.com/orgs/skaft-software/projects/5) track proposed work.
