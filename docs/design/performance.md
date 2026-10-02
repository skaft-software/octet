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

Eligible open-code previews now expand tabs and sanitize character-local controls
incrementally, retaining column state and the last grapheme. Expanded reasoning
uses the assistant suffix-update path; tool-result hydration indexes pending calls
once per batch instead of scanning the transcript for each result. These are
work-reduction changes, not measured end-to-end latency claims.

The same pass polls RPC control admission alongside the run, constructs JSON-mode
deltas without discarded cumulative snapshots, and bounds retained RPC tool
progress to 64 KiB (authoritative results remain unchanged). Provider work removes
repeated argument decoding/prefix copying and adds a 64 MiB serialized-event
queue budget to Responses WebSockets; this is not an exact parsed-JSON heap cap.
Bash spills stop storing after 16 MiB per stream while still draining pipes.
Active and completed spills share a 64 MiB / 32-file owner budget with oldest-first
expiry and lifecycle cleanup; bounded blocking capture workers keep disk work
off the async path. Undo and redo each retain at most 64 snapshots / 4 MiB.
Fleet restart summaries share the existing 256 KiB roster budget, preserving full
committed answers in child sessions.

Responses replay projections and opaque-item capacity estimates advance over
new suffixes and invalidate on route/branch changes; final request encoding still
materializes full input. Accepted run controls hold their 64-input / 64-MiB
reservations through delivery or termination, with explicit admission failure
and independent abort. Those bytes measure retained logical payload, not RSS.
Prospective extension catalogs share a 4-MiB input/output-schema budget and
feature checks no longer clone negotiated catalogs.

Session catalog maintenance uses targeted lookups and 32-summary batches.
Entry reconciliation streams directory entries without sorting and batches up to
32 changed sessions with an 8-MiB projection target. It still enumerates/stats
workspace transcripts on each search: directory mtime alone cannot safely detect
in-place edits, and no cross-process change feed is claimed. Maintained gram
cardinalities select rare postings without query-time posting counts. Each
session has a 524,288-posting quota; overflow sessions retain the complete existing
bounded text projection and use substring-scan fallback. The quota bounds logical
posting rows, not SQLite file bytes or free pages; overflow searches can be slower.
SQLite residency remains bounded.

Search projection consumes JSONL without constructing complete lines or JSON
values. A 64-KiB input buffer and 12-byte scalar staging validate string suffixes
even when discarded; non-ID strings forward at most 512 Unicode scalars to the
projection deserializer. Existing file/record limits and malformed-line recovery
are retained, without new record-size or ID restrictions. Exact entry IDs still
require output-sized memory; this is not a total process-memory bound.

Optional JSONL telemetry uses an ordered off-path writer with 256-record / 1-MiB
admission and observable loss/failure; it is not an accounting authority. See
[telemetry](../telemetry.md) for drain boundaries.

Ephemeral accounting retains its locked, fsynced ledger as authority. A disposable
private SQLite ID/offset index avoids warm whole-ledger parsing on Unix; ledger
identity/size/mtime/ctime changes cause a streaming rebuild. Other platforms use
the conservative streaming path. The cache is published after ledger durability,
and duplicate receipts are checked against the authoritative record. This is
not protection against external mutations with indistinguishable metadata.

Active model/thinking/subagent panels and tool consent are states of the main
run loop, not nested loops that stop caller-driven inference. The renderer uses
shared semantic transcript roots and private layout caches, releasing semantic
ownership before layout and terminal writes. Post-write geometry and approval
receipts are revision-fenced; accepted text is retained even when paints coalesce.

The follow-up cost audit adds narrower bounds, not measured latency claims:

- Ordinary Chat SSE frames decode directly into the typed DTO once; error-bearing
  and malformed frames retain a permissive error fallback. Locked-marker removal
  advances a cursor and compacts once. Responses request construction moves
  already-owned input trees instead of serializing them into extra JSON trees;
  final wire encoding still visits the complete request.
- Small Bash output starts no spill writer or file. Promotion occurs before the
  first lost raw byte, including truncation caused only by the final shared
  stdout/stderr budget; pipe draining and cancellation coverage remain intact.
