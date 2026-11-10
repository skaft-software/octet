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

Start with the credential-free smoke lane (Python 3.10+, Rust 1.88+):

```sh
cargo fetch --locked                  # once, if the local dependency cache is empty
python3 scripts/check-contributor.py  # python on Windows; --list shows commands
```

This runs locked/offline policy, edit, secure-file, ephemeral bulk and real
agent-dispatch approval regressions, the Python SDK and `git diff --check`.
It never installs dependencies, runs the Pi adapter or uses live credentials.
Cold compilation is not fast; the selected checks are small once built. It is
not full CI, durable Windows bulk qualification, a release-artifact test or an
attended desktop test. Known platform/runtime skips print a reason. Do not
relabel a failing test as a skip merely to obtain a green lane.

Then widen as appropriate. These are the same checks CI runs, minus the matrix:

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

## CI tiers

Every pull request runs the fast tier: `dependency-review`, `dependencies`,
`quality`, and `test (ubuntu-24.04)`. A PR labeled `ci:full`, a merge-group
candidate, a push to `main`, or a manual CI dispatch also runs the full tier.
The dispatch-only `release-candidate` job retains its narrower trigger.

| Lane | Tier / runner | What it proves |
| --- | --- | --- |
| `dependency-review` | Fast / ubuntu-24.04; PR only | no newly introduced vulnerable or disallowed dependency |
| `dependencies` | Fast / ubuntu-24.04 | `cargo audit` and `cargo deny` for the workspace |
| `quality` | Fast / ubuntu-24.04 | fmt, clippy, doc tests, offline model-metadata tests, release tooling, packaging determinism |
| `test (ubuntu-24.04)` | Fast / ubuntu-24.04 | workspace tests, then a `--no-default-features` check |
| `test (macos-15)` | Full / macos-15 | the same workspace tests and check on macOS |
| `windows (x86_64-pc-windows-gnu)` | Full / windows-2025 | native Windows build, binary smoke, terminal frontend, ConPTY, renderer, extension launch, Python SDK and computer-use |
| `first-party-extension-tests` | Full / ubuntu-24.04 | the shared Python SDK, release-catalog adapters, and source-only extensions in `extensions/source-catalog.txt` |
| `extension-api-v03` | Full / ubuntu-24.04 | generated API 0.3 artifacts and language conformance |
| `msrv` | Full / ubuntu-24.04 | the workspace still checks on Rust 1.88 (`rust-version` in `Cargo.toml`) |
| `smoke-install` | Full / ubuntu-24.04 | a real `cargo install`, version/help/host hello, then a loopback coding/cancellation/recovery journey against those installed executables |

`ci-required` aggregates all jobs and is intended to be the single required CI
status check. It permits only event-appropriate skips: full-tier jobs on a plain
PR, `dependency-review` outside PRs, and `release-candidate` outside dispatch.
A maintainer must configure hosted branch protection separately; workflow files
alone do not change it. The separate `pr-scope` workflow reports missing issue
references on PRs, but is not included in `ci-required`; with only that status
required, its failure is visible to reviewers rather than merge-blocking.

`quality` is the widest gate and the one most likely to fail for a reason that
has nothing to do with your change. It runs, in order: the offline models.dev
metadata tooling tests, shell and Python syntax checks on the release scripts, the extension bundle determinism
check (each bundle is built twice and compared byte for byte),
`scripts/test-binary-installer.sh`, the container-context build,
`python3 -m unittest discover -s scripts/tests`, fmt, clippy, workspace doc
tests, and finally
`cargo +1.90.0 package --workspace --exclude octet-coding-agent`.

### Advisory model-metadata freshness

`quality` runs offline fixture and snapshot-consistency tests, not a live
freshness check. `models-dev-freshness.yml` compares the checked-in projections
against live models.dev daily and on manual dispatch; it is advisory and can
fail when that service is stale or unavailable. It never rewrites the snapshots.
The receipt records the reviewed raw source digest, so unrelated upstream
changes alone do not invalidate a live projection comparison.

To refresh and review the names, prices, capabilities and receipt together:

```sh
python3 scripts/refresh-models-dev-pricing.py
python3 scripts/refresh-models-dev-pricing.py --check   # live comparison
```

