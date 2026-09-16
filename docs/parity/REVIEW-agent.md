# Final agent review

Scope: `crates/octet-agent/**`; baseline `df5a7e809715961b9344af6b52e43a6ca48f56b3`. Pre-existing uncommitted telemetry and persistence-hook changes are preserved. No other source crate, generated file, trust policy, Codex cap, or Git state is edited.

## Outcome / readiness

Source is ready, including the final search-fixture repair; parent compilation/execution of that fixture-only change is pending. The parent's preceding full agent run had **554 library tests passed, 2 failed, 1 ignored**, with both failures in the search fixtures now repaired in source. All agent integration suites passed, including **146 agent_run tests** and the new theme process regression. Accounting, idempotency, sidecar, and theme unit tests passed. The AI websocket repair also passed both formerly failing agent regressions. No Rust build or test was run by this worker during disk recovery.

Complete current reads: tool/telemetry/codec parity docs, session-format/sessions/subagents, agent design, tools/telemetry/context/Codex-context docs, delegation boundary design, subagents bundle README/reference, extension guide/generated API 0.3 reference/parity detail/minimal-example README, and REVIEW-host-boundary. Stale documentation still calls tool-layer durability primitives end-to-end consumers and describes run-scoped child retirement; this report does not repeat those claims.

## Fixed findings

