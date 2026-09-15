# Host ownership-full qualification

**Issue:** #318  
**Status:** source-only refactor; coordinator verification pending

This record qualifies the host boundary as an ownership change, not as a claim of
live-provider, installed-binary, or end-to-end runtime qualification. The public
`octet_sdk::host::run_stdio()` entry point and the v1 NDJSON contract remain the
external boundary.

## Ownership map

| Boundary | Owner | Responsibility |
| --- | --- | --- |
| Protocol DTOs | `src/host/protocol.rs` | Versioned request shapes, strict JSON decoding, identifiers, and protocol bounds. |
| Framing | `src/host/framing.rs` | Bounded NDJSON line reads and bounded outbound serialization. |
| Transport | `src/host/transport.rs` | Event envelopes, sequence numbers, terminal-event rules, stdout flushing, and process cleanup. |
| Routing | `src/host/routing.rs` | `hello` and `models` responses and catalog discovery. |
| Policy | `src/host/policy.rs` | Workspace/session configuration, controlled effects, inline routes, credentials, headers, and request admission. |
| Orchestration | `src/host/run.rs` | Bootstrap, session setup, prompt execution, cancellation, extension settlement, and terminal results. |
| Event translation | `src/host/events.rs` | Agent-event mapping, bounded protocol text, media metadata, tool progress, and run summaries. |
| Media | `src/host/media.rs` | Workspace-confined, size-bounded, typed image/audio loading in request order. |
| Session authority | `src/host/sessions.rs` | Session directory confinement, regular-file admission, replay/open, creation, and history seeding. |
| Facade | `src/host.rs` | Stdio lifecycle, shutdown selection, decoded-command dispatch, and public re-exports only. |

The child modules are private. `host.rs` remains the only process-facing facade;
transport and orchestration cannot be bypassed by callers through new public
module APIs.

## Preserved boundary properties

- `run_stdio()` remains public at the existing SDK path and the `octet-host`
  binary continues to call it.
- Protocol version and frame-size constants remain re-exported from the host
  facade.
- Unknown top-level and nested request fields remain rejected before command
  dispatch; duplicate JSON fields remain rejected by strict decoding.
- Oversized input is consumed through its newline before the next frame is
  read. Oversized output becomes a bounded protocol error rather than an
  unbounded stdout write.
- Host v1 remains `Controlled`; model-controlled external paths and experimental
  remote MCP cannot be enabled by a request.
- Inline HTTP(S) routes, credentials, custom headers, media modality, and audio
  format admission remain policy-owned and bounded.
- Session paths are canonicalized and confined before replay or append; final
  symlinks and non-regular files are rejected.
- Shutdown still aborts the active run, terminates registered process groups, and
  avoids emitting an unterminated terminal sequence after signal handling.

## Static and module-local evidence

`crates/octet-coding-agent/tests/host_ownership_full.rs` checks that the facade
contains the nine private child modules, retains lifecycle/dispatch entry points,
and does not regain moved definitions or orchestration imports. It also checks
that each named boundary definition occurs in exactly one child source file.

Private module tests are colocated with their owners:

- `src/host/protocol/tests.rs`
- `src/host/framing/tests.rs`
- `src/host/policy/tests.rs`
- `src/host/media/tests.rs`
- `src/host/sessions/tests.rs`
- `src/host/events/tests.rs`

The existing `tests/host_protocol.rs` remains the process-boundary fixture for
NDJSON behavior.

## Run/event borrow-boundary correction (night2-host-fix)

Saved assembled-source diagnostics reported E0502: `app.agent.prompt(input)`
returns a live `Run` that mutably borrows the agent, while the extracted event
translator took `&App`. The translator needs only the endpoint and model IDs for
`HostRunOutcome::from_finish_reason` diagnostics.

`events::translate` now accepts `endpoint_id: &str` and `model_id: &str`.
Its sole production caller in `run.rs` passes the two disjoint `app.model` fields
while the original `Run` stays alive. No agent clone/replacement or early run
drop is introduced. Prompt execution, cancellation, event handling, usage,
effect observations, terminal-head provenance, and settlement order are unchanged.

Focused event tests cover completed, aborted, turn-limited, and failed outcomes,
terminal heads, retained run summaries, and forwarding both diagnostic IDs
without constructing an `App`. The ownership test also guards the narrow
translator inputs and translation-before-drop-before-settlement source order.
These are test definitions and source evidence, not executed qualification.
Lazy extension activation, CLI integration, and other host features remain
outside this correction; it does not close the full142 roadmap or native,
live-provider, or release gates.

## Verification record

No Cargo, rustc, test, build, formatter, or live/native command was run in the
coding-agent ownership lane or this correction packet. Source inspection and
edit-diff review were completed; execution is pending the coordinator's
verification slot. The saved diagnostics also contain out-of-scope failures;
this packet does not claim the assembled candidate compiles.

The following commands are explicitly **UNRUN** here. The coordinator should
run and record them independently against the integrated candidate:

```text
cargo check --locked -p octet-coding-agent --all-targets
cargo test --locked -p octet-coding-agent --lib host::events::tests
cargo test --locked -p octet-coding-agent --test host_ownership_full
cargo test --locked -p octet-coding-agent --test host_protocol
cargo test --locked -p octet-coding-agent --lib
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

A passing static or module-local test must not be reported as live-provider,
installed-host, security-audit, or endurance evidence. Any compile or test
failure should be recorded against the integrated candidate.