- Natural runs collect no terminal-gate evidence. TerminalGate retains the first
  12 and latest 12 bounded action receipts, plus the initial/latest request
  evidence within 8 entries / 32 KiB. Omission counts are explicit; authoritative
  inputs, tool results, and accounting are unchanged.
- Reload enumeration stops at its work budget and reports skipped counts as lower
  bounds. Partial layers cannot infer deletions or advance their baseline. Skill
  descriptors share count/payload limits and a separate rendered-catalog bound;
  this is not a total discovery-work or RSS bound. See [resources](../resources.md).
- Serve borrows posting sets, retains only top-K scored candidates, and creates
  snippets only for winners; it still scores every eligible document. Git status
  is aggregated once per listing. Filesystem search stops on the proven 101st
  match, not merely on reaching 100 results.
- Browse bounds DOM body traversal and substring reads before host-side redaction
  and clipping, with explicit truncation notices. Interactive metadata extraction
  retains its separate existing behavior; these are not whole-page work bounds.
- Select-list filtering retains one exact-content-keyed normalization snapshot
  per calling thread, bounded to 4,096 items / 2 MiB. It still compares source
  content and scans candidates; oversized panels use complete uncached filtering.

These changes and their regression fixtures require qualification on the final
candidate. They do not establish real-terminal stability or provider-side speed.

CR/CRLF normalization, general semantic previews, fenced diffs, and some
content-fitting code geometry still use general tail layout. A bounded rich
paragraph prefix followed by append-only literal text retains styled rows and
replays only its wrapping frontier; prefix flattening and copied layout bytes
have separate work counters. Unbounded individual graphemes can also require
unbounded frontier work. Full-document and
full-lines APIs necessarily materialize their requested output. On-demand
session/export/debug helpers and local shell escapes are not all background jobs;
this pass does not claim every command is latency-independent. Complete
five-client interactive replay remains outstanding. Report these boundaries
alongside improvements; no end-to-end constant-time or all-open-code linearity
claim is supported.

## Scroll and resize work boundary

Octet explicitly enables the preserving Pi renderer. Structural repairs emit
only the live viewport and do not allocate a replay-output buffer proportional
to retained history or send `ED 2`/`ED 3`. Saved native rows remain emitted
snapshots; session/source/copy and application-owned navigation retain canonical
results. Initial native paint and ordinary append still emit complete history,
including an eagerly materialized resumed branch.

Resize notifications settle after 75 ms of quiet with a 150 ms maximum delay;
composer edits/readiness changes bypass settling. Height-only changes keep
wrapped block caches, and a resize epoch covers away-and-back dimensions.
Threaded visual anchors are promoted using the old renderer layout before width
reflow. Tests scale retained history while bounding repair bytes and verify
semantic reading positions, saved sentinels, and Kitty placement restoration.

This removes history-sized **wire replay and replay-output allocation**, not
all history-sized work. Width reflow, canonical frame construction, semantic
source mapping, image-bearing full-component fallbacks, and retrospective layout
can still depend on history. Saved-row preservation does not establish native
wheel-offset/selection behavior or GPU
paint smoothness. Keep physical-terminal and long-duration qualification
separate from library and PTY evidence.

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

### TUI output throughput

The completion line's `tok/s E2E (last turn)` and `/status` throughput divide
provider-reported output tokens (including reasoning) by the latest ordinary
AI-client request-to-canonical-completion interval. The sample freezes before
agent persistence, settlement and terminal gates; polling still includes
network buffering, local decode, scheduling and consumer backpressure. Legacy
external event producers without client metrics retain the explicitly
agent-observed `TurnStarted`-to-`TurnFinished` fallback, including settlement.
Neither is **server-side generation speed** or a whole-task average. Retries
start fresh samples; successor steering segments and deferred submit/poll scopes
do not manufacture a request E2E rate. Missing timing or zero output leaves the
rate unavailable.

Recognized terminal native count/duration pairs appear separately as
`server-reported generation`, with source/original units or explicit
unavailability in `/status`. Their native definition is not automatically
post-first-token decode or GPU-active time. See the unreleased
[inference measurement contract](../inference-metrics.md) for codec/transport
coverage, error/provisional handling and qualification limits.

Do not divide all reported output tokens by first-visible-delta-to-completion
time: hidden reasoning can consume most of the output budget before any text
arrives, inflating that rate by orders of magnitude. Server-side generation
throughput requires provider-supplied timing with matching token semantics.

