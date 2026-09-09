# Performance philosophy and execution contract

**Status:** Engineering direction and qualification plan, not a published speed
ranking or a claim that every target below is already implemented.

## Product promise

**Stay responsive as the work grows.** A long conversation, fast model, busy
tool, or optional extension should not make typing, reading, or stopping feel
unpredictable. Prefer a small, explainable critical path over a flattering empty
startup number.

The architectural idea is to preserve knowledge of completed work across
boundaries: provider deltas, committed Markdown, cached layout, stable terminal
rows, and durable session entries. A downstream consumer should not reconstruct
or rescan an unchanged prefix just to discover that it is unchanged.

This is not a language comparison. Other agents also use native renderers,
incremental parsing, batching, and caches. Sustainable differentiation is an
observable responsiveness contract, adversarial regression coverage, and
repeatable evidence under realistic load—not permanent ownership of an
optimization competitors can adopt.

## Current implementation boundary

The first patches carry append-local scanning/literal-preview work into eligible
plain/open-code row layout, skip unused commit metadata on the default Pi
renderer path, distinguish answer/reasoning telemetry, provide repeated
credential-free renderer replay, and remove premature Bash effects. These are
specific improvements, not completion of the latency targets below.

Tabs, CR/control normalization, general semantic previews, fenced diffs, and some
content-fitting code geometry still use general tail layout. A bounded rich
paragraph prefix followed by append-only literal text retains styled rows and
replays only its wrapping frontier; prefix flattening and copied layout bytes
have separate work counters. Unbounded individual graphemes can also require
unbounded frontier work. Full-document and
full-lines APIs necessarily materialize their requested output. The input/layout
shared lock and complete five-client interactive replay remain outstanding.
Report these boundaries alongside improvements; no end-to-end constant-time or
all-open-code linearity claim is supported.

## Visual stability is separate from throughput

Streaming regressions also record actual Shell → Pi → ANSI/vt100 frames. They
check saved-line erasure (`ED 3`), full redraws, historical sentinel duplication,
and live content—not just final-frame equality or parser timing. Current cases
cover table body growth, rich paragraphs crossing the inline budget, fragmented
closing fences, and huge newline-free prose. Canonical completion is a separate
phase; no-change renders must not repeat it.

The parser keeps ordinary literal previews and interpreted prefixes stable
between meaningful boundaries. A provisional structural-line classifier tracks
new bytes separately (`preview_scanned_bytes`); withheld table-cell bytes do not
invalidate layout. Raw/copy ingestion remains immediate. The compiled default
uses viewport-sized code/table geometry and a stable compact subagent roster.
These tests establish composed frames and protocol output, not actual emulator
paint timing or native wheel-offset preservation. Terminal.app, Ghostty, and
Ghostty-over-SSH journeys on the exact candidate remain separate qualification;
issues #392/#393 are not closed merely by these deterministic checks.

## Non-negotiable rules

1. **Correctness is part of performance.** Missing output, stale selection,
   reordered effects, incomplete cancellation, and corrupted history are failed
   trials, not fast trials. Keep canonical final Markdown, durable ordering,
   effect admission, and exact text/copy semantics.
2. **Preserve deltas and invalidation boundaries.** Mark what changed at mutation
   time; carry that boundary through layout and diffing. Include metadata scans,
   byte copies, Unicode scans, and allocation traffic in the cost model—not only
   parser calls or bytes written to the terminal.
3. **Bound the common path, name the exceptions.** Ordinary append work should
   depend on new bytes and the mutable display frontier, not settled history.
   Resize, theme/disclosure changes, retrospective Markdown dependencies, branch
   replacement, and canonical finalization may need broader work. Measure those
   separately and keep input/control responsive while they run.
4. **Input and cancellation are control traffic.** They must not wait behind an
   arbitrary amount of transcript layout or tool output. A renderer thread alone
   does not provide this guarantee when it shares a long-held lock with input.
5. **Coalesce presentation, never accepted meaning.** Adjacent render requests
   may collapse into the latest frame. Accepted text, tool outcomes, user input,
   and durable records may not be silently discarded. Use explicit byte budgets
   and backpressure; keep cancellation out of saturated data queues.
6. **Display useful provisional text promptly.** Do not wait for a newline merely
   to reveal already received prose. Provisional styling may simplify while a
   construct is incomplete, but do not substitute animation for actual progress.
