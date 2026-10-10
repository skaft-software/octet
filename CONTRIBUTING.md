# Contributing to octet

Focused bug fixes, protocol and provider improvements, terminal UX work, tests
and documentation corrections are welcome. The project is pre-1.0, so keep
changes small and back them with evidence.

## Development setup

The octet source supports macOS and Linux and needs Rust 1.88 or newer. A native
Windows x64 build (`x86_64-pc-windows-gnu`) is built and tested in CI but not
released yet: see [Windows](docs/windows.md). Install Rust through
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
2. Check the [project](https://github.com/orgs/skaft-software/projects/5).
   Discuss substantial changes in an issue before writing a large patch. Broad
   or unresolved ideas can start in
   [Discussions](https://github.com/skaft-software/octet/discussions).
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

Start with the narrowest regression that reproduces the behavior, then test the
affected crate. Before requesting review, run:

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

## Identity and documentation

Use lowercase **octet**. Keep [documentation](docs/README.md), examples,
commands and generated references consistent with the code. Keep required
third-party notices and accurate version labels on benchmark results.

## AI-assisted contributions

AI-generated issues and pull requests are welcome and will be considered. If AI
helped produce your contribution, include a brief, genuine note from you about
what you observed, what you care about, or why the change matters. Fully
machine-generated requests with no sign of human review or diligence may be
treated as spam, and I may not respond to messages that look AI-generated when I
judge them unimportant or spam.

Please also include the relevant prompts you used to reach the conclusions in an
issue or to generate a pull request, with enough context to show what you
verified yourself. Redact secrets and private information. Requests for the
prompts behind a contribution are welcome too.

## Commits and pull requests

Use a short, imperative commit subject, for example:

```text
fix: preserve tool output across reconnect
```

A pull request should say what changed, why, the user or developer impact, a
defect's root cause, the exact checks that passed, and any known limitation or
compatibility effect.

Keep generated build artifacts, local reports, credentials, sessions,
`AGENTS.md` and private research notes out of commits. The repository
`.gitignore` lists the expected local-only paths.

## Licensing

By contributing, you agree your contribution is distributed under the project's
[MIT License](LICENSE). Keep upstream notices when changing vendored or derived
code. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Project tracking

[Issues](https://github.com/skaft-software/octet/issues) and the [engineering
project](https://github.com/orgs/skaft-software/projects/5) track proposed work.