## Startup attribution (opt-in)

`OCTET_STARTUP_TRACE=1` emits monotonic phase boundaries to stderr without
putting timing text on the TUI. `process.enter` is after Tokio runtime creation;
`cli.configured` includes CLI/config loading; `selection.resolved` includes
`--models` scope and resume selector resolution. Catalog phases separate base,
selected-route, Codex credential/inventory, and deferred fleet work;
`session.resolve`, `session.replay`, `app.build`, `history.hydrate`, and
`frame.ready` cover later readiness. Extension startup additionally brackets
`extensions.digest` (manifest/source hashing) and `extensions.handshake`
(process launch and initialize), with counters for catalog entries, hashed
files/bytes, and eligible handshakes. Disabled or untrusted entries do not
hash their source trees. `history.hydrate` reports replayed messages and its
configured tail budget. Differences between adjacent phase times
attribute *in-process* work, not process spawn, a physical keypress, or terminal
paint. Measure spawn-to-first-editable-frame and input-to-PTY separately with a
PTY; record cold/warm caches, selected route, resumed route, custom inventory,
terminal, extensions, and executable hash for each run. No latency target is
established by the presence of these traces alone.

A valid positive custom-model cache is now used for the current catalog even
when stale; an online refresh updates the private cache in the background for a
later catalog build and cannot overwrite a newer cache snapshot. Missing or
negative inventories retain their existing discovery behavior. A narrowed
interactive launch opens `/model` using its current routes and loads deferred
fleet inventories while the picker accepts input. A successful refresh keeps the
typed filter and highlighted `ModelId`; cancellation or a failed fetch retains
the current app catalog. Neither step changes the active model without explicit
selection. Provider/route readiness and actual latency still require a matched
startup campaign.

## Startup audit follow-up

The opt-in phase trace now brackets the workspace marker (`session.marker`) and
HTTP-client construction (`catalog.client`) separately before `bootstrap.ready`.
A repeated launch with an unchanged workspace path validates the owner-only,
no-follow marker and skips its atomic replacement. A missing, changed, or
insecure marker still uses the existing descriptor-bound atomic writer (and
still fails closed on a symlink). This removes one staged write/rename and its
permission checks from the common warm launch, not the marker read or first
launch write. No claim about spawn-to-editable-frame or extension-enabled
startup follows from these in-process phase timestamps.

Catalog registration checks model identity by indexed resolution rather than
scanning every model for each discovered/static registration. Credential pruning
checks each endpoint's authentication once per pass, then retains models by
endpoint membership; endpoints remain available for later registration. These
are work reductions, not measured UI or provider latency improvements.

One local matched release-profile comparison used:

```sh
python3 docs/benchmarks/startup-phase-audit.py \
  /tmp/octet-perf-audit/baseline-octet target/release/octet
```

The baseline executable SHA-256 was `ea6ce41e277a8ea253c8052efa1b9dca2887ebac6a17c92df2baffbe34adfce1`;
the candidate was `6a100e400a3e79ab5bbd635db20b1001d368f5bce134115a147e7128d951d62d`
(built before the separate agent-turn copy change below). On this macOS host,
nine alternating-order warm launches per cell (one cold launch per cell
discarded) gave `catalog.copilot` → `bootstrap.ready` medians of
6,416 µs before and 148 µs after. Within the candidate, marker work was
114 µs and `AiClient::try_new()` 29 µs median. Spawn-to-*offline inference
failure* exit medians were 12,454 and 6,125 µs respectively. This isolates the
unchanged-marker rewrite as the dominant part of that phase on this host, not
HTTP-client construction; it does not show a first-frame speedup or characterize
an extension-enabled/credentialed launch. The script prints the per-trial
samples and uses an intentionally credential-free environment.

## Agent-turn request copy audit

Ordinary provider turns no longer construct an unused native-steering
continuation request. On a native-steering turn, the independent continuation
request still copies settings and tool schemas, but temporarily removes the
full message history before cloning; the canonical request regains that history
before dispatch, and the continuation builder fills only trailing tool results
from the session. The continuation's own later clones remain when required
input is delivered. This removes one full-history clone per turn, without a
measured per-turn latency claim or a change to provider-visible messages.

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
