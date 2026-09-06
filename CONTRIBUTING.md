# Contributing to octet

Focused bug fixes, protocol and provider improvements, terminal UX work, tests,
and documentation corrections are welcome. The project is pre-1.0; keep changes
small and support them with evidence.

## Development setup

The octet source supports macOS and Linux and declares Rust 1.86 as its minimum
supported version. Install Rust through [rustup](https://rustup.rs/) and install
`rg` (ripgrep). Clone the actual repository if you need a checkout:

```sh
git clone https://github.com/skaft-software/octet.git octet
cd octet
```

The public default branch contains the current octet source. Use the `v0.7.0`
tag to reproduce that release, or work from the default branch when contributing.
The Cargo commands below run from the repository root.

```sh
cargo check --workspace --all-targets --all-features --locked
```

Run the binary without installing it:

```sh
cargo run -p octet-coding-agent --bin octet -- --help
```

Cargo does not garbage-collect stale fingerprints from old toolchains, feature
sets, or profiling runs. If `target/` grows unexpectedly, inspect it with
`du -sh target` and reclaim it with `cargo clean`. Use an isolated
`CARGO_TARGET_DIR` for one-off instrumentation and benchmark builds. Build
artifacts are excluded from both Git and the Docker context.

## Before opening a change

1. Search existing issues and pull requests for the same behavior.
2. Check the [project](https://github.com/orgs/skaft-software/projects/5).
   Discuss substantial changes in an issue before writing a large patch; broad
   or unresolved ideas can start in
   [Discussions](https://github.com/skaft-software/octet/discussions).
3. For security-sensitive findings, use the private reporting path in
   [SECURITY.md](SECURITY.md). Do not open a public issue first.
4. Keep unrelated formatting, generated output, local notes, credentials, and
   editor state out of the change.
5. Explain the user-visible problem and the boundary the fix should preserve.

## Change guidelines

- Preserve canonical request/session types unless a compatibility break is
  explicitly required.
- Keep provider-specific behavior in protocol or compatibility layers, not the
  agent loop.
- Treat provider output, repository content, terminal text, resource files,
  session records, and extension frames as untrusted bounded input.
- Never weaken workspace trust, tool-policy, no-follow path, cancellation,
  persistence, or redaction guarantees for convenience.
- Keep the default terminal experience stable across dark/light backgrounds,
  Unicode/ASCII, color/no-color, wide/narrow widths, and redirected output.
- Do not add network-dependent build steps. Checked-in model metadata is the
  deterministic build source.
- New dependencies need a product reason and must pass license, advisory, and
  source policy.

## Tests

Start with the narrowest regression that reproduces the behavior, then test the
affected crate. Before requesting review, run these checks:

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets --all-features --locked
cargo test --workspace --all-targets --all-features --profile ci-test --locked
cargo test --workspace --doc --profile ci-test --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo audit
cargo audit --file extensions/octet-serve/Cargo.lock
cargo deny check
cargo deny --manifest-path extensions/octet-serve/Cargo.toml check
(cd apps/web && npm ci && npm audit --audit-level=high)
git diff --check
```

CI uses the additive `ci-test` profile; ordinary `cargo test` keeps Cargo's
default test profile. See [build profiles](docs/build-profiles.md) for CI and
profiling commands.

Terminal changes should include a renderer, VT100, or PTY regression for cell,
cursor, scrollback, style, or shutdown behavior. Protocol changes need exact wire
fixtures and malformed-stream coverage. Session changes should cover restart
and torn-tail behavior.

The live multimodal test is intentionally ignored unless an explicitly
configured compatible endpoint is available. Stable Serve releases must pass the
disposable configured-provider matrix in ordinary CI. Maintainers may also run
the separately approved credentialed checks in
[configured-provider acceptance](docs/experimental/octet-serve/provider-acceptance.md)
against the immutable release SHA. Live checks are optional; release qualification
does not require live credentials. An unselected check is recorded as **NOT RUN**,
not a pass or waiver.

## Identity and documentation

Use lowercase **octet**. Keep [documentation](docs/README.md), examples,
commands, and generated references consistent with the code. Preserve required
third-party notices and accurate version labels on benchmark results.

## AI-assisted contributions

AI-generated issues and pull requests are welcome and will be considered. If
AI helped produce your contribution, please include a brief, genuine note from
you explaining what you observed, what you care about, or why the change
matters. Fully machine-generated requests without evidence of human review or
diligence may be treated as spam. I may not respond to messages that appear AI-
generated when I judge them to be unimportant or spam.

Please also include any relevant prompts used to reach the conclusions in an
issue or to generate a pull request. Include enough context to make clear what
you verified yourself, and redact secrets or private information. Requests for
the prompts behind a contribution are welcome too.

## Commits and pull requests

Use a short imperative commit subject, for example:

```text
fix: preserve tool output across reconnect
```

A pull request should state what changed, why it was necessary, the user or
developer impact, a defect's root cause, the exact checks that passed, and any
known limitation or compatibility effect.

Keep generated build artifacts, local reports, credentials, sessions,
`AGENTS.md`, and private research notes out of commits. The repository
`.gitignore` contains the expected local-only paths.

## Licensing

By contributing, you agree that your contribution is distributed under the
project's [MIT License](LICENSE). Preserve upstream notices when changing
vendored or derived code; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

## Roadmap

[View the project on GitHub](https://github.com/orgs/skaft-software/projects/5).
