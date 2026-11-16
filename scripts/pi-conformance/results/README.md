# Representative Pi evidence — 2026-11-16

[Machine-readable run](representative.json): **14 passed, 0 failed, 0 skipped**;
90 paired comparisons and 14 migration-safety checks pass. There are no extra
Octet writes to the isolated Pi agent directories. Nine unchanged Pi examples and
five synthetic callback fixtures execute against the real CLIs and a loopback
provider. The binary, harness, lockfile and factory digests are recorded.

Reference: published Pi **1.5.0**; reviewed source snapshot
`9c9ce8547e5a88cbe834a53e9a6d83a789d08e47`. Citations in each journey resolve under
that snapshot's root. Platform: Linux x86_64, Node 25.9.0. This is a working-tree
harness run at `f49b9d3e`, after all three runtime fixes; it is not a frozen release
or a Node 22-specific qualification. Paths in this snapshot use `<repo>` / `<pi>`;
normal run outputs retain exact paths. Two earlier full matrix runs also passed
14/14; the published run uses the final readiness/scoring changes.

| Surface | Result | Exercised behavior |
|---|---|---|
| Commands | pass | unchanged command catalog, selector, confirmation and source notification |
| Tools / dependencies | pass | unchanged hello and with-deps execution, schemas and tool-result wire content |
| Input / context hooks | pass | ordered input transforms, handled suppression, later input and context edits |
| Provider hooks | pass | chained actual payload/header edits and two real HTTP response observations |
| UI components / panels | pass | unchanged footer toggle, status lifecycle, above/below widgets and overlay input/dismissal |
| Editors / dialogs | pass | unchanged animated editor, erase/submit, input dialog and native prefill |
| Session operations | **fixed** | private append/name/leaf, cancellation, writable setup, new and return to original session, later request |
| Compaction | pass | before/after hooks, actual summary calls, durable entry, void return, completion and later request |
| Model switching | **fixed** | applied selection, previous/current model event and next request using new model |
| Terminal input hang | **deviation asserted** | native test retains typing and reports one disabled listener after the documented 1 s limit |

Fixes found by this corpus:

- `79ec98dd`: await `model_select` after an applied idle `pi.setModel` receipt.
- `85897712`: defer resource discovery while native writable setup owns replacement.
- `f49b9d3e`: prepare revisited history without resurrecting captured retired contexts.
- `95cd5769`: require successful observations, not equal failures, and include startup writes in fingerprints.

The terminal-input deviation is registered in
[`docs/pi-compat-deviations.md`](../../../docs/pi-compat-deviations.md#security-invariants-not-compatibility-refusals).
Pi `packages/tui/src/tui.ts:1095-1110` runs its listener chain synchronously without
a hang limit; `native_pi_terminal_input_hung_handler_fails_open_once_without_freezing_typing`
explicitly asserts Octet's bound and single diagnostic. This is not counted among
the 14 paired passes.

## Supplemental regression rows

These existing suites were executed, not reimplemented or counted as PTY journeys:

| Report | Coverage / evidence |
|---|---|
| #577 transcript-flood error | `issues.test.mjs` asserts once-per-extension/stage diagnostics across repeated callback faults, with later handlers still running; `issue-dispatchers.test.mjs` covers input/context-adjacent/provider dispatch faults. |
| #578 rendering/thinking | `native_pi_chrome_controls_existing_loader_title_and_tool_disclosure` covers working/hidden-thinking setters, one rendered loader, disclosure and reset; unchanged rainbow editor exercises styled rendering. Full original thinking-rendering setup is **NOT RUN**. |
| #587 slow/failed load | `handler-budget.test.mjs` covers stalled imports/factories and healthy siblings; `pi-mirror-cold-start.test.mjs` covers failed capture and dependency recovery; native startup tests cover failed-worker admission. |

Final supplemental results: adapter **1617 passed, 33 optional skips, 0 failed**;
`native_pi_` **76 passed, 0 failed**; coding-agent library **3344 passed, 4 ignored,
0 failed** with `RUST_MIN_STACK=16777216`; pinned long-session oracle **1 passed**.
The default-stack library run aborts with stack overflow in
`rpc_provider_reload_streams_only_from_the_replacement_process`; no claim is made
that the default-stack full library command passes.

The documented local `--extension` path was separately exercised with the unchanged
hello factory: **1 passed, 0 failed**. Initial friction: the generic driver sampled
native “Loading your Pi setup” chrome before readiness; it now waits for that
status to clear and requires an actual completed turn. Nine harness selftests pass.

OAuth/live providers, `provider_stream_event`, the full original rendering setup,
arbitrary package combinations and independent human reproduction remain **NOT RUN**.
The [existing deviation register](../../../docs/pi-compat-deviations.md) remains
authoritative for unexercised API gaps; these results do not certify an extension
or close every acceptance criterion of #663.
