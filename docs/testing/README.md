# Testing lanes

This is the map of how octet is tested: which lane proves what, how to run it
locally, and what a green lane still does not prove. It is written for
contributors choosing where a change should be verified.

Every command below is quoted from CI configuration
(`.github/workflows/ci.yml`, `security.yml`, `provider-acceptance.yml`,
`production-panic-audit.yml`) or from a script in `scripts/`. This page maps
lanes; it does not record results. When you report a pass, name the exact
command that produced it.

## The local loop

Run these before pushing. They are the same checks CI runs, minus the matrix.

```sh
cargo check --workspace --all-targets --all-features --locked
cargo test -p <crate> --locked          # narrow first, then widen
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
```

Start with the narrowest test that reproduces the behaviour, then widen to the
workspace. Format only the files you changed; never run a repository-wide
formatter without `--check`.

The `octet-computer-use` Python suite runs offline and touches no desktop:

```sh
cd extensions/octet-computer-use
PYTHONPATH='.' python3 -m unittest discover -s tests -t .
```

## CI lanes on every pull request

| Lane | Runner | What it proves |
| --- | --- | --- |
| `dependencies` | ubuntu-24.04 | `cargo audit` and `cargo deny` for the workspace and `extensions/octet-serve` |
| `quality` | ubuntu-24.04 | fmt, clippy, doc tests, model-metadata gate, release tooling, packaging determinism |
| `test (ubuntu-24.04)`, `test (macos-15)` | matrix | `cargo test --workspace --all-targets --all-features --profile ci-test`, then a `--no-default-features` check |
| `windows (x86_64-pc-windows-gnu)` | windows-2025 | native Windows build, binary smoke, terminal frontend, ConPTY, renderer, extension launch, Python SDK and computer-use |
| `first-party-extension-tests` | ubuntu-24.04 | the shared Python SDK and every official extension adapter in `extensions/release-catalog.txt` |
| `extension-api-v03` | ubuntu-24.04 | generated API 0.3 artifacts and language conformance |
| `msrv` | ubuntu-24.04 | the workspace still checks on Rust 1.88 (`rust-version` in `Cargo.toml`) |
| `web` | ubuntu-24.04 | `apps/web` lint, typecheck, tests, fonts, build, and `npm audit --audit-level=high` |
| `smoke-install` | ubuntu-24.04 | a real `cargo install` of the CLI, then `--version`, `--help`, and the installed native host |
| `dependency-review` | ubuntu-24.04 | no newly introduced vulnerable or disallowed dependency |

`quality` is the widest gate and the one most likely to fail for a reason that
has nothing to do with your change. It runs, in order: the offline models.dev
metadata tooling tests, the pre-release metadata freshness gate, shell and
Python syntax checks on the release scripts, the extension bundle determinism
check (each bundle is built twice and compared byte for byte),
`scripts/test-binary-installer.sh`, the container-context build,
`python3 -m unittest discover -s scripts/tests`, fmt, clippy, workspace doc
tests, the `octet-serve` fmt/clippy/test trio, and finally
`cargo +1.90.0 package --workspace --exclude octet-coding-agent`.

### The pre-release model-metadata gate

`quality` runs `scripts/check-release-model-metadata.py` on every pull request.
It skips while the workspace version is unchanged, and starts comparing against
the live models.dev snapshot the moment `Cargo.toml` moves to a new version — so
it goes from silent to blocking exactly during a release bump.

Regenerate with the generator, never by hand:

```sh
python3 scripts/refresh-models-dev-pricing.py
python3 scripts/refresh-models-dev-pricing.py --check   # verify determinism
```

This lane depends on a third-party service, so it is the one CI check that can
fail for reasons outside the repository. The checked-in receipt is a hash of the
whole upstream `api.json`, while the three data files are projections of only
the fields octet consumes; an upstream edit that changes nothing octet reads
still invalidates the receipt. Treat a receipt-only failure as an upstream
churn signal and re-run the generator, not as a defect in your change.

## Platform lanes that cannot be reproduced locally

**The PTY lane** runs the compiled binary under a controlled pseudo-terminal and
checks bytes and emulated terminal state instead of relying on a human
terminal. See [startup-frame-pty.md](startup-frame-pty.md).

**The Windows lane** builds real `octet.exe` and `octet-host.exe` with
MinGW-w64 and exercises the native terminal frontend, ConPTY, private file
access, extension launch, and Windows process control. Several parts of the
computer-use suite assert POSIX-only contracts — `os.killpg`, `st_mode`
permission bits — and are skipped on Windows rather than weakened. Windows
privacy and job-object shutdown are therefore not asserted there; changes to
`extensions/octet-computer-use/octet_computer_use/jev_use.py` need a real
Windows host to verify properly.

## Deep lanes, off the pull-request path

These do not run on every push. Trigger them deliberately.

| Workflow | Trigger | What it proves |
| --- | --- | --- |
| `security.yml` | weekly, Mon 04:23 UTC, or manual | `cargo audit` and `cargo deny` for the workspace and `octet-serve`; a 90-second `session_record` fuzz run; an AddressSanitizer run of the `octet-agent` library; `cargo llvm-cov` coverage |
| `provider-acceptance.yml` | manual, needs a candidate SHA | live provider behaviour against real endpoints |
| `production-panic-audit.yml` | manual, needs a candidate SHA and confirmation | that the candidate adds no reachable production panic |

Provider acceptance and the panic audit are the only lanes that talk to live
services or drive real hardware. They are excluded from ordinary CI on purpose:
they are slow, they need credentials, and they must never be a hidden
prerequisite for a green build.

## Release lanes

`release-octet.yml` and `release-serve.yml` publish signed artifacts, and
`homebrew-formula.yml` renders the formula. All of them key off an **existing
canonical `vX.Y.Z` tag** — the tag is both the trigger and the pinned source of
provenance, so there is no untagged path through them. Building an untagged
candidate means using the ordinary CI lanes above, not these.

## What a green run does not prove

Worth keeping in mind before reading too much into a green board:

- **No lane proves Windows privacy.** `chmod` is not a privacy boundary on
  Windows; the runtime applies an owner-only DACL instead, and that path is
  currently asserted only indirectly.
- **The Windows lane is partial by design.** Most of the workspace suite has
  never run on Windows; it gates on the suites relevant to that platform.
- **A green `quality` does not mean the model metadata is current forever.** It
  means it matched the live snapshot at that moment.
- **Coverage is measured, not enforced.** `cargo llvm-cov` uploads a report; no
  threshold gates the build.
- **Live provider behaviour is unverified** until provider acceptance is run
  against a specific candidate.

## Invariants for new lanes

- No network-dependent build steps. Checked-in metadata is the deterministic
  source of truth.
- Do not weaken workspace trust, tool policy, no-follow path handling,
  cancellation, persistence, or redaction to make a test pass.
- Skip a test on a platform where its contract cannot hold; do not weaken it
  where it can. Say in the skip reason which mechanism that platform uses
  instead.
- Record qualified, observed results with the exact command. A source-only check
  is not behavioural evidence.
- Generated artifacts are regenerated from their generator, never hand-edited.
