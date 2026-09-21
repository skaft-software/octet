# Pi UI handoff candidate qualification

> **Historical bridge qualification — not a release gate.** The Pi execution
> bridge is removed. These commands and receipts remain evidence for their
> recorded snapshot, not current installation instructions or RC qualification.
> Portable Pi inventory/import and native providers remain separate.

## Scope

This is a bounded **API 0.2** bridge implementation and fixture record, not
completion of #257, #397, #258 or the full Pi roadmap. Pi remains pinned to
`0.84.4`, revision `b79e4cc834970cca69daebffab7df1da7d1e52c4`.
The profile, canonical ledger and canonical API 0.3 tables are unchanged.
See the [bridge contract](../reference/pi-compat/README.md#optional-api-02-ui-handoff)
and [baseline ledger](../reference/pi-compat/COMPATIBILITY.md).

## Implemented boundary

- Explicit legacy feature negotiation enables bounded semantic text, editor
  acknowledgement/snapshots and host-registered suffix autocomplete. Both the
  bridge and its private helper reject API 0.3 UI admission; the helper no longer
  labels legacy method names as canonical API 0.3 schema/capability authority.
- Header/footer projection also requires the declared manifest surface. Terminal
  resize observations are consumed; raw input hooks and replacement editors are
  still rejected rather than emulated.
- Local UI-owner identity fences queued writes and component callbacks. Host
  session/process generation is not fabricated or sent by the adapter; real
  host ownership remains authoritative and needs separate Rust-host qualification.
- Settlement/replacement/shutdown dispose resources and abort editor waits before
  joining the ordered lifecycle lane. Late editor acknowledgements cannot replace
  newer host observations. Non-cooperative cancelled suggestions retain their
  bounded slots until settlement.
- Autocomplete registration/removal uses UI-owner lifetime, not the cancelled
  request that happened to install the provider.
- Text normalization preserves line-break spaces, removes ANSI controls without
  inventing spaces, and consumes Unicode code points for ASCII projection.
- `publish_plan_for_api` includes `semantic_ui.mjs` and `editor_handoff.mjs` as
  private payloads before the manifest publication boundary. The existing
  publication transaction owns rollback; present helpers must be regular files,
  while links generated before helper bundling remain removable. No checked-in
  manifest, dependency, version, generated protocol output or public target was
  changed.

## Observed fixture evidence — 2026-09-15

Coordinator checks used fresh immutable Mac snapshots, isolated HOME/TMP,
Python 3.14.7 and Node 26.7.0. No real Pi/native backend was selected.

| Check | Observed result |
| --- | --- |
| Complete Pi Python suite | 71 tests: 68 passed, 3 real-Pi selections skipped |
| Semantic UI Node suite | 13 passed |
| Editor handoff Node suite | 4 passed |
| Admission negative control | The old helper accepted API 0.3 legacy UI admission; the new rejection regression failed with `Missing expected exception (SemanticUiAdmissionError)` |
| Request-signal negative control | New actual-bridge regression failed with old request-scoped registration: expected 2 host registration messages, observed 1 |
| Unicode negative control | Old production sanitizer failed both the existing ASCII assertion and the new Unicode/ANSI/bounds regression |

The fake loader now executes the selected UI fixture's actual command
registration. Tests assert real bridge JSON-RPC requests, host replies,
contribution tombstones, cancellation, session-owner replacement and old-callback
rejection. They are not helper-only assertions or unchanged-Pi parity evidence.
The baseline public-surface rejection fixtures remain in the complete suite.

Run the non-Rust checks from the repository root:

```sh
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py' -v
node --test extensions/octet-pi-compat/tests/test_semantic_ui.mjs
node --test extensions/octet-pi-compat/tests/test_editor_handoff.mjs
```

Exact source hashes, logs and atomic receipts are preserved in the coordinator
run under `pi-ui-legacy-admission-bounded`, `pi-ui-admission-negative-bounded`,
`pi-ui-payload-review-bounded`, `pi-ui-lifecycle-editor-bounded`,
`pi-ui-signal-negative-bounded` and `pi-unicode-{repaired,negative-control}-bounded`.
The payload-review snapshot added an actual-bridge noncooperative completion
settlement/late-response regression. Those immutable inputs are not silently
replaced by subsequent source edits or a broader acceptance claim.

## Unrun and outstanding gates

- Rust formatting, compilation and the module-owned generated-link helper payload
  assertions are **UNRUN**, blocked by the reserved Temper Rust slot/access.
- Re-run `cargo test --locked -p octet-coding-agent --lib pi::tests` and the
  workspace/all-target and host protocol/ownership checks after a separately
  admitted immutable transfer. No alternate Rust producer is authorized.
- The real generated-link/frontend round trip, native/PTY accessibility,
  unchanged Pi examples/TUI, lifecycle/reload integration, full provider parity,
  flags/shortcuts, root messages and durable session behavior remain independent
  implementation/qualification work. Canonical API 0.3 UI integration remains
  unimplemented here; legacy fixtures do not establish it.
- This record is not release acceptance, publication, or full-roadmap acceptance.
