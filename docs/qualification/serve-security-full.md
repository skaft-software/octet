# Serve cross-boundary security invariants — qualification record

**Issues:** [#341](https://github.com/skaft-software/octet/issues/341)
**Candidate revision:** `00e3ca3e561fc807491931712b93e534c952cf59` (shared dirty worktree; nothing committed)
**Command:** `cargo test --offline --locked --manifest-path extensions/octet-serve/Cargo.toml --profile ci-test`

This record replaces the "pending; source-only and uncommitted" cell for #341
with an executed invariant run. It separates what was **exercised here** from
what still **needs a live host**. Serve remains experimental; no release, beta,
installed-candidate or LAN claim is made.

## Observed run (exercised here)

```
running 3 tests
test internal_service_failures_are_sanitized_and_public_errors_reject_extra_fields ... ok
test public_command_dtos_fail_closed_at_unknown_and_size_boundaries ... ok
test resources_are_opaque_session_scoped_and_reopenable ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s
```

Whole independent workspace (`--profile ci-test`, offline, locked): 15 targets +
doc-tests, `test result: ok` for every target, **284 passed / 0 failed**, e.g.
lib `124 passed`, `runtime_status` `20`, `repository_context` `19`,
`project_fs` `18`, `test_results` `23`, `trusted_files` `11`. One lib test
(`pty::tests::shell_exit_settles_signal_ignoring_descendants`) failed once with
`terminal stream ended: channel lagged by 34` under full-suite load and passed
alone (`1 passed; 0 failed`) and on the recorded full re-run; it is a bounded
channel-lag flake in the PTY harness, unrelated to any file changed here.

## Fail-closed properties asserted

| Property | Evidence in `tests/security_full.rs` |
| --- | --- |
| Resource handles are opaque, not paths | handle is 64 lowercase hex chars; `../report.txt` as a handle returns `NotFound`; a `..\private\report.txt` source name is stored with the sanitized display name `report.txt` |
| Resources are session-scoped | another session's `content` returns `NotFound` for the same handle |
| Boundaries fail closed | a `../stdout` slot returns `InvalidBoundary`; reusing one `(session, tool-call, slot)` with different bytes returns `Storage` |
| Durability has one visibility seam | the binding survives reopen only after `persist_record` publishes the commit sidecar; a staged binding is transient by design |
| Unknown fields are rejected | `SessionCommandEnvelope` with an injected `privatePath` fails deserialization; `SanitizedError` with an injected `privateSource` fails deserialization |
| Bounded input | prompt text over `MAX_PROMPT_BYTES` fails validation; a missing `expected_actor_generation` fails validation |
| Internal failures stay opaque | `ServiceError::OwnerLost.into_public()` reports `ErrorCode::Internal` with the fixed message and never contains the variant name |
| Public text cannot carry controls | `SanitizedError::public(_, "bad\nmessage\t")` becomes `bad\u{fffd}message\u{fffd}`, still validates; `"safe\u{1b}[31m\u{202e}evil"` loses the escape and the bidi override |
| Client-supplied public text is validated | a deserialized `SanitizedError` whose message contains `\n` fails `ProtocolValidation` |

Transport-level checks that ran in the same pass include
`transport::tests::opaque_resource_transport_requires_auth_and_never_interprets_handles`,
`loopback_transport_rejects_cross_origin_and_oversized_requests`,
`attachment_transport_is_authenticated_bounded_and_path_free`,
`trust_revocation_fences_commands_and_retires_matching_actors`, and
`terminal_socket_authenticates_replays_and_stops_with_the_server`.

## Defects found and repaired by running this row

1. `tests/lifecycle_full.rs:232` did not compile: `EventPayload::UsageUpdated`
   takes a `UsageSnapshot`, not a `ContextUsage`
   (`error[E0308]: mismatched types`). Fixed to `UsageSnapshot::default()`.
2. `tests/security_full.rs` expected a staged, uncommitted resource binding to
   survive `ResourceStore::open`. The store documents the commit sidecar as the
   sole restart visibility boundary, so the fixture now calls `persist_record`
   before reopening.
3. `SanitizedError::public` sanitized with `multiline: true`, so a public error
   could carry `\n`/`\r`/`\t` and a client-supplied error could pass validation
   with embedded newlines. The outbound public error surface and its validator
   are now strictly single-line; every caller passes a fixed one-line sentence,
   so no message text changes. This is a fail-closed hardening of the public
   boundary, not a compatibility break in observed behavior.

## Boundary gate (observed)

`scripts/check-octet-serve-boundaries.sh` run in this shared dirty worktree
prints `octet serve crossed its optional extension/application boundary:` and
then ~258 KB of paths (`.dockerignore`, `apps/**/.build/**`, `docs/**`,
`extensions/octet-*`, other workers' scratch files). That output is the whole
shared worktree diff since base `63f73d65`, not a Serve signal.

The Serve-scoped run is therefore taken from an isolated candidate: a local clone
at `00e3ca3e` with only the three changed Serve files copied in, base `HEAD`:

```console
$ sh scripts/check-octet-serve-boundaries.sh "$(git rev-parse HEAD)"
octet serve changes stay within the optional extension/application boundary
rc=0
```

The diff it audited: `extensions/octet-serve/src/error.rs`,
`extensions/octet-serve/tests/security_full.rs`,
`extensions/octet-serve/tests/lifecycle_full.rs`.

## Needs a live host (UNRUN, not substituted)

- A real browser/WebSocket journey against a built candidate: one-use launch
  exchange, same-origin and cross-origin rejection, bounded snapshot/replay
  reconnect, duplicate delivery, detach/abort, ten concurrently visible sessions.
- OS-level process/PTY/file-descriptor/mailbox/output budget observation under
  load; the tests above assert protocol-level bounds, not measured OS ceilings.
- Installed-binary or packaged smoke, LAN/pairing (not supported by the source
  contract), provider credentials, and any publication action.
