# Serve lifecycle, isolation, and reconnect — acceptance record

**Issues:** [#396](https://github.com/skaft-software/octet/issues/396)
**Candidate revision:** `00e3ca3e561fc807491931712b93e534c952cf59` (shared dirty worktree; nothing committed)
**Command:** `cargo test --offline --locked --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test`

The predecessor record
[`serve-lifecycle-current-candidate.md`](serve-lifecycle-current-candidate.md)
listed every fixture as **UNRUN**. This record contains the observed run for the
same fixtures, plus the defect that kept `lifecycle_full.rs` from compiling.
Serve remains experimental; this is a source-candidate acceptance record, not a
release, beta, installed-candidate, or live-host qualification.

## Observed run (exercised here)

```
running 3 tests
test dropping_one_attachment_leaves_owner_running_and_command_effect_once ... ok
test owner_loss_fences_actor_without_ack_cache_or_second_dispatch ... ok
test stale_generation_is_rejected_before_dispatch_and_replay_is_cursor_bound ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

Supporting lifecycle/isolation cells from the same full workspace run
(`284 passed / 0 failed` across 15 targets; see
[serve-security-full.md](serve-security-full.md)):

| Contract slice | Test | Result |
| --- | --- | --- |
| Ten sessions: isolation, attach, handle loss, duplicate cancel, bounded replay | `supervisor::tests::ten_sessions_isolate_attach_loss_duplicate_cancel_and_bounded_replay` | ok |
| Duplicate command reuses the exact ack and never dispatches twice | `supervisor::tests::duplicate_command_reuses_exact_ack_and_never_dispatches_twice` | ok |
| One actor per session across concurrent opens/reopens | `supervisor::tests::concurrent_open_of_one_session_constructs_one_driver`, `concurrent_reopen_after_owner_closes_installs_one_refreshed_actor` | ok |
| Per-session ownership and projection isolation | `supervisor::tests::two_sessions_are_owned_and_projected_in_isolation` | ok |
| Owner-loss fencing without releasing the fence | `supervisor::tests::quarantined_owner_wait_is_bounded_without_releasing_the_fence` | ok |
| Trust revocation fences commands and retires matching actors | `supervisor::tests::trust_revocation_fences_commands_and_retires_matching_actors` | ok |
| Public-API ten-core isolation fixture | `tests/lifecycle_current.rs` (`1 passed`) | ok |
| External public-API lifecycle fixture | `tests/lifecycle_full.rs` (`3 passed`) | ok |

## Fail-closed properties asserted

- **Stale generation is rejected before dispatch.**
  `stale_generation_is_rejected_before_dispatch_and_replay_is_cursor_bound`
  admits a command whose `expected_actor_generation` is `Some(2)` against
  generation 1 through a driver closure that `panic!`s, and observes
  `ErrorCode::StaleGeneration` with `current_generation: Some(1)` and an
  unchanged snapshot cursor (`sequence == 0`). No driver work happens.
- **Cursor-bounded replay.** With `JournalConfig { event_capacity: 2,
  byte_capacity: 64 * 1024 }` and three published events, a zero cursor returns
  `ReplayResponse::Gap` with `earliest_available.sequence == 2`,
  `latest_available.sequence == 3` and a snapshot at cursor 3; a cursor of
  `(generation 1, sequence 1)` replays exactly the two retained events
  (sequences 2 and 3) with `through.sequence == 3`. A journal gap is never
  silently skipped and the retained tail is never unbounded.
- **Owner fencing.** `owner_loss_fences_actor_without_ack_cache_or_second_dispatch`
  drives an owner loss, then asserts the rejected admission is not cached, the
  second admission is also rejected, dispatch count stays `1`, the snapshot has
  no partial items, and the driver settles exactly once (`shutdown_count == 1`).
- **Attachment semantics.** `dropping_one_attachment_leaves_owner_running_and_command_effect_once`
  shows a duplicate submit reuses the exact ack (`duplicate.ack == first.ack`),
  the first result is not cached but the duplicate is, one dispatch occurs, and
  dropping the extra attachment does not stop the owner
  (`shutdown_count == 0`).

## Defect found by running this row

`tests/lifecycle_full.rs:232` did not compile on `00e3ca3e`:

```
error[E0308]: mismatched types
   --> tests/lifecycle_full.rs:232:24
232 |                 usage: ContextUsage::default(),
    |                        ^^^^^^^^^^^^^^^^^^^^^^^ expected `UsageSnapshot`, found `ContextUsage`
error: could not compile `octet-serve-backend` (test "lifecycle_full") due to 1 previous error
```

`EventPayload::UsageUpdated` carries a `UsageSnapshot`
(`src/event.rs`, validated in `src/actor.rs`), so the fixture now constructs
`UsageSnapshot::default()`. The assertion set is unchanged. This is why the
earlier record could only report source inspection; the fixture had never been
built.

## Exercised here vs. needs a live host

**Exercised here:** in-process actor/core lifecycle through the public API and
the supervisor map — generation fencing, owner loss, duplicate idempotence,
attachment drop, journal gap/tail replay, and ten-session isolation.

**Needs a live host (UNRUN, not substituted):**

- Transport disconnect/reconnect in a real client (browser/WebSocket) including
  one-use launch exchange, resync after socket loss, and cross-origin rejection.
- Host process loss and takeover, plus recovery of a real Serve state directory
  after an OS-level crash (the store's commit-sidecar recovery is covered by unit
  fixtures only).
- Measured OS budgets under load (processes, file descriptors, mailbox depth,
  output bytes, dormant sessions); the fixtures assert protocol-level bounds.
- Approval/steer/follow-up/pause journeys, durable restart against an installed
  binary, and any release/publication step.

`pty::tests::shell_exit_settles_signal_ignoring_descendants` failed once with
`terminal stream ended: channel lagged by 34` during a full-suite run and passed
alone and on the recorded re-run; it is a PTY harness flake, not a lifecycle
finding.
