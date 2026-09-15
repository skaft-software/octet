# Configuration diagnostics qualification

Issue #313 extracts diagnostics into the private module
`crates/octet-coding-agent/src/cli/config_diagnostics.rs`, with minimal parent
wiring in `crates/octet-coding-agent/src/cli.rs`. It preserves the existing
[configuration contract](../design/config-diagnostics.md): layer precedence and
persistence, bounded/secure reads, missing-file handling, accepted aliases and
ignored keys, deterministic diagnostics, source and location reporting,
warning/error routing, and `OCTET_STRICT_CONFIG` behavior. The duplicated
setting/key inventory is intentionally unchanged.

## Regression coverage

`crates/octet-coding-agent/tests/configuration_diagnostics_full.rs` exercises
the process boundary with an isolated home, workspace, session directory, and
cleared environment. Its six tests cover:

- exact default unknown-key warning text on stderr, not stdout;
- rejection caused by `OCTET_STRICT_CONFIG=true`, without executing the command;
- sorted diagnostics within each layer and global-before-project reporting,
  retaining typos from both sources even when their names overlap;
- exclusion of untrusted project diagnostics, including in strict mode;
- environment strictness overriding TOML, but not explicit CLI strictness; and
- aliases and ignored legacy settings remaining accepted in strict mode.

All subprocesses use offline `sessions list`, with no provider credentials.
Strict errors retain the existing outer `Error: ` prefix and non-terminal
control sanitization: embedded newlines appear as literal `<U+000A>` markers.
The tests assert that output, rather than changing production routing.

Four diagnostic unit tests moved with the implementation. Five additional
loader regressions cover missing files/parents, malformed TOML, invalid typed
values and UTF-8, the one-MiB read bound and regular-file requirement, symlink
rejection on Unix, and inline-table locations/ignored legacy keys. CLI parsing,
merging, precedence, trust, and persistence tests remain in `cli.rs`.

## Verification status

Candidate: base `df5a7e809715961b9344af6b52e43a6ca48f56b3` plus the working-tree
diff. This was a shared checkout; other owners changed unrelated paths while
checks ran. The [execution record](../swarm-audit/EXECUTION-config.md) contains
chronological exact commands, results, intermediate assertion corrections,
and candidate hashes. No commit was made.

Observed focused results:

- Before extraction: CLI unit baseline **80 passed**; strengthened subprocess
  baseline **6 passed**.
- After extraction: `cargo test -p octet-coding-agent --lib cli:: --profile ci-test --locked`
  — **85 passed** (including the moved tests and five added loader cases).
- `cargo test -p octet-coding-agent --test configuration_diagnostics_full --profile ci-test --locked`
  — **6 passed** after extraction.
- `cargo check -p octet-coding-agent --all-targets --locked` — **passed**, with
  existing warnings outside the changed configuration paths.
- File-local `rustfmt --edition 2021 --check` for `cli.rs`, the extracted module,
  and the subprocess test — **passed**. Scoped `git diff --check` — **passed**.
- An in-memory comparison against the base confirmed the extracted production
  block is identical after normalizing visibility and rustfmt layout; the
  parent differs only by the extraction, four moved tests, and imports.

Full-crate `cargo test -p octet-coding-agent --all-targets --profile ci-test --locked`
is **not qualified**. The pre-extraction run failed in a Pi-provider bootstrap
fixture and a concurrently edited interactive-queue test. The post-extraction
run had **1266 passed, 2 failed, 1 ignored**, failing in the same Pi-provider
bootstrap fixture and a new, unowned extension-hook fixture. The Pi-provider
failure also reproduces in isolation (`runtime startup Launch`, provider
`Parked`). These failures were not patched or skipped by this extraction.

Workspace-wide format/check/test/doc-test/lint and dependency/security checks
from `CONTRIBUTING.md` were **not run by this worker** and remain the integrating
parent's responsibility. Focused passes are not a workspace qualification.
