# Final octet-ai review

Status: source edits settled; newest regressions await the parent build runner.
Worker: `final-providers`. Shared worktree; no Git mutations, generated catalog
edits, live provider requests, or credential-policy changes.

## Repairs

- **Host-owned inference attempts:** `src/responses_ws.rs::run_generation` never
  reconnects and resends `response.create`. A pre-output disconnect/heartbeat
  failure preserves its lifecycle prefix and returns `ResponseNotResumable`
  with zero local replacement attempts. Silence does not prove nonacceptance.
  Connection-limit and stale-continuation provider errors are forwarded intact
  after fencing the socket, so the host retry classifier and cumulative budgets
  see the physical attempt. Subsequent explicit host requests may use HTTP.
- Explicitly stored Responses generations retain bounded **GET-only cursor
  retrieval**, not new inference. Ordinary Codex still uses `store: false`.
- `src/protocol/openai_responses.rs`: non-array computer safety checks fail
  closed; changed terminal computer actions/checks fail before `ToolCallEnd`
  instead of silently discarding late safety information. Identical terminal
  payloads and terminal-only actions retain their existing regression coverage.
  No computer executor or authorization grant was introduced.
- Preserved the inherited Anthropic refusal implementation and regressions;
  repaired the test's temporary-borrow lifetime error.

## Regression evidence to run

- `responses_ws::tests`: pre-output interruption and no-event acceptance never
  replay inference; stale continuation is fenced/forwarded; existing visible
  output and stored-cursor recovery cases remain.
- `tests/client_stream.rs`: connection-limit classification plus subsequent HTTP
  fallback; terminal socket/heartbeat failures assert exactly one inference
  request; a fresh socket that would succeed must never receive a hidden replay.
- `protocol::openai_responses::tests::computer_safety_checks_must_be_an_array` and
  `terminal_computer_payload_changes_fail_before_executable_completion`, plus
  existing computer byte-boundary, missing-action and identical-payload cases.
- Parent must also rerun the agent's connection-limit, attempt-envelope and
  unknown-usage accounting regressions. AI-only success cannot qualify them.

## Honest parity boundaries

- Grammar/Lark/regex tools remain **incomplete**. Chat and Responses emit custom
  tool declarations, but custom-call decoding and canonical history/result
  replay do not form a complete contract. Request-shape tests are not end-to-end
  grammar qualification. No grammar expansion was attempted in this pass.
- Strict JSON-schema conversion and fail-closed policy helpers exist, with
  codec/cross-protocol tests; this is distinct from the missing custom-tool
  lifecycle. Native deferred tool loading remains explicitly rejected.
- `assistant_frame.rs` supplies serde frames, encoder and partial-message
  reducer with prefix/ordering tests. These primitives do not by themselves
  prove product-level durable partial republish or completed-turn settlement.
- Faux/deferred pending-ready-failed-cancelled generation lifecycle and deferred
  stop reason are absent from the inspected public client/types. Image-generation
  API, adapter and generated image-model inventory are likewise not implemented.
  Normal media output is not an image-generation API.

## Verification and limitations

Read the AI design, provider/codec parity ledgers, provider guide and relevant
source/tests; inspected the final scoped diff. `git diff --check` passed for the
owned paths. No cargo/rustc/build/Swift invocation was made during recovery.

Parent artifact `/tmp/octet-final/tests-renderer-ai.log` ends with
`RUNNER_EXIT=0 elapsed=116.4s`; it predates the newest websocket/computer edits and
is historical evidence only. `/tmp/octet-final/test-workspace-lowdisk.log` was
still at compilation when inspected. No current behavioral pass is claimed.

Earlier pre-recovery no-run success is not current qualification. Live-provider,
hardware, real billing and all-feature behavioral verification remain outside
this worker's execution evidence.

### Parent execution and final assertion correction

Inspected `/tmp/octet-final/ai-final.log`: all **372 library tests passed**,
including the new computer-call and websocket regressions. `client_stream` had
**37 passed / 1 failed**; all other listed integration targets passed. The sole
failure was the heartbeat/explicit-fallback test's final stale `1 + 3 + 1`
physical-request expectation, after its earlier one-send/no-retry assertions had
already passed. Observed count was 2, the intended contract.

Corrected that assertion to exactly **one initial WebSocket request plus one
explicit host-requested HTTP replacement**, checking both transports in order.
No production code changed in this follow-up. Scoped diff check passed; parent
must rerun `responses_websocket_heartbeat_failure_is_terminal_and_next_request_falls_back`.
No clean full-suite pass is claimed until that rerun. Sources are settled.
