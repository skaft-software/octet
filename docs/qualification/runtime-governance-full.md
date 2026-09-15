# Runtime governance: bounded qualification packet

Status: **authored and statically reviewed; compilation and execution UNRUN**.
The `full` filename is a qualification target, not a claim that every runtime or
release gate is closed. This packet retains the existing runtime manager and
adds no schema, protocol capability, or host lazy-dispatch implementation.

## Executable coverage

`crates/octet-agent/tests/runtime_governance_full.rs` contains Unix-only tests
using an actual local child process with a deliberately legacy API 0.2 handshake.
The fixture is not an API 0.3 authoring example or evidence of API 0.3 negotiation.
No fixture was launched during this read/edit-only phase.

| Test | Required observation when run |
| --- | --- |
| `one_hundred_lazy_entries_stay_visible_without_spawning` | Catalog/status/eager filtering retain 100 lazy entries with no child or charge. |
| `concurrent_shared_leases_keep_one_durable_owner` | Eight bindings share one child; binding release does not end workspace ownership. |
| `aggregate_budgets_fail_visibly_without_launching_the_rejected_child` | Process/FD/buffer exhaustion is typed, path-free, and does not spawn the rejected child. |
| `dead_one_shot_charge_is_reclaimed_before_the_next_admission` | Process shutdown must return **true**; the next admission reclaims the charge without a prior status/usage read, and settlement stops the new child. |
| `release_wakes_coalesced_and_startup_slot_waiters` | Binding release wakes both wait paths with `BindingClosed`, without launching the queued child. |
| `removed_and_reselected_catalog_cannot_commit_an_old_start_reservation` | Old startup invalidation cannot erase the new reservation or its charge. |
| `canceled_queued_reload_restores_the_old_lease_and_releases_transient_usage` | Dropping a reload waiting for a startup permit restores the old lease and accounting. |
| `canceled_start_owner_hands_its_reservation_to_a_shared_waiter` | Dropping a coalesced waiter leaves accounting intact; dropping its startup owner releases only that reservation and lets a surviving binding start once. Temporary binding/lease drops do not end durable shared ownership. |
| `canceled_candidate_reload_restores_waiting_leases_without_switching_generation` | During candidate initialization, released lease waiters receive `BindingClosed`; dropping reload restores Ready, the old generation and exact baseline usage. A later explicit reload succeeds. |
| `successful_reload_clears_prior_exhaustion_without_losing_the_charge` | Successful reload clears prior resource/failure status and leaves exactly one active charge. |
| `shutdown_wakes_an_inflight_handshake_and_is_terminal` | Manager shutdown invalidates in-flight startup and forbids new bindings. |

The two explicit future-drop tests supplement the retained regressions. They
exercise public manager/binding/process seams, not a duplicate or mock manager.
The process `shutdown() -> bool` assertion is intentionally not replaced by an
ignored return value: true means acknowledgment and child exit within the
configured per-stage timeouts.

## Reviewed implementation boundaries

- Admission rechecks catalog identity, terminal flags and binding ownership
  under the catalog/state/binding lock order before attaching or committing.
- `StartReservation` checks reservation identity before removing a startup;
  its drop releases its own charge and notifies coalesced waiters.
- `ReloadTransition` restores manager state before releasing the lifecycle gate
  on cancellation. A running but non-ready/draining process is parked rather
  than advertised as attachable. Transient reload usage has a separate guard.
- Leases expose process handles; the manager and session binding retain lifetime
  ownership. One-shot settlement and terminal host shutdown remain explicit.

These are static code observations, not stress-test or model-checking results.
The tests do not qualify cancellation after process drain/cutover, cancellation
of release/settlement/shutdown futures themselves, or native process-tree cleanup.
Cleanup callers must drive their teardown futures to completion. The fixture
start counters and manager accounting are not OS-level process/FD leak measures.

## Outstanding evidence

- Host lazy/one-shot dispatch and one-shot terminal/cancel-path settlement still
  need their existing product callers. This packet does not supply them.
- No schema expansion, provider/TLS/credential changes, lifecycle/effect replay
  changes, or presentation/accessibility changes were made.
- Native/live/release gates and the full142 roadmap remain open.

## Coordinator verification — all UNRUN

From the workspace root, with an approved build/test environment:

```sh
cargo check -p octet-agent --test runtime_governance_full
cargo test -p octet-agent --test runtime_governance_full
cargo test -p octet-agent --lib extension_runtime::tests
cargo fmt --all -- --check
```

Frozen diagnostics identified the old boolean-unwrap compiler error, alongside
unrelated assembled-tree errors outside this packet. No fresh compiler run has
confirmed this packet or the assembled workspace.
