# octet presentation contract

octet's default presentation is one coherent terminal instrument, not a fixed
provider hue. Its stable identity is the `01101111` byte mark, typography,
spacing, interaction grammar, semantic colours, and trust treatment. Model
identity is retained as provenance without turning every working surface into
provider branding. The [canonical native assets](../assets/octet/README.md)
fix eight contiguous, baseline-aligned positions; positions 1 and 4 are
half-height. Asset proofs are not evidence of product integration or captures.

## Stable versus adaptive visual tokens

Stable product tokens include the terminal surface, text hierarchy, layout,
spacing, byte silhouette, interaction grammar, and semantic
success/warning/error colours. Adaptive model tokens identify both model
provenance and the active shell atmosphere: the startup atmosphere, each
persisted prompt marker and compact text highlight, the composer for the model
that will receive the next prompt, and focused controls in picker and
completion menus. Changing models changes provenance and ambience, never
behavior or authority.

Every submitted prompt captures its model-lab colour. The compiled default
paints its marker and each wrapped text line with that provenance, retaining
inline Markdown styling and leaving the gutter, blank spacing and trailing row
unpainted. Known dark TrueColor uses a
hue-balanced 0.10-luminance highlight with a light foreground; Light uses a
contrasting pale tint, while unknown/no-colour profiles retain terminal-default
canvas and readable text. Custom theme cards retain their configured surfaces.
A later model switch cannot recolour old prompts. The composer immediately
adopts the selected next model's colour, including while another model's run is still
settling. Picker and completion focus consistently use the active model's
adaptive accent as shell atmosphere, not the focused candidate's provider
provenance. Queued-steering chrome follows the same accent because it previews
input destined for that model. Contrast is normalized for the detected terminal
background, and status is never communicated by hue alone.

## Information layers

The UI separates three layers:

1. **Durable conversation** — user turns, assistant conclusions, meaningful
   summaries, useful tool results, and errors that need action.
2. **Live activity** — the current request, running tools, retries, compaction,
   progress, waiting, and active workers. These update in place and settle into
   one final state.
3. **Diagnostics** — raw or detailed telemetry, complete worker prompts, retry
   metadata, internal IDs, and retained full output. Diagnostics are available
   on demand and do not become default transcript rows.

Structured telemetry is evidence for measurement; it is not a one-row-per-event
rendering instruction. Presentation code coalesces activity by stable request,
tool, and worker identity.

Subagent orchestration occupies one tool-like transcript block. Its bold
heading shows state counts and `/subagents`; while workers run, up to four
indented child lines show task and `↑input ↓output` token counts, with an
overflow count for the rest. Input includes uncached, cache-read, and
cache-write tokens and advances when the provider reports usage. Output
advances from settled provider usage, plus a `~`-marked, throttled estimate
from streamed text/reasoning deltas while generating; retries discard the
provisional estimate. Neither estimate nor UI refresh changes billed usage,
budgets, or cost. Providers without streamed deltas cannot provide live token
progress. The marker blinks and the block
remains the mutable tail; later parent output is placed above it. Once the
workers settle, the child lines disappear and the same block fixes in its
completion position with a green marker for all success, red for all failure,
or yellow for mixed outcomes. Raw calls, arguments, worker prompts and costs
remain hidden. On session hydration, durable call/results without worker
telemetry yield a neutral “activity recorded” row, not a claim
that a spawned child has completed; only proven orchestration failures colour
that restored row as failed. `/subagents` retains the detailed roster,
reasons, usage, and read-only child transcripts after settlement. No duplicate
pinned strip or automatic per-worker notices are created. This does not
suppress ordinary tool/run failures or approval prompts, or alter model-visible
errors, durable results, or accounting.

Opt-in provider readiness (`queued`, `loading`, or `ready`) is live activity,
not a fourth transcript layer. It replaces one mutable request-status row with a
friendly provider label and bounded sanitized detail; real model output or a
terminal outcome removes it. It is never retained as conversation text, session
content, or default telemetry.

## Surface and geometry contract

Transcript surfaces, the composer, footer, and pickers resolve one shared
horizontal grid. In the default theme, full-width rules and event/prompt
markers begin at terminal column 0; primary text begins at column 2; and
nested detail begins at column 4. Narrow pickers collapse to compact rows,
regular terminals stack labels over metadata, and genuinely wide terminals may
use columns. The composer keeps stable full-width top and bottom rules and no
side borders, so copied draft text cannot include frame characters. Its height
grows proportionally but remains bounded by terminal height. Default tool/shell
headers and output share a two-cell right gutter (reduced safely in tiny panes);
this is layout space, never part of source or semantic copy text. Code surfaces and
table columns use viewport-derived geometry so growing payloads do not repeatedly
resize earlier rows.

There is exactly one breathing row between transcript content and the composer.
The composer does not animate or recolour merely because work starts or draft
text changes; transcript activity owns liveness. Assistant prose and submitted
prompts use the available width after their outer gutter, including list/quote
indentation; code, diffs, diagrams and tables retain their existing viewport
geometry. The footer is one quiet line:
`model · reasoning · context%/limit · cost` on the left, with the workspace path
right-aligned. Values remain live and model-bound, not fixed example text. Home
paths use `~`; long paths shorten from the left or disappear before left-hand
metadata is sacrificed. Narrow layouts compact or drop whole secondary fields
before primary identity. Estimated context retains `~`, unavailable cost is not
invented, and uncertain usage remains an explicit subtotal plus unknown amount.

Context composition is a semantic timeline. Segments run left-to-right in the
order the model receives them, from system/provider framing and tool schemas
through chronological messages and pending adjustments to output reserve and
remaining capacity. Every displayed category has its own colour; categories
must not be duplicated merely to create a separate accounting slice.

Queued steering is a pending-state hint, not a second transcript. It occupies at
most two rows: one count and one clipped preview of the oldest queued message,
with a compact count for additional messages.

## Approval contract

An approval panel is an enforcement surface, not decorative chrome.

- The prompt and bounded consequence detail are retained separately from the two
  action labels, so identical descriptions render once without being discarded.
- Consequence detail is terminal-sanitized, wraps inside the shared inset, and is
  capped at three rows with an explicit omission marker.
- At constrained heights, the selected action row takes priority over detail.
  Enter cannot confirm a confirmation action unless that selected action is
  present in the rendered panel frame.
- Confirmation panels do not expose a filter or item count; arbitrary typing
  cannot mutate their choices.

## Outcome contract

Terminal outcomes must remain distinguishable after animation stops:

- normal completion uses the success glyph and `completed`;
- completion with warnings uses the warning glyph and the explicit
  `completed with warnings` label;
- interruption remains a warning-class terminal state; and
- failure uses the error glyph plus `failed` and elapsed time.

A collapsed failure always retains a useful reason immediately below its
headline. The reason is credential-redacted at the inference boundary,
terminal-sanitized again for presentation, bounded to 4 KiB at a UTF-8 boundary,
and included in semantic copy. Raw envelopes and headers remain diagnostic
evidence rather than transcript copy.

Visible tool failures follow the same rule: collapsed rows retain a bounded actionable
summary while complete captured output remains available through disclosure when
it exists.

## Interaction tone

The default should be calm, dense under pressure, and precise about state. Any
startup byte shimmer is subtle, non-blocking, finite, and safe to disable on
limited or reduced-motion terminals. The complete byte remains identifiable
without animation, and input never waits for it. Progressive
disclosure keeps raw detail one action away without imposing a dashboard.

A useful internal rule is: **calm by default, detail on demand, raw truth one
keystroke away**. Any future theme or extension must preserve the default
hierarchy before adding options.