1. **Delegated accounting consumed a watermark before root append, omitted uncertainty-only snapshots, and lost picodollar borrowing.** `src/delegation.rs:4797` now returns non-consuming cumulative snapshots, including zero-turn uncertainty; obsolete process/fleet watermarks are removed. `src/agent.rs:4642` derives the baseline from committed root usage records, so failed appends/restarts cannot consume spend and repeated snapshots cannot duplicate it. Cost-only increments remain observable. `src/delegation.rs:6129` subtracts exact picodollars before splitting whole/remainder. Tests cover read-only append failure/reopen, repeated cumulative snapshots, equal turn counts, cost-only deltas, remainder borrowing, and uncertainty-only roster reload (`agent.rs:11733`, `delegation.rs:9022`). Existing sticky-root-uncertainty tests remain.
2. **Durable spawn retry identity was narrower than the process cache.** Durable worker records retain original message SHA-256, spawning resource owner (distinct from the child's own resource owner), and original requested policy (distinct from attenuated effective policy). `src/delegation.rs:894,4834` scopes lookup by principal/owner/key and compares the original input. Legacy unverifiable records refuse a retry instead of silently executing it. Tests force cache eviction on a real spawn and reconstruct from a persisted roster, rejecting changed messages/foreign owners/principals while accepting the same request even when policy was attenuated (`delegation.rs:8344,9056`).
3. **Partial-assistant sidecars followed links/truncated targets and allocated before enforcing read bounds.** `src/session.rs:3008` uses descriptor-bound exclusive creation and private bounded reads; existing files, symlinks, extra hard links, and special files cannot be overwritten/read as journals. Reader enforces both 1 MiB and 8192 frames, dropping unterminated/invalid-UTF8 tails. Consumption requires successful compare-and-delete; settlement protects different-content replacements. Observed failures/cancellation remove their own journal before retry, while dropping an undriven run still leaves recovery progress. New tests at `session.rs:5561` onward cover existing targets, links/FIFO, byte/frame bounds, torn tails, and replacement-safe cleanup. The four-EOF recovery fixture additionally checks that a successful retry leaves no stale partial.
4. **Unimplemented theme service was advertised (parent follow-up).** `src/extension_process.rs:10440` removes optional `theme_selection` and its methods from the product offer until a host-owned catalog/namespace-bound handler exists. Generated contract artifacts are unchanged. A unit test covers both session-driver configurations; `tests/api_v03_runnable.rs::unimplemented_theme_selection_is_not_offered_and_returns_a_canonical_refusal` starts a real Python peer, negotiates only offered capabilities, sends `theme/select`, and checks the exact API 0.3 `-32601 / unknown or unnegotiated method` response plus healthy shutdown. No principal is manufactured and no trust/auth policy is changed.

## Retry regression: repaired across both owners; parent regressions passed

`/tmp/v12d-agent-run.log:162,164` records the websocket failure and long-running cumulative envelope test, without a final failure backtrace because the process never completed.

The original cause was `crates/octet-ai/src/responses_ws.rs` converting `websocket_connection_limit_reached` before output into `AttemptOutcome::Interrupted`. `run_generation` internally slept/reconnected/resent up to three times, then emitted `ResponseNotResumable` rather than the original rejection. That hid physical inference sends outside the host's 11/29/35 budgets and per-attempt uncertainty evidence. The paused-clock fixture advances only host `ProviderRetry` delays; its old detached always-runnable task prevented the hidden transport timer from advancing forever.

Within agent ownership, `tests/agent_run.rs:7842` now has a 15-second **real-time per-event** watchdog and a 1ms wall pause while waiting for loopback I/O; no detached spinner survives panic/drop. The real-time socket test has a 15-second Tokio deadline. **No retry/count/accounting assertion or 272K cap was relaxed.**

The AI owner has now changed rejection handling to forward connection-limit/stale-cursor failures after fencing the socket and removed autonomous pre-output `response.create` resends. Current source inspection confirms both changes; see `REVIEW-ai.md:20–29`. Stored-response cursor retrieval remains GET-only, and ordinary Codex remains `store: false`. No out-of-scope AI edit was made by this worker. Parent execution now confirms both agent websocket regressions passed (`/tmp/octet-final/agent-tests-final.log:754,759`); the complete 146-test agent_run suite passed at line 761.

## Search fixtures: reproducible startup race repaired (parent follow-up)

- The serial search rerun still failed reading `child.pid` (11 passed, 1 failed; `/tmp/octet-final/search-regressions.log`). The later full agent library run also failed the stderr fixture's two-second wall-time assertion. These are not dismissed as flakes.
- A small Python/subprocess reproduction on this macOS host measured fresh temporary executable scripts taking **1136, 1211, 1584, 1564ms after spawn** to publish the PID; repeat launches took 6–11ms, and invoking the existing system shell took 5–14ms. A separate exact 150ms run observed the child alive with **no PID file at 155ms**, then killed/reaped it. This establishes that the old fixture can time out before entering its intended EOF state; no specific OS security subsystem is assumed to be the cause.
- `src/tools/search.rs:598–699` changes tests only. Paused Tokio time is driven inline with a 15-second real-wall-clock watchdog, yield, and 1ms wall pause; no detached keepalive survives failure. The EOF script closes stdout, publishes a complete PID line, then stops itself (same process, no descendant or natural sleep expiry). The test requires readiness and a live PID before advancing the unchanged **150ms** deadline, asserts an execution-limit error at exactly that virtual deadline, resumes real time, and retains the actual OS disappearance/reaping assertion with its one-second bound.
- The stderr fixture keeps its 1MiB saturation and expected ripgrep exit-2 error, but separates real executable startup from the two-second virtual execution limit. It must complete without advancing that clock; unread-pipe deadlock fails the real watchdog rather than hanging. Production timeout/process behavior is unchanged.
- Shell-only validation confirmed the replacement fixture reaches stdout EOF while alive and can be killed/reaped. Rustfmt parsing and diff checks passed; **the new Rust fixtures have not yet been compiled or run** by the parent.

## Verification

Observed here:
- `git diff --check -- crates/octet-agent docs/parity/REVIEW-agent.md` → exit 0.
- Rustfmt parser via stdin on changed Rust files → exit 0; only newly replaced function ranges formatted, no global formatting. Parsing is **not** type-checking.
- Embedded theme peer Python source compiled in memory → syntax OK; no host-process execution claimed.
- No `cargo`, `rustc`, builds, or Rust tests invoked by this worker.
- Earlier parent library evidence, directly read from `/tmp/octet-final/agent-lib-final.log`: **555 passed, 1 failed, 1 ignored** in 27.91s (runner exit 101). New accounting/idempotency tests passed at lines 39, 67, 131, 133, 144; theme unit at 226; sidecar units at 408, 411, 414, 415.
- Latest pre-fixture-fix parent evidence: `/tmp/octet-final/agent-tests-final.log:592–608` records **554 library tests passed, 2 failed, 1 ignored**. Failures were missing `child.pid` and stderr elapsed-time assertion; the serial search failure and reproduction are detailed above.
- All integration suites in that log passed, including agent_run **146/146** (761), runnable API **2/2** including the new theme process test (766–769), API conformance **4/4 + 5/5** (796, 807), and existing theme selection **13/13** (836). The live-provider test remains ignored.
- Parent reruns of the latest search-fixture changes and overall full-workspace verification remain pending.

Parent commands (retain parent's low-disk profile/environment; run search regressions and the full library first for the final fixture-only change):

```sh
cargo test -p octet-agent --all-features --lib tools::search::tests -- --test-threads=1 --nocapture
cargo test -p octet-agent --all-features --lib
cargo test -p octet-agent --all-features --lib delegated_snapshot
cargo test -p octet-agent --all-features --lib spawn_idempotency
cargo test -p octet-agent --all-features --lib partial_assistant
cargo test -p octet-agent --all-features --lib repeated_delegated_uncertainty
cargo test -p octet-agent --all-features --lib api_v03_theme_selection
cargo test -p octet-agent --all-features --test api_v03_runnable
cargo test -p octet-agent --all-features --test agent_run websocket_connection_limit_is_retried_by_agent -- --exact --nocapture
cargo test -p octet-agent --all-features --test agent_run qualified_codex_ws_http_cumulative_twelve_attempt_envelope -- --exact --nocapture
cargo test -p octet-agent --all-features
```

The accounting/theme filters and websocket integration regressions above already passed before the final search-fixture edit. That edit touches no production or integration-test code. Preserve the exact retry/accounting assertions and require parent search/library reruns before claiming the final suite is green.

## Remaining primitives / limits

- Cross-process child writer ownership and durable command/mailbox recovery require explicit host handoff/leases. Per-append locks do not provide a lifetime single-writer lease; this pass does not claim safe concurrent child launch ownership.
- Partial journals are recovery observations, not authoritative usage or effect outcomes. Republish is not transactional with frontend delivery, and sidecar removal and assistant/session durability are not one atomic transaction; no exactly-once crash-delivery guarantee is claimed.
- Provider deferred-fetch lifecycle and session-backed invocation memos/checkpoints remain missing separate primitives. No provider retention (`store: true`) opt-in is introduced: that changes request/privacy semantics.
- Theme selection remains unavailable in the product until an actual host-owned catalog and owner/namespace-bound handler are wired; pure generated policy tests are not host mediation.
- Windows PowerShell and Windows filesystem/process behavior still require Windows execution evidence.

Artifact reference: this report and the shared source diff. No new octet artifact/session reference was exposed to this worker.
