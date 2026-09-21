# Pi 0.84.4 current-candidate qualification

> **Historical bridge qualification — not a release gate.** The Pi execution
> bridge is removed. These commands and receipts remain evidence for their
> recorded snapshot, not current installation instructions or RC qualification.
> Portable Pi inventory/import and native providers remain separate.

- **Issues:** #397, #258, #257, #259, #260, #272, #262, #156, #157, #279
- **Claim:** `dogfood_conformance` / local source-candidate fixtures only
- **Candidate:** `df5a7e80` plus the shared working-tree diff
- **Pi target:** `@earendil-works/pi-coding-agent@0.84.4` and `@earendil-works/pi-tui@0.84.4`, MIT
- **Pi source revision:** `b79e4cc834970cca69daebffab7df1da7d1e52c4` (`v0.84.4`)
- **Node minimum:** `22.19.0`; observed local Node `26.7.0`, Python `3.14.7`, Darwin
- **Bridge identity:** `0.7.0`

This record now includes actual local execution, superseding its earlier
source-inspection-only status. It does **not** claim release acceptance,
unchanged-source full conformance, live provider/OAuth parity or installed-release
qualification. Exact commands, initial failures and repairs are retained in
[the execution record](../swarm-audit/EXECUTION-pi.md).

## Observed local evidence

| Issue / boundary | Result and limits |
| --- | --- |
| #397 generated-link negotiation | Reproduced `Closed("extension stdout closed")`, then repaired staged-entrypoint helper resolution. The unchanged Rust host test passes native command negotiation/execution, shutdown, symlinked manifest identity and stale-source refusal. |
| #397 local installed package / rollback | Two real CLI fixtures pass: pre-materialized local dependency tree links without importing package/dependency code or running lifecycle scripts; rollback preserves source/dependency bytes. Missing dependencies and unapproved scripts fail before publication. No npm download/install or upstream-package acceptance is claimed. |
| #257 ordered aggregate | Fake-Pi aggregate tests pass order, shared globals/event bus, partial-load rejection, lifecycle, cancellation, restart and source/lock/runtime/trust binding. The actual Pi `ExtensionRunner` journey remains unrun. |
| #258 public ledger | `--check --json` passes: 118 public surfaces, 78 examples (9 directories), 33 TUI rows, 6 plan journeys, profile integrity and fixture links. The Python suite executes bounded declared behavior/safe divergences, not all unchanged upstream example behavior. Five baseline plan journeys remain deferred. |
| #259 / #260 UI and editor | Actual-bridge fake-Pi tests and 17 Node helper tests pass bounded semantic output, disposal/owner fencing, editor acknowledgements, suffix completions, cancellation and resize. No native/PTY accessibility or real Pi TUI parity is established. |
| #272 provider mode | API `0.3` fake-Pi tests pass catalog completion, bounded streaming, cancellation, safe hooks, replacement/unregister and rejection of credential/header/endpoint/OAuth authority. They do not establish real-provider/OAuth equivalence. |
| #262 typed transport | Shared migration schema suite passes 15 tests; migration unit/CLI/adapter package tests pass typed detection/import and host normalization. The public CLI still selects only the built-in adapter. |
| #156 adapter package | `extensions/octet-import-pi/` now contains a manifest, fixed exec launcher, README and 4 passing process tests. Rust validates its manifest. It delegates to the same version-matched typed implementation, not a new parser or arbitrary adapter-selection flag. Not published to a catalog. |
| #157 / #279 host ingestion | 11 migration unit tests, 5 host-full CLI tests and 2 migration-import integration tests pass. Coverage includes disabled outputs, source/credential safety, idempotence, conflict refusal/approval, backups, restore, CAS-failure rollback and preservation of a concurrent writer. Source symlink rejection was repaired at the CLI boundary without weakening adapter validation. |

The full Python Pi suite ran **73 tests: 70 passed, 3 explicitly skipped**.
The Rust `pi::tests` filter reports **22 passed**, but two real-runtime tests
return early without `OCTET_PI_REAL_PACKAGE`; they are **not runtime evidence**.
The ledger still records `real-runtime-aggregate` as **unrun**. Its fixture and
`real-runtime.json` are statically verified plans, not executed upstream journeys.

## Reproduce the local checks

```sh
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py'
node --test extensions/octet-pi-compat/tests/test_semantic_ui.mjs extensions/octet-pi-compat/tests/test_editor_handoff.mjs
python3 extensions/octet-pi-compat/conformance.py --check --json
cargo test --locked -p octet-coding-agent --lib pi::tests
cargo test --locked -p octet-coding-agent --lib migration_import
cargo test --locked -p octet-coding-agent --test pi_install --test migration_import --test migration_host_full
cargo test --locked -p octet-migrate-types --test schemas
OCTET_PI_IMPORT_TEST_BINARY="$PWD/target/debug/octet" python3 -m unittest discover -s extensions/octet-import-pi/tests -p 'test_*.py' -v
```

## Blocked integrity-verified full gate

With the exact local artifacts available, the separate real gate is:

```sh
python3 extensions/octet-pi-compat/conformance.py --full --network-isolated --json \
  --coding-agent-tarball /local/pi-coding-agent-0.84.4.tgz \
  --tui-tarball /local/pi-tui-0.84.4.tgz \
  --pi-package /local/unpacked/pi-coding-agent \
  --source-root /local/pi-source-at-b79e4cc834970cca69daebffab7df1da7d1e52c4
```

Prerequisites remain unsupplied in this lane:

1. Coding-agent and TUI tarballs matching the profile's exact SRI/name/version/MIT license.
2. A reviewed unpacked coding-agent installation whose resolved Pi TUI root matches the verified tarball.
3. A clean checkout at the exact revision, containing the complete example inventory and prepared dependency/lock/cache inputs.
4. Linux `unshare --net` capability. This host is Darwin and `unshare` is absent; `--network-isolated` is not a substitute for isolation.

An actual `--full --network-isolated --json` invocation without artifact arguments
exited 1, explicitly requiring all four artifact selectors. Nothing was loaded.
The gate performs no download, npm installation, source rewrite or credential
import. No fake fixture, newer Pi version, package directory alone, smoke test or
weakened count assertion substitutes for these prerequisites.

Native Windows, real provider/OAuth and installed upstream-package/dependency
acquisition/update acceptance, physical terminal behavior, endurance and release
approval remain separate gates. **No #258 closure or full #397 closure is claimed.**