7. **Overlap only independently admissible work.** Reuse/prewarm connections and
   execute truly independent, effect-approved calls after their required commit
   boundary. A shell-command-name heuristic is not proof of read-only effects,
   observational independence, or crash safety. Do not speculate side effects
   merely because an exact-argument check is possible later.
8. **Optional features pay explicit costs.** Report bare-core and representative
   extension-enabled profiles separately, including resident descendants. Do not
   claim lazy startup when a resident extension is eagerly activated, or claim a
   cheap agent by excluding its workers/browser/server without saying so.
9. **Measure ownership transitions, not convenient log messages.** The provider,
   agent, renderer, PTY, terminal emulator, and operating system have different
   clocks and observation points. Label every metric with its actual boundary.
10. **Optimize verified bottlenecks.** Require a reproducer, baseline, cost model,
    correctness test, work-budget test, and matched timing experiment. Do not add
    a generic cache, background thread, or compatibility layer without a measured
    or structurally demonstrated problem and an invalidation/ownership contract.

## Distinct latency clocks

| Metric | Start and end | What it must not stand in for |
| --- | --- | --- |
| Version-command wall time | process spawn to successful exit of `--version` | editable UI or provider readiness |
| First editable frame | process spawn to composer accepting and presenting an edit | prompt submission readiness |
| Submission readiness | process spawn to a prompt being accepted for execution | first inference or first answer |
| First model output delta | request attempt start to first nonempty text **or reasoning** delta observed by the agent | first answer text or terminal paint |
| First answer-text delta | request attempt start to first nonempty text delta observed by the agent | first visible rendered text |
| Agent-to-PTY presentation delay | accepted text/input marker to corresponding semantic output in the PTY | emulator/GPU paint latency |
| Input-to-paint | physical/injected input to displayed frame, with terminal-side instrumentation | merely enqueueing a redraw |
| Cancellation admission | cancel input to the owning cancellation signal being set | tools/descendants actually stopped |
| Cancellation settlement | cancel input to authoritative settlement and descendant cleanup | success or a hidden continuing process |
| Tool admission/start | completed valid call plus required durability/approval to actual execution start | a UI `ToolStarted` notification |
| Task latency | accepted task to verified correct outcome, including retries and failures | renderer microbenchmark throughput |

Telemetry retains `ttft_ms` for its established agent-output-delta meaning and
adds `first_text_delta_ms` and `first_reasoning_delta_ms`, with
`output_timing_scope = "agent_delta"`. Empty deltas and tool notifications do not
supply these timings. Missing timings remain missing. These clocks include work
before the observer sees a delta and do not expose exact provider headers or
terminal paint. See [benchmark methods](../benchmarks/README.md).

## Work budgets before wall-clock budgets

Deterministic regression tests run in ordinary CI. Assert the work that should
not grow, rather than brittle millisecond thresholds on a shared runner.

- With a fixed changing tail, scale settled history through 1K, 10K, and 100K
  rows/blocks. Track full renders, inspected rows, **commit-metadata visits**,
  allocation/copy bytes, and emitted historical bytes.
- Scale streamed input through N, 2N, and 4N bytes with fixed chunking. Count
  scanned, copied, parsed, and laid-out bytes separately. A parser-only counter
  cannot prove the whole pipeline is linear.
- Include a long unterminated line, repeated soft newlines in one paragraph,
  a large open code fence, many tiny blocks, Unicode split at byte boundaries,
  tables, and late Markdown resolution. Test canonical final output and copy
  text against the static renderer.
- Exercise width changes, theme changes, shrinking tables, disclosure, removed
  status tails, branch replacement, and scroll/selection away from the live tail.
- Simulate slow layout deterministically. Verify cancellation admission and
  composer editing independently of the renderer's lock and write progress.
- Saturate event/progress queues and test order, byte limits, cancellation,
  terminal outcome, and retained text. A bounded notification count is not a
  bounded semantic byte queue.

Wall-clock qualification belongs on a recorded reference machine. Initial
**engineering targets**, to calibrate against a retained baseline rather than
advertise as achieved, are:

