# #350 current-candidate recovery qualification

**Status: bounded deterministic qualification only.** This record does not claim
live-provider availability, Codex parity, remote billing reconciliation, or
endurance.

## Candidate and boundary

- Source baseline: `73a80ada0c85b66e443c320703f03fa4924a1ae2`.
- Qualification fixture: `crates/octet-agent/tests/recovery_current.rs`.
- Faults are local wiremock HTTP/SSE, loopback HTTP, and loopback WebSocket
  fixtures. No provider credentials or production endpoints are used.
- The fixture exercises the released source contract: Codex-qualified
  interrupted inference may replace an uncommitted stream; committed tool
  effects remain durable; a retired Responses WebSocket is not reused; and
  pre-send credential/network outages wait under the host-selected bound.

The test is deliberately not a claim that a provider accepted or billed any
particular generation. A body or provider error after dispatch remains usage
uncertain, and the tests assert that uncertainty where applicable.

## Deterministic acceptance matrix

| Acceptance cell | Local evidence | Qualification boundary |
| --- | --- | --- |
| Interrupted Codex inference, automatic continuation, durable assistant/tool result, no duplicate effect | `current_candidate_interrupted_codex_stream_discards_provisional_tool`; existing `qualified_codex_interrupted_text_reasoning_and_provisional_tool_replace_before_commit` and `qualified_codex_recovery_preserves_committed_mutations_without_replaying_effects` in `agent_run.rs` | Local Responses SSE only; no live interruption campaign. |
| Pre-send outage, bounded delay, cancellation, and no false usage uncertainty | `current_candidate_network_wait_is_bounded_and_cancellable`; existing `qualified_opening_transport_has_thirty_attempt_envelope_without_indefinite_waits` | Dynamic credential failure is the authorized local outage fixture. |
| Responses WebSocket retirement, HTTP fallback, failed fallback, and stale-socket avoidance | `current_candidate_retired_websocket_uses_http_fallback_without_stale_reuse`; existing `qualified_codex_ws_http_cumulative_twelve_attempt_envelope` | The loopback server emits a connection-limit terminal, then one HTTP 503, then success. |
| Provider delay / retry hint | Existing `qualified_codex_postgeneration_rate_limit_honors_retry_hint` | Deterministic Retry-After parsing; not provider latency. |
| Permanent provider failures and finite replacement budgets | Existing `qualified_codex_permanent_failures_do_not_replace`, `qualified_codex_exhaustion_is_finite_and_reports_unknown_usage_and_replacements`, and `qualified_http_503_hard_budget_and_permanent_rejections_never_spend_admission_budget` | Local scripted status/error bodies only. |
| Speculative stream/tool settlement and malformed stream safety | Existing `qualified_provider_stream_json_recovery_never_dispatches_provisional_tools`, `qualified_unknown_responses_terminals_replace_partial_without_tool_replay`, and `qualified_codec_wrapped_utf8_failure_recovers_without_provisional_effects` | No host effect is permitted from a failed provisional turn. |
| Auxiliary recovery and inherited outage deadlines | Existing `main_and_auxiliary_outage_deadline_preempts_pending_retry_hooks`, `main_and_auxiliary_outage_limit_waits_until_actual_deadline`, and `auxiliary_gate_healthy_reconnected_body_outlives_outage_deadline` | Host transport fixtures cover operation-scoped calls; they do not qualify remote service behavior. |
| Durable usage uncertainty after ambiguous work | Existing `qualified_codex_four_eofs_then_success_preserves_unknown_usage` and `manual_native_http_503_with_hard_budget_records_uncertainty_and_never_replaces` | An uncertainty record is evidence, not a billing reservation or reconciliation. |

## Verification record

Observed without consuming the shared Cargo build slot:

- `git diff --check` — passed.
- Source and test inspection — completed for the recovery agent loop, AI
  client/transport seams, Responses WebSocket pool, and the existing recovery
  integration matrix.

Not run because `BUILD-SLOT.md` reserves the sole Cargo/rustc slot for the
integration lane:

```text
cargo test -p octet-agent --test recovery_current
cargo test -p octet-agent --test agent_run
cargo test -p octet-ai --lib
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

These commands are pending exact-candidate execution; the presence of test
definitions is not reported as a passing result.

## Remaining gates and blockers

- No controlled live-provider fault injection, credential rotation campaign,
  remote usage reconciliation, physical-terminal journey, or weeks-scale
  endurance/soak was run. Those cells remain unqualified here.
- Full pinned-Codex parity and real provider connection-lifetime behavior remain
  acceptance targets, not consequences of passing local fixtures.
- Cargo compilation, integration tests, formatting, and Clippy remain blocked
  until the build slot is released. Any failure discovered there must be
  recorded against this candidate rather than inferred from source inspection.