For an offline comparison against a saved source, pass
`--source path/to/api.json --check`; this also checks the source receipt's exact
digest.

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
| `models-dev-freshness.yml` | daily or manual | advisory live models.dev snapshot comparison |
| `security.yml` | weekly, Mon 04:23 UTC, or manual | `cargo audit` and `cargo deny` for the workspace; a 90-second `session_record` fuzz run; an AddressSanitizer run of the `octet-agent` library; `cargo llvm-cov` coverage |
| `provider-acceptance.yml` | manual, needs a candidate SHA | live provider behaviour against real endpoints |
| `production-panic-audit.yml` | manual, needs a candidate SHA and confirmation | that the candidate adds no reachable production panic |

Provider acceptance and the panic audit may require credentials or real
hardware. They and the live models.dev freshness check are separate from the
required CI workflow, not hidden prerequisites for a green build.

## Release lanes

`release-octet.yml` and `release-serve.yml` (the extension-bundle release)
publish signed artifacts, and `homebrew-formula.yml` renders the formula. All of
them key off an **existing canonical `vX.Y.Z` tag** — the tag is both the trigger
and the pinned source of provenance, so there is no untagged path through them.

Before binary, installer or npm publication, `release-octet.yml` requires the
latest main-push or manually dispatched CI run for the **exact immutable source
SHA** to succeed, including every designated CI lane. PR merge-ref results,
missing or skipped lanes and older green runs after a newer failure cannot
qualify it. This gate does not replace separately required hosted branch checks.

Windows GNU CI does not qualify the MSVC release executable. The Windows release
job additionally gates on MSVC terminal, private-file, shell/extension and process
contracts. It packages twice, compares the ZIP digests, extracts the archive,
runs version/host smoke with an OS-only PATH and runs ConPTY scenarios against
the extracted executable before upload or signing. Policy/edit/temporary-bulk
and real agent-dispatch shell approval regressions gate both GNU CI and MSVC
release source. Neither proves durable Windows bulk retention/recovery. These are workflow requirements,
not a claim that the unpublished candidate has passed them.

Untagged candidates come from the `release-candidate` job in `ci.yml`, which
runs only when the workflow is dispatched by hand. It builds release binaries
for GNU/Linux x86-64 and for macOS on Intel and Apple silicon, packages each
with `scripts/package-octet-release.sh`, runs `--version`, `--help` and the
native-host hello and the loopback journey against the extracted executables,
then uploads `octet-rc-<commit>-<target>` for 14 days. Journey JSON includes the
exact executable SHA-256 digests. The
archives are unsigned and not notarized, and they are not releases. The Windows
test build comes from the `windows` lane on the same run.

### Exact-binary POSIX journey

```sh
python3 scripts/check-release-journey.py \
  --binary /path/to/extracted/octet --host-binary /path/to/extracted/octet-host \
  --expected-version X.Y.Z --report /tmp/octet-journey.json
```

This does not compile or install anything. It creates disposable HOME, workspace
and sessions, clears inherited credentials/proxy settings and uses only its own
127.0.0.1 provider fixture. The selected executables must pass version/host hello,
noninteractive provider setup, streamed tool use, read→edit CRLF/hash handling,
a nonzero shell result, SIGINT cancellation and resume, and SIGKILL interruption
after a settled mutation followed by resume without duplicate mutation. Read/edit
text and original/updated byte hashes must reach the provider as tool results,
not merely appear in the scripted assistant arguments. Children have bounded
captures/waits and owned process groups. The release and RC workflows
run it against binaries extracted from the actual archive before upload; ordinary
`smoke-install` runs it against the installed debug build. JSON reports list passed
boundaries and explicit non-claims; a source test cannot substitute for this run.

The harness deliberately refuses Windows: changing HOME/USERPROFILE alone is not
a reliable native known-folder/profile isolation mechanism. Do not point it at a
Windows executable through WSL. Windows uses the native source, process and
extracted-artifact ConPTY gates above; a full Windows coding/recovery journey
still requires an isolated OS account and named candidate. Live paid/local model
quality, attended desktop use and the ambiguous mutation-in-flight crash window
are separate acceptance checks, not inferred from this fixture.

## What a green run does not prove

Worth keeping in mind before reading too much into a green board:

- **Windows privacy is not a Unix mode-bit claim.** Native `secure_fs` tests
  assert owner-only DACL/reparse/hard-link and publication contracts. They do not
  prove every application credential path or every Windows environment; Unix
  `chmod` checks do not qualify Windows privacy.
- **The Windows lane is partial by design.** Most of the workspace suite has
  never run on Windows; it gates on the suites relevant to that platform.
- **A green `quality` does not prove current model metadata.** Its checks are
  offline; consult the separate advisory freshness workflow.
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