| Scenario | Initial target | Qualification boundary |
| --- | --- | --- |
| Typing while streaming, fixed visible geometry | input-to-PTY p95 <= 16.7 ms; p99 <= 50 ms | 80x24 and 120x40; 100K settled rows; both native and app viewport modes |
| Received text to PTY presentation | p95 <= 33 ms; p99 <= 50 ms | 250 deltas/s, fixed deterministic stream; no deliberate newline holdback |
| Cancellation admission under render/tool load | p99 <= 50 ms | separate test of actual descendant settlement |
| Unchanged idle presentation | no periodic full layout/render | measure process and descendants over a sustained idle window |
| History growth with a fixed tail | no history-dependent parser/row-diff work | inspect metadata separately until its regression is closed |

Do not turn these targets into global promises: terminal-emulator paint, cold
provider discovery, arbitrary shell cleanup, and network outages require their
own measurements. A structural violation blocks the claim even if a fast CPU
hides it in one run.

## Qualification ladder

### 1. Library and shell regression evidence

Run deterministic correctness/work-budget tests and the credential-free render
benchmark. Keep ingestion, layout/update, finalization, and output reconstruction
separate. Allocation counts and cumulative requested allocation bytes are not
RSS or peak live memory. The generic renderer benchmark does not include the
shell's shared-state lock, session, provider, or terminal emulator.

### 2. Real Octet loop with deterministic replay

Drive the actual interactive shell and provider adapter with fixed, synthetic
streams. Reuse the existing PTY and mock-provider infrastructure rather than
creating a second presentation implementation. Record monotonic timestamps for
request receipt, emitted provider chunks, agent deltas, input delivery, PTY
output, and cancellation/settlement. Never call a PTY byte timestamp a GPU paint
timestamp. Preserve raw events and distinguish first text from reasoning and
tool-only events.

### 3. Matched cross-client replay

Pin each executable/package hash, runtime, terminal, viewport dimensions,
configuration, transport, theme/motion mode, tool policy, and enabled extensions.
Use equivalent synthetic text/timing through the protocols each client actually
supports; transport adapters and prewarming are part of the recorded setup.

Run both a common-feature profile and a realistic feature-enabled profile.
Check effective model/reasoning/sandbox settings from each client's actual
request or authoritative state, not just command-line intent. Never compare an
extension-disabled Octet profile with an extension-loaded competitor and call
the difference architecture. Unknown settings make the cell unqualified.

Interleave/randomize client order, record seeds, retain failures, and separate
cold processes/caches from warm sessions/connections. Use enough independent
trials for the reported quantile; frames within one run are not independent
trials. Publish per-case distributions and history-size slopes, not just one
geometric-mean speedup. A nine-trial interpolated p95 is descriptive, not a
well-estimated extreme tail.

### 4. Same-model successful-task throughput

Only after replay qualification, run paid/live task campaigns with explicit
operator authorization and a pinned task manifest. Match model, reasoning,
context/output limits, transport/service tier, permissions, tools, and cache
policy. Report verified success/hour, wall time per correct outcome, failures,
retries, provider input/cache/output buckets, cost, and process-tree resources.
Do not infer a GPU speedup from a UI change. Never optimize against private
verifier data or hide a correctness loss in successful-run-only latency.

## Delivery sequence and exit criteria

1. **Close demonstrated common-path leaks.** Incremental suffix scanning/copying,
   eligible mutable-tail layout, and unused/history-wide commit bookkeeping.
   Exit: adversarial work counters and correctness regressions pass; remaining
   broad-work cases are named rather than hidden.
2. **Make evidence trustworthy.** Repair timing-unit defects, separate delta
   clocks, provide structured repeated replay measurements, and quarantine
   invalid historical comparisons. Exit: unit tests reject missing/mixed metrics;
   raw independent trials reproduce the summary.
3. **Separate control ownership from transcript layout.** Keep input and semantic
   event reduction on the async owner; give the renderer private layout caches
   and one latest immutable, structurally shared render-model root. Publishing
   may replace an unrendered revision, not lose accepted semantic events. Share
   unchanged blocks/segments rather than cloning `ShellState` or the growing
   assistant prefix. Fence geometry feedback by transcript, block, input,
   theme/disclosure, and viewport revisions. Exit: a deliberately blocked
   renderer cannot delay cancellation admission or accepting composer edits;
   recovery preserves every accepted character. Keep polling the caller-driven
   `Run` through cancellation settlement, independent of renderer progress.
4. **Qualify expensive boundaries.** Resize, native-history resume, disclosure,
   image fallback, selection, and finalization get explicit workloads and
   latency budgets. Native terminal scrollback cannot prepend deferred history;
   do not silently change ownership to fake a faster resume.
