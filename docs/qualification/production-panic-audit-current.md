# Production panic audit qualification

**Status: source-only, uncollected candidate.** This record defines the production-only Clippy JSONL gate for issue #111. It does not claim Rust compilation, Clippy execution, passing tests, or a collected baseline. The committed baseline remains `uncollected` until a verifier reviews genuine Rust 1.86 output.

## Audit boundary

- Workspace production packages are the five members in the root `Cargo.toml`: `sexy-tui-rs`, `octet-ai`, `octet-agent`, `octet-coding-agent`, and `octet-migrate-types`. The excluded `extensions/octet-serve` manifest is not silently folded into this gate.
- The collection contract is `cargo clippy --workspace --all-features --lib --bins --no-deps --locked --message-format=json` with warning-level `clippy::expect_used` and `clippy::unwrap_used` enabled.
- Production target kinds are `bin`, `cdylib`, `custom-build`, `dylib`, `lib`, `proc-macro`, and `staticlib`. `test`, `example`, and `bench` targets are excluded. Mixed or unknown target kinds fail closed.
- Only warning diagnostics with one primary repository source span are findings. Fingerprints contain the lint, normalized target kind, target, repository-relative file, line, and column.
- Cargo framing is validated rather than scraped: records must be valid duplicate-free JSONL, use supported Cargo reasons, contain complete production compiler artifacts, and contain exactly one successful `build-finished` record. Diagnostic source paths must be real repository files.

## Reviewed baseline contract

`scripts/production-panic-baseline.json` uses schema version 1 and records the Rust `1.86`/Cargo JSONL scope. It is intentionally `uncollected` with no findings. A collected baseline must be generated from real `1.86.0` Cargo output and must contain exact current fingerprints, warning messages, normalized identities, a non-empty review justification, recognized evidence, and `reviewed: true` for every finding. New, stale, changed, duplicate, malformed, or unreviewed entries fail the gate; an uncollected baseline always fails it.

`scripts/test-production-panics.py` supplies deterministic in-memory Cargo-shaped fixtures for production matches, test-only exclusions, malformed framing, target classification, span/level validation, duplicate fingerprints, reviewed metadata, and baseline scope. It also checks the dispatch workflow and the committed uncollected state without collecting Rust evidence.

## Manual gated workflow

`.github/workflows/production-panic-audit.yml` is a separately gated `workflow_dispatch` candidate. It requires an exact full source SHA, an explicit confirmation token, and the protected `production-panic-audit` environment. Checkout, Rust toolchain setup, and artifact upload actions are pinned to immutable commits. The run binds `GITHUB_SHA` and the checked-out commit to the selected source SHA, writes stdout JSONL and stderr separately, validates the artifact with `scripts/check-production-panics.py`, and uploads the raw evidence even when collection or validation fails.

The workflow is deliberately not attached to `push`, `pull_request`, or a schedule. This prevents an unreviewed baseline from becoming an automatic merge or release gate before a verifier has collected and reviewed real evidence.

## Verification record

No command, test, build, Clippy run, formatter, installation, or verification was performed in this lane. The following remain **UNRUN** for the centralized verifier:

```text
python3 scripts/test-production-panics.py
python3 -m py_compile scripts/check-production-panics.py scripts/test-production-panics.py
python3 scripts/check-production-panics.py --diagnostics <real-rust-1.86-cargo-jsonl> --baseline scripts/production-panic-baseline.json
```

The verifier must run the manual workflow or an equivalent Rust 1.86.0 collection, inspect the raw JSONL and the resulting diff, review each finding, and only then replace `status: uncollected` with a collected baseline. This agent has not made that status transition.
