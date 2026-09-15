# Pi and migration execution evidence

Candidate: `df5a7e80` plus the working-tree diff (shared checkout; no commit,
branch, reset, clean, or global formatting). Local Darwin, Node `v26.7.0`,
Python `3.14.7`; existing `target/` only. No network/package downloads.

## #397 diagnosis and repair

Before editing, this exact command exited 101 (0 passed, 1 failed):

```sh
cargo test --locked -p octet-coding-agent --lib pi::tests::generated_link_negotiates_runtime_commands_with_the_real_octet_host -- --exact --nocapture
```

Failure reproduced `Closed("extension stdout closed")` at `pi.rs:3374`.
Copying only `bridge.mjs` into a temporary directory and executing it with Node
reproduced `ERR_MODULE_NOT_FOUND` for `semantic_ui.mjs`. The Rust host stages
only entrypoint bytes (`extension_process.rs::stage_entrypoint`); the bridge
used static relative helper imports, resolving beside the staged copy rather
than the generated package. This was not a fake-Pi negotiation defect.

The bridge now resolves helpers from host-owned `OCTET_EXTENSION_DIR`, retaining
module-relative imports for direct developer runs. An explicit missing helper
fails closed, without falling back. New subprocess regressions exercise staging,
strict identity, native commands, command execution, graceful shutdown and the
missing-helper boundary. Existing Rust assertions were not weakened.

After repair the exact command above exited 0: **1 passed**, including the
symlinked extension-root identity path, native command execution, shutdown and
stale-source refusal.

## Local conformance execution (not release conformance)

```sh
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py' -v
```

Initial run exited 1: 71 tests, 1 failure, 3 skipped. The full-gate *mock* test
expected a `/var/...` source root while production canonicalized to
`/private/var/...`. Canonicalized the fixture source root; retained the exact
78-entry source-order/path and clean-environment assertions, not fewer checks.

```sh
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py'
node --test extensions/octet-pi-compat/tests/test_semantic_ui.mjs extensions/octet-pi-compat/tests/test_editor_handoff.mjs
python3 extensions/octet-pi-compat/conformance.py --check --json
```

After repair: Python exited 0, **73 tests / 70 passed / 3 skipped**; Node exited
0, **17 passed**; ledger check exited 0 with `ok: true`, 118 public rows,
78 examples (9 directories), 33 TUI rows, 6 plan journeys and
`real_runtime: not_supplied`. Provider/UI cases use fake Pi, not real parity.

## #156 / #262 / #157 / #279 adapter and ingestion

Added `extensions/octet-import-pi/`: API 0.3 manifest, a fixed POSIX exec launcher
for `octet migrate adapter pi`, README and four process tests. It shares the
existing Rust typed implementation. The public CLI continues to select its own
current-binary adapter; it cannot load an arbitrary adapter path or package.
The package requires matching octet on PATH, is not catalog-published, and does
not replace host-owned ingestion with extension-owned writes.

```sh
cargo test --locked -p octet-coding-agent --test migration_import --test migration_host_full
```

The initial host-full run exited 101: 3 passed, 1 failed at
`adapter_rejection_is_explicit_and_leaves_destination_untouched`. The CLI's
`absolute_path` canonicalized the source before the adapter's no-follow check,
hiding a selected symlink. Production now retains the source leaf through
explicit, relative, environment and default selection. The unchanged rejection
assertion passes; added environment/default and relative regressions also pass.

A new deterministic rollback test uses an actual competing destination write
after planning. The production CAS fails at the second target, restores the
first, preserves the competing bytes and retains its backup without publishing
migration state. Its first run failed at a fixture setup assertion (the old
`gpt-4o` fixture did not select a unique current catalog model); selecting the
already-qualified `gpt-4o-mini` made the intended config-write/CAS journey run.
No production assertion or rollback condition was relaxed.