5. **Publish the matched comparison.** Ship the replay fixture, manifests, raw
   results, failures, and executable analysis with a new release. State exactly
   which responsiveness and resource claims it qualifies. Re-run on material
   renderer/runtime changes and on pinned competitor updates.

The ownership split and complete five-client interactive comparison are release
qualification work, not implied by a passing library test. Track remaining work
explicitly; do not mark the whole philosophy complete after the first patches.

### Ownership refactor acceptance details

The current renderer holds `SharedState`'s `Arc<Mutex<ShellState>>` through
layout. Input also takes that lock; a separate render thread and capacity-one
paint notification are not isolation. `try_lock` on the renderer cannot undo a
layout already holding the lock. A transcript-event FIFO can bound memory, but
when it fills, backpressuring the caller-driven `Run` also delays cancellation
settlement. Prefer the shared immutable semantic-root design above for the
stronger control guarantee.

The touch set starts in `tui/view.rs`, `view/renderer_runtime.rs`,
`view/assistant_block.rs`, and `view/transcript_cache.rs`; navigation, semantic
copy, history hydration, panel confirmation visibility, and
`modes/interactive.rs` must migrate with the ownership contract. Merely cloning
the current `AssistantBlock`/`StreamingMarkdown` is neither immutable layout
separation nor cheap publication.

Use a test-only gate inside the actual threaded layout path, an independent
observer with a finite timeout, and an always-releasing cleanup guard. While
layout is gated, verify edits/paste/backspace, draft-sensitive Ctrl+C,
modal/popup Escape, unconditional close, terminal run settlement, bounded
snapshot retention, and stale geometry rejection. Do not use the inline test
shell as evidence of threaded responsiveness, or a Tokio timeout around a
blocking mutex as the watchdog.

Processing an edit independently still does not guarantee **visible echo** if
the sole renderer is blocked. Budget/interrupt layout and reuse valid transcript
geometry for chrome-only updates to close that separate gap. Synchronous input
parsing, filesystem completion, and blocked terminal writes also need their own
workloads.

### Replay adapter acceptance details

Start with the real PTY fixture at
`crates/octet-coding-agent/tests/startup_frame_pty.rs` and the protocol fixtures
under `crates/octet-ai/tests/fixtures/`. Extend them with scheduled semantic
chunks, timestamped PTY reads, actual server-write timestamps, attempt identity,
and correctness markers; their current deadline assertions are not latency
distributions. Respect synchronized-output frame boundaries and distinguish
application-handled input from terminal line-discipline echo.

Octet/Pi can replay Chat Completions SSE; Codex needs Responses SSE. A fair
cross-protocol fixture preserves semantic bytes, fragment boundaries, and
schedule, not identical wire bytes. Qualify OpenCode's selected SDK/base URL and
Claude's Anthropic Messages/onboarding/auxiliary-request behavior before adding
their results. Package strings alone do not verify a working configuration.
Unexpected discovery/title/compaction/retry requests must be classified, never
silently consume the next answer fixture. Inject load by elapsed time, not UI
acknowledgments, so slow consumers are not accidentally given less work. Record
server backpressure and actual emission jitter.

## Historical evidence boundary

The retained Ygg 0.6.3 systems campaign measures named isolated headless modes and
`--version`, not current Octet UI latency. The Terminal-Bench aggregates do not
establish task-speed superiority. Preserve those artifacts unchanged.

The local, ignored `artifacts/ygg-vs-codex` exploratory campaign is **not qualified
for performance or reliability superiority claims**: its analyzer mixes first
content with first answer text and incompatible input-token buckets, and its
recorder applies reasoning and feature/policy configuration asymmetrically.
Correcting an analyzer cannot repair the already-executed configuration cells.
Use a new, pinned campaign; do not overwrite historical evidence with corrected
numbers and imply it was rerun.

## Review checklist

Every performance change should answer:

- Which user-visible delay or resource cost changes, at which boundary?
- What repeated work disappeared? What expensive case remains?
- Who owns the state, invalidates the cache, and enforces the queue byte budget?
- What correctness, permission, durability, or observation-order tradeoff exists?
- Which adversarial regression fails before this change?
- Which matched measurement verifies the effect, and what was not measured?
- Can this improvement be explained without saying another language is slow?
