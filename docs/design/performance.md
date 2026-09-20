# Performance and measurement contract

This reference describes implementation boundaries and measurement semantics,
not a published speed ranking.

## Product promise

**Stay responsive as the work grows.** A long conversation, fast model, busy
tool, or optional extension should not make typing, reading, or stopping feel
unpredictable. Prefer a small, explainable critical path over a flattering empty
startup number.

The architectural idea is to preserve knowledge of completed work across
boundaries: provider deltas, committed Markdown, cached layout, stable terminal
rows, and durable session entries. A downstream consumer should not reconstruct
or rescan an unchanged prefix just to discover that it is unchanged.

## Current implementation boundary

The renderer carries append-local scanning/literal-preview work into eligible
plain/open-code row layout and skips unused commit metadata on the default
renderer path. Telemetry distinguishes answer and reasoning deltas, and a
credential-free renderer replay driver measures bounded synthetic workloads.
Tool effects occur only after the required persistence and admission boundaries.

Tabs, CR/control normalization, general semantic previews, fenced diffs, and some
content-fitting code geometry still use general tail layout. A bounded rich
paragraph prefix followed by append-only literal text retains styled rows and
replays only its wrapping frontier; prefix flattening and copied layout bytes
have separate work counters. Unbounded individual graphemes can also require
unbounded frontier work. Full-document and
full-lines APIs necessarily materialize their requested output. Input and layout
still share a lock.
Report these boundaries alongside improvements; no end-to-end constant-time or
all-open-code linearity claim is supported.

## Visual stability is separate from throughput

Streaming regressions also record actual shell/renderer ANSI/vt100 frames. They
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
these deterministic checks do not qualify physical terminal behavior.

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

## Review checklist

Every performance change should answer:

- Which user-visible delay or resource cost changes, at which boundary?
- What repeated work disappeared? What expensive case remains?
- Who owns the state, invalidates the cache, and enforces the queue byte budget?
- What correctness, permission, durability, or observation-order tradeoff exists?
- Which adversarial regression fails before this change?
- Which matched measurement verifies the effect, and what was not measured?
- Can this improvement be explained without saying another language is slow?