```sh
cargo test --locked -p octet-coding-agent --test pi_install --test migration_import --test migration_host_full
cargo test --locked -p octet-coding-agent --lib migration_import
cargo test --locked -p octet-coding-agent --lib pi::tests
cargo test --locked -p octet-migrate-types --test schemas
OCTET_PI_IMPORT_TEST_BINARY="$PWD/target/debug/octet" python3 -m unittest discover -s extensions/octet-import-pi/tests -p 'test_*.py' -v
```

Observed successful results: CLI integration **2 pi_install + 2 migration_import
+ 5 migration_host_full**; migration units **11 passed**; Pi units **22 reported
passed** (two real-Pi test bodies return early without the selected runtime,
so only 20 are executed local fixture bodies); schema suite **15 passed**;
new adapter package **4 passed, no skips**. Rust builds emitted existing
unused-import/dead-code warnings; no broad workspace check is claimed here.

The two `pi_install` fixtures invoke the production CLI against a pre-materialized
local package/dependency tree and fake Pi metadata. They verify inert linking,
rollback/source preservation, missing-dependency rejection and the scripts/network
consent gate. No npm installer, lifecycle script or upstream Pi package executed.

## Explicit remaining gates

This machine is Darwin and `command -v unshare` returned no executable.
`OCTET_PI_REAL_PACKAGE` was not supplied (the three real-Pi Python tests skip).
The exact reviewed Pi 0.84.4 coding-agent/TUI tarballs, matching installation,
clean `b79e4cc834970cca69daebffab7df1da7d1e52c4` checkout and prepared dependencies
are not supplied. Linux network isolation and those artifacts remain blockers;
no fake package, other Pi version, weakened assertion, arbitrary npm install or
network/script bypass substitutes for them. #258 and broader #397 installed
package/dependency release acceptance remain open.

```sh
python3 extensions/octet-pi-compat/conformance.py --full --network-isolated --json
```

Observed exit 1: `--full requires --coding-agent-tarball, --tui-tarball,
--pi-package, and --source-root`. This is a prerequisite refusal, not a full run.

## Final source checks and identity

```sh
rustfmt --edition 2021 --check crates/octet-coding-agent/tests/pi_install.rs crates/octet-coding-agent/tests/migration_import.rs crates/octet-coding-agent/tests/migration_host_full.rs
sh -n extensions/octet-import-pi/extension.sh
node --check extensions/octet-pi-compat/bridge.mjs
git diff --check -- extensions/octet-pi-compat extensions/octet-import-pi crates/octet-coding-agent/src/migrate/migration_import.rs crates/octet-coding-agent/tests/migration_import.rs crates/octet-coding-agent/tests/migration_host_full.rs crates/octet-coding-agent/tests/pi_install.rs docs/pi-migration.md docs/qualification/pi-runtime-current-candidate.md docs/swarm-audit/EXECUTION-pi.md
```

All four final checks exited 0. A prior formatting check caught one new test
array layout; a prior diff check caught Markdown hard-break trailing spaces.
Both were corrected locally. The new launcher mode is `0755`. Only the new
`pi_install.rs` was passed through rustfmt; no global formatting was run.

Source/test payload SHA-256:
`6b3cf3ea775a12f1a901c9584fac93714dcb397fc433e8267ae24428e53534a9`.
Computed in sorted path order by appending UTF-8 path, NUL, eight-byte big-endian
file length and raw bytes for exactly these paths (docs excluded):

```text
crates/octet-coding-agent/src/migrate/migration_import.rs
crates/octet-coding-agent/tests/migration_host_full.rs
crates/octet-coding-agent/tests/migration_import.rs
crates/octet-coding-agent/tests/pi_install.rs
extensions/octet-import-pi/extension.sh
extensions/octet-import-pi/extension.toml
extensions/octet-import-pi/tests/test_adapter.py
extensions/octet-pi-compat/bridge.mjs
extensions/octet-pi-compat/tests/helpers.py
extensions/octet-pi-compat/tests/test_bridge_protocol.py
extensions/octet-pi-compat/tests/test_conformance.py
```

The shared checkout also contains other owners' diffs. This payload identity and
`df5a7e80` baseline do not claim an immutable full-workspace or released binary.
