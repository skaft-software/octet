# PR #480 takeover: source progress, not completion

Work is confined to the candidate worktree. The implementation checkpoint was
committed locally as `5165e670d6e415039a5a85180562e9cdec478d97`. No push, install
or release has been performed. Protected installed Pi adapter hashes remain unchanged.

## Coverage artifacts

- [Pi 1.0.2 extension API ledger](pi-extension-api.md): the public API Octet
  replicates, one row per member, with what is done and what code remains.
- [Language SDK parity](../sdk/conformance/sdk-parity.md): public authoring
  surfaces, actual host reachability, tests and genuine missing implementations.

## Implemented and observed

| Area | Repair | Evidence and remaining scope |
|---|---|---|
| Session startup/history | Owner-filtered native snapshots before startup and ordinary callbacks; history-only updates, unavailable-state invalidation and reload fences | `native-mirror-r2`: 5 native tests passed. `native-clm-r3`: unchanged pinned CLM restores selected-branch settings before its first real command, excluding abandoned/foreign settings. Not full CLM/provider acceptance. |
| Awaited idle operations | Owned command execution releases fleet borrows to the sole App lifecycle consumer; stale-owner contributions are fenced | `native-lifecycle-r4`: 2 tests passed, including real create/fork/switch/reload and idle receipts. Missing Pi lifecycle facade signatures remain gaps. |
| Mouse construction | Admission states, bounded intent reconciliation and late-open cleanup | `native-termdraw-pty-r3/receipt.json`: unchanged termDRAW through the receipt-bound production binary and real PTY passes mouse drawing, exact saved composer text, coordinated exit and terminal restoration. Both source and original hashes were stable. |
| UI close/composer ordering | Publish native close ownership within the request drain; reject unfocused writes rather than ACK a no-op; resolve `ui.custom()` only after close ACK, rejecting failed restoration | Production-host same-drain regression and adapter close-ACK/refusal barriers pass. Native termDRAW red r1/r2 receipts exposed these two separate defects; r3 passes. |
| Fullscreen menu | Successful fullscreen admission no longer swallows later command errors | `native-menu-r3`: 2 tests passed, including rendered error visibility. |
| Provider/session projections | Inline images, representable signed visible reasoning, transient mixed-message provenance and durable `pi_details`, including null | Adapter tests pass. Opaque reasoning/replay and native mixed-entry/checkpoint semantics remain open. |
| Theme colors | Pinned OKLCH/OKHSL conversion in native and adapter paths | `native-resources-r2`: 24 tests passed, including pinned color vectors and actual resource loader flows. Full theme API parity is not inferred. |
| TS process SDK | Hooks, resources/operation slots/disposal, diagnostics, artifact/media output, allowlisted reverse calls and single-use append successor handling | `native-ts-r3`: 9 native tests passed; Node process suite 57 passed; both compiler projects passed. Bulk is descriptors/raw services only, not secure file-transfer helpers. |
| Compaction regression | Existing corrected runtime annotation and current native frontend | `native-compaction-r3`: 61 passed on a stable source snapshot. This does not close generic compaction dialogs without an active UI owner. |

Receipt names above resolve under `artifacts/takeover/` with `.receipt.json`.
Every receipt records its own exact source snapshot; later changes are not
retroactively qualified. Earlier failed/unstable/zero-test receipts are retained.

Full native regression: `full-frontend-r2` passed 2,288 with 2 ignored;
`full-agent-r2` passed 1,032 with 2 ignored. The separately invoked real Codemode
VM/production-transport gate passed (`native-codemode-r1`). SDK production-host
runs `full-python-r2`, `full-rust-r2`, `full-ts-r2` passed 43, 49 and 9 tests
respectively; Rust includes the actual C/C++ executables, not full ABI parity.
Python units ran 199 tests, OK with 1 skip (`python-unit-r4.receipt.json`).
The production binary builds successfully with four dead-code warnings.

Adapter-wide synthetic run: 253 passed, 19 skipped (`adapter-all-r2.log`). Explicit unchanged-original
run `originals/originals-r8`: 7 passed, 3 skipped (CLM, termDRAW, rainbow/footer
selected scenarios). These are not native durability or provider acceptance.
The opt-in native CLM test is ignored in the ordinary suite; explicitly running
it requires the reviewed original path and fails if prerequisites are absent.

## Still open — not silently excluded

- Full pinned Pi API/event/options/package/subpath/SDK/CLI/private-path semantics,
  including genuinely absent facade members identified in the inventory.
- Full CLM behavioral gates: native annotation/recall/checkpoint/provider flows,
  mixed/media/opaque continuity and preparation bounds.
- Generic compaction confirmation/input without an active UI owner.
- General Pi child authorization: the first-party product gate is preserved;
  protocol availability is not product admission.
- Full multilingual authoring parity: Rust commands/hooks/media/general reverse
  services, C/C++ richer ABI surfaces, TS secure bulk helpers, and the complete
  A–E/race/restart/platform matrix. See the language table, not aggregate counts.
- Installed-release qualification (installation remains outside this work),
  broader original native/PTY corpus acceptance beyond the observed scenarios,
  live-provider/endurance and cross-platform work.

This candidate is **not claimed complete, shippable, or fully Pi-compatible**.
